//! Common RFC 011 typed-fact projectors.
//!
//! This is the only boundary that translates adapter facts into storage.
//! Adapters never receive a SQLite handle, table name, or change-log topic.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use rusqlite::{
    params, params_from_iter, Connection, OptionalExtension, Params, Transaction,
    TransactionBehavior,
};

use crate::adapter::{
    DelegationFact, DelegationKind, DelegationMetadataFact, DelegationSpawnFact, EntityKey,
    EvidenceKind, EvidenceStrength, Fact, FactBatch, FactEnvelope, MessageRole, QualifiedTimestamp,
    RawRetentionPolicy, RelationStrength, SessionFact, TimestampQuality, TokenUsage,
    UsageAccounting, UsageFact, UsageScope, ValueQuality,
};

use super::artifact_projection::apply_artifact_facts;
use super::commit::{
    apply_observation_commit_with_projection, apply_observation_commit_with_projection_and_hook,
    apply_observation_commit_with_projection_in_transaction, ChangeEntry, CommitDetail, CommitHook,
    CommitReceipt, ObservationCommit, ProjectionCommitContext, TransactionalProjectionWork,
};
use super::memory_projection::apply_project_memory_facts;
use super::presence_projection::apply_presence_facts;
use super::session_index_projection::apply_session_index_facts;
use super::settings_projection::apply_interpretation_settings_facts;
use super::storage_codec::{omitted, BlobEncoder, EncodedBlob};
use super::task_projection::apply_task_snapshots;
use super::team_projection::apply_team_snapshots;
use super::timeline_projection::{
    insert_message_content_blocks, replace_message_content_blocks, MessageContentBlocks,
};
use super::tool_result_projection::{
    apply_persisted_tool_result_facts, replace_message_references, replaced_message_reference_keys,
};
use super::workflow_projection::apply_workflow_facts;
use super::EngineError;

const CHANGE_SCHEMA_VERSION: u32 = 1;
const FACT_INSERT_BATCH_ROWS: usize = 512;
const MESSAGE_INSERT_BATCH_ROWS: usize = 256;
const RUN_EVIDENCE_INSERT_BATCH_ROWS: usize = 512;

/// Submit one already-decoded fact batch through the catalog/projection/cursor
/// transaction. Public changes and the durable fact count are derived here;
/// callers cannot supply adapter-owned event topics for typed commits.
pub(super) fn apply_fact_observation_commit(
    connection: &mut Connection,
    request: &ObservationCommit,
    batch: &FactBatch,
) -> Result<CommitReceipt, EngineError> {
    apply_fact_observation_commit_inner(connection, request, batch, None)
}

pub(super) fn apply_fact_observation_commit_with_hook(
    connection: &mut Connection,
    request: &ObservationCommit,
    batch: &FactBatch,
    hook: &dyn CommitHook,
) -> Result<CommitReceipt, EngineError> {
    apply_fact_observation_commit_inner(connection, request, batch, Some(hook))
}

fn apply_fact_observation_commit_inner(
    connection: &mut Connection,
    request: &ObservationCommit,
    batch: &FactBatch,
    hook: Option<&dyn CommitHook>,
) -> Result<CommitReceipt, EngineError> {
    let (request, projection) = prepare_fact_observation_commit(request, batch, hook)?;
    match hook {
        Some(hook) => apply_observation_commit_with_projection_and_hook(
            connection,
            &request,
            &projection,
            hook,
        ),
        None => apply_observation_commit_with_projection(connection, &request, &projection),
    }
}

pub(super) fn apply_fact_observation_commit_in_transaction(
    transaction: &Transaction<'_>,
    request: &ObservationCommit,
    batch: &FactBatch,
    hook: &dyn CommitHook,
    persist_public_changes: bool,
    query_bootstrap: bool,
) -> Result<CommitReceipt, EngineError> {
    let (request, projection) = prepare_fact_observation_commit(request, batch, Some(hook))?;
    apply_observation_commit_with_projection_in_transaction(
        transaction,
        &request,
        &projection,
        hook,
        persist_public_changes,
        query_bootstrap,
    )
}

fn prepare_fact_observation_commit<'a>(
    request: &ObservationCommit,
    batch: &'a FactBatch,
    hook: Option<&'a dyn CommitHook>,
) -> Result<(ObservationCommit, FactProjectionWork<'a>), EngineError> {
    if !request.changes.is_empty() {
        return Err(EngineError::InvalidCommit(
            "typed fact commits cannot supply public changes".to_string(),
        ));
    }
    let fact_count = u32::try_from(batch.facts().len()).map_err(|_| {
        EngineError::InvalidCommit("fact batch exceeds durable count range".to_string())
    })?;
    // A multi-record append commit can legitimately end with a known ignored
    // record (for example telemetry following a message). In that case the
    // last fact precedes the committed driver cursor. Provenance is validated
    // per fact below; requiring equality here would make batching depend on
    // every native record producing a common fact.
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
    let encoded_payloads = encode_fact_payloads(batch, request.stream.retention)?;
    let redundant_activity_owners =
        if super::ingest_profile::IngestProfileSkip::current().activity_evidence_ownership {
            BTreeMap::new()
        } else {
            redundant_activity_evidence_owners(batch)
        };
    let projection = FactProjectionWork {
        batch,
        encoded_payloads,
        redundant_activity_owners,
        retention: request.stream.retention,
        delegation_run_invalidations: RefCell::new(BTreeSet::new()),
        hook,
    };
    Ok((request, projection))
}

struct FactProjectionWork<'a> {
    batch: &'a FactBatch,
    encoded_payloads: Vec<EncodedFactPayload>,
    /// A message already proves activity for its run at the same source
    /// record and timestamp. Keep the adapter's richer activity dimensions,
    /// but let the durable message fact own that evidence instead of writing
    /// a second provenance row for the identical observation.
    redundant_activity_owners: BTreeMap<Vec<u8>, Vec<u8>>,
    retention: RawRetentionPolicy,
    /// Runs whose presence changed in this logical commit. Delegation
    /// resolution depends on presence, not on the latest provenance-bearing
    /// RunFact. Ordinary transcript batches restate the same run, so treating
    /// every upsert as an invalidation repeatedly re-reduced all of its child
    /// delegations during cold ingestion.
    delegation_run_invalidations: RefCell<BTreeSet<Vec<u8>>>,
    hook: Option<&'a dyn CommitHook>,
}

#[derive(Debug)]
struct EncodedFactPayload {
    audit: EncodedBlob,
    message_raw: Option<EncodedBlob>,
    message_content: Option<EncodedBlob>,
}

impl TransactionalProjectionWork for FactProjectionWork<'_> {
    fn apply_canonical(
        &self,
        transaction: &Transaction<'_>,
        context: &ProjectionCommitContext,
    ) -> Result<Vec<ChangeEntry>, EngineError> {
        let history_started = Instant::now();
        if self.retention != context.retention {
            return Err(EngineError::InvalidCommit(
                "encoded fact retention does not match the committed stream policy".to_string(),
            ));
        }
        validate_batch_provenance(self.batch, context)?;
        let (
            retracted_run_keys,
            mut changed_session_keys,
            mut changed_tool_references,
            mut changes,
        ) = if context.replaces_prior_generation {
            (
                    old_generation_keys(
                        transaction,
                        "SELECT DISTINCT run_key FROM canonical_runs WHERE source_object_id = ?1 AND source_generation <> ?2",
                        context,
                        "read replaced canonical runs",
                    )?,
                    old_generation_keys(
                        transaction,
                        "SELECT DISTINCT session_key FROM canonical_sessions WHERE source_object_id = ?1 AND source_generation <> ?2",
                        context,
                        "read replaced canonical sessions",
                    )?,
                    replaced_message_reference_keys(transaction, context)?,
                    retract_canonical_generation(transaction, context)?,
                )
        } else {
            (
                BTreeSet::new(),
                BTreeSet::new(),
                BTreeSet::new(),
                Vec::new(),
            )
        };
        let existing_run_keys = existing_run_keys(transaction, self.batch)?;
        let mut delegation_run_invalidations = retracted_run_keys;
        let coalesced_sessions = coalesced_session_projections(self.batch);
        let last_run_facts = last_run_fact_indices(self.batch);
        let existing_message_keys = existing_message_keys(transaction, self.batch)?;
        let mut seen_message_keys = BTreeSet::new();
        let duplicate_message_keys = duplicate_message_keys(self.batch);
        let mut new_message_content = Vec::new();
        self.record_detail(CommitDetail::HistoryPreparation, history_started);
        let fact_storage_started = Instant::now();
        if !super::ingest_profile::IngestProfileSkip::current().facts {
            persist_facts(
                transaction,
                context,
                self.batch,
                &self.encoded_payloads,
                &self.redundant_activity_owners,
            )?;
        }
        self.record_detail(CommitDetail::FactStorage, fact_storage_started);
        let message_storage_started = Instant::now();
        if !super::ingest_profile::IngestProfileSkip::current().messages {
            persist_canonical_messages(transaction, context, self.batch, &self.encoded_payloads)?;
        }
        self.record_detail(
            CommitDetail::CanonicalMessageStorage,
            message_storage_started,
        );
        let projection_walk_started = Instant::now();
        for (index, envelope) in self.batch.facts().iter().enumerate() {
            match &envelope.value {
                Fact::Session(original) => {
                    let Some((fact, last_index)) =
                        coalesced_sessions.get(original.session.as_bytes())
                    else {
                        return Err(EngineError::InvalidCommit(
                            "session projection coalescing lost an input fact".to_string(),
                        ));
                    };
                    if *last_index != index {
                        continue;
                    }
                    if !session_source_wins(transaction, context, fact.session.as_bytes())? {
                        continue;
                    }
                    changed_session_keys.insert(fact.session.as_bytes().to_vec());
                    execute_cached(
                        transaction,
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
                    let message_key = fact.message.as_bytes();
                    let replaces_existing_message = existing_message_keys.contains(message_key)
                        || !seen_message_keys.insert(message_key.to_vec());
                    changes.push(upsert_change(
                        "history.message.changed",
                        fact.message.as_bytes(),
                        envelope,
                    )?);
                    if !super::ingest_profile::IngestProfileSkip::current().messages {
                        changed_tool_references.extend(replace_message_references(
                            transaction,
                            context,
                            message_key,
                            fact.session.as_bytes(),
                            &fact.content,
                            replaces_existing_message,
                        )?);
                        if replaces_existing_message || duplicate_message_keys.contains(message_key)
                        {
                            replace_message_content_blocks(
                                transaction,
                                message_key,
                                fact.session.as_bytes(),
                                fact.run.as_bytes(),
                                &fact.content,
                                replaces_existing_message,
                            )?;
                        } else {
                            new_message_content.push(MessageContentBlocks {
                                message_key,
                                session_key: fact.session.as_bytes(),
                                run_key: fact.run.as_bytes(),
                                content: &fact.content,
                            });
                        }
                    }
                }
                Fact::Run(fact) => {
                    if last_run_facts.get(fact.run.as_bytes()) != Some(&index) {
                        continue;
                    }
                    if !existing_run_keys.contains(fact.run.as_bytes()) {
                        delegation_run_invalidations.insert(fact.run.as_bytes().to_vec());
                    }
                    execute_cached(
                        transaction,
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
                Fact::Delegation(_)
                | Fact::SessionIndexSnapshot(_)
                | Fact::ProjectMemoryDocument(_)
                | Fact::PersistedToolResult(_)
                | Fact::InterpretationSettings(_)
                | Fact::DelegationMetadata(_)
                | Fact::DelegationSpawn(_)
                | Fact::TeamSnapshot(_)
                | Fact::TeamInboxSnapshot(_)
                | Fact::Presence(_)
                | Fact::TaskSnapshot(_)
                | Fact::PlanSnapshot(_)
                | Fact::ArtifactMetadataSnapshot(_)
                | Fact::ArtifactContent(_)
                | Fact::WorkflowSnapshot(_)
                | Fact::WorkflowMemberEvent(_)
                | Fact::RunEvidence(_)
                | Fact::Usage(_) => {}
            }
        }
        self.record_detail(CommitDetail::HistoryProjectionWalk, projection_walk_started);
        let content_block_storage_started = Instant::now();
        if !super::ingest_profile::IngestProfileSkip::current().messages {
            insert_message_content_blocks(transaction, &new_message_content)?;
        }
        self.record_detail(
            CommitDetail::ContentBlockStorage,
            content_block_storage_started,
        );
        self.delegation_run_invalidations
            .replace(delegation_run_invalidations);
        self.record_detail(CommitDetail::HistoryAndFactStorage, history_started);
        if !super::ingest_profile::IngestProfileSkip::current().extras {
            changes.extend(self.measure(CommitDetail::SessionIndex, || {
                apply_session_index_facts(transaction, context, self.batch, &changed_session_keys)
            })?);
            changes.extend(self.measure(CommitDetail::ProjectMemory, || {
                apply_project_memory_facts(transaction, context, self.batch)
            })?);
            changes.extend(self.measure(CommitDetail::PersistedToolResult, || {
                apply_persisted_tool_result_facts(
                    transaction,
                    context,
                    self.batch,
                    &changed_tool_references,
                )
            })?);
            changes.extend(self.measure(CommitDetail::InterpretationSettings, || {
                apply_interpretation_settings_facts(transaction, context, self.batch)
            })?);
        }
        Ok(changes)
    }

    fn apply_runtime(
        &self,
        transaction: &Transaction<'_>,
        context: &ProjectionCommitContext,
    ) -> Result<Vec<ChangeEntry>, EngineError> {
        let run_state_started = Instant::now();
        let mut changes = Vec::new();
        if !super::ingest_profile::IngestProfileSkip::current().runtime {
            let mut affected_states = BTreeSet::new();
            if context.replaces_prior_generation {
                affected_states = old_generation_keys(
                    transaction,
                    "SELECT DISTINCT run_key FROM run_evidence WHERE source_object_id = ?1 AND source_generation <> ?2",
                    context,
                    "read replaced run evidence",
                )?;
                execute_cached(
                    transaction,
                    "DELETE FROM run_evidence WHERE source_object_id = ?1 AND source_generation <> ?2",
                    params![
                        sqlite_u64(context.source_object_id, "source object id")?,
                        sqlite_u64(context.generation, "source generation")?,
                    ],
                )
                .map_err(|error| sqlite_error("retract replaced run evidence", error))?;
            }

            persist_run_evidence(
                transaction,
                context,
                self.batch,
                &self.redundant_activity_owners,
                &mut affected_states,
            )?;

            changes.reserve(affected_states.len());
            for run_key in affected_states {
                // run_evidence stores compact per-source/category reductions,
                // so its bounded SQL reduction is authoritative for both
                // appends and generation replacement. In particular, an
                // UPSERT may update the winning fact_id through ON UPDATE
                // CASCADE before this state row is refreshed.
                let state = reduce_run_state(transaction, &run_key, context.commit_seq)?;
                changes.push(state_change(&run_key, state.as_deref())?);
            }
        }
        self.record_detail(CommitDetail::RunState, run_state_started);

        let delegation_started = Instant::now();
        let has_delegation_fact = self.batch.facts().iter().any(|envelope| {
            matches!(
                envelope.value,
                Fact::Delegation(_) | Fact::DelegationMetadata(_) | Fact::DelegationSpawn(_)
            )
        });
        let changed_runs = self.delegation_run_invalidations.borrow().clone();
        let has_run_fact = !changed_runs.is_empty();
        let skip_delegation = if super::ingest_profile::IngestProfileSkip::current().delegation {
            true
        } else if context.skip_unowned_replace_document(has_delegation_fact)
            && !context.replaces_prior_generation
        {
            !has_run_fact || !runs_need_delegation_reduce(transaction, &changed_runs)?
        } else {
            false
        };
        self.record_detail(CommitDetail::DelegationProbe, delegation_started);
        if skip_delegation {
            self.record_detail(CommitDetail::Delegation, delegation_started);
            changes.extend(self.measure(CommitDetail::Presence, || {
                apply_presence_facts(transaction, context, self.batch)
            })?);
            changes.extend(self.measure(CommitDetail::Team, || {
                apply_team_snapshots(transaction, context, self.batch)
            })?);
            changes.extend(self.measure(CommitDetail::Task, || {
                apply_task_snapshots(transaction, context, self.batch)
            })?);
            if !super::ingest_profile::IngestProfileSkip::current().artifact {
                changes.extend(self.measure(CommitDetail::Artifact, || {
                    apply_artifact_facts(transaction, context, self.batch, self.hook)
                })?);
            }
            changes.extend(self.measure(CommitDetail::Workflow, || {
                apply_workflow_facts(transaction, context, self.batch)
            })?);
            return Ok(changes);
        }

        let delegation_projection_started = Instant::now();
        let mut affected_delegations = BTreeSet::new();
        if context.replaces_prior_generation {
            affected_delegations = old_generation_keys(
                transaction,
                "SELECT DISTINCT child_run_key FROM delegation_assertions WHERE source_object_id = ?1 AND source_generation <> ?2",
                context,
                "read replaced delegation assertions",
            )?;
            transaction
                .execute(
                    "DELETE FROM delegation_assertions WHERE source_object_id = ?1 AND source_generation <> ?2",
                    params![
                        sqlite_u64(context.source_object_id, "source object id")?,
                        sqlite_u64(context.generation, "source generation")?,
                    ],
                )
                .map_err(|error| sqlite_error("retract replaced delegation assertions", error))?;
        }

        for envelope in self.batch.facts() {
            let Fact::Delegation(fact) = &envelope.value else {
                continue;
            };
            if let Some(previous_child) = transaction
                .query_row(
                    "SELECT child_run_key FROM delegation_assertions WHERE fact_id = ?1",
                    [envelope.id.as_bytes().as_slice()],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .optional()
                .map_err(|error| sqlite_error("read prior delegation assertion", error))?
            {
                affected_delegations.insert(previous_child);
            }
            write_delegation_assertion(transaction, context, envelope, fact)?;
            affected_delegations.insert(fact.child_run.as_bytes().to_vec());
        }

        // Delegation metadata is a replaceable snapshot fact. Every commit for
        // one source object replaces that object's prior metadata assertions,
        // even when the source generation is unchanged or the replacement is
        // empty because the sidecar was deleted. Both the old and new child
        // keys can gain or lose a native spawn correlation.
        let has_delegation_metadata = self
            .batch
            .facts()
            .iter()
            .any(|envelope| matches!(envelope.value, Fact::DelegationMetadata(_)));
        let mut affected_metadata = if context
            .skip_unowned_replace_document(has_delegation_metadata)
        {
            BTreeSet::new()
        } else {
            source_object_keys(
                transaction,
                "SELECT DISTINCT child_run_key FROM delegation_metadata_assertions WHERE source_object_id = ?1",
                context,
                "read replaced delegation metadata",
            )?
        };
        let owns_delegation_metadata = has_delegation_metadata || !affected_metadata.is_empty();
        if owns_delegation_metadata {
            affected_delegations.extend(affected_metadata.iter().cloned());
            transaction
                .execute(
                    "DELETE FROM delegation_metadata_assertions WHERE source_object_id = ?1",
                    [sqlite_u64(context.source_object_id, "source object id")?],
                )
                .map_err(|error| sqlite_error("retract replaced delegation metadata", error))?;
        }

        for envelope in self.batch.facts() {
            let Fact::DelegationMetadata(fact) = &envelope.value else {
                continue;
            };
            write_delegation_metadata_assertion(transaction, context, envelope, fact)?;
            let child_run_key = fact.child_run.as_bytes().to_vec();
            affected_metadata.insert(child_run_key.clone());
            affected_delegations.insert(child_run_key);
        }

        // Transcript streams are append/replay sources, so spawn assertions
        // retract only when the object generation changes. Capture joined
        // child keys before deletion so a disappeared native match can fall
        // back to weaker durable layout evidence in the same transaction.
        let mut affected_spawns = BTreeSet::new();
        if context.replaces_prior_generation {
            affected_delegations.extend(old_generation_keys(
                transaction,
                r#"
                SELECT DISTINCT metadata.child_run_key
                FROM delegation_spawn_assertions AS spawn
                JOIN delegation_metadata_assertions AS metadata
                  ON metadata.session_key = spawn.session_key
                 AND metadata.native_task_id = spawn.native_task_id
                WHERE spawn.source_object_id = ?1
                  AND spawn.source_generation <> ?2
                "#,
                context,
                "read replaced spawn correlations",
            )?);
            affected_spawns = old_generation_keys(
                transaction,
                "SELECT DISTINCT spawn_key FROM delegation_spawn_assertions WHERE source_object_id = ?1 AND source_generation <> ?2",
                context,
                "read replaced delegation spawns",
            )?;
            transaction
                .execute(
                    "DELETE FROM delegation_spawn_assertions WHERE source_object_id = ?1 AND source_generation <> ?2",
                    params![
                        sqlite_u64(context.source_object_id, "source object id")?,
                        sqlite_u64(context.generation, "source generation")?,
                    ],
                )
                .map_err(|error| sqlite_error("retract replaced delegation spawns", error))?;
        }

        for envelope in self.batch.facts() {
            let Fact::DelegationSpawn(fact) = &envelope.value else {
                continue;
            };
            if let Some(previous_spawn) = transaction
                .query_row(
                    "SELECT spawn_key FROM delegation_spawn_assertions WHERE fact_id = ?1",
                    [envelope.id.as_bytes().as_slice()],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .optional()
                .map_err(|error| sqlite_error("read prior delegation spawn", error))?
            {
                affected_delegations
                    .extend(correlated_children_for_spawn(transaction, &previous_spawn)?);
                affected_spawns.insert(previous_spawn);
            }
            write_delegation_spawn_assertion(transaction, context, envelope, fact)?;
            affected_spawns.insert(fact.spawn.as_bytes().to_vec());
        }
        for spawn_key in &affected_spawns {
            affected_delegations.extend(correlated_children_for_spawn(transaction, spawn_key)?);
        }

        for run_key in &changed_runs {
            affected_delegations.extend(delegation_children_for_run(transaction, run_key)?);
            affected_delegations.extend(correlated_children_for_run(transaction, run_key)?);
            affected_spawns.extend(delegation_spawns_for_parent(transaction, run_key)?);
            affected_metadata.extend(delegation_metadata_children_for_run(transaction, run_key)?);
        }

        self.record_detail(
            CommitDetail::DelegationProjection,
            delegation_projection_started,
        );
        let delegation_reductions_started = Instant::now();
        if !super::ingest_profile::IngestProfileSkip::current().delegation_reductions {
            for spawn_key in affected_spawns {
                let reduction =
                    reduce_delegation_spawn(transaction, &spawn_key, context.commit_seq)?;
                changes.push(delegation_spawn_change(&spawn_key, &reduction)?);
                changes.push(delegation_spawn_conflict_change(&spawn_key, &reduction)?);
            }

            for child_run_key in affected_metadata {
                let reduction =
                    reduce_delegation_metadata(transaction, &child_run_key, context.commit_seq)?;
                changes.push(delegation_metadata_change(&child_run_key, &reduction)?);
                changes.push(delegation_metadata_conflict_change(
                    &child_run_key,
                    &reduction,
                )?);
            }

            // Native correlation must reduce after both source halves are durable,
            // so metadata-first and transcript-first arrival produce the same row.
            for child_run_key in affected_delegations {
                let reduction = reduce_delegation(transaction, &child_run_key, context.commit_seq)?;
                changes.push(delegation_change(&child_run_key, &reduction)?);
                changes.push(delegation_conflict_change(&child_run_key, &reduction)?);
            }
        }
        self.record_detail(
            CommitDetail::DelegationReductions,
            delegation_reductions_started,
        );
        self.record_detail(CommitDetail::Delegation, delegation_started);

        changes.extend(self.measure(CommitDetail::Presence, || {
            apply_presence_facts(transaction, context, self.batch)
        })?);
        changes.extend(self.measure(CommitDetail::Team, || {
            apply_team_snapshots(transaction, context, self.batch)
        })?);
        changes.extend(self.measure(CommitDetail::Task, || {
            apply_task_snapshots(transaction, context, self.batch)
        })?);
        if !super::ingest_profile::IngestProfileSkip::current().artifact {
            changes.extend(self.measure(CommitDetail::Artifact, || {
                apply_artifact_facts(transaction, context, self.batch, self.hook)
            })?);
        }
        changes.extend(self.measure(CommitDetail::Workflow, || {
            apply_workflow_facts(transaction, context, self.batch)
        })?);

        // Delegation metadata is a replace-document fact. Transcript objects
        // reach this shared projector too; scanning their growing fact ledger
        // after every append is both unrelated and quadratic at corpus scale.
        if owns_delegation_metadata {
            transaction
                .execute(
                    r#"
                    DELETE FROM fact_records
                    WHERE source_object_id = ?1
                      AND fact_kind = 'delegation_metadata'
                      AND last_commit_seq <> ?2
                    "#,
                    params![
                        sqlite_u64(context.source_object_id, "source object id")?,
                        sqlite_u64(context.commit_seq, "commit sequence")?,
                    ],
                )
                .map_err(|error| {
                    sqlite_error("retract replaced delegation metadata facts", error)
                })?;
        }
        Ok(changes)
    }

    fn apply_usage(
        &self,
        transaction: &Transaction<'_>,
        context: &ProjectionCommitContext,
    ) -> Result<Vec<ChangeEntry>, EngineError> {
        if super::ingest_profile::IngestProfileSkip::current().usage {
            return Ok(Vec::new());
        }
        let usage_started = Instant::now();
        let mut touched_sessions = BTreeSet::new();
        let mut pending_totals = BTreeMap::new();
        let existing_contribution_ids = existing_usage_contribution_ids(transaction, self.batch)?;
        let mut seen_contribution_ids = BTreeSet::new();
        let replaced = if context.replaces_prior_generation {
            read_replaced_contributions(transaction, context)?
        } else {
            Vec::new()
        };
        for contribution in &replaced {
            adjust_usage_total(transaction, &mut pending_totals, contribution, -1)?;
            touched_sessions.insert(contribution.session_key.clone());
        }
        if context.replaces_prior_generation {
            transaction
                .execute(
                    "DELETE FROM usage_contributions WHERE source_object_id = ?1 AND source_generation <> ?2",
                    params![
                        sqlite_u64(context.source_object_id, "source object id")?,
                        sqlite_u64(context.generation, "source generation")?,
                    ],
                )
                .map_err(|error| sqlite_error("retract replaced usage contributions", error))?;
        }

        for envelope in self.batch.facts() {
            let Fact::Usage(fact) = &envelope.value else {
                continue;
            };
            let fact_id = envelope.id.as_bytes().as_slice();
            let replaces_existing = existing_contribution_ids.contains(fact_id)
                || !seen_contribution_ids.insert(fact_id.to_vec());
            let previous = if replaces_existing {
                read_contribution(transaction, fact_id)?
            } else {
                None
            };
            if let Some(previous) = previous.as_ref() {
                adjust_usage_total(transaction, &mut pending_totals, previous, -1)?;
                touched_sessions.insert(previous.session_key.clone());
                delete_contribution(transaction, envelope.id.as_bytes())?;
            }

            let series_key = usage_series_key(context, fact);
            if fact.accounting == UsageAccounting::Snapshot {
                for replaced in read_snapshot_series_contributions(
                    transaction,
                    &series_key,
                    context.generation,
                )? {
                    adjust_usage_total(transaction, &mut pending_totals, &replaced, -1)?;
                    touched_sessions.insert(replaced.session_key.clone());
                    delete_contribution(transaction, &replaced.fact_id)?;
                }
            }

            let reported = token_values(fact.values)?;
            let values = if fact.accounting == UsageAccounting::Cumulative {
                cumulative_values(
                    reported,
                    read_cumulative_predecessor(
                        transaction,
                        &series_key,
                        context.generation,
                        &envelope.provenance.cursor_end,
                    )?
                    .map(|contribution| contribution.reported),
                )?
            } else {
                reported
            };
            let contribution = UsageContribution::from_fact(
                envelope.id.as_bytes(),
                fact,
                series_key,
                reported,
                values,
                context,
                &envelope.provenance.cursor_end,
            )?;
            write_contribution(transaction, context, envelope, fact, &contribution)?;
            adjust_usage_total(transaction, &mut pending_totals, &contribution, 1)?;
            touched_sessions.insert(contribution.session_key.clone());

            if let Some(previous) = previous {
                if previous.accounting == UsageAccounting::Cumulative
                    && (previous.series_key != contribution.series_key
                        || previous.source_generation != contribution.source_generation)
                {
                    if let Some(session_key) = repair_cumulative_successor(
                        transaction,
                        &previous.series_key,
                        previous.source_generation,
                        &previous.cursor_end,
                        context.commit_seq,
                        &mut pending_totals,
                    )? {
                        touched_sessions.insert(session_key);
                    }
                }
            }
            if fact.accounting == UsageAccounting::Cumulative {
                if let Some(session_key) = repair_cumulative_successor(
                    transaction,
                    &contribution.series_key,
                    contribution.source_generation,
                    &contribution.cursor_end,
                    context.commit_seq,
                    &mut pending_totals,
                )? {
                    touched_sessions.insert(session_key);
                }
            }
        }

        // Generation-owned fact rows are removed only after every projector
        // has retracted its dependent state and usage delta.
        if context.replaces_prior_generation {
            transaction
                .execute(
                    "DELETE FROM fact_records WHERE source_object_id = ?1 AND source_generation <> ?2",
                    params![
                        sqlite_u64(context.source_object_id, "source object id")?,
                        sqlite_u64(context.generation, "source generation")?,
                    ],
                )
                .map_err(|error| sqlite_error("retract replaced fact records", error))?;
        }

        flush_usage_totals(transaction, pending_totals, context.commit_seq)?;

        let changes = touched_sessions
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
            .collect();
        self.record_detail(CommitDetail::UsageAggregation, usage_started);
        changes
    }
}

impl FactProjectionWork<'_> {
    fn record_detail(&self, detail: CommitDetail, started_at: Instant) {
        if let Some(hook) = self.hook {
            hook.record_detail(detail, started_at.elapsed());
        }
    }

    fn measure<T>(
        &self,
        detail: CommitDetail,
        operation: impl FnOnce() -> Result<T, EngineError>,
    ) -> Result<T, EngineError> {
        let started_at = Instant::now();
        let result = operation();
        self.record_detail(detail, started_at);
        result
    }
}

fn existing_usage_contribution_ids(
    transaction: &Transaction<'_>,
    batch: &FactBatch,
) -> Result<BTreeSet<Vec<u8>>, EngineError> {
    let ids = batch
        .facts()
        .iter()
        .filter_map(|envelope| {
            matches!(envelope.value, Fact::Usage(_)).then_some(envelope.id.as_bytes().as_slice())
        })
        .collect::<BTreeSet<_>>();
    if ids.is_empty() {
        return Ok(BTreeSet::new());
    }
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("SELECT fact_id FROM usage_contributions WHERE fact_id IN ({placeholders})");
    let mut statement = transaction
        .prepare(&sql)
        .map_err(|error| sqlite_error("prepare existing usage contribution batch", error))?;
    let existing = statement
        .query_map(params_from_iter(ids), |row| row.get::<_, Vec<u8>>(0))
        .map_err(|error| sqlite_error("read existing usage contribution batch", error))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| sqlite_error("collect existing usage contribution batch", error))?;
    Ok(existing)
}

fn coalesced_session_projections(batch: &FactBatch) -> BTreeMap<Vec<u8>, (SessionFact, usize)> {
    let mut sessions = BTreeMap::new();
    for (index, envelope) in batch.facts().iter().enumerate() {
        let Fact::Session(next) = &envelope.value else {
            continue;
        };
        let entry = sessions
            .entry(next.session.as_bytes().to_vec())
            .or_insert_with(|| (next.clone(), index));
        if entry.1 != index {
            merge_session_projection(&mut entry.0, next);
            entry.1 = index;
        }
    }
    sessions
}

fn merge_session_projection(current: &mut SessionFact, next: &SessionFact) {
    current.project = next.project.clone();
    current
        .native_session_id
        .clone_from(&next.native_session_id);
    current
        .native_project_key
        .clone_from(&next.native_project_key);
    replace_if_some(&mut current.cwd, &next.cwd);
    replace_if_some(&mut current.git_branch, &next.git_branch);
    if next.first_prompt.is_some()
        && current
            .first_prompt
            .as_deref()
            .is_none_or(|value| value.trim().is_empty() || value == "No prompt")
    {
        current.first_prompt.clone_from(&next.first_prompt);
    }
    replace_if_some(&mut current.ai_title, &next.ai_title);
    replace_if_some(&mut current.custom_title, &next.custom_title);
    replace_if_some(&mut current.source_time, &next.source_time);
}

fn replace_if_some<T: Clone>(current: &mut Option<T>, next: &Option<T>) {
    if next.is_some() {
        current.clone_from(next);
    }
}

struct SessionSourceIdentity {
    stream_key: String,
    object_key: Vec<u8>,
}

/// Multiple independently scheduled objects can establish the same canonical
/// session. Choose one by adapter-owned stable object identity instead of
/// commit order so cold, live, and restart projections converge without the
/// common engine branching on an adapter or stream convention.
fn session_source_wins(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    session_key: &[u8],
) -> Result<bool, EngineError> {
    let existing_object_id = transaction
        .query_row(
            "SELECT source_object_id FROM canonical_sessions WHERE session_key = ?1",
            [session_key],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|error| sqlite_error("read canonical session source", error))?;
    let Some(existing_object_id) = existing_object_id else {
        return Ok(true);
    };
    let incoming_object_id = sqlite_u64(context.source_object_id, "source object id")?;
    if existing_object_id == incoming_object_id {
        return Ok(true);
    }

    let incoming = session_source_identity(transaction, incoming_object_id)?;
    let existing = session_source_identity(transaction, existing_object_id)?;
    Ok((incoming.object_key, incoming.stream_key) < (existing.object_key, existing.stream_key))
}

fn session_source_identity(
    transaction: &Transaction<'_>,
    source_object_id: i64,
) -> Result<SessionSourceIdentity, EngineError> {
    transaction
        .query_row(
            r#"
            SELECT ss.stream_key, so.object_key
            FROM source_objects so
            JOIN source_streams ss
              ON ss.source_stream_id = so.source_stream_id
            WHERE so.source_object_id = ?1
            "#,
            [source_object_id],
            |row| {
                Ok(SessionSourceIdentity {
                    stream_key: row.get(0)?,
                    object_key: row.get(1)?,
                })
            },
        )
        .map_err(|error| sqlite_error("read session source identity", error))
}

fn last_run_fact_indices(batch: &FactBatch) -> BTreeMap<Vec<u8>, usize> {
    batch
        .facts()
        .iter()
        .enumerate()
        .filter_map(|(index, envelope)| {
            let Fact::Run(fact) = &envelope.value else {
                return None;
            };
            Some((fact.run.as_bytes().to_vec(), index))
        })
        .collect()
}

fn duplicate_message_keys(batch: &FactBatch) -> BTreeSet<Vec<u8>> {
    let mut seen = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    for envelope in batch.facts() {
        let Fact::Message(fact) = &envelope.value else {
            continue;
        };
        let key = fact.message.as_bytes().to_vec();
        if !seen.insert(key.clone()) {
            duplicates.insert(key);
        }
    }
    duplicates
}

fn existing_message_keys(
    transaction: &Transaction<'_>,
    batch: &FactBatch,
) -> Result<BTreeSet<Vec<u8>>, EngineError> {
    let keys = batch
        .facts()
        .iter()
        .filter_map(|envelope| {
            let Fact::Message(fact) = &envelope.value else {
                return None;
            };
            Some(fact.message.as_bytes())
        })
        .collect::<BTreeSet<_>>();
    if keys.is_empty() {
        return Ok(BTreeSet::new());
    }
    let placeholders = std::iter::repeat_n("?", keys.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql =
        format!("SELECT message_key FROM canonical_messages WHERE message_key IN ({placeholders})");
    let mut statement = transaction
        .prepare(&sql)
        .map_err(|error| sqlite_error("prepare existing canonical message batch", error))?;
    let existing = statement
        .query_map(params_from_iter(keys), |row| row.get::<_, Vec<u8>>(0))
        .map_err(|error| sqlite_error("read existing canonical message batch", error))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| sqlite_error("collect existing canonical message batch", error))?;
    Ok(existing)
}

fn existing_run_keys(
    transaction: &Transaction<'_>,
    batch: &FactBatch,
) -> Result<BTreeSet<Vec<u8>>, EngineError> {
    let keys = batch
        .facts()
        .iter()
        .filter_map(|envelope| {
            let Fact::Run(fact) = &envelope.value else {
                return None;
            };
            Some(fact.run.as_bytes())
        })
        .collect::<BTreeSet<_>>();
    if keys.is_empty() {
        return Ok(BTreeSet::new());
    }
    let placeholders = std::iter::repeat_n("?", keys.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("SELECT run_key FROM canonical_runs WHERE run_key IN ({placeholders})");
    let mut statement = transaction
        .prepare(&sql)
        .map_err(|error| sqlite_error("prepare existing canonical run batch", error))?;
    let existing = statement
        .query_map(params_from_iter(keys), |row| row.get::<_, Vec<u8>>(0))
        .map_err(|error| sqlite_error("read existing canonical run batch", error))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| sqlite_error("collect existing canonical run batch", error))?;
    Ok(existing)
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

#[derive(Debug, Clone)]
struct FoldedRunEvidence {
    fact_id: Vec<u8>,
    kind: &'static str,
    kind_rank: i64,
    strength_rank: i64,
    source_generation: i64,
    cursor_end: Vec<u8>,
    last_commit_seq: i64,
    source_time: Option<String>,
    last_activity_at: Option<String>,
}

#[derive(Debug, Clone)]
struct CompactRunEvidence {
    candidate: FoldedRunEvidence,
    run_key: Vec<u8>,
    strength: &'static str,
    native_state: Option<String>,
    source_time_quality: Option<String>,
    evidence_count: i64,
}

type ActivityOwnerKey = (Vec<u8>, Vec<u8>, Vec<u8>, Option<String>, Option<String>);

fn activity_owner_key(
    envelope: &FactEnvelope,
    run_key: &[u8],
    source_time: Option<&QualifiedTimestamp>,
) -> ActivityOwnerKey {
    (
        envelope.provenance.cursor_start.clone(),
        envelope.provenance.cursor_end.clone(),
        run_key.to_vec(),
        timestamp_value(source_time).map(str::to_string),
        timestamp_quality(source_time).map(str::to_string),
    )
}

fn redundant_activity_evidence_owners(batch: &FactBatch) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let mut messages = BTreeMap::<ActivityOwnerKey, Vec<Vec<u8>>>::new();
    let mut activity = BTreeMap::<ActivityOwnerKey, Vec<Vec<u8>>>::new();
    for envelope in batch.facts() {
        match &envelope.value {
            Fact::Message(fact) => {
                let key =
                    activity_owner_key(envelope, fact.run.as_bytes(), fact.source_time.as_ref());
                messages
                    .entry(key)
                    .or_default()
                    .push(envelope.id.as_bytes().to_vec());
            }
            Fact::RunEvidence(fact)
                if fact.kind == EvidenceKind::ActivityObserved
                    && fact.strength == EvidenceStrength::NativeActivity =>
            {
                let key =
                    activity_owner_key(envelope, fact.run.as_bytes(), fact.source_time.as_ref());
                activity
                    .entry(key)
                    .or_default()
                    .push(envelope.id.as_bytes().to_vec());
            }
            _ => {}
        }
    }

    activity
        .into_iter()
        .filter_map(|(key, evidence_ids)| {
            let message_ids = messages.get(&key)?;
            (message_ids.len() == 1 && evidence_ids.len() == 1)
                .then(|| (evidence_ids[0].clone(), message_ids[0].clone()))
        })
        .collect()
}

fn persist_run_evidence(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    batch: &FactBatch,
    redundant_activity_owners: &BTreeMap<Vec<u8>, Vec<u8>>,
    affected_states: &mut BTreeSet<Vec<u8>>,
) -> Result<(), EngineError> {
    let mut summaries = BTreeMap::new();
    let source_object_id = sqlite_u64(context.source_object_id, "source object id")?;
    let source_generation = sqlite_u64(context.generation, "source generation")?;
    let commit_seq = sqlite_u64(context.commit_seq, "commit sequence")?;
    for envelope in batch.facts() {
        let Fact::RunEvidence(fact) = &envelope.value else {
            continue;
        };
        let run_key = fact.run.as_bytes().to_vec();
        let kind = evidence_kind(fact.kind);
        let source_time = timestamp_value(fact.source_time.as_ref()).map(str::to_string);
        let last_activity_at = is_activity_evidence(fact.kind)
            .then(|| source_time.clone())
            .flatten();
        let candidate = FoldedRunEvidence {
            fact_id: redundant_activity_owners
                .get(envelope.id.as_bytes().as_slice())
                .cloned()
                .unwrap_or_else(|| envelope.id.as_bytes().to_vec()),
            kind,
            kind_rank: evidence_kind_rank(fact.kind),
            strength_rank: evidence_strength_rank(fact.strength),
            source_generation,
            cursor_end: envelope.provenance.cursor_end.clone(),
            last_commit_seq: commit_seq,
            source_time,
            last_activity_at,
        };
        affected_states.insert(run_key);
        let strength = evidence_strength(fact.strength);
        let summary_key = (fact.run.as_bytes().to_vec(), kind, strength);
        match summaries.get_mut(&summary_key) {
            Some(CompactRunEvidence {
                candidate: current,
                native_state,
                source_time_quality,
                evidence_count,
                ..
            }) => {
                *evidence_count += 1;
                let last_activity_at = max_optional_time(
                    current.last_activity_at.clone(),
                    candidate.last_activity_at.clone(),
                );
                if run_evidence_outranks(&candidate, current) {
                    *current = candidate;
                    *native_state = fact.native_state.clone();
                    *source_time_quality =
                        timestamp_quality(fact.source_time.as_ref()).map(str::to_string);
                }
                current.last_activity_at = last_activity_at;
            }
            None => {
                summaries.insert(
                    summary_key,
                    CompactRunEvidence {
                        candidate,
                        run_key: fact.run.as_bytes().to_vec(),
                        strength,
                        native_state: fact.native_state.clone(),
                        source_time_quality: timestamp_quality(fact.source_time.as_ref())
                            .map(str::to_string),
                        evidence_count: 1,
                    },
                );
            }
        }
    }

    let rows = summaries.into_values().collect::<Vec<_>>();
    for chunk in rows.chunks(RUN_EVIDENCE_INSERT_BATCH_ROWS) {
        let row = "(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";
        let sql = format!(
            r#"
            INSERT INTO run_evidence (
                fact_id, run_key, evidence_kind, evidence_strength,
                native_state, source_time, source_time_quality,
                source_object_id, source_generation, cursor_end,
                last_commit_seq, evidence_count, last_activity_at
            ) VALUES {}
            ON CONFLICT(
                run_key, source_object_id, source_generation,
                evidence_kind, evidence_strength
            ) DO UPDATE SET
                fact_id = CASE WHEN
                  (excluded.cursor_end, excluded.last_commit_seq, excluded.fact_id) >
                  (run_evidence.cursor_end, run_evidence.last_commit_seq, run_evidence.fact_id)
                  THEN excluded.fact_id ELSE run_evidence.fact_id END,
                native_state = CASE WHEN
                  (excluded.cursor_end, excluded.last_commit_seq, excluded.fact_id) >
                  (run_evidence.cursor_end, run_evidence.last_commit_seq, run_evidence.fact_id)
                  THEN excluded.native_state ELSE run_evidence.native_state END,
                source_time = CASE WHEN
                  (excluded.cursor_end, excluded.last_commit_seq, excluded.fact_id) >
                  (run_evidence.cursor_end, run_evidence.last_commit_seq, run_evidence.fact_id)
                  THEN excluded.source_time ELSE run_evidence.source_time END,
                source_time_quality = CASE WHEN
                  (excluded.cursor_end, excluded.last_commit_seq, excluded.fact_id) >
                  (run_evidence.cursor_end, run_evidence.last_commit_seq, run_evidence.fact_id)
                  THEN excluded.source_time_quality ELSE run_evidence.source_time_quality END,
                cursor_end = CASE WHEN
                  (excluded.cursor_end, excluded.last_commit_seq, excluded.fact_id) >
                  (run_evidence.cursor_end, run_evidence.last_commit_seq, run_evidence.fact_id)
                  THEN excluded.cursor_end ELSE run_evidence.cursor_end END,
                last_commit_seq = CASE WHEN
                  (excluded.cursor_end, excluded.last_commit_seq, excluded.fact_id) >
                  (run_evidence.cursor_end, run_evidence.last_commit_seq, run_evidence.fact_id)
                  THEN excluded.last_commit_seq ELSE run_evidence.last_commit_seq END,
                evidence_count = run_evidence.evidence_count + excluded.evidence_count,
                last_activity_at = CASE
                  WHEN run_evidence.last_activity_at IS NULL THEN excluded.last_activity_at
                  WHEN excluded.last_activity_at IS NULL THEN run_evidence.last_activity_at
                  WHEN excluded.last_activity_at > run_evidence.last_activity_at
                    THEN excluded.last_activity_at
                  ELSE run_evidence.last_activity_at
                END
            "#,
            std::iter::repeat_n(row, chunk.len())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let mut values = Vec::with_capacity(chunk.len() * 13);
        for summary in chunk {
            use rusqlite::types::Value;
            values.push(Value::Blob(summary.candidate.fact_id.clone()));
            values.push(Value::Blob(summary.run_key.clone()));
            values.push(Value::Text(summary.candidate.kind.to_string()));
            values.push(Value::Text(summary.strength.to_string()));
            values.push(optional_text_value(summary.native_state.as_deref()));
            values.push(optional_text_value(
                summary.candidate.source_time.as_deref(),
            ));
            values.push(optional_text_value(summary.source_time_quality.as_deref()));
            values.push(Value::Integer(source_object_id));
            values.push(Value::Integer(source_generation));
            values.push(Value::Blob(summary.candidate.cursor_end.clone()));
            values.push(Value::Integer(commit_seq));
            values.push(Value::Integer(summary.evidence_count));
            values.push(optional_text_value(
                summary.candidate.last_activity_at.as_deref(),
            ));
        }
        let result = if chunk.len() == RUN_EVIDENCE_INSERT_BATCH_ROWS {
            transaction
                .prepare_cached(&sql)
                .and_then(|mut statement| statement.execute(params_from_iter(values.iter())))
        } else {
            transaction
                .prepare(&sql)
                .and_then(|mut statement| statement.execute(params_from_iter(values.iter())))
        };
        result.map_err(|error| sqlite_error("project run evidence batch", error))?;
    }
    Ok(())
}

fn persist_facts(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    batch: &FactBatch,
    payloads: &[EncodedFactPayload],
    redundant_activity_owners: &BTreeMap<Vec<u8>, Vec<u8>>,
) -> Result<(), EngineError> {
    if batch.facts().len() != payloads.len() {
        return Err(EngineError::InvalidCommit(
            "encoded fact payload count did not match fact batch".to_string(),
        ));
    }
    for dependency in batch.dependency_reads() {
        if dependency.source_instance_id != context.source_instance_id
            || dependency.root_name.trim().is_empty()
            || dependency.object_key.is_empty()
            || dependency.revision.iter().all(|byte| *byte == 0)
        {
            return Err(EngineError::InvalidCommit(
                "fact dependency revision is incomplete or crosses source instances".to_string(),
            ));
        }
    }

    let source_instance_id = sqlite_u64(context.source_instance_id, "source instance id")?;
    let source_stream_id = sqlite_u64(context.source_stream_id, "source stream id")?;
    let source_object_id = sqlite_u64(context.source_object_id, "source object id")?;
    let source_generation = sqlite_u64(context.generation, "source generation")?;
    let commit_seq = sqlite_u64(context.commit_seq, "commit sequence")?;
    let durable_facts = batch
        .facts()
        .iter()
        .zip(payloads)
        .filter(|(envelope, _)| {
            !redundant_activity_owners.contains_key(envelope.id.as_bytes().as_slice())
        })
        .collect::<Vec<_>>();
    for fact_chunk in durable_facts.chunks(FACT_INSERT_BATCH_ROWS) {
        let row = "(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";
        let sql = format!(
            r#"
            INSERT INTO fact_records (
                fact_id, fact_kind, entity_key, source_instance_id,
                source_stream_id, source_object_id, source_generation,
                cursor_start, cursor_end, payload_hash, local_fact_ordinal,
                observed_at, payload_json, payload_codec, last_commit_seq
            ) VALUES {}
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
                payload_codec = excluded.payload_codec,
                last_commit_seq = excluded.last_commit_seq
            "#,
            std::iter::repeat_n(row, fact_chunk.len())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let mut values = Vec::with_capacity(fact_chunk.len() * 15);
        for (envelope, payload) in fact_chunk {
            use rusqlite::types::Value;

            values.push(Value::Blob(envelope.id.as_bytes().to_vec()));
            values.push(Value::Text(envelope.value.kind().to_string()));
            // Canonical and assertion projections already retain the semantic
            // entity key and point back to this row by fact_id. Keeping a
            // second copy in the provenance ledger has no query consumer and
            // materially amplifies transcript storage; source provenance is
            // carried by the remaining columns.
            values.push(Value::Null);
            values.push(Value::Integer(source_instance_id));
            values.push(Value::Integer(source_stream_id));
            values.push(Value::Integer(source_object_id));
            values.push(Value::Integer(source_generation));
            values.push(Value::Blob(envelope.provenance.cursor_start.clone()));
            values.push(Value::Blob(envelope.provenance.cursor_end.clone()));
            values.push(Value::Blob(envelope.provenance.record_hash.to_vec()));
            values.push(Value::Integer(i64::from(
                envelope.provenance.local_fact_ordinal,
            )));
            values.push(Value::Integer(envelope.provenance.observed_at));
            values.push(Value::Blob(payload.audit.bytes.clone()));
            values.push(Value::Text(payload.audit.codec.to_string()));
            values.push(Value::Integer(commit_seq));
        }
        let result = if fact_chunk.len() == FACT_INSERT_BATCH_ROWS {
            transaction
                .prepare_cached(&sql)
                .and_then(|mut statement| statement.execute(params_from_iter(values.iter())))
        } else {
            transaction
                .prepare(&sql)
                .and_then(|mut statement| statement.execute(params_from_iter(values.iter())))
        };
        result.map_err(|error| sqlite_error("persist typed fact batch", error))?;
    }

    // A cursor-valid same-generation append cannot revisit an already
    // committed fact ID. A contract/source replay advances the generation and
    // cascades old dependencies with the old fact rows. Therefore an empty
    // dependency set has nothing to replace and should not perform a B-tree
    // lookup for every ordinary history fact.
    if !batch.dependency_reads().is_empty() {
        for envelope in batch.facts() {
            if redundant_activity_owners.contains_key(envelope.id.as_bytes().as_slice()) {
                continue;
            }
            execute_cached(
                transaction,
                "DELETE FROM fact_dependency_reads WHERE fact_id = ?1",
                [envelope.id.as_bytes().as_slice()],
            )
            .map_err(|error| sqlite_error("replace fact dependency reads", error))?;
            for dependency in batch.dependency_reads() {
                transaction
                    .execute(
                        r#"
                INSERT INTO fact_dependency_reads (
                    fact_id, source_instance_id, root_name, object_key, revision
                ) VALUES (?1, ?2, ?3, ?4, ?5)
                "#,
                        params![
                            envelope.id.as_bytes().as_slice(),
                            sqlite_u64(
                                dependency.source_instance_id,
                                "dependency source instance"
                            )?,
                            dependency.root_name,
                            dependency.object_key,
                            dependency.revision.as_slice(),
                        ],
                    )
                    .map_err(|error| sqlite_error("persist fact dependency read", error))?;
            }
        }
    }
    Ok(())
}

fn persist_canonical_messages(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    batch: &FactBatch,
    payloads: &[EncodedFactPayload],
) -> Result<(), EngineError> {
    if batch.facts().len() != payloads.len() {
        return Err(EngineError::InvalidCommit(
            "encoded fact payload count did not match fact batch".to_string(),
        ));
    }
    let messages = batch
        .facts()
        .iter()
        .zip(payloads)
        .filter_map(|(envelope, payload)| match &envelope.value {
            Fact::Message(fact) => Some((envelope, fact, payload)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let source_object_id = sqlite_u64(context.source_object_id, "source object id")?;
    let source_generation = sqlite_u64(context.generation, "source generation")?;
    let commit_seq = sqlite_u64(context.commit_seq, "commit sequence")?;
    for chunk in messages.chunks(MESSAGE_INSERT_BATCH_ROWS) {
        let row = "(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";
        let sql = format!(
            r#"
            INSERT INTO canonical_messages (
                message_key, session_key, run_key, native_message_id,
                native_kind, role, content_json, content_json_codec, source_time,
                source_time_quality, parent_native_message_id, model,
                search_text, raw_json, raw_json_codec, fact_id,
                source_object_id, source_generation, cursor_start,
                cursor_end, last_commit_seq
            ) VALUES {}
            ON CONFLICT(message_key) DO UPDATE SET
                session_key = excluded.session_key,
                run_key = excluded.run_key,
                native_message_id = excluded.native_message_id,
                native_kind = excluded.native_kind,
                role = excluded.role,
                content_json = excluded.content_json,
                content_json_codec = excluded.content_json_codec,
                source_time = excluded.source_time,
                source_time_quality = excluded.source_time_quality,
                parent_native_message_id = excluded.parent_native_message_id,
                model = excluded.model,
                search_text = excluded.search_text,
                raw_json = excluded.raw_json,
                raw_json_codec = excluded.raw_json_codec,
                fact_id = excluded.fact_id,
                source_object_id = excluded.source_object_id,
                source_generation = excluded.source_generation,
                cursor_start = excluded.cursor_start,
                cursor_end = excluded.cursor_end,
                last_commit_seq = excluded.last_commit_seq
            "#,
            std::iter::repeat_n(row, chunk.len())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let mut values = Vec::with_capacity(chunk.len() * 21);
        for (envelope, fact, payload) in chunk {
            use rusqlite::types::Value;

            let native_payload = payload.message_raw.as_ref().ok_or_else(|| {
                EngineError::InvalidCommit(
                    "message fact is missing encoded native payload".to_string(),
                )
            })?;
            let canonical_content = payload.message_content.as_ref().ok_or_else(|| {
                EngineError::InvalidCommit(
                    "message fact is missing encoded canonical content".to_string(),
                )
            })?;
            values.push(Value::Blob(fact.message.as_bytes().to_vec()));
            values.push(Value::Blob(fact.session.as_bytes().to_vec()));
            values.push(Value::Blob(fact.run.as_bytes().to_vec()));
            values.push(optional_text_value(fact.native_message_id.as_deref()));
            values.push(Value::Text(fact.native_kind.clone()));
            values.push(Value::Text(message_role(&fact.role).to_string()));
            values.push(Value::Blob(canonical_content.bytes.clone()));
            values.push(Value::Text(canonical_content.codec.to_string()));
            values.push(optional_text_value(timestamp_value(
                fact.source_time.as_ref(),
            )));
            values.push(optional_text_value(timestamp_quality(
                fact.source_time.as_ref(),
            )));
            values.push(optional_text_value(
                fact.parent_native_message_id.as_deref(),
            ));
            values.push(optional_text_value(fact.model.as_deref()));
            values.push(optional_text_value(fact.search_text.as_deref()));
            values.push(Value::Blob(native_payload.bytes.clone()));
            values.push(Value::Text(native_payload.codec.to_string()));
            values.push(Value::Blob(envelope.id.as_bytes().to_vec()));
            values.push(Value::Integer(source_object_id));
            values.push(Value::Integer(source_generation));
            values.push(Value::Blob(envelope.provenance.cursor_start.clone()));
            values.push(Value::Blob(envelope.provenance.cursor_end.clone()));
            values.push(Value::Integer(commit_seq));
        }
        let result = if chunk.len() == MESSAGE_INSERT_BATCH_ROWS {
            transaction
                .prepare_cached(&sql)
                .and_then(|mut statement| statement.execute(params_from_iter(values.iter())))
        } else {
            transaction
                .prepare(&sql)
                .and_then(|mut statement| statement.execute(params_from_iter(values.iter())))
        };
        result.map_err(|error| sqlite_error("project canonical message batch", error))?;
    }
    Ok(())
}

fn optional_text_value(value: Option<&str>) -> rusqlite::types::Value {
    value.map_or(rusqlite::types::Value::Null, |value| {
        rusqlite::types::Value::Text(value.to_string())
    })
}

fn encode_fact_payloads(
    batch: &FactBatch,
    retention: RawRetentionPolicy,
) -> Result<Vec<EncodedFactPayload>, EngineError> {
    let mut encoder = BlobEncoder::new()?;
    batch
        .facts()
        .iter()
        .map(|envelope| {
            // fact_records is the durable provenance/ownership ledger. Full
            // opts into normalized fact bodies. DiagnosticExcerpt retains
            // only the already-redacted bounded UnknownRecord shape; ordinary
            // facts would merely duplicate canonical semantics.
            let retain_audit = retention == RawRetentionPolicy::Full
                || (retention == RawRetentionPolicy::DiagnosticExcerpt
                    && matches!(envelope.value, Fact::UnknownRecord { .. }));
            let audit = if retain_audit {
                let audit_json = serialize(&envelope.value, "serialize fact audit payload")?;
                encoder.encode(&audit_json, "compress fact audit payload")?
            } else {
                omitted()
            };
            let (message_raw, message_content) =
                match &envelope.value {
                    Fact::Message(fact) => {
                        let content = serialize(&fact.content, "serialize canonical content")?;
                        (
                            Some(encoder.encode(
                                &fact.raw_json,
                                "compress canonical native message payload",
                            )?),
                            Some(encoder.encode(
                                &content,
                                "compress canonical normalized message content",
                            )?),
                        )
                    }
                    _ => (None, None),
                };
            Ok(EncodedFactPayload {
                audit,
                message_raw,
                message_content,
            })
        })
        .collect()
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

fn source_object_keys(
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
            [sqlite_u64(context.source_object_id, "source object id")?],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .map_err(|error| sqlite_error(operation, error))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| sqlite_error(operation, error))?;
    Ok(keys)
}

fn run_evidence_outranks(left: &FoldedRunEvidence, right: &FoldedRunEvidence) -> bool {
    (
        left.kind_rank,
        left.strength_rank,
        left.source_generation,
        left.cursor_end.as_slice(),
        left.last_commit_seq,
        left.fact_id.as_slice(),
    ) > (
        right.kind_rank,
        right.strength_rank,
        right.source_generation,
        right.cursor_end.as_slice(),
        right.last_commit_seq,
        right.fact_id.as_slice(),
    )
}

fn max_optional_time(left: Option<String>, right: Option<String>) -> Option<String> {
    match (left, right) {
        (None, right) => right,
        (left, None) => left,
        (Some(left), Some(right)) => Some(if right > left { right } else { left }),
    }
}

fn is_activity_evidence(kind: EvidenceKind) -> bool {
    matches!(
        kind,
        EvidenceKind::RunStarted | EvidenceKind::ActivityObserved
    )
}

fn evidence_kind_rank(kind: EvidenceKind) -> i64 {
    match kind {
        EvidenceKind::TerminalSucceeded
        | EvidenceKind::TerminalFailed
        | EvidenceKind::TerminalCancelled => 60,
        EvidenceKind::InputRequested => 50,
        EvidenceKind::WaitingObserved => 45,
        EvidenceKind::RunStarted => 40,
        EvidenceKind::ActivityObserved => 35,
        EvidenceKind::RunDeclared => 20,
    }
}

fn evidence_strength_rank(strength: EvidenceStrength) -> i64 {
    match strength {
        EvidenceStrength::NativeExplicit => 40,
        EvidenceStrength::NativeActivity => 30,
        EvidenceStrength::Presence => 20,
        EvidenceStrength::Layout => 10,
    }
}

fn run_state_label(kind: &str) -> &'static str {
    match kind {
        "terminal_succeeded" => "succeeded",
        "terminal_failed" => "failed",
        "terminal_cancelled" => "cancelled",
        "input_requested" | "waiting_observed" => "waiting",
        "run_started" | "activity_observed" => "active",
        "run_declared" => "declared",
        _ => "unknown",
    }
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
            SELECT MAX(last_activity_at) FROM run_evidence
            WHERE run_key = ?1
              AND last_activity_at IS NOT NULL
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

fn write_delegation_assertion(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    envelope: &FactEnvelope,
    fact: &DelegationFact,
) -> Result<(), EngineError> {
    transaction
        .execute(
            r#"
            INSERT INTO delegation_assertions (
                fact_id, child_run_key, parent_run_key, session_key,
                relation_kind, relation_strength, native_child_id,
                native_task_id, label, prompt, cwd, worktree_path,
                source_time, source_time_quality, source_object_id,
                source_generation, cursor_end, last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17, ?18
            )
            ON CONFLICT(fact_id) DO UPDATE SET
                child_run_key = excluded.child_run_key,
                parent_run_key = excluded.parent_run_key,
                session_key = excluded.session_key,
                relation_kind = excluded.relation_kind,
                relation_strength = excluded.relation_strength,
                native_child_id = excluded.native_child_id,
                native_task_id = excluded.native_task_id,
                label = excluded.label,
                prompt = excluded.prompt,
                cwd = excluded.cwd,
                worktree_path = excluded.worktree_path,
                source_time = excluded.source_time,
                source_time_quality = excluded.source_time_quality,
                source_object_id = excluded.source_object_id,
                source_generation = excluded.source_generation,
                cursor_end = excluded.cursor_end,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                envelope.id.as_bytes().as_slice(),
                fact.child_run.as_bytes(),
                fact.parent_run.as_ref().map(EntityKey::as_bytes),
                fact.session.as_bytes(),
                delegation_kind(&fact.kind),
                relation_strength(fact.relation_strength),
                fact.native_child_id,
                fact.native_task_id,
                fact.label,
                fact.prompt,
                fact.cwd,
                fact.worktree_path,
                timestamp_value(fact.source_time.as_ref()),
                timestamp_quality(fact.source_time.as_ref()),
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
                envelope.provenance.cursor_end,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map(|_| ())
        .map_err(|error| sqlite_error("project delegation assertion", error))
}

fn runs_need_delegation_reduce(
    transaction: &Transaction<'_>,
    run_keys: &BTreeSet<Vec<u8>>,
) -> Result<bool, EngineError> {
    for run_key in run_keys {
        let exists: i64 = transaction
            .prepare_cached(
                r#"
                SELECT CASE
                  WHEN EXISTS (
                    SELECT 1 FROM delegation_assertions
                    WHERE child_run_key = ?1 OR parent_run_key = ?1
                  ) THEN 1
                  WHEN EXISTS (
                    SELECT 1 FROM delegation_spawn_assertions
                    WHERE parent_run_key = ?1
                  ) THEN 1
                  WHEN EXISTS (
                    SELECT 1 FROM delegation_metadata_assertions
                    WHERE child_run_key = ?1
                  ) THEN 1
                  ELSE 0
                END
                "#,
            )
            .and_then(|mut statement| statement.query_row([run_key.as_slice()], |row| row.get(0)))
            .map_err(|error| sqlite_error("probe existing delegation rows for run", error))?;
        if exists != 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

fn delegation_children_for_run(
    transaction: &Transaction<'_>,
    run_key: &[u8],
) -> Result<BTreeSet<Vec<u8>>, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT DISTINCT child_run_key FROM delegation_assertions
            WHERE child_run_key = ?1 OR parent_run_key = ?1
            "#,
        )
        .map_err(|error| sqlite_error("prepare delegation run correlation", error))?;
    let children = statement
        .query_map([run_key], |row| row.get::<_, Vec<u8>>(0))
        .map_err(|error| sqlite_error("read delegation run correlation", error))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| sqlite_error("collect delegation run correlation", error))?;
    Ok(children)
}

fn write_delegation_spawn_assertion(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    envelope: &FactEnvelope,
    fact: &DelegationSpawnFact,
) -> Result<(), EngineError> {
    transaction
        .execute(
            r#"
            INSERT INTO delegation_spawn_assertions (
                fact_id, spawn_key, parent_run_key, parent_message_key,
                session_key, native_task_id, tool_name, label, prompt,
                requested_agent_type, source_time, source_time_quality,
                source_object_id, source_generation, cursor_end,
                last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16
            )
            ON CONFLICT(fact_id) DO UPDATE SET
                spawn_key = excluded.spawn_key,
                parent_run_key = excluded.parent_run_key,
                parent_message_key = excluded.parent_message_key,
                session_key = excluded.session_key,
                native_task_id = excluded.native_task_id,
                tool_name = excluded.tool_name,
                label = excluded.label,
                prompt = excluded.prompt,
                requested_agent_type = excluded.requested_agent_type,
                source_time = excluded.source_time,
                source_time_quality = excluded.source_time_quality,
                source_object_id = excluded.source_object_id,
                source_generation = excluded.source_generation,
                cursor_end = excluded.cursor_end,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                envelope.id.as_bytes().as_slice(),
                fact.spawn.as_bytes(),
                fact.parent_run.as_bytes(),
                fact.parent_message.as_bytes(),
                fact.session.as_bytes(),
                fact.native_task_id,
                fact.tool_name,
                fact.label,
                fact.prompt,
                fact.requested_agent_type,
                timestamp_value(fact.source_time.as_ref()),
                timestamp_quality(fact.source_time.as_ref()),
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
                envelope.provenance.cursor_end,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map(|_| ())
        .map_err(|error| sqlite_error("project delegation spawn assertion", error))
}

fn correlated_children_for_spawn(
    transaction: &Transaction<'_>,
    spawn_key: &[u8],
) -> Result<BTreeSet<Vec<u8>>, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT DISTINCT metadata.child_run_key
            FROM delegation_spawn_assertions AS spawn
            JOIN delegation_metadata_assertions AS metadata
              ON metadata.session_key = spawn.session_key
             AND metadata.native_task_id = spawn.native_task_id
            WHERE spawn.spawn_key = ?1
            "#,
        )
        .map_err(|error| sqlite_error("prepare spawn child correlation", error))?;
    let children = statement
        .query_map([spawn_key], |row| row.get::<_, Vec<u8>>(0))
        .map_err(|error| sqlite_error("read spawn child correlation", error))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| sqlite_error("collect spawn child correlation", error))?;
    Ok(children)
}

fn correlated_children_for_run(
    transaction: &Transaction<'_>,
    run_key: &[u8],
) -> Result<BTreeSet<Vec<u8>>, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT DISTINCT metadata.child_run_key
            FROM delegation_spawn_assertions AS spawn
            JOIN delegation_metadata_assertions AS metadata
              ON metadata.session_key = spawn.session_key
             AND metadata.native_task_id = spawn.native_task_id
            WHERE spawn.parent_run_key = ?1 OR metadata.child_run_key = ?1
            "#,
        )
        .map_err(|error| sqlite_error("prepare correlated delegation run lookup", error))?;
    let children = statement
        .query_map([run_key], |row| row.get::<_, Vec<u8>>(0))
        .map_err(|error| sqlite_error("read correlated delegation run lookup", error))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| sqlite_error("collect correlated delegation run lookup", error))?;
    Ok(children)
}

fn delegation_spawns_for_parent(
    transaction: &Transaction<'_>,
    run_key: &[u8],
) -> Result<BTreeSet<Vec<u8>>, EngineError> {
    let mut statement = transaction
        .prepare(
            "SELECT DISTINCT spawn_key FROM delegation_spawn_assertions WHERE parent_run_key = ?1",
        )
        .map_err(|error| sqlite_error("prepare parent delegation spawns", error))?;
    let spawns = statement
        .query_map([run_key], |row| row.get::<_, Vec<u8>>(0))
        .map_err(|error| sqlite_error("read parent delegation spawns", error))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| sqlite_error("collect parent delegation spawns", error))?;
    Ok(spawns)
}

#[derive(Debug)]
struct DelegationCandidate {
    decisive_relation_fact_id: Option<Vec<u8>>,
    decisive_spawn_fact_id: Option<Vec<u8>>,
    decisive_metadata_fact_id: Option<Vec<u8>>,
    parent_run_key: Option<Vec<u8>>,
    session_key: Vec<u8>,
    relation_kind: String,
    relation_strength: String,
    native_child_id: Option<String>,
    native_task_id: Option<String>,
    label: Option<String>,
    prompt: Option<String>,
    cwd: Option<String>,
    worktree_path: Option<String>,
    source_time: Option<String>,
    source_time_quality: Option<String>,
    source_generation: i64,
    cursor_end: Vec<u8>,
    last_commit_seq: i64,
    tie_breaker: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
struct DelegationReduction {
    status: Option<String>,
    assertion_count: usize,
    competing_relation_count: usize,
}

fn reduce_delegation(
    transaction: &Transaction<'_>,
    child_run_key: &[u8],
    commit_seq: u64,
) -> Result<DelegationReduction, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT fact_id, parent_run_key, session_key, relation_kind,
                   relation_strength, native_child_id, native_task_id,
                   label, prompt, cwd, worktree_path, source_time,
                   source_time_quality, source_generation, cursor_end,
                   last_commit_seq
            FROM delegation_assertions
            WHERE child_run_key = ?1
            "#,
        )
        .map_err(|error| sqlite_error("prepare delegation reduction", error))?;
    let mut assertions = statement
        .query_map([child_run_key], |row| {
            let fact_id = row.get::<_, Vec<u8>>(0)?;
            Ok(DelegationCandidate {
                decisive_relation_fact_id: Some(fact_id.clone()),
                decisive_spawn_fact_id: None,
                decisive_metadata_fact_id: None,
                parent_run_key: row.get(1)?,
                session_key: row.get(2)?,
                relation_kind: row.get(3)?,
                relation_strength: row.get(4)?,
                native_child_id: row.get(5)?,
                native_task_id: row.get(6)?,
                label: row.get(7)?,
                prompt: row.get(8)?,
                cwd: row.get(9)?,
                worktree_path: row.get(10)?,
                source_time: row.get(11)?,
                source_time_quality: row.get(12)?,
                source_generation: row.get(13)?,
                cursor_end: row.get(14)?,
                last_commit_seq: row.get(15)?,
                tie_breaker: fact_id,
            })
        })
        .map_err(|error| sqlite_error("read delegation assertions", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("collect delegation assertions", error))?;
    drop(statement);

    let mut correlated = transaction
        .prepare(
            r#"
            SELECT spawn.fact_id, metadata.fact_id, spawn.parent_run_key,
                   metadata.session_key, metadata.native_child_id,
                   spawn.native_task_id,
                   COALESCE(spawn.label, metadata.description, metadata.native_name),
                   spawn.prompt, metadata.worktree_path, spawn.source_time,
                   spawn.source_time_quality,
                   MAX(spawn.source_generation, metadata.source_generation),
                   spawn.cursor_end,
                   MAX(spawn.last_commit_seq, metadata.last_commit_seq)
            FROM delegation_spawn_assertions AS spawn
            JOIN delegation_metadata_assertions AS metadata
              ON metadata.session_key = spawn.session_key
             AND metadata.native_task_id = spawn.native_task_id
            WHERE metadata.child_run_key = ?1
              AND metadata.native_task_id IS NOT NULL
              AND trim(metadata.native_task_id) <> ''
            "#,
        )
        .map_err(|error| sqlite_error("prepare native delegation correlation", error))?;
    let correlated_assertions = correlated
        .query_map([child_run_key], |row| {
            let spawn_fact_id = row.get::<_, Vec<u8>>(0)?;
            let metadata_fact_id = row.get::<_, Vec<u8>>(1)?;
            let mut tie_breaker = spawn_fact_id.clone();
            tie_breaker.extend_from_slice(&metadata_fact_id);
            Ok(DelegationCandidate {
                decisive_relation_fact_id: None,
                decisive_spawn_fact_id: Some(spawn_fact_id),
                decisive_metadata_fact_id: Some(metadata_fact_id),
                parent_run_key: Some(row.get(2)?),
                session_key: row.get(3)?,
                relation_kind: "vendor_native_subagent".to_string(),
                relation_strength: "native_explicit".to_string(),
                native_child_id: Some(row.get(4)?),
                native_task_id: Some(row.get(5)?),
                label: row.get(6)?,
                prompt: row.get(7)?,
                cwd: None,
                worktree_path: row.get(8)?,
                source_time: row.get(9)?,
                source_time_quality: row.get(10)?,
                source_generation: row.get(11)?,
                cursor_end: row.get(12)?,
                last_commit_seq: row.get(13)?,
                tie_breaker,
            })
        })
        .map_err(|error| sqlite_error("read native delegation correlation", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("collect native delegation correlation", error))?;
    drop(correlated);
    assertions.extend(correlated_assertions);
    assertions.sort_by(|left, right| {
        relation_strength_rank(&right.relation_strength)
            .cmp(&relation_strength_rank(&left.relation_strength))
            .then_with(|| right.source_generation.cmp(&left.source_generation))
            .then_with(|| right.cursor_end.cmp(&left.cursor_end))
            .then_with(|| right.last_commit_seq.cmp(&left.last_commit_seq))
            .then_with(|| right.tie_breaker.cmp(&left.tie_breaker))
    });

    let Some(decisive) = assertions.first() else {
        transaction
            .execute(
                "DELETE FROM canonical_delegations WHERE child_run_key = ?1",
                [child_run_key],
            )
            .map_err(|error| sqlite_error("remove empty canonical delegation", error))?;
        return Ok(DelegationReduction {
            status: None,
            assertion_count: 0,
            competing_relation_count: 0,
        });
    };

    let strongest_relations = assertions
        .iter()
        .filter(|assertion| assertion.relation_strength == decisive.relation_strength)
        .map(|assertion| {
            (
                assertion.parent_run_key.clone(),
                assertion.relation_kind.clone(),
            )
        })
        .collect::<BTreeSet<_>>();
    let competing_relation_count = strongest_relations.len().saturating_sub(1);
    let child_present = canonical_run_exists(transaction, child_run_key)?;
    let parent_present = decisive
        .parent_run_key
        .as_deref()
        .map(|parent| canonical_run_exists(transaction, parent))
        .transpose()?
        .unwrap_or(false);
    let status = if competing_relation_count > 0 {
        "conflicting"
    } else if !child_present {
        "unresolved_child"
    } else if decisive.parent_run_key.is_none() {
        "unresolved_relation"
    } else if !parent_present {
        "unresolved_parent"
    } else {
        "resolved"
    };
    let assertion_count = i64::try_from(assertions.len()).map_err(|_| {
        EngineError::InvalidCommit("delegation assertion count exceeds SQLite range".to_string())
    })?;
    let competing_count = i64::try_from(competing_relation_count).map_err(|_| {
        EngineError::InvalidCommit("delegation conflict count exceeds SQLite range".to_string())
    })?;
    transaction
        .execute(
            r#"
            INSERT INTO canonical_delegations (
                child_run_key, parent_run_key, session_key, relation_kind,
                relation_strength, relation_status, native_child_id,
                native_task_id, label, prompt, cwd, worktree_path,
                source_time, source_time_quality, decisive_relation_fact_id,
                decisive_spawn_fact_id, decisive_metadata_fact_id,
                assertion_count, competing_relation_count, child_present,
                parent_present, last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22
            )
            ON CONFLICT(child_run_key) DO UPDATE SET
                parent_run_key = excluded.parent_run_key,
                session_key = excluded.session_key,
                relation_kind = excluded.relation_kind,
                relation_strength = excluded.relation_strength,
                relation_status = excluded.relation_status,
                native_child_id = excluded.native_child_id,
                native_task_id = excluded.native_task_id,
                label = excluded.label,
                prompt = excluded.prompt,
                cwd = excluded.cwd,
                worktree_path = excluded.worktree_path,
                source_time = excluded.source_time,
                source_time_quality = excluded.source_time_quality,
                decisive_relation_fact_id = excluded.decisive_relation_fact_id,
                decisive_spawn_fact_id = excluded.decisive_spawn_fact_id,
                decisive_metadata_fact_id = excluded.decisive_metadata_fact_id,
                assertion_count = excluded.assertion_count,
                competing_relation_count = excluded.competing_relation_count,
                child_present = excluded.child_present,
                parent_present = excluded.parent_present,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                child_run_key,
                decisive.parent_run_key,
                decisive.session_key,
                decisive.relation_kind,
                decisive.relation_strength,
                status,
                decisive.native_child_id,
                decisive.native_task_id,
                decisive.label,
                decisive.prompt,
                decisive.cwd,
                decisive.worktree_path,
                decisive.source_time,
                decisive.source_time_quality,
                decisive.decisive_relation_fact_id,
                decisive.decisive_spawn_fact_id,
                decisive.decisive_metadata_fact_id,
                assertion_count,
                competing_count,
                i64::from(child_present),
                i64::from(parent_present),
                sqlite_u64(commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("write canonical delegation", error))?;
    Ok(DelegationReduction {
        status: Some(status.to_string()),
        assertion_count: assertions.len(),
        competing_relation_count,
    })
}

fn relation_strength_rank(strength: &str) -> u8 {
    match strength {
        "native_explicit" => 30,
        "native_indirect" => 20,
        "layout" => 10,
        _ => 0,
    }
}

fn write_delegation_metadata_assertion(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    envelope: &FactEnvelope,
    fact: &DelegationMetadataFact,
) -> Result<(), EngineError> {
    transaction
        .execute(
            r#"
            INSERT INTO delegation_metadata_assertions (
                fact_id, child_run_key, session_key, native_child_id,
                agent_type, description, native_name, spawn_depth,
                worktree_path, native_task_id, source_object_id,
                source_generation, cursor_end, last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14
            )
            ON CONFLICT(fact_id) DO UPDATE SET
                child_run_key = excluded.child_run_key,
                session_key = excluded.session_key,
                native_child_id = excluded.native_child_id,
                agent_type = excluded.agent_type,
                description = excluded.description,
                native_name = excluded.native_name,
                spawn_depth = excluded.spawn_depth,
                worktree_path = excluded.worktree_path,
                native_task_id = excluded.native_task_id,
                source_object_id = excluded.source_object_id,
                source_generation = excluded.source_generation,
                cursor_end = excluded.cursor_end,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                envelope.id.as_bytes().as_slice(),
                fact.child_run.as_bytes(),
                fact.session.as_bytes(),
                fact.native_child_id,
                fact.agent_type,
                fact.description,
                fact.name,
                fact.spawn_depth.map(i64::from),
                fact.worktree_path,
                fact.native_task_id,
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
                envelope.provenance.cursor_end,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map(|_| ())
        .map_err(|error| sqlite_error("project delegation metadata assertion", error))
}

fn delegation_metadata_children_for_run(
    transaction: &Transaction<'_>,
    run_key: &[u8],
) -> Result<BTreeSet<Vec<u8>>, EngineError> {
    let mut statement = transaction
        .prepare(
            "SELECT DISTINCT child_run_key FROM delegation_metadata_assertions WHERE child_run_key = ?1",
        )
        .map_err(|error| sqlite_error("prepare delegation metadata run correlation", error))?;
    let children = statement
        .query_map([run_key], |row| row.get::<_, Vec<u8>>(0))
        .map_err(|error| sqlite_error("read delegation metadata run correlation", error))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| sqlite_error("collect delegation metadata run correlation", error))?;
    Ok(children)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct DelegationMetadataValue {
    session_key: Vec<u8>,
    native_child_id: String,
    agent_type: String,
    description: Option<String>,
    native_name: Option<String>,
    spawn_depth: Option<i64>,
    worktree_path: Option<String>,
    native_task_id: Option<String>,
}

#[derive(Debug)]
struct DelegationMetadataAssertionRow {
    fact_id: Vec<u8>,
    value: DelegationMetadataValue,
}

#[derive(Debug, PartialEq, Eq)]
struct DelegationMetadataReduction {
    status: Option<String>,
    assertion_count: usize,
    competing_metadata_count: usize,
}

fn reduce_delegation_metadata(
    transaction: &Transaction<'_>,
    child_run_key: &[u8],
    commit_seq: u64,
) -> Result<DelegationMetadataReduction, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT fact_id, session_key, native_child_id, agent_type,
                   description, native_name, spawn_depth, worktree_path,
                   native_task_id
            FROM delegation_metadata_assertions
            WHERE child_run_key = ?1
            ORDER BY fact_id DESC
            "#,
        )
        .map_err(|error| sqlite_error("prepare delegation metadata reduction", error))?;
    let assertions = statement
        .query_map([child_run_key], |row| {
            Ok(DelegationMetadataAssertionRow {
                fact_id: row.get(0)?,
                value: DelegationMetadataValue {
                    session_key: row.get(1)?,
                    native_child_id: row.get(2)?,
                    agent_type: row.get(3)?,
                    description: row.get(4)?,
                    native_name: row.get(5)?,
                    spawn_depth: row.get(6)?,
                    worktree_path: row.get(7)?,
                    native_task_id: row.get(8)?,
                },
            })
        })
        .map_err(|error| sqlite_error("read delegation metadata assertions", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("collect delegation metadata assertions", error))?;
    drop(statement);

    let Some(decisive) = assertions.first() else {
        transaction
            .execute(
                "DELETE FROM canonical_delegation_metadata WHERE child_run_key = ?1",
                [child_run_key],
            )
            .map_err(|error| sqlite_error("remove empty canonical delegation metadata", error))?;
        return Ok(DelegationMetadataReduction {
            status: None,
            assertion_count: 0,
            competing_metadata_count: 0,
        });
    };
    let distinct_values = assertions
        .iter()
        .map(|assertion| assertion.value.clone())
        .collect::<BTreeSet<_>>();
    let competing_metadata_count = distinct_values.len().saturating_sub(1);
    let run_present = canonical_run_exists(transaction, child_run_key)?;
    let status = if competing_metadata_count > 0 {
        "conflicting"
    } else if run_present {
        "resolved"
    } else {
        "unresolved_run"
    };
    let assertion_count = i64::try_from(assertions.len()).map_err(|_| {
        EngineError::InvalidCommit(
            "delegation metadata assertion count exceeds SQLite range".to_string(),
        )
    })?;
    let competing_count = i64::try_from(competing_metadata_count).map_err(|_| {
        EngineError::InvalidCommit(
            "delegation metadata conflict count exceeds SQLite range".to_string(),
        )
    })?;
    transaction
        .execute(
            r#"
            INSERT INTO canonical_delegation_metadata (
                child_run_key, session_key, native_child_id, agent_type,
                description, native_name, spawn_depth, worktree_path,
                native_task_id, metadata_status, decisive_fact_id,
                assertion_count, competing_metadata_count, run_present,
                last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15
            )
            ON CONFLICT(child_run_key) DO UPDATE SET
                session_key = excluded.session_key,
                native_child_id = excluded.native_child_id,
                agent_type = excluded.agent_type,
                description = excluded.description,
                native_name = excluded.native_name,
                spawn_depth = excluded.spawn_depth,
                worktree_path = excluded.worktree_path,
                native_task_id = excluded.native_task_id,
                metadata_status = excluded.metadata_status,
                decisive_fact_id = excluded.decisive_fact_id,
                assertion_count = excluded.assertion_count,
                competing_metadata_count = excluded.competing_metadata_count,
                run_present = excluded.run_present,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                child_run_key,
                decisive.value.session_key,
                decisive.value.native_child_id,
                decisive.value.agent_type,
                decisive.value.description,
                decisive.value.native_name,
                decisive.value.spawn_depth,
                decisive.value.worktree_path,
                decisive.value.native_task_id,
                status,
                decisive.fact_id,
                assertion_count,
                competing_count,
                i64::from(run_present),
                sqlite_u64(commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("write canonical delegation metadata", error))?;
    Ok(DelegationMetadataReduction {
        status: Some(status.to_string()),
        assertion_count: assertions.len(),
        competing_metadata_count,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct DelegationSpawnValue {
    parent_run_key: Vec<u8>,
    parent_message_key: Vec<u8>,
    session_key: Vec<u8>,
    native_task_id: String,
    tool_name: String,
    label: Option<String>,
    prompt: Option<String>,
    requested_agent_type: Option<String>,
    source_time: Option<String>,
    source_time_quality: Option<String>,
}

#[derive(Debug)]
struct DelegationSpawnAssertionRow {
    fact_id: Vec<u8>,
    value: DelegationSpawnValue,
}

#[derive(Debug, PartialEq, Eq)]
struct DelegationSpawnReduction {
    status: Option<String>,
    assertion_count: usize,
    competing_spawn_count: usize,
}

fn reduce_delegation_spawn(
    transaction: &Transaction<'_>,
    spawn_key: &[u8],
    commit_seq: u64,
) -> Result<DelegationSpawnReduction, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT fact_id, parent_run_key, parent_message_key, session_key,
                   native_task_id, tool_name, label, prompt,
                   requested_agent_type, source_time, source_time_quality
            FROM delegation_spawn_assertions
            WHERE spawn_key = ?1
            ORDER BY source_generation DESC, cursor_end DESC,
                     last_commit_seq DESC, fact_id DESC
            "#,
        )
        .map_err(|error| sqlite_error("prepare delegation spawn reduction", error))?;
    let assertions = statement
        .query_map([spawn_key], |row| {
            Ok(DelegationSpawnAssertionRow {
                fact_id: row.get(0)?,
                value: DelegationSpawnValue {
                    parent_run_key: row.get(1)?,
                    parent_message_key: row.get(2)?,
                    session_key: row.get(3)?,
                    native_task_id: row.get(4)?,
                    tool_name: row.get(5)?,
                    label: row.get(6)?,
                    prompt: row.get(7)?,
                    requested_agent_type: row.get(8)?,
                    source_time: row.get(9)?,
                    source_time_quality: row.get(10)?,
                },
            })
        })
        .map_err(|error| sqlite_error("read delegation spawn assertions", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("collect delegation spawn assertions", error))?;
    drop(statement);

    let Some(decisive) = assertions.first() else {
        transaction
            .execute(
                "DELETE FROM canonical_delegation_spawns WHERE spawn_key = ?1",
                [spawn_key],
            )
            .map_err(|error| sqlite_error("remove empty canonical delegation spawn", error))?;
        return Ok(DelegationSpawnReduction {
            status: None,
            assertion_count: 0,
            competing_spawn_count: 0,
        });
    };
    let distinct_values = assertions
        .iter()
        .map(|assertion| assertion.value.clone())
        .collect::<BTreeSet<_>>();
    let competing_spawn_count = distinct_values.len().saturating_sub(1);
    let parent_present = canonical_run_exists(transaction, &decisive.value.parent_run_key)?;
    let status = if competing_spawn_count > 0 {
        "conflicting"
    } else if parent_present {
        "resolved"
    } else {
        "unresolved_parent"
    };
    let assertion_count = i64::try_from(assertions.len()).map_err(|_| {
        EngineError::InvalidCommit(
            "delegation spawn assertion count exceeds SQLite range".to_string(),
        )
    })?;
    let competing_count = i64::try_from(competing_spawn_count).map_err(|_| {
        EngineError::InvalidCommit(
            "delegation spawn conflict count exceeds SQLite range".to_string(),
        )
    })?;
    transaction
        .execute(
            r#"
            INSERT INTO canonical_delegation_spawns (
                spawn_key, parent_run_key, parent_message_key, session_key,
                native_task_id, tool_name, label, prompt,
                requested_agent_type, source_time, source_time_quality,
                spawn_status, decisive_fact_id, assertion_count,
                competing_spawn_count, parent_present, last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17
            )
            ON CONFLICT(spawn_key) DO UPDATE SET
                parent_run_key = excluded.parent_run_key,
                parent_message_key = excluded.parent_message_key,
                session_key = excluded.session_key,
                native_task_id = excluded.native_task_id,
                tool_name = excluded.tool_name,
                label = excluded.label,
                prompt = excluded.prompt,
                requested_agent_type = excluded.requested_agent_type,
                source_time = excluded.source_time,
                source_time_quality = excluded.source_time_quality,
                spawn_status = excluded.spawn_status,
                decisive_fact_id = excluded.decisive_fact_id,
                assertion_count = excluded.assertion_count,
                competing_spawn_count = excluded.competing_spawn_count,
                parent_present = excluded.parent_present,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                spawn_key,
                decisive.value.parent_run_key,
                decisive.value.parent_message_key,
                decisive.value.session_key,
                decisive.value.native_task_id,
                decisive.value.tool_name,
                decisive.value.label,
                decisive.value.prompt,
                decisive.value.requested_agent_type,
                decisive.value.source_time,
                decisive.value.source_time_quality,
                status,
                decisive.fact_id,
                assertion_count,
                competing_count,
                i64::from(parent_present),
                sqlite_u64(commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("write canonical delegation spawn", error))?;
    Ok(DelegationSpawnReduction {
        status: Some(status.to_string()),
        assertion_count: assertions.len(),
        competing_spawn_count,
    })
}

fn canonical_run_exists(
    transaction: &Transaction<'_>,
    run_key: &[u8],
) -> Result<bool, EngineError> {
    transaction
        .query_row(
            "SELECT 1 FROM canonical_runs WHERE run_key = ?1",
            [run_key],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
        .map_err(|error| sqlite_error("read delegation run presence", error))
}

#[derive(Debug, Clone)]
struct UsageContribution {
    fact_id: Vec<u8>,
    session_key: Vec<u8>,
    subject_key: Vec<u8>,
    series_key: Vec<u8>,
    accounting: UsageAccounting,
    quality_bucket: &'static str,
    values: [i64; 4],
    reported: [i64; 4],
    source_generation: u64,
    cursor_end: Vec<u8>,
}

impl UsageContribution {
    #[allow(clippy::too_many_arguments)]
    fn from_fact(
        fact_id: &[u8],
        fact: &UsageFact,
        series_key: Vec<u8>,
        reported: [i64; 4],
        values: [i64; 4],
        context: &ProjectionCommitContext,
        cursor_end: &[u8],
    ) -> Result<Self, EngineError> {
        Ok(Self {
            fact_id: fact_id.to_vec(),
            session_key: fact.session.as_bytes().to_vec(),
            subject_key: fact.subject.as_bytes().to_vec(),
            series_key,
            accounting: fact.accounting,
            quality_bucket: quality_bucket(fact.quality),
            values,
            reported,
            source_generation: context.generation,
            cursor_end: cursor_end.to_vec(),
        })
    }
}

fn usage_series_key(context: &ProjectionCommitContext, fact: &UsageFact) -> Vec<u8> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti-usage-series-v1\0");
    hasher.update(&context.source_instance_id.to_be_bytes());
    hasher.update(&context.source_stream_id.to_be_bytes());
    hasher.update(&context.source_object_id.to_be_bytes());
    hasher.update(fact.subject.as_bytes());
    hasher.update(usage_scope(fact.scope).as_bytes());
    if let Some(model) = fact.model.as_deref() {
        hasher.update(model.as_bytes());
    }
    hasher.finalize().as_bytes().to_vec()
}

fn read_replaced_contributions(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
) -> Result<Vec<UsageContribution>, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT fact_id, session_key, subject_key, series_key, accounting,
                   quality_bucket, input_tokens, output_tokens,
                   cache_creation_tokens, cache_read_tokens,
                   reported_input_tokens, reported_output_tokens,
                   reported_cache_creation_tokens, reported_cache_read_tokens,
                   source_generation, cursor_end
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
        .prepare_cached(
            r#"
            SELECT fact_id, session_key, subject_key, series_key, accounting,
                   quality_bucket, input_tokens, output_tokens,
                   cache_creation_tokens, cache_read_tokens,
                   reported_input_tokens, reported_output_tokens,
                   reported_cache_creation_tokens, reported_cache_read_tokens,
                   source_generation, cursor_end
            FROM usage_contributions WHERE fact_id = ?1
            "#,
        )
        .and_then(|mut statement| statement.query_row([fact_id], contribution_from_row))
        .optional()
        .map_err(|error| sqlite_error("read prior usage contribution", error))
}

fn contribution_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<UsageContribution> {
    let accounting: String = row.get(4)?;
    let accounting = match accounting.as_str() {
        "delta" => UsageAccounting::Delta,
        "cumulative" => UsageAccounting::Cumulative,
        "snapshot" => UsageAccounting::Snapshot,
        value => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                format!("unknown usage accounting {value}").into(),
            ));
        }
    };
    let bucket: String = row.get(5)?;
    let quality_bucket = match bucket.as_str() {
        "exact" => "exact",
        _ => "estimated",
    };
    let source_generation = row.get::<_, i64>(14)?;
    Ok(UsageContribution {
        fact_id: row.get(0)?,
        session_key: row.get(1)?,
        subject_key: row.get(2)?,
        series_key: row.get(3)?,
        accounting,
        quality_bucket,
        values: [row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?],
        reported: [row.get(10)?, row.get(11)?, row.get(12)?, row.get(13)?],
        source_generation: u64::try_from(source_generation).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                14,
                rusqlite::types::Type::Integer,
                Box::new(error),
            )
        })?,
        cursor_end: row.get(15)?,
    })
}

fn read_snapshot_series_contributions(
    transaction: &Transaction<'_>,
    series_key: &[u8],
    generation: u64,
) -> Result<Vec<UsageContribution>, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT fact_id, session_key, subject_key, series_key, accounting,
                   quality_bucket, input_tokens, output_tokens,
                   cache_creation_tokens, cache_read_tokens,
                   reported_input_tokens, reported_output_tokens,
                   reported_cache_creation_tokens, reported_cache_read_tokens,
                   source_generation, cursor_end
            FROM usage_contributions
            WHERE series_key = ?1 AND source_generation = ?2
              AND accounting = 'snapshot'
            "#,
        )
        .map_err(|error| sqlite_error("prepare snapshot usage replacement", error))?;
    let contributions = statement
        .query_map(
            params![series_key, sqlite_u64(generation, "source generation")?],
            contribution_from_row,
        )
        .map_err(|error| sqlite_error("read snapshot usage replacement", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("collect snapshot usage replacement", error))?;
    Ok(contributions)
}

fn read_cumulative_predecessor(
    transaction: &Transaction<'_>,
    series_key: &[u8],
    generation: u64,
    cursor_end: &[u8],
) -> Result<Option<UsageContribution>, EngineError> {
    let generation = sqlite_u64(generation, "source generation")?;
    transaction
        .prepare_cached(
            r#"
            SELECT fact_id, session_key, subject_key, series_key, accounting,
                   quality_bucket, input_tokens, output_tokens,
                   cache_creation_tokens, cache_read_tokens,
                   reported_input_tokens, reported_output_tokens,
                   reported_cache_creation_tokens, reported_cache_read_tokens,
                   source_generation, cursor_end
            FROM usage_contributions
            WHERE series_key = ?1 AND source_generation = ?2
              AND accounting = 'cumulative' AND cursor_end < ?3
            ORDER BY cursor_end DESC, fact_id DESC
            LIMIT 1
            "#,
        )
        .and_then(|mut statement| {
            statement.query_row(
                params![series_key, generation, cursor_end,],
                contribution_from_row,
            )
        })
        .optional()
        .map_err(|error| sqlite_error("read cumulative usage predecessor", error))
}

fn cumulative_values(
    reported: [i64; 4],
    predecessor: Option<[i64; 4]>,
) -> Result<[i64; 4], EngineError> {
    let Some(predecessor) = predecessor else {
        return Ok(reported);
    };
    let mut values = [0_i64; 4];
    for index in 0..values.len() {
        values[index] = if reported[index] >= predecessor[index] {
            reported[index]
                .checked_sub(predecessor[index])
                .ok_or_else(|| EngineError::InvalidCommit("usage counter underflow".to_string()))?
        } else {
            // A decrease is a native counter reset, not a negative usage
            // contribution. The first value after reset starts a new epoch.
            reported[index]
        };
    }
    Ok(values)
}

fn delete_contribution(transaction: &Transaction<'_>, fact_id: &[u8]) -> Result<(), EngineError> {
    execute_cached(
        transaction,
        "DELETE FROM usage_contributions WHERE fact_id = ?1",
        [fact_id],
    )
    .map(|_| ())
    .map_err(|error| sqlite_error("delete usage contribution", error))
}

fn write_contribution(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    envelope: &FactEnvelope,
    fact: &UsageFact,
    contribution: &UsageContribution,
) -> Result<(), EngineError> {
    execute_cached(
        transaction,
        r#"
            INSERT INTO usage_contributions (
                fact_id, subject_key, session_key, series_key, scope, accounting,
                quality, quality_bucket, input_tokens, output_tokens,
                cache_creation_tokens, cache_read_tokens, reported_input_tokens,
                reported_output_tokens, reported_cache_creation_tokens,
                reported_cache_read_tokens, model, source_time,
                source_time_quality, source_object_id, source_generation,
                cursor_end, last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23
            )
            ON CONFLICT(fact_id) DO UPDATE SET
                subject_key = excluded.subject_key,
                session_key = excluded.session_key,
                series_key = excluded.series_key,
                scope = excluded.scope,
                accounting = excluded.accounting,
                quality = excluded.quality,
                quality_bucket = excluded.quality_bucket,
                input_tokens = excluded.input_tokens,
                output_tokens = excluded.output_tokens,
                cache_creation_tokens = excluded.cache_creation_tokens,
                cache_read_tokens = excluded.cache_read_tokens,
                reported_input_tokens = excluded.reported_input_tokens,
                reported_output_tokens = excluded.reported_output_tokens,
                reported_cache_creation_tokens = excluded.reported_cache_creation_tokens,
                reported_cache_read_tokens = excluded.reported_cache_read_tokens,
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
            contribution.series_key,
            usage_scope(fact.scope),
            usage_accounting(fact.accounting),
            value_quality(fact.quality),
            contribution.quality_bucket,
            contribution.values[0],
            contribution.values[1],
            contribution.values[2],
            contribution.values[3],
            contribution.reported[0],
            contribution.reported[1],
            contribution.reported[2],
            contribution.reported[3],
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

fn repair_cumulative_successor(
    transaction: &Transaction<'_>,
    series_key: &[u8],
    generation: u64,
    cursor_end: &[u8],
    commit_seq: u64,
    pending_totals: &mut BTreeMap<Vec<u8>, [i64; 8]>,
) -> Result<Option<Vec<u8>>, EngineError> {
    let generation_value = sqlite_u64(generation, "source generation")?;
    let successor = transaction
        .prepare_cached(
            r#"
            SELECT fact_id, session_key, subject_key, series_key, accounting,
                   quality_bucket, input_tokens, output_tokens,
                   cache_creation_tokens, cache_read_tokens,
                   reported_input_tokens, reported_output_tokens,
                   reported_cache_creation_tokens, reported_cache_read_tokens,
                   source_generation, cursor_end
            FROM usage_contributions
            WHERE series_key = ?1 AND source_generation = ?2
              AND accounting = 'cumulative' AND cursor_end > ?3
            ORDER BY cursor_end ASC, fact_id ASC
            LIMIT 1
            "#,
        )
        .and_then(|mut statement| {
            statement.query_row(
                params![series_key, generation_value, cursor_end,],
                contribution_from_row,
            )
        })
        .optional()
        .map_err(|error| sqlite_error("read cumulative usage successor", error))?;
    let Some(mut successor) = successor else {
        return Ok(None);
    };
    let predecessor =
        read_cumulative_predecessor(transaction, series_key, generation, &successor.cursor_end)?;
    let next_values = cumulative_values(
        successor.reported,
        predecessor.map(|contribution| contribution.reported),
    )?;
    if next_values == successor.values {
        return Ok(Some(successor.session_key));
    }
    adjust_usage_total(transaction, pending_totals, &successor, -1)?;
    successor.values = next_values;
    transaction
        .execute(
            r#"
            UPDATE usage_contributions
            SET input_tokens = ?2, output_tokens = ?3,
                cache_creation_tokens = ?4, cache_read_tokens = ?5,
                last_commit_seq = ?6
            WHERE fact_id = ?1
            "#,
            params![
                successor.fact_id,
                successor.values[0],
                successor.values[1],
                successor.values[2],
                successor.values[3],
                sqlite_u64(commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("repair cumulative usage successor", error))?;
    adjust_usage_total(transaction, pending_totals, &successor, 1)?;
    Ok(Some(successor.session_key))
}

fn adjust_usage_total(
    transaction: &Transaction<'_>,
    pending_totals: &mut BTreeMap<Vec<u8>, [i64; 8]>,
    contribution: &UsageContribution,
    direction: i64,
) -> Result<(), EngineError> {
    let current = if let Some(current) = pending_totals.get(&contribution.session_key) {
        *current
    } else {
        let current = transaction
            .prepare_cached(
                r#"
                SELECT exact_input_tokens, exact_output_tokens,
                       exact_cache_creation_tokens, exact_cache_read_tokens,
                       estimated_input_tokens, estimated_output_tokens,
                       estimated_cache_creation_tokens, estimated_cache_read_tokens
                FROM usage_totals WHERE session_key = ?1
                "#,
            )
            .and_then(|mut statement| {
                statement.query_row([&contribution.session_key], |row| {
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
                })
            })
            .optional()
            .map_err(|error| sqlite_error("read usage aggregate", error))?
            .unwrap_or([0_i64; 8]);
        pending_totals.insert(contribution.session_key.clone(), current);
        current
    };
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
    pending_totals.insert(contribution.session_key.clone(), next);
    Ok(())
}

fn flush_usage_totals(
    transaction: &Transaction<'_>,
    pending_totals: BTreeMap<Vec<u8>, [i64; 8]>,
    commit_seq: u64,
) -> Result<(), EngineError> {
    for (session_key, next) in pending_totals {
        if next.iter().all(|value| *value == 0) {
            execute_cached(
                transaction,
                "DELETE FROM usage_totals WHERE session_key = ?1",
                [&session_key],
            )
            .map_err(|error| sqlite_error("remove empty usage aggregate", error))?;
            continue;
        }
        execute_cached(
            transaction,
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
                session_key,
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
        .map_err(|error| sqlite_error("apply usage aggregate delta", error))?;
    }
    Ok(())
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

fn delegation_change(
    entity_key: &[u8],
    reduction: &DelegationReduction,
) -> Result<ChangeEntry, EngineError> {
    let payload = serialize(
        &serde_json::json!({
            "status": reduction.status,
            "assertion_count": reduction.assertion_count,
            "competing_relation_count": reduction.competing_relation_count,
        }),
        "serialize delegation change",
    )?;
    Ok(ChangeEntry {
        topic: "runtime.delegation.changed".to_string(),
        schema_version: CHANGE_SCHEMA_VERSION,
        entity_key: entity_key.to_vec(),
        operation: if reduction.status.is_some() {
            "upsert"
        } else {
            "delete"
        }
        .to_string(),
        payload,
    })
}

fn delegation_conflict_change(
    entity_key: &[u8],
    reduction: &DelegationReduction,
) -> Result<ChangeEntry, EngineError> {
    let conflicting = reduction.competing_relation_count > 0;
    let payload = serialize(
        &serde_json::json!({
            "conflicting": conflicting,
            "competing_relation_count": reduction.competing_relation_count,
        }),
        "serialize delegation conflict change",
    )?;
    Ok(ChangeEntry {
        topic: "diagnostic.runtime.delegation-conflict".to_string(),
        schema_version: CHANGE_SCHEMA_VERSION,
        entity_key: entity_key.to_vec(),
        operation: if conflicting { "upsert" } else { "delete" }.to_string(),
        payload,
    })
}

fn delegation_metadata_change(
    entity_key: &[u8],
    reduction: &DelegationMetadataReduction,
) -> Result<ChangeEntry, EngineError> {
    let payload = serialize(
        &serde_json::json!({
            "status": reduction.status,
            "assertion_count": reduction.assertion_count,
            "competing_metadata_count": reduction.competing_metadata_count,
        }),
        "serialize delegation metadata change",
    )?;
    Ok(ChangeEntry {
        topic: "runtime.delegation-metadata.changed".to_string(),
        schema_version: CHANGE_SCHEMA_VERSION,
        entity_key: entity_key.to_vec(),
        operation: if reduction.status.is_some() {
            "upsert"
        } else {
            "delete"
        }
        .to_string(),
        payload,
    })
}

fn delegation_metadata_conflict_change(
    entity_key: &[u8],
    reduction: &DelegationMetadataReduction,
) -> Result<ChangeEntry, EngineError> {
    let conflicting = reduction.competing_metadata_count > 0;
    let payload = serialize(
        &serde_json::json!({
            "conflicting": conflicting,
            "competing_metadata_count": reduction.competing_metadata_count,
        }),
        "serialize delegation metadata conflict change",
    )?;
    Ok(ChangeEntry {
        topic: "diagnostic.runtime.delegation-metadata-conflict".to_string(),
        schema_version: CHANGE_SCHEMA_VERSION,
        entity_key: entity_key.to_vec(),
        operation: if conflicting { "upsert" } else { "delete" }.to_string(),
        payload,
    })
}

fn delegation_spawn_change(
    entity_key: &[u8],
    reduction: &DelegationSpawnReduction,
) -> Result<ChangeEntry, EngineError> {
    let payload = serialize(
        &serde_json::json!({
            "status": reduction.status,
            "assertion_count": reduction.assertion_count,
            "competing_spawn_count": reduction.competing_spawn_count,
        }),
        "serialize delegation spawn change",
    )?;
    Ok(ChangeEntry {
        topic: "runtime.delegation-spawn.changed".to_string(),
        schema_version: CHANGE_SCHEMA_VERSION,
        entity_key: entity_key.to_vec(),
        operation: if reduction.status.is_some() {
            "upsert"
        } else {
            "delete"
        }
        .to_string(),
        payload,
    })
}

fn delegation_spawn_conflict_change(
    entity_key: &[u8],
    reduction: &DelegationSpawnReduction,
) -> Result<ChangeEntry, EngineError> {
    let conflicting = reduction.competing_spawn_count > 0;
    let payload = serialize(
        &serde_json::json!({
            "conflicting": conflicting,
            "competing_spawn_count": reduction.competing_spawn_count,
        }),
        "serialize delegation spawn conflict change",
    )?;
    Ok(ChangeEntry {
        topic: "diagnostic.runtime.delegation-spawn-conflict".to_string(),
        schema_version: CHANGE_SCHEMA_VERSION,
        entity_key: entity_key.to_vec(),
        operation: if conflicting { "upsert" } else { "delete" }.to_string(),
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

fn delegation_kind(kind: &DelegationKind) -> String {
    match kind {
        DelegationKind::VendorNativeSubagent => "vendor_native_subagent".to_string(),
        DelegationKind::ForkedConversation => "forked_conversation".to_string(),
        DelegationKind::ChildProcess => "child_process".to_string(),
        DelegationKind::Other(value) => format!("other:{value}"),
    }
}

fn relation_strength(strength: RelationStrength) -> &'static str {
    match strength {
        RelationStrength::Layout => "layout",
        RelationStrength::NativeIndirect => "native_indirect",
        RelationStrength::NativeExplicit => "native_explicit",
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

pub(super) fn execute_cached<P: Params>(
    transaction: &Transaction<'_>,
    sql: &str,
    params: P,
) -> rusqlite::Result<usize> {
    transaction.prepare_cached(sql)?.execute(params)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell as StdRefCell;
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::path::Path;

    use tempfile::TempDir;
    use walkdir::WalkDir;

    use crate::adapter::{
        AdapterId, AdapterObjectContext, AgentAdapter, ArtifactCapture, ArtifactContentFact,
        ArtifactMetadataEntry, ArtifactMetadataSnapshotFact, ArtifactObservationKind, ContentBlock,
        DecodeContext, DecoderId, DependencyRevision, EvidenceKind, EvidenceStrength, FactBatch,
        HookEventSummary, InterpretationSettingsDocumentStatus, InterpretationSettingsFact,
        InterpretationSettingsLayer, InterpretationSettingsSnapshot, MessageFact,
        PersistedToolResultFact, PlanSnapshotFact, PresenceFact, ProjectMemoryDocumentFact,
        RunEvidenceFact, RunFact, SessionFact, SessionIndexEntrySnapshot, SessionIndexSnapshotFact,
        SourceInstance, SourceInstanceKey, SourceInstanceSpec as AdapterSourceInstanceSpec,
        SourceObjectDescriptor, SourceRoot, StreamId, TaskCollectionKind, TaskItemSnapshot,
        TaskSnapshotCoverage, TaskSnapshotFact, TaskStatus, TeamInboxMessageSnapshot,
        TeamInboxSnapshotFact, TeamMemberSnapshot, TeamSnapshotFact, WorkflowMemberEventFact,
        WorkflowMemberEventKind, WorkflowSnapshotFact, WorkflowStatus,
    };
    use crate::claude::ClaudeCodeAdapter;
    use crate::core::schema;
    #[cfg(feature = "legacy-oracle")]
    use crate::orchestrate::ingest::{run_ingest, IngestOptions};
    use crate::source::{
        AppendCheckpoint, AppendDelimitedConfig, AppendDelimitedFile, AppendItem, AppendRead,
        RecordOrigin, SourceCursor, SourceMediaType, SourceRecord,
    };

    use super::*;
    use crate::engine::commit::{
        apply_observation_commit, source_instance_catalog_id, source_stream_catalog_id,
        ExpectedSourceCursor, SourceInstanceSpec, SourceObjectUpdate, SourceStreamSpec,
    };

    const SESSION: &str = "01234567-89ab-cdef-0123-456789abcdef";
    const PROJECT: &str = "-Users-fixture-project";
    const STREAM: &str = "session-transcripts";
    const DECODER: &str = "claude-session-record";

    struct TestCommitHook;

    impl CommitHook for TestCommitHook {
        fn reach(&self, _stage: crate::engine::commit::CommitStage) -> Result<(), EngineError> {
            Ok(())
        }
    }

    thread_local! {
        static TEST_OBJECT_KEYS: StdRefCell<Vec<Vec<u8>>> = const { StdRefCell::new(Vec::new()) };
    }

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
        TEST_OBJECT_KEYS.with(|keys| keys.borrow_mut().clear());
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
                        identity_contract_version: 1,
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
                adapter_version: "1.0.0".to_string(),
                adapter_contract_version: 2,
                source_schema_versions: Vec::new(),
                capabilities: Vec::new(),
                discovered_at: 1,
                last_seen_at: started_at,
            },
            stream: SourceStreamSpec {
                stream_key: STREAM.to_string(),
                driver_kind: "append_delimited_file".to_string(),
                decoder_key: DECODER.to_string(),
                stream_state: "available".to_string(),
                last_reconciled_at: Some(started_at),
                consistency: crate::adapter::ConsistencyPolicy::IncrementalCursor,
                retention: RawRetentionPolicy::Full,
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
                driver_checkpoint: None,
                driver_checkpoint_version: None,
                decoder_state: None,
                decoder_state_version: None,
                retry_state: None,
                size_bytes: None,
                mtime_ns: None,
                decoder_contract_version: 2,
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

    fn request_for_object(
        object_key: &[u8],
        expected: ExpectedSourceCursor,
        generation: u64,
        committed_cursor: Vec<u8>,
        started_at: i64,
    ) -> ObservationCommit {
        remember_test_object(object_key);
        let mut request = request(expected, generation, committed_cursor, started_at);
        request.stream.consistency = crate::adapter::ConsistencyPolicy::SnapshotReplace;
        request.object.object_key = object_key.to_vec();
        request.object.display_path = Some(String::from_utf8_lossy(object_key).into_owned());
        request
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
        remember_test_object(b"fixture-transcript");
    }

    fn register_object_key(connection: &mut Connection, object_key: &[u8], clock: i64) {
        apply_observation_commit(
            connection,
            &request_for_object(
                object_key,
                ExpectedSourceCursor::Absent,
                1,
                SourceCursor::append_offset(0).into_bytes(),
                clock,
            ),
        )
        .unwrap();
        remember_test_object(object_key);
    }

    fn remember_test_object(object_key: &[u8]) {
        TEST_OBJECT_KEYS.with(|keys| {
            let mut keys = keys.borrow_mut();
            if !keys.iter().any(|known| known == object_key) {
                keys.push(object_key.to_vec());
            }
        });
    }

    fn origin(observed_at: i64) -> RecordOrigin {
        let source_instance_id = source_instance_catalog_id("claude-code", b"fixture-root");
        RecordOrigin {
            source_instance_id,
            stream_id: source_stream_catalog_id(source_instance_id, STREAM),
            object_id: crate::engine::commit::source_object_catalog_id(
                source_stream_catalog_id(source_instance_id, STREAM),
                b"fixture-transcript",
            ),
            observed_at,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        }
    }

    fn object_catalog_id(object_key: &[u8]) -> i64 {
        let source_instance_id = source_instance_catalog_id("claude-code", b"fixture-root");
        crate::engine::commit::source_object_catalog_id(
            source_stream_catalog_id(source_instance_id, STREAM),
            object_key,
        )
        .try_into()
        .expect("catalog identifiers fit SQLite INTEGER")
    }

    fn direct_record(
        generation: u64,
        start: u64,
        end: u64,
        observed_at: i64,
        payload: &[u8],
    ) -> SourceRecord {
        SourceRecord::new(
            &origin(observed_at),
            generation,
            SourceCursor::append_offset(start),
            SourceCursor::append_offset(end),
            0,
            payload.to_vec(),
        )
    }

    fn object_record(
        object_ordinal: u64,
        generation: u64,
        cursor_start: SourceCursor,
        cursor_end: SourceCursor,
        observed_at: i64,
        payload: &[u8],
    ) -> SourceRecord {
        let source_instance_id = source_instance_catalog_id("claude-code", b"fixture-root");
        let stream_id = source_stream_catalog_id(source_instance_id, STREAM);
        let object_key = TEST_OBJECT_KEYS.with(|keys| {
            keys.borrow()
                .get(usize::try_from(object_ordinal - 1).unwrap())
                .cloned()
                .unwrap_or_else(|| panic!("test object ordinal {object_ordinal} is not registered"))
        });
        SourceRecord::new(
            &RecordOrigin {
                source_instance_id,
                stream_id,
                object_id: crate::engine::commit::source_object_catalog_id(stream_id, &object_key),
                observed_at,
                source_timestamp_hint: None,
                media_type: SourceMediaType::new("application/json").unwrap(),
            },
            generation,
            cursor_start,
            cursor_end,
            0,
            payload.to_vec(),
        )
    }

    fn entity(kind: &str, native_key: &str) -> EntityKey {
        EntityKey::native(
            &AdapterId::new("claude-code").unwrap(),
            1,
            kind,
            native_key.as_bytes(),
        )
        .unwrap()
    }

    fn exact(value: &str) -> QualifiedTimestamp {
        QualifiedTimestamp {
            value: value.to_string(),
            quality: TimestampQuality::NativeExact,
        }
    }

    fn presence_fact(native_status: &str, cwd: &str) -> PresenceFact {
        PresenceFact {
            presence: entity("presence", "4242/session/proc-start"),
            session: entity("session", SESSION),
            run: entity("run", SESSION),
            native_session_id: SESSION.to_string(),
            native_pid: 4242,
            cwd: cwd.to_string(),
            started_at: exact("2026-08-11T17:11:50.233Z"),
            native_kind: Some("interactive".to_string()),
            entrypoint: Some("cli".to_string()),
            name: Some("engine work".to_string()),
            native_status: Some(native_status.to_string()),
            updated_at: Some(exact("2026-08-11T18:08:24.949Z")),
            status_updated_at: Some(exact("2026-08-11T18:08:24.000Z")),
            native_process_started_at: Some("Tue Aug 11 17:11:48 2026".to_string()),
            version: Some("2.1.227".to_string()),
            peer_protocol: Some(1),
            name_source: Some("derived".to_string()),
            bridge_session_id: None,
            messaging_socket_path: Some("/tmp/claude-4242.sock".to_string()),
        }
    }

    fn task_item(native_key: &str, subject: &str, status: TaskStatus) -> TaskItemSnapshot {
        TaskItemSnapshot {
            task: entity("task", native_key),
            native_task_id: Some(native_key.to_string()),
            subject: subject.to_string(),
            description: Some(format!("description for {subject}")),
            active_form: Some(format!("working on {subject}")),
            native_owner: Some("worker".to_string()),
            status,
            blocks: vec!["later".to_string()],
            blocked_by: vec!["earlier".to_string()],
        }
    }

    fn todo_fact(items: Vec<TaskItemSnapshot>) -> TaskSnapshotFact {
        TaskSnapshotFact {
            collection: entity("task_collection", "todo-list"),
            session: Some(entity("session", SESSION)),
            run: Some(entity("run", SESSION)),
            team: None,
            native_collection_id: format!("{SESSION}-agent-{SESSION}"),
            native_owner_id: Some(SESSION.to_string()),
            kind: TaskCollectionKind::TodoList,
            coverage: TaskSnapshotCoverage::Complete,
            items,
        }
    }

    fn native_task_fact(items: Vec<TaskItemSnapshot>) -> TaskSnapshotFact {
        TaskSnapshotFact {
            collection: entity("task_collection", "native-list"),
            session: None,
            run: None,
            team: None,
            native_collection_id: "native-list".to_string(),
            native_owner_id: None,
            kind: TaskCollectionKind::NativeTaskList,
            coverage: TaskSnapshotCoverage::ItemDocument,
            items,
        }
    }

    fn plan_fact(title: &str, content: &str) -> PlanSnapshotFact {
        PlanSnapshotFact {
            plan: entity("plan", "ship-it"),
            native_plan_id: "ship-it".to_string(),
            title: title.to_string(),
            content: content.to_string(),
            size_bytes: content.len() as u64,
            source_time: None,
        }
    }

    fn artifact_metadata_fact(
        message_id: &str,
        artifacts: Vec<ArtifactMetadataEntry>,
    ) -> ArtifactMetadataSnapshotFact {
        ArtifactMetadataSnapshotFact {
            session: entity("session", SESSION),
            native_message_id: message_id.to_string(),
            native_snapshot_message_id: "checkpoint".to_string(),
            observation_kind: ArtifactObservationKind::Delta,
            is_snapshot_update: false,
            source_time: Some(exact("2026-08-11T00:00:02.000Z")),
            artifacts,
        }
    }

    fn artifact_metadata_entry(
        artifact: EntityKey,
        native_artifact_id: Option<&str>,
        tracking_path: &str,
        backup_time: &str,
        capture: ArtifactCapture,
    ) -> ArtifactMetadataEntry {
        ArtifactMetadataEntry {
            artifact,
            native_artifact_id: native_artifact_id.map(str::to_string),
            tracking_path: tracking_path.to_string(),
            real_parent_dir: Some("/fixture/project/src".to_string()),
            version: 1,
            backup_time: exact(backup_time),
            capture,
        }
    }

    fn artifact_content_fact(artifact: EntityKey, content: &str) -> ArtifactContentFact {
        ArtifactContentFact {
            artifact,
            session: entity("session", SESSION),
            native_artifact_id: "71f902cd51ee4c6e@v1".to_string(),
            native_file_hash: "71f902cd51ee4c6e".to_string(),
            version: 1,
            content: content.as_bytes().to_vec(),
            size_bytes: content.len() as u64,
        }
    }

    fn workflow_snapshot_fact(
        native_status: &str,
        agent_count: u64,
        summary: &str,
    ) -> WorkflowSnapshotFact {
        WorkflowSnapshotFact {
            workflow: entity("workflow", "workflow-main"),
            session: entity("session", SESSION),
            project: entity("project", PROJECT),
            native_workflow_id: "wf_main".to_string(),
            native_task_id: "task-main".to_string(),
            name: "Main workflow".to_string(),
            native_status: native_status.to_string(),
            status: match native_status {
                "completed" => WorkflowStatus::Succeeded,
                "failed" => WorkflowStatus::Failed,
                "killed" => WorkflowStatus::Cancelled,
                "running" => WorkflowStatus::Running,
                other => WorkflowStatus::Other(other.to_string()),
            },
            default_model: "claude-sonnet".to_string(),
            script: "await run({ task: 'work' });".to_string(),
            script_path: "/fixture/workflows/main.js".to_string(),
            args: Some("--fixture".to_string()),
            summary: summary.to_string(),
            error: None,
            started_at: exact("2026-08-11T00:00:00.000Z"),
            finished_at: exact("2026-08-11T00:00:01.000Z"),
            duration_ms: 1_000,
            agent_count,
            total_tokens: 42,
            total_tool_calls: 3,
            native_snapshot: serde_json::json!({
                "runId": "wf_main",
                "status": native_status,
                "summary": summary,
                "agentCount": agent_count,
            }),
        }
    }

    fn workflow_member_fact(
        native_agent_id: &str,
        native_event_key: &str,
        kind: WorkflowMemberEventKind,
        result: Option<serde_json::Value>,
    ) -> WorkflowMemberEventFact {
        WorkflowMemberEventFact {
            workflow: entity("workflow", "workflow-main"),
            member: entity(
                "workflow_member",
                &format!("workflow-main/{native_agent_id}"),
            ),
            child_run: entity("run", &format!("{SESSION}\0wf_main\0{native_agent_id}")),
            session: entity("session", SESSION),
            project: entity("project", PROJECT),
            native_workflow_id: "wf_main".to_string(),
            native_agent_id: native_agent_id.to_string(),
            native_event_key: native_event_key.to_string(),
            kind,
            result,
        }
    }

    fn session_index_entry(
        session: EntityKey,
        native_session_id: &str,
        prompt: &str,
        summary: Option<&str>,
    ) -> SessionIndexEntrySnapshot {
        SessionIndexEntrySnapshot {
            session,
            native_session_id: native_session_id.to_string(),
            full_path: format!("/fixture/project/{native_session_id}.jsonl"),
            file_mtime_ms: 1_770_000_000_123,
            first_prompt: prompt.to_string(),
            summary: summary.map(str::to_string),
            message_count: 7,
            created_at: exact("2026-02-02T00:00:00.000Z"),
            modified_at: exact("2026-02-02T00:01:00.000Z"),
            git_branch: "main".to_string(),
            project_path: "/fixture/project".to_string(),
            is_sidechain: false,
        }
    }

    fn session_index_fact(
        project: EntityKey,
        native_project_key: &str,
        entries: Vec<SessionIndexEntrySnapshot>,
    ) -> SessionIndexSnapshotFact {
        SessionIndexSnapshotFact {
            project,
            native_project_key: native_project_key.to_string(),
            native_version: 1,
            original_path: Some("/fixture/project".to_string()),
            native_snapshot: serde_json::json!({
                "version": 1,
                "originalPath": "/fixture/project",
                "entryCount": entries.len(),
            }),
            entries,
        }
    }

    fn project_memory_fact(
        native_document_path: &str,
        title: &str,
        content: &str,
        is_index: bool,
    ) -> ProjectMemoryDocumentFact {
        ProjectMemoryDocumentFact {
            document: entity(
                "project_memory_document",
                &format!("{PROJECT}/{native_document_path}"),
            ),
            project: entity("project", PROJECT),
            native_project_key: PROJECT.to_string(),
            native_document_path: native_document_path.to_string(),
            title: title.to_string(),
            content: content.to_string(),
            size_bytes: content.len() as u64,
            is_index,
        }
    }

    fn persisted_tool_result_fact(
        native_tool_use_id: &str,
        content: &str,
    ) -> PersistedToolResultFact {
        PersistedToolResultFact {
            result: entity(
                "persisted_tool_result",
                &format!("{SESSION}/{native_tool_use_id}"),
            ),
            session: entity("session", SESSION),
            project: entity("project", PROJECT),
            native_project_key: PROJECT.to_string(),
            native_session_id: SESSION.to_string(),
            native_tool_use_id: native_tool_use_id.to_string(),
            native_document_path: format!("tool-results/{native_tool_use_id}.txt"),
            content: content.to_string(),
            size_bytes: content.len() as u64,
        }
    }

    fn interpretation_settings_fact(
        layer: InterpretationSettingsLayer,
        status: InterpretationSettingsDocumentStatus,
        settings: Option<InterpretationSettingsSnapshot>,
        error_code: Option<&str>,
    ) -> InterpretationSettingsFact {
        let native_document_path = match layer {
            InterpretationSettingsLayer::Global => "settings.json",
            InterpretationSettingsLayer::Local => "settings.local.json",
        };
        InterpretationSettingsFact {
            document: entity("interpretation_settings_document", native_document_path),
            scope: entity("interpretation_settings_scope", "root"),
            layer,
            native_document_path: native_document_path.to_string(),
            document_status: status,
            settings,
            error_code: error_code.map(str::to_string),
            size_bytes: 128,
        }
    }

    fn tool_message(
        native_message_id: &str,
        role: MessageRole,
        content: Vec<ContentBlock>,
    ) -> MessageFact {
        MessageFact {
            message: entity("message", native_message_id),
            session: entity("session", SESSION),
            run: entity("run", SESSION),
            native_message_id: Some(native_message_id.to_string()),
            native_kind: match &role {
                MessageRole::Assistant => "assistant",
                MessageRole::User => "user",
                _ => "fixture",
            }
            .to_string(),
            role,
            content,
            source_time: None,
            parent_native_message_id: None,
            model: None,
            search_text: None,
            raw_json: b"{}".to_vec(),
        }
    }

    fn team_fact(name: &str, members: &[(&str, &str)]) -> TeamSnapshotFact {
        let team = entity("team", "alpha");
        let members = members
            .iter()
            .map(|(agent_id, native_name)| TeamMemberSnapshot {
                member: entity("team_member", native_name),
                native_agent_id: (*agent_id).to_string(),
                native_name: (*native_name).to_string(),
                agent_type: Some("general-purpose".to_string()),
                model: Some("claude-sonnet".to_string()),
                prompt: Some("work".to_string()),
                color: Some("blue".to_string()),
                plan_mode_required: Some(false),
                joined_at: exact("2026-08-11T00:00:00.001Z"),
                tmux_pane_id: "%1".to_string(),
                cwd: "/fixture/project".to_string(),
                subscriptions: vec!["changes".to_string()],
                backend_type: Some("tmux".to_string()),
            })
            .collect::<Vec<_>>();
        TeamSnapshotFact {
            team,
            native_team_id: "alpha".to_string(),
            name: name.to_string(),
            description: Some("fixture team".to_string()),
            created_at: exact("2026-08-11T00:00:00.000Z"),
            lead_member: members.first().map(|member| member.member.clone()),
            native_lead_agent_id: members
                .first()
                .map(|member| member.native_agent_id.clone())
                .unwrap_or_else(|| "lead@alpha".to_string()),
            lead_session: entity("session", SESSION),
            native_lead_session_id: SESSION.to_string(),
            members,
        }
    }

    fn inbox_message(native_key: &str, text: &str, read: bool) -> TeamInboxMessageSnapshot {
        TeamInboxMessageSnapshot {
            message: entity("team_inbox_message", native_key),
            sender: entity("team_member", "worker"),
            native_message_id: Some(native_key.to_string()),
            native_kind: Some("message".to_string()),
            native_version: Some(1),
            native_sender_name: "worker".to_string(),
            text: text.to_string(),
            summary: None,
            color: Some("green".to_string()),
            source_time: exact("2026-08-11T00:00:01.000Z"),
            read,
        }
    }

    fn inbox_fact(messages: Vec<TeamInboxMessageSnapshot>) -> TeamInboxSnapshotFact {
        TeamInboxSnapshotFact {
            inbox: entity("team_inbox", "alpha/team-lead"),
            team: entity("team", "alpha"),
            recipient: entity("team_member", "team-lead"),
            native_team_id: "alpha".to_string(),
            native_recipient_name: "team-lead".to_string(),
            messages,
        }
    }

    fn commit_direct_batch(
        connection: &mut Connection,
        record: &SourceRecord,
        expected_generation: u64,
        expected_cursor: u64,
        clock: i64,
        batch: &FactBatch,
    ) {
        apply_fact_observation_commit(
            connection,
            &request(
                ExpectedSourceCursor::At {
                    generation: expected_generation,
                    committed_cursor: SourceCursor::append_offset(expected_cursor).into_bytes(),
                },
                record.generation,
                record.cursor_end.as_bytes().to_vec(),
                clock,
            ),
            batch,
        )
        .unwrap();
    }

    fn commit_direct_snapshot_batch(
        connection: &mut Connection,
        record: &SourceRecord,
        expected_generation: u64,
        expected_cursor: u64,
        clock: i64,
        batch: &FactBatch,
    ) {
        let mut commit = request(
            ExpectedSourceCursor::At {
                generation: expected_generation,
                committed_cursor: SourceCursor::append_offset(expected_cursor).into_bytes(),
            },
            record.generation,
            record.cursor_end.as_bytes().to_vec(),
            clock,
        );
        commit.stream.consistency = crate::adapter::ConsistencyPolicy::SnapshotReplace;
        apply_fact_observation_commit(connection, &commit, batch).unwrap();
    }

    fn commit_object_batch(
        connection: &mut Connection,
        object_key: &[u8],
        record: &SourceRecord,
        expected_generation: u64,
        expected_cursor: Vec<u8>,
        clock: i64,
        batch: &FactBatch,
    ) {
        apply_fact_observation_commit(
            connection,
            &request_for_object(
                object_key,
                ExpectedSourceCursor::At {
                    generation: expected_generation,
                    committed_cursor: expected_cursor,
                },
                record.generation,
                record.cursor_end.as_bytes().to_vec(),
                clock,
            ),
            batch,
        )
        .unwrap();
    }

    struct DecodeCommitState {
        expected_generation: u64,
        cursor: Vec<u8>,
        decoder_state: Option<Vec<u8>>,
    }

    fn decode_commit(
        connection: &mut Connection,
        adapter: &ClaudeCodeAdapter,
        object_context: &AdapterObjectContext,
        record: &SourceRecord,
        state: &mut DecodeCommitState,
        clock: i64,
    ) {
        let decoder = DecoderId::new(DECODER).unwrap();
        let mut batch = FactBatch::new(16, 8).unwrap();
        adapter
            .decode(
                DecodeContext {
                    decoder: &decoder,
                    object_context,
                    decoder_state: state.decoder_state.as_deref(),
                },
                record,
                &mut batch,
            )
            .unwrap();
        let committed_cursor = record.cursor_end.as_bytes().to_vec();
        let mut request = request(
            ExpectedSourceCursor::At {
                generation: state.expected_generation,
                committed_cursor: state.cursor.clone(),
            },
            record.generation,
            committed_cursor.clone(),
            clock,
        );
        if batch.next_decoder_state().is_some() {
            request.object.decoder_state_version = Some(adapter.manifest().contract_version);
        }
        let receipt = apply_fact_observation_commit(connection, &request, &batch).unwrap();
        state.cursor = committed_cursor;
        state.decoder_state = batch.next_decoder_state().map(ToOwned::to_owned);
        assert!(receipt.change_count >= 2);
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
        let mut state = DecodeCommitState {
            expected_generation: 1,
            cursor: SourceCursor::append_offset(0).into_bytes(),
            decoder_state: None,
        };
        for (index, record) in records.iter().enumerate() {
            decode_commit(
                &mut connection,
                &adapter,
                &context,
                record,
                &mut state,
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
        let mut state = DecodeCommitState {
            expected_generation: 1,
            cursor: SourceCursor::append_offset(0).into_bytes(),
            decoder_state: None,
        };
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
            decode_commit(
                &mut connection,
                &adapter,
                &context,
                &records[0],
                &mut state,
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

    fn usage_fact(
        native_subject: &str,
        accounting: UsageAccounting,
        quality: ValueQuality,
        values: [u64; 4],
    ) -> Fact {
        Fact::Usage(UsageFact {
            subject: entity("usage_subject", native_subject),
            session: entity("session", SESSION),
            scope: UsageScope::Session,
            accounting,
            quality,
            values: TokenUsage {
                input_tokens: values[0],
                output_tokens: values[1],
                cache_creation_tokens: values[2],
                cache_read_tokens: values[3],
            },
            model: Some("fixture-model".to_string()),
            source_time: None,
        })
    }

    fn usage_totals(connection: &Connection) -> [i64; 8] {
        connection
            .query_row(
                r#"SELECT exact_input_tokens, exact_output_tokens,
                          exact_cache_creation_tokens, exact_cache_read_tokens,
                          estimated_input_tokens, estimated_output_tokens,
                          estimated_cache_creation_tokens, estimated_cache_read_tokens
                   FROM usage_totals WHERE session_key = ?1"#,
                [entity("session", SESSION).as_bytes()],
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
            .unwrap()
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
                adapter_version: "1.0.0".to_string(),
                adapter_contract_version: 2,
                source_schema_versions: Vec::new(),
                capabilities: Vec::new(),
                discovered_at: 1,
                last_seen_at: input.clock,
            },
            stream: SourceStreamSpec {
                stream_key: input.stream.to_string(),
                driver_kind: "append_delimited_file".to_string(),
                decoder_key: input.decoder.to_string(),
                stream_state: "available".to_string(),
                last_reconciled_at: Some(input.clock),
                consistency: crate::adapter::ConsistencyPolicy::IncrementalCursor,
                retention: RawRetentionPolicy::Full,
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
                driver_checkpoint: None,
                driver_checkpoint_version: None,
                decoder_state: None,
                decoder_state_version: None,
                retry_state: None,
                size_bytes: None,
                mtime_ns: None,
                decoder_contract_version: 2,
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
                    identity_contract_version: 1,
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
            let mut decoder_state = None;
            for record in records {
                let mut batch = FactBatch::new(16, 8).unwrap();
                adapter
                    .decode(
                        DecodeContext {
                            decoder: &decoder_id,
                            object_context: &object_context,
                            decoder_state: decoder_state.as_deref(),
                        },
                        &record,
                        &mut batch,
                    )
                    .unwrap();
                let next_cursor = record.cursor_end.as_bytes().to_vec();
                let mut request = shadow_request(ShadowCommit {
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
                });
                if batch.next_decoder_state().is_some() {
                    request.object.decoder_state_version =
                        Some(adapter.manifest().contract_version);
                }
                apply_fact_observation_commit(&mut connection, &request, &batch).unwrap();
                cursor = next_cursor;
                decoder_state = batch.next_decoder_state().map(ToOwned::to_owned);
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
    type ExplicitDelegationRow = (
        Vec<u8>,
        String,
        String,
        String,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        i64,
    );
    type DelegationProvenanceRow = (
        Vec<u8>,
        String,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
    );

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
        let rows = connection
            .prepare(
                r#"
                SELECT cs.native_session_id, cm.native_kind,
                       cm.native_message_id, cm.source_time,
                       cm.raw_json, cm.raw_json_codec,
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
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let mut rows = rows
            .into_iter()
            .map(
                |(
                    session,
                    kind,
                    message,
                    time,
                    raw,
                    codec,
                    input,
                    output,
                    creation,
                    read,
                    text,
                )| {
                    let raw = crate::engine::storage_codec::decode(
                        &codec,
                        &raw,
                        64 * 1024 * 1024,
                        "decode parent parity payload",
                    )
                    .unwrap();
                    (
                        session,
                        kind,
                        message,
                        time,
                        normalized_json(String::from_utf8(raw).unwrap()),
                        input,
                        output,
                        creation,
                        read,
                        text,
                    )
                },
            )
            .collect::<Vec<_>>();
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
        let rows = connection
            .prepare(
                r#"
                SELECT cs.native_session_id, cm.source_time,
                       cm.raw_json, cm.raw_json_codec,
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
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let mut rows = rows
            .into_iter()
            .map(
                |(session, time, raw, codec, input, output, creation, read)| {
                    let raw = crate::engine::storage_codec::decode(
                        &codec,
                        &raw,
                        64 * 1024 * 1024,
                        "decode subagent parity payload",
                    )
                    .unwrap();
                    (
                        session,
                        time,
                        normalized_json(String::from_utf8(raw).unwrap()),
                        input,
                        output,
                        creation,
                        read,
                    )
                },
            )
            .collect::<Vec<_>>();
        rows.sort();
        rows
    }

    #[test]
    fn message_storage_compresses_native_and_normalized_content_with_policy_bound_audit() {
        let native_json = format!(
            "{{\"type\":\"assistant\",\"nativeOnly\":\"{}\"}}",
            "secret-padding-".repeat(1_024)
        )
        .into_bytes();
        let record = direct_record(1, 0, native_json.len() as u64, 20, &native_json);
        let normalized_text = "normalized-padding-".repeat(1_024);
        let mut message = tool_message(
            "compact-message",
            MessageRole::Assistant,
            vec![ContentBlock::Text {
                text: normalized_text.clone(),
            }],
        );
        message.raw_json = native_json.clone();
        let mut batch = FactBatch::new(1, 1).unwrap();
        batch.push(&record, Fact::Message(message)).unwrap();

        let encoded = encode_fact_payloads(&batch, RawRetentionPolicy::HashOnly).unwrap();
        let stored = &encoded[0];
        assert_eq!(
            stored.audit.codec,
            crate::engine::storage_codec::OMITTED_CODEC
        );
        assert!(stored.audit.bytes.is_empty());

        let raw = stored.message_raw.as_ref().unwrap();
        assert_eq!(raw.codec, crate::engine::storage_codec::ZSTD_V1_CODEC);
        assert!(raw.bytes.len() < native_json.len() / 4);
        assert!(stored.audit.bytes.len() + raw.bytes.len() < native_json.len() / 3);
        assert_eq!(
            crate::engine::storage_codec::decode(
                raw.codec,
                &raw.bytes,
                native_json.len(),
                "decode native test payload",
            )
            .unwrap(),
            native_json
        );
        let normalized = stored.message_content.as_ref().unwrap();
        assert_eq!(
            normalized.codec,
            crate::engine::storage_codec::ZSTD_V1_CODEC
        );
        let decoded_normalized = crate::engine::storage_codec::decode(
            normalized.codec,
            &normalized.bytes,
            1024 * 1024,
            "decode normalized test content",
        )
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<Vec<ContentBlock>>(&decoded_normalized).unwrap(),
            vec![ContentBlock::Text {
                text: normalized_text,
            }]
        );

        let full = encode_fact_payloads(&batch, RawRetentionPolicy::Full).unwrap();
        let full_audit = crate::engine::storage_codec::decode(
            full[0].audit.codec,
            &full[0].audit.bytes,
            1024 * 1024,
            "decode full audit test payload",
        )
        .unwrap();
        let full_audit: serde_json::Value = serde_json::from_slice(&full_audit).unwrap();
        assert!(full_audit["Message"].get("raw_json").is_none());

        let mut unknown_batch = FactBatch::new(1, 1).unwrap();
        unknown_batch
            .push(
                &record,
                Fact::UnknownRecord {
                    native_kind: Some("future".to_string()),
                    raw_payload: br#"{"shape":["string"]}"#.to_vec(),
                    reason: "preserved for a future decoder".to_string(),
                },
            )
            .unwrap();
        let diagnostic =
            encode_fact_payloads(&unknown_batch, RawRetentionPolicy::DiagnosticExcerpt).unwrap();
        assert_ne!(
            diagnostic[0].audit.codec,
            crate::engine::storage_codec::OMITTED_CODEC
        );
        assert!(!diagnostic[0].audit.bytes.is_empty());

        let mut connection = database();
        let mut hash_only_request = request(
            ExpectedSourceCursor::Absent,
            1,
            record.cursor_end.as_bytes().to_vec(),
            20,
        );
        hash_only_request.stream.retention = RawRetentionPolicy::HashOnly;
        apply_fact_observation_commit(&mut connection, &hash_only_request, &batch).unwrap();
        let (
            audit_bytes,
            audit_codec,
            native_bytes,
            native_codec,
            content_bytes,
            content_codec,
            retention,
            entity_is_null,
        ): (i64, String, i64, String, i64, String, String, i64) = connection
            .query_row(
                r#"
                SELECT length(fr.payload_json), fr.payload_codec,
                       length(cm.raw_json), cm.raw_json_codec,
                       length(cm.content_json), cm.content_json_codec,
                       ss.raw_retention,
                       fr.entity_key IS NULL
                FROM fact_records fr
                JOIN canonical_messages cm ON cm.fact_id = fr.fact_id
                JOIN source_streams ss ON ss.source_stream_id = fr.source_stream_id
                "#,
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(audit_bytes, 0);
        assert_eq!(audit_codec, crate::engine::storage_codec::OMITTED_CODEC);
        assert!(native_bytes < i64::try_from(native_json.len() / 4).unwrap());
        assert_eq!(native_codec, crate::engine::storage_codec::ZSTD_V1_CODEC);
        assert!(content_bytes < 256);
        assert_eq!(content_codec, crate::engine::storage_codec::ZSTD_V1_CODEC);
        assert_eq!(retention, "hash_only");
        assert_eq!(entity_is_null, 1);
    }

    #[test]
    fn batched_message_upsert_and_content_replacement_keep_the_last_duplicate() {
        let first_record = direct_record(1, 0, 1, 20, b"first");
        let second_record = direct_record(1, 1, 2, 21, b"second");
        let mut first = tool_message(
            "duplicate-message",
            MessageRole::User,
            vec![ContentBlock::Text {
                text: "first".to_string(),
            }],
        );
        first.raw_json = br#"{"version":1}"#.to_vec();
        let mut second = tool_message(
            "duplicate-message",
            MessageRole::Assistant,
            vec![ContentBlock::Thinking {
                text: "final".to_string(),
                redacted: false,
            }],
        );
        second.raw_json = br#"{"version":2}"#.to_vec();
        let mut batch = FactBatch::new(2, 1).unwrap();
        batch.push(&first_record, Fact::Message(first)).unwrap();
        batch.push(&second_record, Fact::Message(second)).unwrap();
        let mut connection = database();
        apply_fact_observation_commit(
            &mut connection,
            &request(
                ExpectedSourceCursor::Absent,
                1,
                second_record.cursor_end.as_bytes().to_vec(),
                20,
            ),
            &batch,
        )
        .unwrap();

        assert_eq!(
            connection
                .query_row("SELECT role FROM canonical_messages", [], |row| row
                    .get::<_, String>(0),)
                .unwrap(),
            "assistant"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT content_kind FROM canonical_message_content_blocks",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "thinking"
        );
        assert_eq!(count(&connection, "canonical_message_content_blocks"), 1);
        assert_eq!(count(&connection, "fact_records"), 2);
    }

    #[test]
    fn new_source_object_replaces_existing_message_content_blocks() {
        let mut connection = database();
        remember_test_object(b"object-a");
        remember_test_object(b"object-b");
        let first_record = object_record(
            1,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            20,
            b"first",
        );
        let mut first = tool_message(
            "shared-message",
            MessageRole::User,
            vec![
                ContentBlock::Text {
                    text: "old-a".to_string(),
                },
                ContentBlock::Text {
                    text: "old-b".to_string(),
                },
            ],
        );
        first.raw_json = br#"{"version":1}"#.to_vec();
        let mut first_batch = FactBatch::new(1, 1).unwrap();
        first_batch
            .push(&first_record, Fact::Message(first))
            .unwrap();
        apply_fact_observation_commit(
            &mut connection,
            &request_for_object(
                b"object-a",
                ExpectedSourceCursor::Absent,
                1,
                first_record.cursor_end.as_bytes().to_vec(),
                20,
            ),
            &first_batch,
        )
        .unwrap();
        assert_eq!(count(&connection, "canonical_message_content_blocks"), 2);

        let second_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            30,
            b"second",
        );
        let mut second = tool_message(
            "shared-message",
            MessageRole::Assistant,
            vec![ContentBlock::Thinking {
                text: "replacement".to_string(),
                redacted: false,
            }],
        );
        second.raw_json = br#"{"version":2}"#.to_vec();
        let mut second_batch = FactBatch::new(1, 1).unwrap();
        second_batch
            .push(&second_record, Fact::Message(second))
            .unwrap();
        apply_fact_observation_commit(
            &mut connection,
            &request_for_object(
                b"object-b",
                ExpectedSourceCursor::Absent,
                1,
                second_record.cursor_end.as_bytes().to_vec(),
                30,
            ),
            &second_batch,
        )
        .unwrap();

        assert_eq!(
            connection
                .query_row(
                    "SELECT content_kind FROM canonical_message_content_blocks",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "thinking"
        );
        assert_eq!(count(&connection, "canonical_message_content_blocks"), 1);
    }

    #[test]
    fn fact_batch_insert_crosses_the_full_chunk_and_tail_boundary() {
        const FACTS: usize = FACT_INSERT_BATCH_ROWS + 1;
        let mut batch = FactBatch::new(FACTS, 1).unwrap();
        let mut last_cursor = Vec::new();
        for index in 0..FACTS {
            let record = direct_record(
                1,
                u64::try_from(index).unwrap(),
                u64::try_from(index + 1).unwrap(),
                20 + i64::try_from(index).unwrap(),
                b"{}",
            );
            last_cursor = record.cursor_end.as_bytes().to_vec();
            batch
                .push(
                    &record,
                    Fact::UnknownRecord {
                        native_kind: Some("batch-boundary".to_string()),
                        raw_payload: Vec::new(),
                        reason: "batch-boundary".to_string(),
                    },
                )
                .unwrap();
        }
        let mut request = request(ExpectedSourceCursor::Absent, 1, last_cursor, 20);
        request.stream.retention = RawRetentionPolicy::HashOnly;
        let mut connection = database();
        apply_fact_observation_commit(&mut connection, &request, &batch).unwrap();
        assert_eq!(
            count(&connection, "fact_records"),
            i64::try_from(FACTS).unwrap()
        );
    }

    #[test]
    fn incremental_run_evidence_order_matches_sql_ranks() {
        let declared = FoldedRunEvidence {
            fact_id: vec![1],
            kind: "run_declared",
            kind_rank: evidence_kind_rank(EvidenceKind::RunDeclared),
            strength_rank: evidence_strength_rank(EvidenceStrength::Layout),
            source_generation: 1,
            cursor_end: vec![1],
            last_commit_seq: 1,
            source_time: None,
            last_activity_at: None,
        };
        let started = FoldedRunEvidence {
            fact_id: vec![2],
            kind: "run_started",
            kind_rank: evidence_kind_rank(EvidenceKind::RunStarted),
            strength_rank: evidence_strength_rank(EvidenceStrength::NativeActivity),
            source_generation: 1,
            cursor_end: vec![2],
            last_commit_seq: 2,
            source_time: Some("2026-01-01T00:00:00.000Z".to_string()),
            last_activity_at: Some("2026-01-01T00:00:00.000Z".to_string()),
        };
        let succeeded = FoldedRunEvidence {
            fact_id: vec![3],
            kind: "terminal_succeeded",
            kind_rank: evidence_kind_rank(EvidenceKind::TerminalSucceeded),
            strength_rank: evidence_strength_rank(EvidenceStrength::NativeExplicit),
            source_generation: 1,
            cursor_end: vec![3],
            last_commit_seq: 3,
            source_time: Some("2026-01-01T00:00:01.000Z".to_string()),
            last_activity_at: None,
        };
        assert!(run_evidence_outranks(&started, &declared));
        assert!(run_evidence_outranks(&succeeded, &started));
        assert!(!run_evidence_outranks(&declared, &succeeded));
        assert_eq!(run_state_label(succeeded.kind), "succeeded");
        assert_eq!(
            max_optional_time(
                started.last_activity_at.clone(),
                succeeded.last_activity_at.clone()
            ),
            started.last_activity_at
        );
    }

    #[test]
    fn paired_message_owns_native_activity_evidence_without_losing_projection_semantics() {
        let mut connection = database();
        register_object(&mut connection);
        let record = direct_record(1, 0, 1, 20, b"message-with-activity");
        let source_time = exact("2026-08-11T00:00:10.000Z");
        let run = entity("run", SESSION);
        let standalone_run = entity("run", "standalone-activity");
        let mut message = tool_message(
            "message-with-activity",
            MessageRole::Assistant,
            vec![ContentBlock::Text {
                text: "hello".to_string(),
            }],
        );
        message.source_time = Some(source_time.clone());

        let mut batch = FactBatch::new(3, 1).unwrap();
        let message_id = batch.push(&record, Fact::Message(message)).unwrap();
        let redundant_evidence_id = batch
            .push(
                &record,
                Fact::RunEvidence(RunEvidenceFact {
                    run: run.clone(),
                    kind: EvidenceKind::ActivityObserved,
                    strength: EvidenceStrength::NativeActivity,
                    native_state: Some("response_item/message".to_string()),
                    source_time: Some(source_time),
                }),
            )
            .unwrap();
        let standalone_evidence_id = batch
            .push(
                &record,
                Fact::RunEvidence(RunEvidenceFact {
                    run: standalone_run.clone(),
                    kind: EvidenceKind::ActivityObserved,
                    strength: EvidenceStrength::NativeActivity,
                    native_state: Some("event_msg/token_count".to_string()),
                    source_time: Some(exact("2026-08-11T00:00:11.000Z")),
                }),
            )
            .unwrap();

        commit_direct_batch(&mut connection, &record, 1, 0, 21, &batch);

        let paired: (Vec<u8>, String, String, i64, Option<String>) = connection
            .query_row(
                r#"
                SELECT re.fact_id, fr.fact_kind, re.evidence_strength,
                       re.evidence_count, re.native_state
                FROM run_evidence re
                JOIN fact_records fr ON fr.fact_id = re.fact_id
                WHERE re.run_key = ?1
                "#,
                [run.as_bytes()],
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
        assert_eq!(paired.0, message_id.as_bytes());
        assert_eq!(paired.1, "message");
        assert_eq!(paired.2, "native_activity");
        assert_eq!(paired.3, 1);
        assert_eq!(paired.4.as_deref(), Some("response_item/message"));
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM fact_records WHERE fact_id = ?1",
                    [redundant_evidence_id.as_bytes()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );

        let standalone: (Vec<u8>, String) = connection
            .query_row(
                r#"
                SELECT re.fact_id, fr.fact_kind
                FROM run_evidence re
                JOIN fact_records fr ON fr.fact_id = re.fact_id
                WHERE re.run_key = ?1
                "#,
                [standalone_run.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(standalone.0, standalone_evidence_id.as_bytes());
        assert_eq!(standalone.1, "run_evidence");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM fact_records", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            2
        );
        assert_eq!(
            connection
                .query_row("PRAGMA foreign_key_check", [], |_| Ok(1_i64))
                .optional()
                .unwrap(),
            None
        );

        // If either side is ambiguous, retain every fact instead of guessing
        // which observation should own the evidence.
        let ambiguous_record = direct_record(1, 1, 2, 22, b"ambiguous-message-activity");
        let ambiguous_time = exact("2026-08-11T00:00:12.000Z");
        let mut ambiguous = FactBatch::new(3, 1).unwrap();
        for native_message_id in ["ambiguous-one", "ambiguous-two"] {
            let mut message = tool_message(
                native_message_id,
                MessageRole::Assistant,
                vec![ContentBlock::Text {
                    text: native_message_id.to_string(),
                }],
            );
            message.source_time = Some(ambiguous_time.clone());
            ambiguous
                .push(&ambiguous_record, Fact::Message(message))
                .unwrap();
        }
        ambiguous
            .push(
                &ambiguous_record,
                Fact::RunEvidence(RunEvidenceFact {
                    run,
                    kind: EvidenceKind::ActivityObserved,
                    strength: EvidenceStrength::NativeActivity,
                    native_state: None,
                    source_time: Some(ambiguous_time),
                }),
            )
            .unwrap();
        assert!(redundant_activity_evidence_owners(&ambiguous).is_empty());
    }

    #[test]
    fn run_evidence_compaction_preserves_count_winner_activity_and_replacement() {
        let mut connection = database();
        register_object(&mut connection);
        let run = entity("run", SESSION);

        let first_record = direct_record(1, 0, 1, 20, b"activity-newer-time");
        let mut first = FactBatch::new(1, 1).unwrap();
        first
            .push(
                &first_record,
                Fact::RunEvidence(RunEvidenceFact {
                    run: run.clone(),
                    kind: EvidenceKind::ActivityObserved,
                    strength: EvidenceStrength::NativeActivity,
                    native_state: Some("first".to_string()),
                    source_time: Some(exact("2026-08-11T00:00:10.000Z")),
                }),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &first_record, 1, 0, 21, &first);

        // Cursor order, not timestamp order, selects the decisive evidence.
        // The independent activity maximum must nevertheless retain 00:00:10.
        let second_record = direct_record(1, 1, 2, 22, b"activity-later-cursor");
        let mut second = FactBatch::new(1, 1).unwrap();
        let second_id = second
            .push(
                &second_record,
                Fact::RunEvidence(RunEvidenceFact {
                    run: run.clone(),
                    kind: EvidenceKind::ActivityObserved,
                    strength: EvidenceStrength::NativeActivity,
                    native_state: Some("second".to_string()),
                    source_time: Some(exact("2026-08-11T00:00:05.000Z")),
                }),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &second_record, 1, 1, 23, &second);

        let compact: (i64, i64, Vec<u8>, String, Option<String>) = connection
            .query_row(
                r#"
                SELECT COUNT(*), SUM(evidence_count), fact_id, native_state,
                       last_activity_at
                FROM run_evidence WHERE run_key = ?1
                "#,
                [run.as_bytes()],
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
        assert_eq!(compact.0, 1);
        assert_eq!(compact.1, 2);
        assert_eq!(compact.2, second_id.as_bytes());
        assert_eq!(compact.3, "second");
        assert_eq!(compact.4.as_deref(), Some("2026-08-11T00:00:10.000Z"));
        let active: (String, Vec<u8>, Option<String>) = connection
            .query_row(
                "SELECT state, decisive_evidence_id, last_activity_at FROM observed_run_states WHERE run_key = ?1",
                [run.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(active.0, "active");
        assert_eq!(active.1, second_id.as_bytes());
        assert_eq!(active.2, compact.4);

        let terminal_record = direct_record(1, 2, 3, 24, b"terminal");
        let mut terminal = FactBatch::new(1, 1).unwrap();
        let terminal_id = terminal
            .push(
                &terminal_record,
                Fact::RunEvidence(RunEvidenceFact {
                    run: run.clone(),
                    kind: EvidenceKind::TerminalSucceeded,
                    strength: EvidenceStrength::NativeExplicit,
                    native_state: Some("done".to_string()),
                    source_time: Some(exact("2026-08-11T00:00:20.000Z")),
                }),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &terminal_record, 1, 2, 25, &terminal);
        let succeeded: (String, Vec<u8>, Option<String>, Option<String>, i64) = connection
            .query_row(
                r#"
                SELECT ors.state, ors.decisive_evidence_id,
                       ors.last_activity_at, ors.terminal_at,
                       (SELECT SUM(evidence_count) FROM run_evidence WHERE run_key = ?1)
                FROM observed_run_states ors WHERE ors.run_key = ?1
                "#,
                [run.as_bytes()],
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
        assert_eq!(succeeded.0, "succeeded");
        assert_eq!(succeeded.1, terminal_id.as_bytes());
        assert_eq!(succeeded.2.as_deref(), Some("2026-08-11T00:00:10.000Z"));
        assert_eq!(succeeded.3.as_deref(), Some("2026-08-11T00:00:20.000Z"));
        assert_eq!(succeeded.4, 3);

        // A new generation must retract both compact categories and their
        // counts before reducing solely from replacement evidence.
        let replacement_record = direct_record(2, 0, 1, 26, b"replacement");
        let mut replacement = FactBatch::new(1, 1).unwrap();
        let replacement_id = replacement
            .push(
                &replacement_record,
                Fact::RunEvidence(RunEvidenceFact {
                    run: run.clone(),
                    kind: EvidenceKind::WaitingObserved,
                    strength: EvidenceStrength::NativeExplicit,
                    native_state: Some("waiting".to_string()),
                    source_time: Some(exact("2026-08-11T00:00:30.000Z")),
                }),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &replacement_record, 1, 3, 27, &replacement);
        let replaced: (String, Vec<u8>, i64, i64) = connection
            .query_row(
                r#"
                SELECT ors.state, ors.decisive_evidence_id,
                       (SELECT COUNT(*) FROM run_evidence WHERE run_key = ?1),
                       (SELECT SUM(evidence_count) FROM run_evidence WHERE run_key = ?1)
                FROM observed_run_states ors WHERE ors.run_key = ?1
                "#,
                [run.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            replaced,
            (
                "waiting".to_string(),
                replacement_id.as_bytes().to_vec(),
                1,
                1
            )
        );
        assert_eq!(
            connection
                .query_row("PRAGMA foreign_key_check", [], |_| Ok(1_i64))
                .optional()
                .unwrap(),
            None
        );
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
        let mut state = DecodeCommitState {
            expected_generation: 1,
            cursor: checkpoint.cursor().into_bytes(),
            decoder_state: None,
        };
        for (index, record) in records.iter().enumerate() {
            decode_commit(
                &mut cold,
                &adapter,
                &context,
                record,
                &mut state,
                80 + index as i64,
            );
            state.expected_generation = 2;
        }
        assert_eq!(baseline, semantic_snapshot(&cold));
        assert_eq!(count(&cold, "canonical_messages"), 2);
        assert_eq!(count(&cold, "usage_contributions"), 2);
        assert_eq!(
            count(&cold, "fact_records"),
            6,
            "each message owns its paired native-activity observation"
        );

        let before_audit = semantic_snapshot(&cold);
        rebuild_usage_totals_for_audit(&mut cold).unwrap();
        assert_eq!(before_audit, semantic_snapshot(&cold));
    }

    #[test]
    fn usage_snapshot_replaces_its_series_and_can_reclassify_quality() {
        let mut connection = database();
        register_object(&mut connection);

        let first = direct_record(1, 0, 1, 20, b"snapshot-one");
        let mut first_batch = FactBatch::new(2, 2).unwrap();
        first_batch
            .push(
                &first,
                usage_fact(
                    "session-counter",
                    UsageAccounting::Snapshot,
                    ValueQuality::NativeExact,
                    [10, 2, 0, 0],
                ),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &first, 1, 0, 21, &first_batch);
        assert_eq!(usage_totals(&connection), [10, 2, 0, 0, 0, 0, 0, 0]);

        let replacement = direct_record(1, 1, 2, 22, b"snapshot-two");
        let mut replacement_batch = FactBatch::new(2, 2).unwrap();
        replacement_batch
            .push(
                &replacement,
                usage_fact(
                    "session-counter",
                    UsageAccounting::Snapshot,
                    ValueQuality::Estimated,
                    [14, 3, 1, 0],
                ),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &replacement, 1, 1, 23, &replacement_batch);

        assert_eq!(count(&connection, "usage_contributions"), 1);
        assert_eq!(usage_totals(&connection), [0, 0, 0, 0, 14, 3, 1, 0]);
    }

    #[test]
    fn typed_fact_commit_persists_stamped_dependency_reads_atomically() {
        let mut connection = database();
        register_object(&mut connection);
        let record = direct_record(1, 0, 1, 20, b"dependency-backed");
        let mut batch = FactBatch::new(2, 2).unwrap();
        batch
            .push(
                &record,
                usage_fact(
                    "dependency-counter",
                    UsageAccounting::Delta,
                    ValueQuality::NativeExact,
                    [1, 2, 3, 4],
                ),
            )
            .unwrap();
        batch
            .add_dependency_read(DependencyRevision {
                source_instance_id: record.source_instance_id,
                root_name: "sessions".to_string(),
                object_key: b"session/summary.json".to_vec(),
                revision: [7; 32],
            })
            .unwrap();

        commit_direct_batch(&mut connection, &record, 1, 0, 21, &batch);
        let stored: (i64, String, Vec<u8>, Vec<u8>) = connection
            .query_row(
                "SELECT source_instance_id, root_name, object_key, revision FROM fact_dependency_reads",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(stored.0, record.source_instance_id as i64);
        assert_eq!(stored.1, "sessions");
        assert_eq!(stored.2, b"session/summary.json");
        assert_eq!(stored.3, vec![7; 32]);
    }

    #[test]
    fn cumulative_usage_differences_and_counter_resets_never_double_count() {
        let mut connection = database();
        register_object(&mut connection);
        let samples = [
            (0, 1, b"cumulative-100".as_slice(), [100, 10, 0, 0]),
            (1, 2, b"cumulative-150".as_slice(), [150, 14, 2, 0]),
            (2, 3, b"cumulative-reset".as_slice(), [20, 3, 0, 1]),
        ];
        for (index, (start, end, payload, values)) in samples.into_iter().enumerate() {
            let record = direct_record(1, start, end, 30 + index as i64, payload);
            let mut batch = FactBatch::new(2, 2).unwrap();
            batch
                .push(
                    &record,
                    usage_fact(
                        "session-counter",
                        UsageAccounting::Cumulative,
                        ValueQuality::NativeExact,
                        values,
                    ),
                )
                .unwrap();
            commit_direct_batch(
                &mut connection,
                &record,
                1,
                start,
                40 + index as i64,
                &batch,
            );
        }

        assert_eq!(count(&connection, "usage_contributions"), 3);
        assert_eq!(usage_totals(&connection), [170, 17, 2, 1, 0, 0, 0, 0]);
    }

    #[test]
    fn late_cumulative_sample_repairs_only_its_immediate_successor() {
        let mut connection = database();
        register_object(&mut connection);

        let first = direct_record(1, 0, 10, 50, b"cumulative-100");
        let mut first_batch = FactBatch::new(2, 2).unwrap();
        first_batch
            .push(
                &first,
                usage_fact(
                    "session-counter",
                    UsageAccounting::Cumulative,
                    ValueQuality::NativeExact,
                    [100, 0, 0, 0],
                ),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &first, 1, 0, 51, &first_batch);

        let third = direct_record(1, 20, 30, 52, b"cumulative-180");
        let mut third_batch = FactBatch::new(2, 2).unwrap();
        third_batch
            .push(
                &third,
                usage_fact(
                    "session-counter",
                    UsageAccounting::Cumulative,
                    ValueQuality::NativeExact,
                    [180, 0, 0, 0],
                ),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &third, 1, 10, 53, &third_batch);
        assert_eq!(usage_totals(&connection)[0], 180);

        let second = direct_record(1, 10, 20, 54, b"cumulative-140");
        let mut second_batch = FactBatch::new(2, 2).unwrap();
        second_batch
            .push(
                &second,
                usage_fact(
                    "session-counter",
                    UsageAccounting::Cumulative,
                    ValueQuality::NativeExact,
                    [140, 0, 0, 0],
                ),
            )
            .unwrap();
        apply_fact_observation_commit(
            &mut connection,
            &request(
                ExpectedSourceCursor::At {
                    generation: 1,
                    committed_cursor: SourceCursor::append_offset(30).into_bytes(),
                },
                1,
                SourceCursor::append_offset(30).into_bytes(),
                55,
            ),
            &second_batch,
        )
        .unwrap();

        assert_eq!(count(&connection, "usage_contributions"), 3);
        assert_eq!(usage_totals(&connection)[0], 180);
        let contributions = connection
            .prepare("SELECT input_tokens FROM usage_contributions ORDER BY cursor_end")
            .unwrap()
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(contributions, vec![100, 40, 40]);
    }

    #[cfg(feature = "legacy-oracle")]
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
    fn delegation_child_first_late_correlates_and_replays_without_completion() {
        let mut connection = database();
        register_object(&mut connection);
        let session = entity("session", SESSION);
        let parent = entity("run", SESSION);
        let child = entity("run", &format!("{SESSION}\0\0child"));

        let child_record = direct_record(1, 0, 1, 20, b"child");
        let mut child_batch = FactBatch::new(4, 2).unwrap();
        child_batch
            .push(
                &child_record,
                Fact::Run(RunFact {
                    run: child.clone(),
                    session: session.clone(),
                    native_run_id: "child".to_string(),
                    parent_run: Some(parent.clone()),
                }),
            )
            .unwrap();
        child_batch
            .push(
                &child_record,
                Fact::Delegation(DelegationFact {
                    child_run: child.clone(),
                    parent_run: Some(parent.clone()),
                    session: session.clone(),
                    kind: DelegationKind::VendorNativeSubagent,
                    relation_strength: RelationStrength::Layout,
                    native_child_id: Some("child".to_string()),
                    native_task_id: None,
                    label: None,
                    prompt: None,
                    cwd: Some("/repo".to_string()),
                    worktree_path: None,
                    source_time: None,
                }),
            )
            .unwrap();
        child_batch
            .push(
                &child_record,
                Fact::RunEvidence(RunEvidenceFact {
                    run: child.clone(),
                    kind: EvidenceKind::ActivityObserved,
                    strength: EvidenceStrength::NativeActivity,
                    native_state: None,
                    source_time: None,
                }),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &child_record, 1, 0, 21, &child_batch);
        let unresolved: (String, i64, i64) = connection
            .query_row(
                "SELECT relation_status, child_present, parent_present FROM canonical_delegations WHERE child_run_key = ?1",
                [child.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(unresolved, ("unresolved_parent".to_string(), 1, 0));

        let parent_record = direct_record(1, 1, 2, 22, b"parent");
        let mut parent_batch = FactBatch::new(2, 2).unwrap();
        parent_batch
            .push(
                &parent_record,
                Fact::Run(RunFact {
                    run: parent.clone(),
                    session: session.clone(),
                    native_run_id: SESSION.to_string(),
                    parent_run: None,
                }),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &parent_record, 1, 1, 23, &parent_batch);
        assert_eq!(
            connection
                .query_row(
                    "SELECT relation_status FROM canonical_delegations WHERE child_run_key = ?1",
                    [child.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "resolved"
        );

        let replay_child = direct_record(2, 0, 1, 24, b"child");
        let mut replay_child_batch = FactBatch::new(4, 2).unwrap();
        for fact in [
            Fact::Run(RunFact {
                run: child.clone(),
                session: session.clone(),
                native_run_id: "child".to_string(),
                parent_run: Some(parent.clone()),
            }),
            Fact::Delegation(DelegationFact {
                child_run: child.clone(),
                parent_run: Some(parent.clone()),
                session: session.clone(),
                kind: DelegationKind::VendorNativeSubagent,
                relation_strength: RelationStrength::Layout,
                native_child_id: Some("child".to_string()),
                native_task_id: None,
                label: None,
                prompt: None,
                cwd: Some("/repo".to_string()),
                worktree_path: None,
                source_time: None,
            }),
            Fact::RunEvidence(RunEvidenceFact {
                run: child.clone(),
                kind: EvidenceKind::ActivityObserved,
                strength: EvidenceStrength::NativeActivity,
                native_state: None,
                source_time: None,
            }),
        ] {
            replay_child_batch.push(&replay_child, fact).unwrap();
        }
        commit_direct_batch(
            &mut connection,
            &replay_child,
            1,
            2,
            25,
            &replay_child_batch,
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT relation_status FROM canonical_delegations WHERE child_run_key = ?1",
                    [child.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "unresolved_parent"
        );
        let replay_parent = direct_record(2, 1, 2, 26, b"parent");
        let mut replay_parent_batch = FactBatch::new(2, 2).unwrap();
        replay_parent_batch
            .push(
                &replay_parent,
                Fact::Run(RunFact {
                    run: parent,
                    session,
                    native_run_id: SESSION.to_string(),
                    parent_run: None,
                }),
            )
            .unwrap();
        commit_direct_batch(
            &mut connection,
            &replay_parent,
            2,
            1,
            27,
            &replay_parent_batch,
        );
        let final_state: (String, i64, i64, i64) = connection
            .query_row(
                r#"
                SELECT cd.relation_status, cd.assertion_count,
                       cd.competing_relation_count,
                       (SELECT COUNT(*) FROM delegation_assertions)
                FROM canonical_delegations cd WHERE child_run_key = ?1
                "#,
                [child.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(final_state, ("resolved".to_string(), 1, 0, 1));
        assert_eq!(
            connection
                .query_row(
                    "SELECT state FROM observed_run_states WHERE run_key = ?1",
                    [child.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "active"
        );
    }

    #[test]
    fn delegation_metadata_replaces_sidecars_and_correlates_in_either_order() {
        let mut connection = database();
        register_object(&mut connection);
        let session = entity("session", SESSION);
        let child = entity("run", &format!("{SESSION}\0\0child-meta"));
        let metadata_fact = |agent_type: &str, description: &str, worktree_path: &str| {
            Fact::DelegationMetadata(DelegationMetadataFact {
                child_run: child.clone(),
                session: session.clone(),
                native_child_id: "child-meta".to_string(),
                agent_type: agent_type.to_string(),
                description: Some(description.to_string()),
                name: Some("metadata-child".to_string()),
                spawn_depth: Some(1),
                worktree_path: Some(worktree_path.to_string()),
                native_task_id: Some("tool-meta".to_string()),
            })
        };

        let initial_cursor = SourceCursor::append_offset(0);
        let first_cursor = SourceCursor::snapshot(crate::source::Revision::digest(b"meta-one"));
        let first_record = object_record(
            1,
            1,
            initial_cursor.clone(),
            first_cursor.clone(),
            20,
            b"meta-one",
        );
        let mut first_batch = FactBatch::new(2, 2).unwrap();
        first_batch
            .push(
                &first_record,
                metadata_fact("general-purpose", "first description", "/repo/first"),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            b"fixture-transcript",
            &first_record,
            1,
            initial_cursor.as_bytes().to_vec(),
            21,
            &first_batch,
        );
        let sidecar_first: (String, String, i64) = connection
            .query_row(
                r#"
                SELECT metadata_status, agent_type, run_present
                FROM canonical_delegation_metadata WHERE child_run_key = ?1
                "#,
                [child.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            sidecar_first,
            (
                "unresolved_run".to_string(),
                "general-purpose".to_string(),
                0
            )
        );

        let child_object_key = b"child-transcript";
        apply_observation_commit(
            &mut connection,
            &request_for_object(
                child_object_key,
                ExpectedSourceCursor::Absent,
                1,
                SourceCursor::append_offset(0).into_bytes(),
                22,
            ),
        )
        .unwrap();
        let child_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            23,
            b"child-run",
        );
        let mut child_batch = FactBatch::new(2, 2).unwrap();
        child_batch
            .push(
                &child_record,
                Fact::Run(RunFact {
                    run: child.clone(),
                    session: session.clone(),
                    native_run_id: format!("{SESSION}\0\0child-meta"),
                    parent_run: None,
                }),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            child_object_key,
            &child_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            24,
            &child_batch,
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT metadata_status FROM canonical_delegation_metadata WHERE child_run_key = ?1",
                    [child.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "resolved"
        );

        let second_cursor = SourceCursor::snapshot(crate::source::Revision::digest(b"meta-two"));
        let second_record = object_record(
            1,
            1,
            first_cursor.clone(),
            second_cursor.clone(),
            25,
            b"meta-two",
        );
        let mut second_batch = FactBatch::new(2, 2).unwrap();
        second_batch
            .push(
                &second_record,
                metadata_fact("Explore", "updated description", "/repo/updated"),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            b"fixture-transcript",
            &second_record,
            1,
            first_cursor.as_bytes().to_vec(),
            26,
            &second_batch,
        );
        let replaced: (String, String, i64, i64) = connection
            .query_row(
                r#"
                SELECT agent_type, worktree_path,
                       (SELECT COUNT(*) FROM delegation_metadata_assertions),
                       (SELECT COUNT(*) FROM fact_records WHERE fact_kind = 'delegation_metadata')
                FROM canonical_delegation_metadata WHERE child_run_key = ?1
                "#,
                [child.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            replaced,
            ("Explore".to_string(), "/repo/updated".to_string(), 1, 1)
        );

        let deleted_cursor =
            SourceCursor::snapshot(crate::source::Revision::digest(b"meta-deleted"));
        let empty_batch = FactBatch::new(2, 2).unwrap();
        apply_fact_observation_commit(
            &mut connection,
            &request_for_object(
                b"fixture-transcript",
                ExpectedSourceCursor::At {
                    generation: 1,
                    committed_cursor: second_cursor.as_bytes().to_vec(),
                },
                1,
                deleted_cursor.as_bytes().to_vec(),
                27,
            ),
            &empty_batch,
        )
        .unwrap();
        assert_eq!(count(&connection, "canonical_delegation_metadata"), 0);
        assert_eq!(count(&connection, "delegation_metadata_assertions"), 0);
        assert_eq!(
            connection
                .query_row(
                    r#"
                    SELECT operation FROM change_log
                    WHERE topic = 'runtime.delegation-metadata.changed'
                      AND entity_key = ?1
                    ORDER BY commit_seq DESC LIMIT 1
                    "#,
                    [child.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "delete"
        );

        let third_cursor = SourceCursor::snapshot(crate::source::Revision::digest(b"meta-three"));
        let third_record = object_record(
            1,
            1,
            deleted_cursor.clone(),
            third_cursor,
            28,
            b"meta-three",
        );
        let mut third_batch = FactBatch::new(2, 2).unwrap();
        third_batch
            .push(
                &third_record,
                metadata_fact("Plan", "recreated after run", "/repo/recreated"),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            b"fixture-transcript",
            &third_record,
            1,
            deleted_cursor.as_bytes().to_vec(),
            29,
            &third_batch,
        );
        let transcript_first: (String, String, i64) = connection
            .query_row(
                r#"
                SELECT metadata_status, agent_type, run_present
                FROM canonical_delegation_metadata WHERE child_run_key = ?1
                "#,
                [child.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            transcript_first,
            ("resolved".to_string(), "Plan".to_string(), 1)
        );
    }

    #[test]
    fn native_spawn_and_metadata_join_in_either_order_and_retract_to_layout() {
        let mut connection = database();
        register_object(&mut connection);
        let metadata_object = b"native-child-meta";
        let child_object = b"native-child-transcript";
        for (object_key, clock) in [
            (metadata_object.as_slice(), 12),
            (child_object.as_slice(), 14),
        ] {
            apply_observation_commit(
                &mut connection,
                &request_for_object(
                    object_key,
                    ExpectedSourceCursor::Absent,
                    1,
                    SourceCursor::append_offset(0).into_bytes(),
                    clock,
                ),
            )
            .unwrap();
        }

        let session = entity("session", SESSION);
        let parent = entity("run", "native-parent");
        let layout_parent = entity("run", "layout-parent");
        let child = entity("run", "native-child");
        let spawn = entity("delegation_spawn", "native-parent\0tool-native");
        let parent_message = entity("message", "native-parent-message");

        let child_record = object_record(
            3,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            20,
            b"child-layout",
        );
        let mut child_batch = FactBatch::new(4, 2).unwrap();
        for fact in [
            Fact::Run(RunFact {
                run: child.clone(),
                session: session.clone(),
                native_run_id: "native-child".to_string(),
                parent_run: Some(layout_parent.clone()),
            }),
            Fact::Delegation(DelegationFact {
                child_run: child.clone(),
                parent_run: Some(layout_parent.clone()),
                session: session.clone(),
                kind: DelegationKind::VendorNativeSubagent,
                relation_strength: RelationStrength::Layout,
                native_child_id: Some("native-child".to_string()),
                native_task_id: None,
                label: Some("layout label".to_string()),
                prompt: None,
                cwd: Some("/repo".to_string()),
                worktree_path: None,
                source_time: None,
            }),
        ] {
            child_batch.push(&child_record, fact).unwrap();
        }
        commit_object_batch(
            &mut connection,
            child_object,
            &child_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            21,
            &child_batch,
        );

        let first_metadata_cursor =
            SourceCursor::snapshot(crate::source::Revision::digest(b"native-meta-one"));
        let metadata_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            first_metadata_cursor.clone(),
            22,
            b"native-meta-one",
        );
        let metadata_fact = || {
            Fact::DelegationMetadata(DelegationMetadataFact {
                child_run: child.clone(),
                session: session.clone(),
                native_child_id: "native-child".to_string(),
                agent_type: "Explore".to_string(),
                description: Some("metadata label".to_string()),
                name: Some("native-child".to_string()),
                spawn_depth: Some(1),
                worktree_path: Some("/repo/worktrees/native-child".to_string()),
                native_task_id: Some("tool-native".to_string()),
            })
        };
        let mut metadata_batch = FactBatch::new(2, 2).unwrap();
        metadata_batch
            .push(&metadata_record, metadata_fact())
            .unwrap();
        commit_object_batch(
            &mut connection,
            metadata_object,
            &metadata_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            23,
            &metadata_batch,
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT relation_strength FROM canonical_delegations WHERE child_run_key = ?1",
                    [child.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "layout"
        );

        let spawn_record = direct_record(1, 0, 1, 24, b"native-spawn");
        let mut spawn_batch = FactBatch::new(6, 2).unwrap();
        for fact in [
            Fact::Run(RunFact {
                run: parent.clone(),
                session: session.clone(),
                native_run_id: "native-parent".to_string(),
                parent_run: None,
            }),
            Fact::Run(RunFact {
                run: layout_parent.clone(),
                session: session.clone(),
                native_run_id: "layout-parent".to_string(),
                parent_run: None,
            }),
            Fact::DelegationSpawn(DelegationSpawnFact {
                spawn: spawn.clone(),
                parent_run: parent.clone(),
                parent_message: parent_message.clone(),
                session: session.clone(),
                native_task_id: "tool-native".to_string(),
                tool_name: "Task".to_string(),
                label: Some("native label".to_string()),
                prompt: Some("inspect the parser".to_string()),
                requested_agent_type: Some("Explore".to_string()),
                source_time: None,
            }),
        ] {
            spawn_batch.push(&spawn_record, fact).unwrap();
        }
        commit_direct_batch(&mut connection, &spawn_record, 1, 0, 25, &spawn_batch);

        let explicit: ExplicitDelegationRow = connection
            .query_row(
                r#"
                SELECT parent_run_key, relation_strength, label, worktree_path,
                       decisive_relation_fact_id, decisive_spawn_fact_id,
                       decisive_metadata_fact_id, assertion_count
                FROM canonical_delegations WHERE child_run_key = ?1
                "#,
                [child.as_bytes()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(explicit.0, parent.as_bytes());
        assert_eq!(explicit.1, "native_explicit");
        assert_eq!(explicit.2, "native label");
        assert_eq!(explicit.3, "/repo/worktrees/native-child");
        assert!(explicit.4.is_none());
        assert!(explicit.5.is_some());
        assert!(explicit.6.is_some());
        assert_eq!(explicit.7, 2);
        assert_eq!(count(&connection, "observed_run_states"), 0);
        assert_eq!(
            connection
                .query_row(
                    "SELECT spawn_status FROM canonical_delegation_spawns WHERE spawn_key = ?1",
                    [spawn.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "resolved"
        );

        let deleted_metadata_cursor =
            SourceCursor::snapshot(crate::source::Revision::digest(b"native-meta-deleted"));
        apply_fact_observation_commit(
            &mut connection,
            &request_for_object(
                metadata_object,
                ExpectedSourceCursor::At {
                    generation: 1,
                    committed_cursor: first_metadata_cursor.as_bytes().to_vec(),
                },
                1,
                deleted_metadata_cursor.as_bytes().to_vec(),
                26,
            ),
            &FactBatch::new(2, 2).unwrap(),
        )
        .unwrap();
        let fallback: DelegationProvenanceRow = connection
            .query_row(
                r#"
                    SELECT parent_run_key, relation_strength,
                           decisive_relation_fact_id, decisive_spawn_fact_id,
                           decisive_metadata_fact_id
                    FROM canonical_delegations WHERE child_run_key = ?1
                    "#,
                [child.as_bytes()],
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
        assert_eq!(fallback.0, layout_parent.as_bytes());
        assert_eq!(fallback.1, "layout");
        assert!(fallback.2.is_some());
        assert!(fallback.3.is_none());
        assert!(fallback.4.is_none());

        let second_metadata_cursor =
            SourceCursor::snapshot(crate::source::Revision::digest(b"native-meta-two"));
        let second_metadata_record = object_record(
            2,
            1,
            deleted_metadata_cursor.clone(),
            second_metadata_cursor,
            27,
            b"native-meta-two",
        );
        let mut second_metadata_batch = FactBatch::new(2, 2).unwrap();
        second_metadata_batch
            .push(&second_metadata_record, metadata_fact())
            .unwrap();
        commit_object_batch(
            &mut connection,
            metadata_object,
            &second_metadata_record,
            1,
            deleted_metadata_cursor.as_bytes().to_vec(),
            28,
            &second_metadata_batch,
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT relation_strength FROM canonical_delegations WHERE child_run_key = ?1",
                    [child.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "native_explicit"
        );

        let replay_record = direct_record(2, 0, 1, 29, b"spawn-retracted");
        let mut replay_batch = FactBatch::new(4, 2).unwrap();
        for fact in [
            Fact::Run(RunFact {
                run: parent,
                session: session.clone(),
                native_run_id: "native-parent".to_string(),
                parent_run: None,
            }),
            Fact::Run(RunFact {
                run: layout_parent,
                session,
                native_run_id: "layout-parent".to_string(),
                parent_run: None,
            }),
        ] {
            replay_batch.push(&replay_record, fact).unwrap();
        }
        commit_direct_batch(&mut connection, &replay_record, 1, 1, 30, &replay_batch);
        assert_eq!(count(&connection, "delegation_spawn_assertions"), 0);
        assert_eq!(count(&connection, "canonical_delegation_spawns"), 0);
        assert_eq!(
            connection
                .query_row(
                    "SELECT relation_strength FROM canonical_delegations WHERE child_run_key = ?1",
                    [child.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "layout"
        );
    }

    #[test]
    fn equal_native_task_matches_from_two_parents_are_conflicting() {
        let mut connection = database();
        register_object(&mut connection);
        let metadata_object = b"conflicting-native-meta";
        apply_observation_commit(
            &mut connection,
            &request_for_object(
                metadata_object,
                ExpectedSourceCursor::Absent,
                1,
                SourceCursor::append_offset(0).into_bytes(),
                12,
            ),
        )
        .unwrap();
        let session = entity("session", SESSION);
        let child = entity("run", "conflicting-native-child");
        let parent_a = entity("run", "native-parent-a");
        let parent_b = entity("run", "native-parent-b");

        let metadata_cursor =
            SourceCursor::snapshot(crate::source::Revision::digest(b"conflicting-meta"));
        let metadata_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            metadata_cursor,
            20,
            b"conflicting-meta",
        );
        let mut metadata_batch = FactBatch::new(4, 2).unwrap();
        for fact in [
            Fact::Run(RunFact {
                run: child.clone(),
                session: session.clone(),
                native_run_id: "conflicting-native-child".to_string(),
                parent_run: None,
            }),
            Fact::DelegationMetadata(DelegationMetadataFact {
                child_run: child.clone(),
                session: session.clone(),
                native_child_id: "conflicting-native-child".to_string(),
                agent_type: "Explore".to_string(),
                description: None,
                name: None,
                spawn_depth: Some(1),
                worktree_path: None,
                native_task_id: Some("shared-tool-id".to_string()),
            }),
        ] {
            metadata_batch.push(&metadata_record, fact).unwrap();
        }
        commit_object_batch(
            &mut connection,
            metadata_object,
            &metadata_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            21,
            &metadata_batch,
        );

        let spawn_record = direct_record(1, 0, 1, 22, b"conflicting-spawns");
        let mut spawn_batch = FactBatch::new(8, 2).unwrap();
        for fact in [
            Fact::Run(RunFact {
                run: parent_a.clone(),
                session: session.clone(),
                native_run_id: "native-parent-a".to_string(),
                parent_run: None,
            }),
            Fact::Run(RunFact {
                run: parent_b.clone(),
                session: session.clone(),
                native_run_id: "native-parent-b".to_string(),
                parent_run: None,
            }),
            Fact::DelegationSpawn(DelegationSpawnFact {
                spawn: entity("delegation_spawn", "parent-a\0shared-tool-id"),
                parent_run: parent_a,
                parent_message: entity("message", "parent-a-message"),
                session: session.clone(),
                native_task_id: "shared-tool-id".to_string(),
                tool_name: "Task".to_string(),
                label: None,
                prompt: None,
                requested_agent_type: None,
                source_time: None,
            }),
            Fact::DelegationSpawn(DelegationSpawnFact {
                spawn: entity("delegation_spawn", "parent-b\0shared-tool-id"),
                parent_run: parent_b,
                parent_message: entity("message", "parent-b-message"),
                session,
                native_task_id: "shared-tool-id".to_string(),
                tool_name: "Task".to_string(),
                label: None,
                prompt: None,
                requested_agent_type: None,
                source_time: None,
            }),
        ] {
            spawn_batch.push(&spawn_record, fact).unwrap();
        }
        commit_direct_batch(&mut connection, &spawn_record, 1, 0, 23, &spawn_batch);

        let conflict: (String, String, i64, i64) = connection
            .query_row(
                r#"
                SELECT relation_status, relation_strength, assertion_count,
                       competing_relation_count
                FROM canonical_delegations WHERE child_run_key = ?1
                "#,
                [child.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            conflict,
            (
                "conflicting".to_string(),
                "native_explicit".to_string(),
                2,
                1
            )
        );
        assert_eq!(
            connection
                .query_row(
                    r#"
                    SELECT operation FROM change_log
                    WHERE topic = 'diagnostic.runtime.delegation-conflict'
                      AND entity_key = ?1
                    ORDER BY commit_seq DESC LIMIT 1
                    "#,
                    [child.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "upsert"
        );
    }

    #[test]
    fn conflicting_native_delegation_metadata_is_preserved_and_diagnosed() {
        let mut connection = database();
        register_object(&mut connection);
        let session = entity("session", SESSION);
        let child = entity("run", "metadata-conflict-child");
        let metadata = |agent_type: &str| {
            Fact::DelegationMetadata(DelegationMetadataFact {
                child_run: child.clone(),
                session: session.clone(),
                native_child_id: "metadata-conflict-child".to_string(),
                agent_type: agent_type.to_string(),
                description: None,
                name: None,
                spawn_depth: None,
                worktree_path: None,
                native_task_id: None,
            })
        };

        let initial_cursor = SourceCursor::append_offset(0);
        let first_cursor = SourceCursor::snapshot(crate::source::Revision::digest(b"meta-a"));
        let first_record = object_record(1, 1, initial_cursor.clone(), first_cursor, 20, b"meta-a");
        let mut first_batch = FactBatch::new(2, 2).unwrap();
        first_batch
            .push(&first_record, metadata("Explore"))
            .unwrap();
        commit_object_batch(
            &mut connection,
            b"fixture-transcript",
            &first_record,
            1,
            initial_cursor.as_bytes().to_vec(),
            21,
            &first_batch,
        );

        let second_key = b"duplicate-meta";
        apply_observation_commit(
            &mut connection,
            &request_for_object(
                second_key,
                ExpectedSourceCursor::Absent,
                1,
                SourceCursor::append_offset(0).into_bytes(),
                22,
            ),
        )
        .unwrap();
        let second_cursor = SourceCursor::snapshot(crate::source::Revision::digest(b"meta-b"));
        let second_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            second_cursor.clone(),
            23,
            b"meta-b",
        );
        let mut second_batch = FactBatch::new(2, 2).unwrap();
        second_batch.push(&second_record, metadata("Plan")).unwrap();
        commit_object_batch(
            &mut connection,
            second_key,
            &second_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            24,
            &second_batch,
        );

        let conflict: (String, i64, i64) = connection
            .query_row(
                r#"
                SELECT metadata_status, assertion_count, competing_metadata_count
                FROM canonical_delegation_metadata WHERE child_run_key = ?1
                "#,
                [child.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(conflict, ("conflicting".to_string(), 2, 1));
        assert_eq!(count(&connection, "delegation_metadata_assertions"), 2);
        assert_eq!(
            connection
                .query_row(
                    r#"
                    SELECT operation FROM change_log
                    WHERE topic = 'diagnostic.runtime.delegation-metadata-conflict'
                      AND entity_key = ?1
                    ORDER BY commit_seq DESC LIMIT 1
                    "#,
                    [child.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "upsert"
        );

        let deleted_cursor =
            SourceCursor::snapshot(crate::source::Revision::digest(b"meta-b-deleted"));
        let empty_batch = FactBatch::new(2, 2).unwrap();
        apply_fact_observation_commit(
            &mut connection,
            &request_for_object(
                second_key,
                ExpectedSourceCursor::At {
                    generation: 1,
                    committed_cursor: second_cursor.as_bytes().to_vec(),
                },
                1,
                deleted_cursor.as_bytes().to_vec(),
                25,
            ),
            &empty_batch,
        )
        .unwrap();
        let resolved: (String, i64, i64) = connection
            .query_row(
                r#"
                SELECT metadata_status, assertion_count, competing_metadata_count
                FROM canonical_delegation_metadata WHERE child_run_key = ?1
                "#,
                [child.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(resolved, ("unresolved_run".to_string(), 1, 0));
        assert_eq!(
            connection
                .query_row(
                    r#"
                    SELECT operation FROM change_log
                    WHERE topic = 'diagnostic.runtime.delegation-metadata-conflict'
                      AND entity_key = ?1
                    ORDER BY commit_seq DESC LIMIT 1
                    "#,
                    [child.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "delete"
        );
    }

    #[test]
    fn delegation_parent_first_resolves_in_the_child_commit() {
        let mut connection = database();
        register_object(&mut connection);
        let session = entity("session", SESSION);
        let parent = entity("run", SESSION);
        let child = entity("run", &format!("{SESSION}\0\0child"));

        let parent_record = direct_record(1, 0, 1, 20, b"parent-first");
        let mut parent_batch = FactBatch::new(2, 2).unwrap();
        parent_batch
            .push(
                &parent_record,
                Fact::Run(RunFact {
                    run: parent.clone(),
                    session: session.clone(),
                    native_run_id: SESSION.to_string(),
                    parent_run: None,
                }),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &parent_record, 1, 0, 21, &parent_batch);

        let child_record = direct_record(1, 1, 2, 22, b"child-second");
        let mut child_batch = FactBatch::new(3, 2).unwrap();
        child_batch
            .push(
                &child_record,
                Fact::Run(RunFact {
                    run: child.clone(),
                    session: session.clone(),
                    native_run_id: "child".to_string(),
                    parent_run: Some(parent.clone()),
                }),
            )
            .unwrap();
        child_batch
            .push(
                &child_record,
                Fact::Delegation(DelegationFact {
                    child_run: child.clone(),
                    parent_run: Some(parent),
                    session,
                    kind: DelegationKind::VendorNativeSubagent,
                    relation_strength: RelationStrength::Layout,
                    native_child_id: Some("child".to_string()),
                    native_task_id: None,
                    label: None,
                    prompt: None,
                    cwd: None,
                    worktree_path: None,
                    source_time: None,
                }),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &child_record, 1, 1, 23, &child_batch);
        assert_eq!(
            connection
                .query_row(
                    "SELECT relation_status FROM canonical_delegations WHERE child_run_key = ?1",
                    [child.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "resolved"
        );
    }

    #[test]
    fn equal_strength_delegation_conflicts_are_preserved_and_diagnosed() {
        let mut connection = database();
        register_object(&mut connection);
        let session = entity("session", SESSION);
        let child = entity("run", "conflicted-child");
        let parent_a = entity("run", "parent-a");
        let parent_b = entity("run", "parent-b");
        let record = direct_record(1, 0, 1, 20, b"conflicting-relations");
        let mut batch = FactBatch::new(6, 2).unwrap();
        for (run, native_run_id) in [
            (child.clone(), "child"),
            (parent_a.clone(), "parent-a"),
            (parent_b.clone(), "parent-b"),
        ] {
            batch
                .push(
                    &record,
                    Fact::Run(RunFact {
                        run,
                        session: session.clone(),
                        native_run_id: native_run_id.to_string(),
                        parent_run: None,
                    }),
                )
                .unwrap();
        }
        for parent in [parent_a, parent_b] {
            batch
                .push(
                    &record,
                    Fact::Delegation(DelegationFact {
                        child_run: child.clone(),
                        parent_run: Some(parent),
                        session: session.clone(),
                        kind: DelegationKind::VendorNativeSubagent,
                        relation_strength: RelationStrength::NativeExplicit,
                        native_child_id: Some("child".to_string()),
                        native_task_id: None,
                        label: None,
                        prompt: None,
                        cwd: None,
                        worktree_path: None,
                        source_time: None,
                    }),
                )
                .unwrap();
        }
        commit_direct_batch(&mut connection, &record, 1, 0, 21, &batch);
        let projected: (String, i64, i64) = connection
            .query_row(
                "SELECT relation_status, assertion_count, competing_relation_count FROM canonical_delegations WHERE child_run_key = ?1",
                [child.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(projected, ("conflicting".to_string(), 2, 1));
        assert_eq!(count(&connection, "delegation_assertions"), 2);
        assert_eq!(
            connection
                .query_row(
                    r#"
                    SELECT operation FROM change_log
                    WHERE topic = 'diagnostic.runtime.delegation-conflict'
                      AND entity_key = ?1
                    ORDER BY commit_seq DESC LIMIT 1
                    "#,
                    [child.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "upsert"
        );
    }

    #[test]
    fn presence_replaces_in_place_retracts_on_absence_and_joins_late_history() {
        let mut connection = database();
        register_object(&mut connection);
        let presence_key = entity("presence", "4242/session/proc-start");
        let session_key = entity("session", SESSION);
        let run_key = entity("run", SESSION);

        let first_record = direct_record(1, 0, 1, 20, b"presence-idle");
        let mut first = FactBatch::new(2, 1).unwrap();
        first
            .push(
                &first_record,
                Fact::Presence(presence_fact("idle", "/fixture/project")),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &first_record, 1, 0, 21, &first);

        let initial: (String, String, i64, i64) = connection
            .query_row(
                "SELECT presence_status, native_status, assertion_count, competing_assertion_count FROM canonical_presences WHERE presence_key = ?1",
                [presence_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(initial, ("resolved".to_string(), "idle".to_string(), 1, 0));
        assert_eq!(count(&connection, "canonical_sessions"), 0);
        assert_eq!(count(&connection, "canonical_runs"), 0);
        assert_eq!(count(&connection, "observed_run_states"), 0);

        // Presence is independently queryable before history. A later run
        // joins through stable keys without rewriting the presence assertion.
        let history_object_key = b"late-history";
        apply_observation_commit(
            &mut connection,
            &request_for_object(
                history_object_key,
                ExpectedSourceCursor::Absent,
                1,
                SourceCursor::append_offset(0).into_bytes(),
                25,
            ),
        )
        .unwrap();
        let history_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            26,
            b"late-history-run",
        );
        let mut history = FactBatch::new(2, 1).unwrap();
        history
            .push(
                &history_record,
                Fact::Run(RunFact {
                    run: run_key.clone(),
                    session: session_key,
                    native_run_id: SESSION.to_string(),
                    parent_run: None,
                }),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            history_object_key,
            &history_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            27,
            &history,
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM canonical_presences AS presence JOIN canonical_runs AS run ON run.run_key = presence.run_key WHERE presence.presence_key = ?1",
                    [presence_key.as_bytes()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(count(&connection, "observed_run_states"), 0);

        let updated_record = direct_record(1, 1, 2, 30, b"presence-working");
        let mut updated = FactBatch::new(2, 1).unwrap();
        updated
            .push(
                &updated_record,
                Fact::Presence(presence_fact("working", "/fixture/renamed")),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &updated_record, 1, 1, 31, &updated);
        let replaced: (String, String, i64) = connection
            .query_row(
                "SELECT native_status, cwd, assertion_count FROM canonical_presences WHERE presence_key = ?1",
                [presence_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            replaced,
            ("working".to_string(), "/fixture/renamed".to_string(), 1)
        );
        assert_eq!(count(&connection, "presence_assertions"), 1);
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM fact_records WHERE fact_kind = 'presence'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );

        let absent_record = direct_record(1, 2, 3, 40, b"presence-absent");
        let absent = FactBatch::new(1, 1).unwrap();
        commit_direct_snapshot_batch(&mut connection, &absent_record, 1, 2, 41, &absent);
        assert_eq!(count(&connection, "canonical_presences"), 0);
        assert_eq!(count(&connection, "presence_assertions"), 0);
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM fact_records WHERE fact_kind = 'presence'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert_eq!(count(&connection, "canonical_runs"), 1);
        assert_eq!(count(&connection, "observed_run_states"), 0);
        assert_eq!(
            connection
                .query_row(
                    "SELECT operation FROM change_log WHERE topic = 'runtime.presence.changed' AND entity_key = ?1 ORDER BY commit_seq DESC LIMIT 1",
                    [presence_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "delete"
        );
    }

    #[test]
    fn competing_presence_assertions_are_diagnosed_and_resolve_on_retraction() {
        let mut connection = database();
        register_object(&mut connection);
        let second_object_key = b"presence-secondary";
        apply_observation_commit(
            &mut connection,
            &request_for_object(
                second_object_key,
                ExpectedSourceCursor::Absent,
                1,
                SourceCursor::append_offset(0).into_bytes(),
                12,
            ),
        )
        .unwrap();
        let presence_key = entity("presence", "4242/session/proc-start");

        let primary_record = direct_record(1, 0, 1, 20, b"presence-primary");
        let mut primary = FactBatch::new(2, 1).unwrap();
        primary
            .push(
                &primary_record,
                Fact::Presence(presence_fact("idle", "/primary")),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &primary_record, 1, 0, 21, &primary);

        let secondary_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            30,
            b"presence-secondary",
        );
        let mut secondary = FactBatch::new(2, 1).unwrap();
        secondary
            .push(
                &secondary_record,
                Fact::Presence(presence_fact("working", "/secondary")),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            second_object_key,
            &secondary_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            31,
            &secondary,
        );

        let conflicting: (String, i64, i64) = connection
            .query_row(
                "SELECT presence_status, assertion_count, competing_assertion_count FROM canonical_presences WHERE presence_key = ?1",
                [presence_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(conflicting, ("conflicting".to_string(), 2, 1));
        assert_eq!(count(&connection, "presence_assertions"), 2);
        assert_eq!(
            connection
                .query_row(
                    "SELECT operation FROM change_log WHERE topic = 'diagnostic.runtime.presence-conflict' AND entity_key = ?1 ORDER BY commit_seq DESC LIMIT 1",
                    [presence_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "upsert"
        );

        let retracted_record = object_record(
            2,
            1,
            SourceCursor::append_offset(1),
            SourceCursor::append_offset(2),
            40,
            b"presence-secondary-absent",
        );
        let retracted = FactBatch::new(1, 1).unwrap();
        commit_object_batch(
            &mut connection,
            second_object_key,
            &retracted_record,
            1,
            SourceCursor::append_offset(1).into_bytes(),
            41,
            &retracted,
        );
        let resolved: (String, i64, i64, String, String) = connection
            .query_row(
                "SELECT presence_status, assertion_count, competing_assertion_count, native_status, cwd FROM canonical_presences WHERE presence_key = ?1",
                [presence_key.as_bytes()],
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
            resolved,
            (
                "resolved".to_string(),
                1,
                0,
                "idle".to_string(),
                "/primary".to_string(),
            )
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT operation FROM change_log WHERE topic = 'diagnostic.runtime.presence-conflict' AND entity_key = ?1 ORDER BY commit_seq DESC LIMIT 1",
                    [presence_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "delete"
        );
    }

    #[test]
    fn complete_task_snapshot_replaces_items_retracts_and_joins_late_session_history() {
        let mut connection = database();
        register_object(&mut connection);
        let collection_key = entity("task_collection", "todo-list");
        let first_task_key = entity("task", "todo-one");
        let second_task_key = entity("task", "todo-two");

        let first_record = direct_record(1, 0, 1, 20, b"todo-v1");
        let mut first = FactBatch::new(2, 1).unwrap();
        first
            .push(
                &first_record,
                Fact::TaskSnapshot(todo_fact(vec![
                    task_item("todo-one", "first", TaskStatus::Pending),
                    task_item("todo-two", "second", TaskStatus::InProgress),
                ])),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &first_record, 1, 0, 21, &first);

        let initial: (String, i64, i64, i64, i64) = connection
            .query_row(
                r#"
                SELECT resolution_status, assertion_count,
                       complete_snapshot_count, item_document_count, item_count
                FROM canonical_task_collections WHERE collection_key = ?1
                "#,
                [collection_key.as_bytes()],
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
        assert_eq!(initial, ("resolved".to_string(), 1, 1, 0, 2));
        assert_eq!(count(&connection, "canonical_tasks"), 2);
        assert_eq!(count(&connection, "canonical_sessions"), 0);
        assert_eq!(count(&connection, "observed_run_states"), 0);

        // A complete task snapshot is independently queryable. Stable scope
        // keys let later history join without rewriting the task assertion.
        let history_object_key = b"late-task-history";
        apply_observation_commit(
            &mut connection,
            &request_for_object(
                history_object_key,
                ExpectedSourceCursor::Absent,
                1,
                SourceCursor::append_offset(0).into_bytes(),
                25,
            ),
        )
        .unwrap();
        let history_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            26,
            b"late-task-session",
        );
        let mut history = FactBatch::new(2, 1).unwrap();
        history
            .push(
                &history_record,
                Fact::Session(SessionFact {
                    session: entity("session", SESSION),
                    project: entity("project", PROJECT),
                    native_session_id: SESSION.to_string(),
                    native_project_key: PROJECT.to_string(),
                    cwd: Some("/fixture/project".to_string()),
                    git_branch: None,
                    first_prompt: None,
                    ai_title: None,
                    custom_title: None,
                    source_time: None,
                }),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            history_object_key,
            &history_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            27,
            &history,
        );
        assert_eq!(
            connection
                .query_row(
                    r#"
                    SELECT COUNT(*)
                    FROM canonical_task_collections AS collection
                    JOIN canonical_sessions AS session
                      ON session.session_key = collection.session_key
                    WHERE collection.collection_key = ?1
                    "#,
                    [collection_key.as_bytes()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );

        let updated_record = direct_record(1, 1, 2, 30, b"todo-v2");
        let mut updated = FactBatch::new(2, 1).unwrap();
        updated
            .push(
                &updated_record,
                Fact::TaskSnapshot(todo_fact(vec![task_item(
                    "todo-one",
                    "first",
                    TaskStatus::Completed,
                )])),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &updated_record, 1, 1, 31, &updated);
        assert_eq!(
            connection
                .query_row(
                    "SELECT task_status FROM canonical_tasks WHERE task_key = ?1",
                    [first_task_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "completed"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM canonical_tasks WHERE task_key = ?1",
                    [second_task_key.as_bytes()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert_eq!(count(&connection, "task_snapshot_assertions"), 1);
        assert_eq!(count(&connection, "task_item_assertions"), 1);
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM fact_records WHERE fact_kind = 'task_snapshot'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(count(&connection, "observed_run_states"), 0);

        let deleted_record = direct_record(1, 2, 3, 40, b"todo-deleted");
        let deleted = FactBatch::new(1, 1).unwrap();
        commit_direct_snapshot_batch(&mut connection, &deleted_record, 1, 2, 41, &deleted);
        assert_eq!(count(&connection, "canonical_task_collections"), 0);
        assert_eq!(count(&connection, "canonical_tasks"), 0);
        assert_eq!(count(&connection, "task_snapshot_assertions"), 0);
        assert_eq!(count(&connection, "task_item_assertions"), 0);
        assert_eq!(count(&connection, "canonical_sessions"), 1);
        assert_eq!(count(&connection, "observed_run_states"), 0);
        assert_eq!(
            connection
                .query_row(
                    r#"
                    SELECT operation FROM change_log
                    WHERE topic = 'runtime.task-collection.changed'
                      AND entity_key = ?1
                    ORDER BY commit_seq DESC LIMIT 1
                    "#,
                    [collection_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "delete"
        );
    }

    #[test]
    fn task_item_documents_merge_then_conflict_and_resolve_per_object() {
        let mut connection = database();
        register_object(&mut connection);
        let second_object_key = b"task-item-secondary";
        apply_observation_commit(
            &mut connection,
            &request_for_object(
                second_object_key,
                ExpectedSourceCursor::Absent,
                1,
                SourceCursor::append_offset(0).into_bytes(),
                12,
            ),
        )
        .unwrap();
        let collection_key = entity("task_collection", "native-list");
        let shared_task_key = entity("task", "shared");
        let secondary_task_key = entity("task", "secondary");

        let primary_record = direct_record(1, 0, 1, 20, b"task-primary");
        let mut primary = FactBatch::new(2, 1).unwrap();
        primary
            .push(
                &primary_record,
                Fact::TaskSnapshot(native_task_fact(vec![task_item(
                    "shared",
                    "shared task",
                    TaskStatus::Pending,
                )])),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &primary_record, 1, 0, 21, &primary);

        let secondary_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            30,
            b"task-secondary",
        );
        let mut secondary = FactBatch::new(2, 1).unwrap();
        secondary
            .push(
                &secondary_record,
                Fact::TaskSnapshot(native_task_fact(vec![task_item(
                    "secondary",
                    "second task",
                    TaskStatus::InProgress,
                )])),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            second_object_key,
            &secondary_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            31,
            &secondary,
        );
        let merged: (String, i64, i64, i64) = connection
            .query_row(
                r#"
                SELECT resolution_status, assertion_count,
                       item_document_count, item_count
                FROM canonical_task_collections WHERE collection_key = ?1
                "#,
                [collection_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(merged, ("resolved".to_string(), 2, 2, 2));
        assert_eq!(count(&connection, "canonical_tasks"), 2);

        // Replacing only the second item document retracts its old task and
        // can create a competing assertion for the first without touching
        // the primary source object's evidence.
        let competing_record = object_record(
            2,
            1,
            SourceCursor::append_offset(1),
            SourceCursor::append_offset(2),
            40,
            b"task-competing",
        );
        let mut competing = FactBatch::new(2, 1).unwrap();
        competing
            .push(
                &competing_record,
                Fact::TaskSnapshot(native_task_fact(vec![task_item(
                    "shared",
                    "competing subject",
                    TaskStatus::Completed,
                )])),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            second_object_key,
            &competing_record,
            1,
            SourceCursor::append_offset(1).into_bytes(),
            41,
            &competing,
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM canonical_tasks WHERE task_key = ?1",
                    [secondary_task_key.as_bytes()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        let conflict: (String, i64, i64) = connection
            .query_row(
                "SELECT resolution_status, assertion_count, competing_item_count FROM canonical_tasks WHERE task_key = ?1",
                [shared_task_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(conflict, ("conflicting".to_string(), 2, 1));
        assert_eq!(count(&connection, "task_item_assertions"), 2);
        assert_eq!(
            connection
                .query_row(
                    r#"
                    SELECT operation FROM change_log
                    WHERE topic = 'diagnostic.runtime.task-conflict'
                      AND entity_key = ?1
                    ORDER BY commit_seq DESC LIMIT 1
                    "#,
                    [shared_task_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "upsert"
        );

        let retracted_record = object_record(
            2,
            1,
            SourceCursor::append_offset(2),
            SourceCursor::append_offset(3),
            50,
            b"task-retracted",
        );
        let retracted = FactBatch::new(1, 1).unwrap();
        commit_object_batch(
            &mut connection,
            second_object_key,
            &retracted_record,
            1,
            SourceCursor::append_offset(2).into_bytes(),
            51,
            &retracted,
        );
        let resolved: (String, String, i64, i64) = connection
            .query_row(
                "SELECT resolution_status, task_status, assertion_count, competing_item_count FROM canonical_tasks WHERE task_key = ?1",
                [shared_task_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            resolved,
            ("resolved".to_string(), "pending".to_string(), 1, 0)
        );
        assert_eq!(count(&connection, "task_snapshot_assertions"), 1);
        assert_eq!(count(&connection, "task_item_assertions"), 1);
        assert_eq!(count(&connection, "observed_run_states"), 0);
        assert_eq!(
            connection
                .query_row(
                    r#"
                    SELECT operation FROM change_log
                    WHERE topic = 'diagnostic.runtime.task-conflict'
                      AND entity_key = ?1
                    ORDER BY commit_seq DESC LIMIT 1
                    "#,
                    [shared_task_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "delete"
        );
    }

    #[test]
    fn competing_complete_task_snapshots_are_diagnosed_and_resolve() {
        let mut connection = database();
        register_object(&mut connection);
        let second_object_key = b"todo-secondary";
        apply_observation_commit(
            &mut connection,
            &request_for_object(
                second_object_key,
                ExpectedSourceCursor::Absent,
                1,
                SourceCursor::append_offset(0).into_bytes(),
                12,
            ),
        )
        .unwrap();
        let collection_key = entity("task_collection", "todo-list");

        let primary_record = direct_record(1, 0, 1, 20, b"todo-primary");
        let mut primary = FactBatch::new(2, 1).unwrap();
        primary
            .push(
                &primary_record,
                Fact::TaskSnapshot(todo_fact(vec![task_item(
                    "todo-one",
                    "primary item",
                    TaskStatus::Pending,
                )])),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &primary_record, 1, 0, 21, &primary);

        let secondary_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            30,
            b"todo-secondary",
        );
        let mut secondary = FactBatch::new(2, 1).unwrap();
        secondary
            .push(
                &secondary_record,
                Fact::TaskSnapshot(todo_fact(vec![task_item(
                    "todo-two",
                    "competing item",
                    TaskStatus::InProgress,
                )])),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            second_object_key,
            &secondary_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            31,
            &secondary,
        );
        let conflict: (String, i64, i64, i64) = connection
            .query_row(
                r#"
                SELECT resolution_status, assertion_count,
                       competing_metadata_count, item_count
                FROM canonical_task_collections WHERE collection_key = ?1
                "#,
                [collection_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(conflict, ("conflicting".to_string(), 2, 1, 2));
        assert_eq!(
            connection
                .query_row(
                    r#"
                    SELECT operation FROM change_log
                    WHERE topic = 'diagnostic.runtime.task-collection-conflict'
                      AND entity_key = ?1
                    ORDER BY commit_seq DESC LIMIT 1
                    "#,
                    [collection_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "upsert"
        );

        let retracted_record = object_record(
            2,
            1,
            SourceCursor::append_offset(1),
            SourceCursor::append_offset(2),
            40,
            b"todo-secondary-retracted",
        );
        let retracted = FactBatch::new(1, 1).unwrap();
        commit_object_batch(
            &mut connection,
            second_object_key,
            &retracted_record,
            1,
            SourceCursor::append_offset(1).into_bytes(),
            41,
            &retracted,
        );
        let resolved: (String, i64, i64, i64) = connection
            .query_row(
                r#"
                SELECT resolution_status, assertion_count,
                       competing_metadata_count, item_count
                FROM canonical_task_collections WHERE collection_key = ?1
                "#,
                [collection_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(resolved, ("resolved".to_string(), 1, 0, 1));
        assert_eq!(
            connection
                .query_row(
                    r#"
                    SELECT operation FROM change_log
                    WHERE topic = 'diagnostic.runtime.task-collection-conflict'
                      AND entity_key = ?1
                    ORDER BY commit_seq DESC LIMIT 1
                    "#,
                    [collection_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "delete"
        );
    }

    #[test]
    fn plan_documents_replace_conflict_resolve_and_retract() {
        let mut connection = database();
        register_object(&mut connection);
        let second_object_key = b"plan-secondary";
        apply_observation_commit(
            &mut connection,
            &request_for_object(
                second_object_key,
                ExpectedSourceCursor::Absent,
                1,
                SourceCursor::append_offset(0).into_bytes(),
                12,
            ),
        )
        .unwrap();
        let plan_key = entity("plan", "ship-it");

        let first_record = direct_record(1, 0, 1, 20, b"plan-v1");
        let mut first = FactBatch::new(2, 1).unwrap();
        first
            .push(
                &first_record,
                Fact::PlanSnapshot(plan_fact("Ship It", "# Ship It\n\nVersion one.\n")),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &first_record, 1, 0, 21, &first);

        let updated_record = direct_record(1, 1, 2, 30, b"plan-v2");
        let mut updated = FactBatch::new(2, 1).unwrap();
        updated
            .push(
                &updated_record,
                Fact::PlanSnapshot(plan_fact("Ship It Safely", "# Ship It Safely\n\nV2.\n")),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &updated_record, 1, 1, 31, &updated);
        assert_eq!(
            connection
                .query_row(
                    "SELECT title FROM canonical_plans WHERE plan_key = ?1",
                    [plan_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "Ship It Safely"
        );
        assert_eq!(count(&connection, "plan_assertions"), 1);
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM fact_records WHERE fact_kind = 'plan_snapshot'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );

        let competing_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            40,
            b"plan-competing",
        );
        let mut competing = FactBatch::new(2, 1).unwrap();
        competing
            .push(
                &competing_record,
                Fact::PlanSnapshot(plan_fact("Competing", "# Competing\n")),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            second_object_key,
            &competing_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            41,
            &competing,
        );
        let conflict: (String, i64, i64) = connection
            .query_row(
                "SELECT resolution_status, assertion_count, competing_plan_count FROM canonical_plans WHERE plan_key = ?1",
                [plan_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(conflict, ("conflicting".to_string(), 2, 1));

        let retracted_record = object_record(
            2,
            1,
            SourceCursor::append_offset(1),
            SourceCursor::append_offset(2),
            50,
            b"plan-retracted",
        );
        let retracted = FactBatch::new(1, 1).unwrap();
        commit_object_batch(
            &mut connection,
            second_object_key,
            &retracted_record,
            1,
            SourceCursor::append_offset(1).into_bytes(),
            51,
            &retracted,
        );
        let resolved: (String, String, i64, i64) = connection
            .query_row(
                "SELECT resolution_status, title, assertion_count, competing_plan_count FROM canonical_plans WHERE plan_key = ?1",
                [plan_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            resolved,
            ("resolved".to_string(), "Ship It Safely".to_string(), 1, 0)
        );
        assert_eq!(
            connection
                .query_row(
                    r#"
                    SELECT operation FROM change_log
                    WHERE topic = 'diagnostic.runtime.plan-conflict'
                      AND entity_key = ?1
                    ORDER BY commit_seq DESC LIMIT 1
                    "#,
                    [plan_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "delete"
        );

        let deleted_record = direct_record(1, 2, 3, 60, b"plan-deleted");
        let deleted = FactBatch::new(1, 1).unwrap();
        commit_direct_snapshot_batch(&mut connection, &deleted_record, 1, 2, 61, &deleted);
        assert_eq!(count(&connection, "canonical_plans"), 0);
        assert_eq!(count(&connection, "plan_assertions"), 0);
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM fact_records WHERE fact_kind = 'plan_snapshot'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn team_snapshot_same_generation_replacement_retracts_members_without_run_inference() {
        let mut connection = database();
        register_object(&mut connection);
        let team_key = entity("team", "alpha");
        let lead_key = entity("team_member", "team-lead");
        let worker_key = entity("team_member", "worker");

        let first_record = direct_record(1, 0, 1, 20, b"team-v1");
        let mut first = FactBatch::new(4, 2).unwrap();
        first
            .push(
                &first_record,
                Fact::TeamSnapshot(team_fact(
                    "alpha",
                    &[("lead@alpha", "team-lead"), ("worker@alpha", "worker")],
                )),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &first_record, 1, 0, 21, &first);

        let initial: (String, i64, i64) = connection
            .query_row(
                "SELECT config_status, assertion_count, member_count FROM canonical_teams WHERE team_key = ?1",
                [team_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(initial, ("resolved".to_string(), 1, 2));
        assert_eq!(count(&connection, "canonical_team_members"), 2);
        assert_eq!(count(&connection, "observed_run_states"), 0);

        let second_record = direct_record(1, 1, 2, 30, b"team-v2");
        let mut second = FactBatch::new(4, 2).unwrap();
        second
            .push(
                &second_record,
                Fact::TeamSnapshot(team_fact("alpha renamed", &[("lead@alpha", "team-lead")])),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &second_record, 1, 1, 31, &second);

        let replaced: (String, i64) = connection
            .query_row(
                "SELECT name, member_count FROM canonical_teams WHERE team_key = ?1",
                [team_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(replaced, ("alpha renamed".to_string(), 1));
        assert_eq!(count(&connection, "team_snapshot_assertions"), 1);
        assert_eq!(count(&connection, "team_member_assertions"), 1);
        assert_eq!(count(&connection, "canonical_team_members"), 1);
        assert!(connection
            .query_row(
                "SELECT 1 FROM canonical_team_members WHERE member_key = ?1",
                [lead_key.as_bytes()],
                |_| Ok(()),
            )
            .optional()
            .unwrap()
            .is_some());
        assert!(connection
            .query_row(
                "SELECT 1 FROM canonical_team_members WHERE member_key = ?1",
                [worker_key.as_bytes()],
                |_| Ok(()),
            )
            .optional()
            .unwrap()
            .is_none());
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM fact_records WHERE fact_kind = 'team_snapshot'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );

        let deleted_record = direct_record(1, 2, 3, 40, b"team-deleted");
        let deleted = FactBatch::new(1, 1).unwrap();
        commit_direct_snapshot_batch(&mut connection, &deleted_record, 1, 2, 41, &deleted);
        assert_eq!(count(&connection, "canonical_teams"), 0);
        assert_eq!(count(&connection, "canonical_team_members"), 0);
        assert_eq!(count(&connection, "team_snapshot_assertions"), 0);
        assert_eq!(
            connection
                .query_row(
                    "SELECT operation FROM change_log WHERE topic = 'runtime.team.changed' AND entity_key = ?1 ORDER BY commit_seq DESC LIMIT 1",
                    [team_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "delete"
        );
    }

    #[test]
    fn orphan_inbox_mirrors_read_edits_empty_arrays_and_file_deletion() {
        let mut connection = database();
        register_object(&mut connection);
        let inbox_key = entity("team_inbox", "alpha/team-lead");
        let first_message_key = entity("team_inbox_message", "msg-1");

        let first_record = direct_record(1, 0, 1, 20, b"inbox-v1");
        let mut first = FactBatch::new(4, 2).unwrap();
        first
            .push(
                &first_record,
                Fact::TeamInboxSnapshot(inbox_fact(vec![
                    inbox_message("msg-1", "first", false),
                    inbox_message("msg-2", "second", false),
                ])),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &first_record, 1, 0, 21, &first);
        assert_eq!(count(&connection, "canonical_teams"), 0);
        assert_eq!(count(&connection, "canonical_team_inboxes"), 1);
        assert_eq!(count(&connection, "canonical_team_inbox_messages"), 2);

        let second_record = direct_record(1, 1, 2, 30, b"inbox-v2");
        let mut second = FactBatch::new(4, 2).unwrap();
        second
            .push(
                &second_record,
                Fact::TeamInboxSnapshot(inbox_fact(vec![inbox_message("msg-1", "first", true)])),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &second_record, 1, 1, 31, &second);
        let projected: (i64, i64, String) = connection
            .query_row(
                "SELECT read, message_ordinal, message_status FROM canonical_team_inbox_messages WHERE message_key = ?1",
                [first_message_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(projected, (1, 0, "resolved".to_string()));
        assert_eq!(count(&connection, "canonical_team_inbox_messages"), 1);
        assert_eq!(count(&connection, "team_inbox_snapshot_assertions"), 1);

        let empty_record = direct_record(1, 2, 3, 40, b"inbox-empty-array");
        let mut empty = FactBatch::new(2, 1).unwrap();
        empty
            .push(
                &empty_record,
                Fact::TeamInboxSnapshot(inbox_fact(Vec::new())),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &empty_record, 1, 2, 41, &empty);
        let message_count: i64 = connection
            .query_row(
                "SELECT message_count FROM canonical_team_inboxes WHERE inbox_key = ?1",
                [inbox_key.as_bytes()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(message_count, 0);
        assert_eq!(count(&connection, "canonical_team_inbox_messages"), 0);

        let deleted_record = direct_record(1, 3, 4, 50, b"inbox-deleted");
        let deleted = FactBatch::new(1, 1).unwrap();
        commit_direct_snapshot_batch(&mut connection, &deleted_record, 1, 3, 51, &deleted);
        assert_eq!(count(&connection, "canonical_team_inboxes"), 0);
        assert_eq!(count(&connection, "team_inbox_snapshot_assertions"), 0);
    }

    #[test]
    fn competing_team_snapshots_are_retained_and_resolve_after_retraction() {
        let mut connection = database();
        register_object(&mut connection);
        let second_object_key = b"team-config-secondary";
        apply_observation_commit(
            &mut connection,
            &request_for_object(
                second_object_key,
                ExpectedSourceCursor::Absent,
                1,
                SourceCursor::append_offset(0).into_bytes(),
                12,
            ),
        )
        .unwrap();
        let team_key = entity("team", "alpha");

        let primary_record = direct_record(1, 0, 1, 20, b"team-primary");
        let mut primary = FactBatch::new(2, 1).unwrap();
        primary
            .push(
                &primary_record,
                Fact::TeamSnapshot(team_fact("primary", &[("lead@alpha", "team-lead")])),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &primary_record, 1, 0, 21, &primary);

        let secondary_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            30,
            b"team-secondary",
        );
        let mut secondary = FactBatch::new(2, 1).unwrap();
        secondary
            .push(
                &secondary_record,
                Fact::TeamSnapshot(team_fact("secondary", &[("lead@alpha", "team-lead")])),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            second_object_key,
            &secondary_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            31,
            &secondary,
        );

        let conflicting: (String, i64, i64) = connection
            .query_row(
                "SELECT config_status, assertion_count, competing_snapshot_count FROM canonical_teams WHERE team_key = ?1",
                [team_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(conflicting, ("conflicting".to_string(), 2, 1));
        assert_eq!(count(&connection, "team_snapshot_assertions"), 2);
        assert_eq!(
            connection
                .query_row(
                    "SELECT operation FROM change_log WHERE topic = 'diagnostic.runtime.team-config-conflict' AND entity_key = ?1 ORDER BY commit_seq DESC LIMIT 1",
                    [team_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "upsert"
        );

        let retracted_record = object_record(
            2,
            1,
            SourceCursor::append_offset(1),
            SourceCursor::append_offset(2),
            40,
            b"team-secondary-deleted",
        );
        let retracted = FactBatch::new(1, 1).unwrap();
        commit_object_batch(
            &mut connection,
            second_object_key,
            &retracted_record,
            1,
            SourceCursor::append_offset(1).into_bytes(),
            41,
            &retracted,
        );
        let resolved: (String, i64, i64, String) = connection
            .query_row(
                "SELECT config_status, assertion_count, competing_snapshot_count, name FROM canonical_teams WHERE team_key = ?1",
                [team_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            resolved,
            ("resolved".to_string(), 1, 0, "primary".to_string())
        );
    }

    #[test]
    fn artifact_metadata_and_replaceable_content_join_retract_and_clean_audit_facts() {
        let mut connection = database();
        register_object(&mut connection);
        let content_object = b"artifact-content";
        register_object_key(&mut connection, content_object, 12);
        let artifact_key = entity("artifact", "named-backup");

        let metadata_record = direct_record(1, 0, 1, 20, b"artifact-metadata");
        let mut metadata_batch = FactBatch::new(2, 1).unwrap();
        metadata_batch
            .push(
                &metadata_record,
                Fact::ArtifactMetadataSnapshot(artifact_metadata_fact(
                    "delta-1",
                    vec![artifact_metadata_entry(
                        artifact_key.clone(),
                        Some("71f902cd51ee4c6e@v1"),
                        "src/lib.rs",
                        "2026-08-11T00:00:01.000Z",
                        ArtifactCapture::ContentExpected,
                    )],
                )),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &metadata_record, 1, 0, 21, &metadata_batch);
        let missing: (String, String, String, i64, i64) = connection
            .query_row(
                "SELECT resolution_status, content_status, capture_status, metadata_assertion_count, content_assertion_count FROM canonical_artifacts WHERE artifact_key = ?1",
                [artifact_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            missing,
            (
                "incomplete".to_string(),
                "missing_content".to_string(),
                "content_expected".to_string(),
                1,
                0,
            )
        );

        // Repeated checkpoints commonly restate the same backup. They retain
        // both provenance assertions without manufacturing a conflict.
        let repeated_metadata_record = direct_record(1, 1, 2, 25, b"artifact-metadata-repeat");
        let mut repeated_metadata_batch = FactBatch::new(2, 1).unwrap();
        repeated_metadata_batch
            .push(
                &repeated_metadata_record,
                Fact::ArtifactMetadataSnapshot(artifact_metadata_fact(
                    "checkpoint-repeat",
                    vec![artifact_metadata_entry(
                        artifact_key.clone(),
                        Some("71f902cd51ee4c6e@v1"),
                        "src/lib.rs",
                        "2026-08-11T00:00:01.000Z",
                        ArtifactCapture::ContentExpected,
                    )],
                )),
            )
            .unwrap();
        commit_direct_batch(
            &mut connection,
            &repeated_metadata_record,
            1,
            1,
            26,
            &repeated_metadata_batch,
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT metadata_assertion_count * 10 + competing_metadata_count FROM canonical_artifacts WHERE artifact_key = ?1",
                    [artifact_key.as_bytes()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            20
        );

        // Artifact metadata is transcript history. A later ordinary record
        // from the same append object must not be mistaken for an empty
        // replace-document snapshot and retract that accumulated history.
        let ordinary_record = direct_record(1, 2, 3, 27, b"ordinary-message");
        let mut ordinary_batch = FactBatch::new(2, 1).unwrap();
        ordinary_batch
            .push(
                &ordinary_record,
                Fact::Message(tool_message(
                    "ordinary-message",
                    MessageRole::Assistant,
                    vec![ContentBlock::Text {
                        text: "after the file-history record".to_string(),
                    }],
                )),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &ordinary_record, 1, 2, 28, &ordinary_batch);
        assert_eq!(count(&connection, "artifact_snapshot_assertions"), 2);
        assert_eq!(
            connection
                .query_row(
                    "SELECT metadata_assertion_count FROM canonical_artifacts WHERE artifact_key = ?1",
                    [artifact_key.as_bytes()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            2
        );

        let content_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            30,
            b"content-v1",
        );
        let mut content_batch = FactBatch::new(2, 1).unwrap();
        content_batch
            .push(
                &content_record,
                Fact::ArtifactContent(artifact_content_fact(artifact_key.clone(), "before edit\n")),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            content_object,
            &content_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            31,
            &content_batch,
        );
        let captured: (String, String, Vec<u8>, i64, i64) = connection
            .query_row(
                r#"
                SELECT ca.resolution_status, ca.content_status, content.content,
                       ca.metadata_assertion_count, ca.content_assertion_count
                FROM canonical_artifacts AS ca
                LEFT JOIN artifact_content_assertions AS content
                  ON content.fact_id = ca.decisive_content_fact_id
                WHERE ca.artifact_key = ?1
                "#,
                [artifact_key.as_bytes()],
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
            captured,
            (
                "resolved".to_string(),
                "captured".to_string(),
                b"before edit\n".to_vec(),
                2,
                1,
            )
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT content IS NULL FROM canonical_artifacts WHERE artifact_key = ?1",
                    [artifact_key.as_bytes()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1,
            "canonical artifacts reference rather than duplicate decisive content",
        );

        let replacement_record = object_record(
            2,
            1,
            SourceCursor::append_offset(1),
            SourceCursor::append_offset(2),
            40,
            b"content-v2",
        );
        let mut replacement_batch = FactBatch::new(2, 1).unwrap();
        replacement_batch
            .push(
                &replacement_record,
                Fact::ArtifactContent(artifact_content_fact(
                    artifact_key.clone(),
                    "earlier content\n",
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            content_object,
            &replacement_record,
            1,
            SourceCursor::append_offset(1).into_bytes(),
            41,
            &replacement_batch,
        );
        assert_eq!(
            connection
                .query_row(
                    r#"
                    SELECT content.content
                    FROM canonical_artifacts AS ca
                    JOIN artifact_content_assertions AS content
                      ON content.fact_id = ca.decisive_content_fact_id
                    WHERE ca.artifact_key = ?1
                    "#,
                    [artifact_key.as_bytes()],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .unwrap(),
            b"earlier content\n"
        );
        assert_eq!(count(&connection, "artifact_content_assertions"), 1);
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM fact_records WHERE fact_kind = 'artifact_content'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );

        let deleted_content_record = object_record(
            2,
            1,
            SourceCursor::append_offset(2),
            SourceCursor::append_offset(3),
            50,
            b"content-deleted",
        );
        commit_object_batch(
            &mut connection,
            content_object,
            &deleted_content_record,
            1,
            SourceCursor::append_offset(2).into_bytes(),
            51,
            &FactBatch::new(1, 1).unwrap(),
        );
        let after_content_delete: (String, String, i64) = connection
            .query_row(
                "SELECT resolution_status, content_status, content_assertion_count FROM canonical_artifacts WHERE artifact_key = ?1",
                [artifact_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            after_content_delete,
            ("incomplete".to_string(), "missing_content".to_string(), 0,)
        );

        let rewritten_metadata_record = direct_record(2, 0, 1, 60, b"metadata-rewritten");
        commit_direct_batch(
            &mut connection,
            &rewritten_metadata_record,
            1,
            3,
            61,
            &FactBatch::new(1, 1).unwrap(),
        );
        assert_eq!(count(&connection, "canonical_artifacts"), 0);
        assert_eq!(count(&connection, "artifact_snapshot_assertions"), 0);
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM fact_records WHERE fact_kind IN ('artifact_metadata_snapshot', 'artifact_content')",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn bootstrap_defers_artifact_reduction_and_rebuilds_final_state_idempotently() {
        let mut connection = database();
        let artifact_key = entity("artifact", "bootstrap-artifact");
        let record = direct_record(1, 0, 1, 20, b"bootstrap-artifact");
        let mut batch = FactBatch::new(2, 1).unwrap();
        batch
            .push(
                &record,
                Fact::ArtifactMetadataSnapshot(artifact_metadata_fact(
                    "bootstrap-metadata",
                    vec![artifact_metadata_entry(
                        artifact_key.clone(),
                        Some("71f902cd51ee4c6e@v1"),
                        "src/bootstrap.rs",
                        "2026-08-11T00:00:01.000Z",
                        ArtifactCapture::ContentExpected,
                    )],
                )),
            )
            .unwrap();
        batch
            .push(
                &record,
                Fact::ArtifactContent(artifact_content_fact(
                    artifact_key.clone(),
                    "bootstrap content\n",
                )),
            )
            .unwrap();

        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let request = request(
            ExpectedSourceCursor::Absent,
            1,
            record.cursor_end.as_bytes().to_vec(),
            21,
        );
        let receipt = apply_fact_observation_commit_in_transaction(
            &transaction,
            &request,
            &batch,
            &TestCommitHook,
            false,
            true,
        )
        .unwrap();
        transaction.commit().unwrap();
        crate::engine::commit::complete_observation_commit(&TestCommitHook).unwrap();

        assert_eq!(count(&connection, "artifact_snapshot_assertions"), 1);
        assert_eq!(count(&connection, "artifact_metadata_assertions"), 1);
        assert_eq!(count(&connection, "artifact_content_assertions"), 1);
        assert_eq!(count(&connection, "canonical_artifacts"), 0);

        assert_eq!(
            crate::engine::artifact_projection::rebuild_artifacts_for_bootstrap(&mut connection)
                .unwrap(),
            1
        );
        let canonical = connection
            .query_row(
                r#"
                SELECT resolution_status, content_status,
                       metadata_assertion_count, content_assertion_count,
                       last_commit_seq, decisive_content_fact_id
                FROM canonical_artifacts WHERE artifact_key = ?1
                "#,
                [artifact_key.as_bytes()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Vec<u8>>(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(canonical.0, "resolved");
        assert_eq!(canonical.1, "captured");
        assert_eq!(canonical.2, 1);
        assert_eq!(canonical.3, 1);
        assert_eq!(canonical.4, i64::try_from(receipt.commit_seq).unwrap());

        assert_eq!(
            crate::engine::artifact_projection::rebuild_artifacts_for_bootstrap(&mut connection)
                .unwrap(),
            1
        );
        let rebuilt = connection
            .query_row(
                r#"
                SELECT resolution_status, content_status,
                       metadata_assertion_count, content_assertion_count,
                       last_commit_seq, decisive_content_fact_id
                FROM canonical_artifacts WHERE artifact_key = ?1
                "#,
                [artifact_key.as_bytes()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Vec<u8>>(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(rebuilt, canonical);
        assert_eq!(
            connection
                .query_row("PRAGMA foreign_key_check", [], |_| Ok(1_i64))
                .optional()
                .unwrap(),
            None
        );
    }

    #[test]
    fn artifact_content_first_and_explicit_no_backup_states_converge_without_run_state() {
        let mut connection = database();
        register_object(&mut connection);
        let metadata_object = b"artifact-metadata";
        register_object_key(&mut connection, metadata_object, 12);
        let named_key = entity("artifact", "named-backup");
        let unbacked_key = entity("artifact", "not-captured");

        let content_record = direct_record(1, 0, 1, 20, b"orphan-content");
        let mut content_batch = FactBatch::new(2, 1).unwrap();
        content_batch
            .push(
                &content_record,
                Fact::ArtifactContent(artifact_content_fact(named_key.clone(), "orphan first\n")),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &content_record, 1, 0, 21, &content_batch);
        assert_eq!(
            connection
                .query_row(
                    "SELECT resolution_status || ':' || content_status FROM canonical_artifacts WHERE artifact_key = ?1",
                    [named_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "incomplete:orphan_content"
        );

        let metadata_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            30,
            b"late-metadata",
        );
        let mut metadata_batch = FactBatch::new(2, 1).unwrap();
        metadata_batch
            .push(
                &metadata_record,
                Fact::ArtifactMetadataSnapshot(artifact_metadata_fact(
                    "checkpoint-late",
                    vec![
                        artifact_metadata_entry(
                            named_key.clone(),
                            Some("71f902cd51ee4c6e@v1"),
                            "src/lib.rs",
                            "2026-08-11T00:00:01.000Z",
                            ArtifactCapture::ContentExpected,
                        ),
                        artifact_metadata_entry(
                            unbacked_key.clone(),
                            None,
                            "src/new.rs",
                            "2026-08-11T00:00:03.000Z",
                            ArtifactCapture::NotCaptured,
                        ),
                    ],
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            metadata_object,
            &metadata_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            31,
            &metadata_batch,
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT resolution_status || ':' || content_status FROM canonical_artifacts WHERE artifact_key = ?1",
                    [named_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "resolved:captured"
        );
        let unbacked: (String, String, String, Option<String>, Option<Vec<u8>>) = connection
            .query_row(
                "SELECT resolution_status, content_status, capture_status, native_artifact_id, content FROM canonical_artifacts WHERE artifact_key = ?1",
                [unbacked_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            unbacked,
            (
                "resolved".to_string(),
                "not_captured".to_string(),
                "not_captured".to_string(),
                None,
                None,
            )
        );
        assert_eq!(count(&connection, "observed_run_states"), 0);
        assert_eq!(count(&connection, "canonical_runs"), 0);
    }

    #[test]
    fn artifact_join_mismatch_is_explicit_and_correctable_by_blob_replacement() {
        let mut connection = database();
        register_object(&mut connection);
        let content_object = b"artifact-content";
        register_object_key(&mut connection, content_object, 12);
        let artifact_key = entity("artifact", "join-check");

        let metadata_record = direct_record(1, 0, 1, 20, b"join-metadata");
        let mut metadata = FactBatch::new(2, 1).unwrap();
        metadata
            .push(
                &metadata_record,
                Fact::ArtifactMetadataSnapshot(artifact_metadata_fact(
                    "join-metadata",
                    vec![artifact_metadata_entry(
                        artifact_key.clone(),
                        Some("71f902cd51ee4c6e@v1"),
                        "src/lib.rs",
                        "2026-08-11T00:00:01.000Z",
                        ArtifactCapture::ContentExpected,
                    )],
                )),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &metadata_record, 1, 0, 21, &metadata);

        let mismatched_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            30,
            b"join-mismatch",
        );
        let mut mismatched_fact = artifact_content_fact(artifact_key.clone(), "content");
        mismatched_fact.session = entity("session", "different-session");
        let mut mismatched = FactBatch::new(2, 1).unwrap();
        mismatched
            .push(&mismatched_record, Fact::ArtifactContent(mismatched_fact))
            .unwrap();
        commit_object_batch(
            &mut connection,
            content_object,
            &mismatched_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            31,
            &mismatched,
        );
        let conflicting: (String, i64) = connection
            .query_row(
                "SELECT resolution_status, join_conflict FROM canonical_artifacts WHERE artifact_key = ?1",
                [artifact_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(conflicting, ("conflicting".to_string(), 1));

        let corrected_record = object_record(
            2,
            1,
            SourceCursor::append_offset(1),
            SourceCursor::append_offset(2),
            40,
            b"join-corrected",
        );
        let mut corrected = FactBatch::new(2, 1).unwrap();
        corrected
            .push(
                &corrected_record,
                Fact::ArtifactContent(artifact_content_fact(artifact_key.clone(), "content")),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            content_object,
            &corrected_record,
            1,
            SourceCursor::append_offset(1).into_bytes(),
            41,
            &corrected,
        );
        let resolved: (String, String, i64) = connection
            .query_row(
                "SELECT resolution_status, content_status, join_conflict FROM canonical_artifacts WHERE artifact_key = ?1",
                [artifact_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            resolved,
            ("resolved".to_string(), "captured".to_string(), 0)
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT operation FROM change_log WHERE topic = 'diagnostic.runtime.artifact-conflict' AND entity_key = ?1 ORDER BY commit_seq DESC LIMIT 1",
                    [artifact_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "delete"
        );
    }

    #[test]
    fn competing_artifact_metadata_and_content_are_diagnosed_and_resolve() {
        let mut connection = database();
        register_object(&mut connection);
        let metadata_secondary = b"metadata-secondary";
        let content_primary = b"content-primary";
        let content_secondary = b"content-secondary";
        register_object_key(&mut connection, metadata_secondary, 12);
        register_object_key(&mut connection, content_primary, 13);
        register_object_key(&mut connection, content_secondary, 14);
        let artifact_key = entity("artifact", "named-backup");

        let primary_record = direct_record(1, 0, 1, 20, b"metadata-primary");
        let mut primary = FactBatch::new(2, 1).unwrap();
        primary
            .push(
                &primary_record,
                Fact::ArtifactMetadataSnapshot(artifact_metadata_fact(
                    "primary",
                    vec![artifact_metadata_entry(
                        artifact_key.clone(),
                        Some("71f902cd51ee4c6e@v1"),
                        "src/lib.rs",
                        "2026-08-11T00:00:01.000Z",
                        ArtifactCapture::ContentExpected,
                    )],
                )),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &primary_record, 1, 0, 21, &primary);

        let secondary_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            30,
            b"metadata-secondary",
        );
        let mut secondary = FactBatch::new(2, 1).unwrap();
        secondary
            .push(
                &secondary_record,
                Fact::ArtifactMetadataSnapshot(artifact_metadata_fact(
                    "secondary",
                    vec![artifact_metadata_entry(
                        artifact_key.clone(),
                        Some("71f902cd51ee4c6e@v1"),
                        "src/renamed.rs",
                        "2026-08-11T00:00:01.000Z",
                        ArtifactCapture::ContentExpected,
                    )],
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            metadata_secondary,
            &secondary_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            31,
            &secondary,
        );
        let metadata_conflict: (String, i64, i64) = connection
            .query_row(
                "SELECT resolution_status, metadata_assertion_count, competing_metadata_count FROM canonical_artifacts WHERE artifact_key = ?1",
                [artifact_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(metadata_conflict, ("conflicting".to_string(), 2, 1));

        let metadata_retracted = object_record(
            2,
            2,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            40,
            b"metadata-secondary-rewritten",
        );
        commit_object_batch(
            &mut connection,
            metadata_secondary,
            &metadata_retracted,
            1,
            SourceCursor::append_offset(1).into_bytes(),
            41,
            &FactBatch::new(1, 1).unwrap(),
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT competing_metadata_count FROM canonical_artifacts WHERE artifact_key = ?1",
                    [artifact_key.as_bytes()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );

        let content_primary_record = object_record(
            3,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            50,
            b"content-primary",
        );
        let mut primary_content = FactBatch::new(2, 1).unwrap();
        primary_content
            .push(
                &content_primary_record,
                Fact::ArtifactContent(artifact_content_fact(
                    artifact_key.clone(),
                    "primary content",
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            content_primary,
            &content_primary_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            51,
            &primary_content,
        );

        let content_secondary_record = object_record(
            4,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            60,
            b"content-secondary",
        );
        let mut secondary_content = FactBatch::new(2, 1).unwrap();
        secondary_content
            .push(
                &content_secondary_record,
                Fact::ArtifactContent(artifact_content_fact(
                    artifact_key.clone(),
                    "secondary content",
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            content_secondary,
            &content_secondary_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            61,
            &secondary_content,
        );
        let content_conflict: (String, i64, i64) = connection
            .query_row(
                "SELECT resolution_status, content_assertion_count, competing_content_count FROM canonical_artifacts WHERE artifact_key = ?1",
                [artifact_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(content_conflict, ("conflicting".to_string(), 2, 1));
        assert_eq!(
            connection
                .query_row(
                    "SELECT operation FROM change_log WHERE topic = 'diagnostic.runtime.artifact-conflict' AND entity_key = ?1 ORDER BY commit_seq DESC LIMIT 1",
                    [artifact_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "upsert"
        );

        let content_retracted = object_record(
            4,
            1,
            SourceCursor::append_offset(1),
            SourceCursor::append_offset(2),
            70,
            b"content-secondary-deleted",
        );
        commit_object_batch(
            &mut connection,
            content_secondary,
            &content_retracted,
            1,
            SourceCursor::append_offset(1).into_bytes(),
            71,
            &FactBatch::new(1, 1).unwrap(),
        );
        let resolved: (String, String, i64, i64) = connection
            .query_row(
                "SELECT resolution_status, content_status, content_assertion_count, competing_content_count FROM canonical_artifacts WHERE artifact_key = ?1",
                [artifact_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            resolved,
            ("resolved".to_string(), "captured".to_string(), 1, 0)
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT operation FROM change_log WHERE topic = 'diagnostic.runtime.artifact-conflict' AND entity_key = ?1 ORDER BY commit_seq DESC LIMIT 1",
                    [artifact_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "delete"
        );
    }

    #[test]
    fn workflow_journal_first_late_snapshot_and_rewrites_preserve_evidence_boundaries() {
        let mut connection = database();
        register_object(&mut connection);
        let snapshot_object = b"workflow-snapshot";
        register_object_key(&mut connection, snapshot_object, 12);
        let workflow_key = entity("workflow", "workflow-main");
        let member_key = entity("workflow_member", "workflow-main/agent-a");

        let started_record = direct_record(1, 0, 1, 20, b"workflow-started");
        let mut started = FactBatch::new(2, 1).unwrap();
        started
            .push(
                &started_record,
                Fact::WorkflowMemberEvent(workflow_member_fact(
                    "agent-a",
                    "member-1",
                    WorkflowMemberEventKind::Started,
                    None,
                )),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &started_record, 1, 0, 21, &started);

        let placeholder: (String, String, String, i64, i64) = connection
            .query_row(
                "SELECT snapshot_status, resolution_status, membership_count_status, observed_member_count, unresolved_member_count FROM canonical_workflows WHERE workflow_key = ?1",
                [workflow_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            placeholder,
            (
                "missing".to_string(),
                "incomplete".to_string(),
                "snapshot_missing".to_string(),
                1,
                1,
            )
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT member_status FROM canonical_workflow_members WHERE member_key = ?1",
                    [member_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "started"
        );

        let result_record = direct_record(1, 1, 2, 30, b"workflow-result");
        let mut result = FactBatch::new(2, 1).unwrap();
        result
            .push(
                &result_record,
                Fact::WorkflowMemberEvent(workflow_member_fact(
                    "agent-a",
                    "member-1",
                    WorkflowMemberEventKind::Result,
                    Some(serde_json::json!({"answer": 42})),
                )),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &result_record, 1, 1, 31, &result);
        let member: (String, String, Vec<u8>, i64, i64) = connection
            .query_row(
                "SELECT member_status, resolution_status, result_json, started_assertion_count, result_assertion_count FROM canonical_workflow_members WHERE member_key = ?1",
                [member_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();
        assert_eq!(member.0, "result_observed");
        assert_eq!(member.1, "resolved");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&member.2).unwrap(),
            serde_json::json!({"answer": 42})
        );
        assert_eq!((member.3, member.4), (1, 1));

        let snapshot_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            40,
            b"workflow-snapshot",
        );
        let mut snapshot = FactBatch::new(2, 1).unwrap();
        snapshot
            .push(
                &snapshot_record,
                Fact::WorkflowSnapshot(workflow_snapshot_fact("completed", 1, "done")),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            snapshot_object,
            &snapshot_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            41,
            &snapshot,
        );
        let joined: (String, String, String, String, i64, i64) = connection
            .query_row(
                "SELECT snapshot_status, resolution_status, workflow_status, membership_count_status, observed_member_count, result_member_count FROM canonical_workflows WHERE workflow_key = ?1",
                [workflow_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .unwrap();
        assert_eq!(
            joined,
            (
                "present".to_string(),
                "resolved".to_string(),
                "succeeded".to_string(),
                "matched".to_string(),
                1,
                1,
            )
        );
        assert_eq!(count(&connection, "canonical_runs"), 0);
        assert_eq!(count(&connection, "observed_run_states"), 0);

        let deleted_snapshot_record = object_record(
            2,
            1,
            SourceCursor::append_offset(1),
            SourceCursor::append_offset(2),
            50,
            b"workflow-snapshot-deleted",
        );
        commit_object_batch(
            &mut connection,
            snapshot_object,
            &deleted_snapshot_record,
            1,
            SourceCursor::append_offset(1).into_bytes(),
            51,
            &FactBatch::new(1, 1).unwrap(),
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT snapshot_status || ':' || resolution_status FROM canonical_workflows WHERE workflow_key = ?1",
                    [workflow_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "missing:incomplete"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM fact_records WHERE fact_kind = 'workflow_snapshot'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );

        let rewritten_journal = direct_record(2, 0, 1, 60, b"workflow-journal-rewritten");
        commit_direct_batch(
            &mut connection,
            &rewritten_journal,
            1,
            2,
            61,
            &FactBatch::new(1, 1).unwrap(),
        );
        assert_eq!(count(&connection, "canonical_workflow_members"), 0);
        assert_eq!(count(&connection, "canonical_workflows"), 0);
        assert_eq!(count(&connection, "workflow_member_event_assertions"), 0);
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM fact_records WHERE fact_kind IN ('workflow_snapshot', 'workflow_member_event')",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn completed_workflow_count_mismatch_does_not_complete_child_runs_or_conflict() {
        let mut connection = database();
        register_object(&mut connection);
        let journal_object = b"workflow-journal";
        register_object_key(&mut connection, journal_object, 12);
        let workflow_key = entity("workflow", "workflow-main");
        let member_key = entity("workflow_member", "workflow-main/agent-a");

        let snapshot_record = direct_record(1, 0, 1, 20, b"completed-workflow");
        let mut snapshot = FactBatch::new(2, 1).unwrap();
        snapshot
            .push(
                &snapshot_record,
                Fact::WorkflowSnapshot(workflow_snapshot_fact("completed", 2, "done")),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &snapshot_record, 1, 0, 21, &snapshot);

        let started_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            30,
            b"only-one-member",
        );
        let mut started = FactBatch::new(2, 1).unwrap();
        started
            .push(
                &started_record,
                Fact::WorkflowMemberEvent(workflow_member_fact(
                    "agent-a",
                    "member-1",
                    WorkflowMemberEventKind::Started,
                    None,
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            journal_object,
            &started_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            31,
            &started,
        );

        let workflow: (String, String, String, i64, i64) = connection
            .query_row(
                "SELECT workflow_status, resolution_status, membership_count_status, join_conflict, competing_snapshot_count FROM canonical_workflows WHERE workflow_key = ?1",
                [workflow_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            workflow,
            (
                "succeeded".to_string(),
                "resolved".to_string(),
                "different".to_string(),
                0,
                0,
            )
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT member_status FROM canonical_workflow_members WHERE member_key = ?1",
                    [member_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "started"
        );
        assert_eq!(count(&connection, "canonical_runs"), 0);
        assert_eq!(count(&connection, "observed_run_states"), 0);
        assert_eq!(
            connection
                .query_row(
                    "SELECT operation FROM change_log WHERE topic = 'diagnostic.runtime.workflow-conflict' AND entity_key = ?1 ORDER BY commit_seq DESC LIMIT 1",
                    [workflow_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "delete"
        );
    }

    #[test]
    fn competing_workflow_snapshots_and_member_results_resolve_after_retraction() {
        let mut connection = database();
        register_object(&mut connection);
        let snapshot_secondary = b"workflow-snapshot-secondary";
        let journal_primary = b"workflow-journal-primary";
        let journal_secondary = b"workflow-journal-secondary";
        register_object_key(&mut connection, snapshot_secondary, 12);
        register_object_key(&mut connection, journal_primary, 13);
        register_object_key(&mut connection, journal_secondary, 14);
        let workflow_key = entity("workflow", "workflow-main");
        let member_key = entity("workflow_member", "workflow-main/agent-a");

        let primary_snapshot_record = direct_record(1, 0, 1, 20, b"snapshot-primary");
        let mut primary_snapshot = FactBatch::new(2, 1).unwrap();
        primary_snapshot
            .push(
                &primary_snapshot_record,
                Fact::WorkflowSnapshot(workflow_snapshot_fact("completed", 1, "primary")),
            )
            .unwrap();
        commit_direct_batch(
            &mut connection,
            &primary_snapshot_record,
            1,
            0,
            21,
            &primary_snapshot,
        );

        let secondary_snapshot_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            30,
            b"snapshot-secondary",
        );
        let mut secondary_snapshot = FactBatch::new(2, 1).unwrap();
        secondary_snapshot
            .push(
                &secondary_snapshot_record,
                Fact::WorkflowSnapshot(workflow_snapshot_fact("failed", 1, "secondary")),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            snapshot_secondary,
            &secondary_snapshot_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            31,
            &secondary_snapshot,
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT resolution_status || ':' || snapshot_assertion_count || ':' || competing_snapshot_count FROM canonical_workflows WHERE workflow_key = ?1",
                    [workflow_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "conflicting:2:1"
        );

        let retracted_snapshot_record = object_record(
            2,
            1,
            SourceCursor::append_offset(1),
            SourceCursor::append_offset(2),
            40,
            b"snapshot-secondary-deleted",
        );
        commit_object_batch(
            &mut connection,
            snapshot_secondary,
            &retracted_snapshot_record,
            1,
            SourceCursor::append_offset(1).into_bytes(),
            41,
            &FactBatch::new(1, 1).unwrap(),
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT resolution_status || ':' || competing_snapshot_count FROM canonical_workflows WHERE workflow_key = ?1",
                    [workflow_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "resolved:0"
        );

        let primary_result_record = object_record(
            3,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            50,
            b"member-result-primary",
        );
        let mut primary_result = FactBatch::new(2, 1).unwrap();
        primary_result
            .push(
                &primary_result_record,
                Fact::WorkflowMemberEvent(workflow_member_fact(
                    "agent-a",
                    "member-1",
                    WorkflowMemberEventKind::Result,
                    Some(serde_json::json!({"answer": "primary"})),
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            journal_primary,
            &primary_result_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            51,
            &primary_result,
        );

        let secondary_result_record = object_record(
            4,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            60,
            b"member-result-secondary",
        );
        let mut secondary_result = FactBatch::new(2, 1).unwrap();
        secondary_result
            .push(
                &secondary_result_record,
                Fact::WorkflowMemberEvent(workflow_member_fact(
                    "agent-a",
                    "member-other-key",
                    WorkflowMemberEventKind::Result,
                    Some(serde_json::json!({"answer": "secondary"})),
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            journal_secondary,
            &secondary_result_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            61,
            &secondary_result,
        );
        let conflicting_member: (String, i64, i64, i64) = connection
            .query_row(
                "SELECT resolution_status, result_assertion_count, competing_result_count, event_key_conflict FROM canonical_workflow_members WHERE member_key = ?1",
                [member_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(conflicting_member, ("conflicting".to_string(), 2, 1, 1));

        let retracted_result_record = object_record(
            4,
            2,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            70,
            b"member-result-secondary-rewritten",
        );
        commit_object_batch(
            &mut connection,
            journal_secondary,
            &retracted_result_record,
            1,
            SourceCursor::append_offset(1).into_bytes(),
            71,
            &FactBatch::new(1, 1).unwrap(),
        );
        let resolved_member: (String, String, i64, i64, i64) = connection
            .query_row(
                "SELECT member_status, resolution_status, result_assertion_count, competing_result_count, event_key_conflict FROM canonical_workflow_members WHERE member_key = ?1",
                [member_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            resolved_member,
            ("orphan_result".to_string(), "resolved".to_string(), 1, 0, 0)
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT operation FROM change_log WHERE topic = 'diagnostic.runtime.workflow-member-conflict' AND entity_key = ?1 ORDER BY commit_seq DESC LIMIT 1",
                    [member_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "delete"
        );
    }

    #[test]
    fn retracting_a_decisive_member_identity_conflict_refreshes_both_workflows() {
        let mut connection = database();
        register_object(&mut connection);
        let primary_object = b"workflow-member-primary";
        let secondary_object = b"workflow-member-secondary";
        register_object_key(&mut connection, primary_object, 12);
        register_object_key(&mut connection, secondary_object, 13);
        let primary_workflow = entity("workflow", "workflow-main");
        let secondary_workflow = entity("workflow", "workflow-other");
        let member_key = entity("workflow_member", "workflow-main/agent-a");

        let primary_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            20,
            b"member-primary",
        );
        let mut primary = FactBatch::new(2, 1).unwrap();
        primary
            .push(
                &primary_record,
                Fact::WorkflowMemberEvent(workflow_member_fact(
                    "agent-a",
                    "member-1",
                    WorkflowMemberEventKind::Started,
                    None,
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            primary_object,
            &primary_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            21,
            &primary,
        );

        let secondary_record = object_record(
            3,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            30,
            b"member-secondary",
        );
        let mut competing_fact = workflow_member_fact(
            "agent-a",
            "member-1",
            WorkflowMemberEventKind::Started,
            None,
        );
        competing_fact.workflow = secondary_workflow.clone();
        competing_fact.native_workflow_id = "wf_other".to_string();
        let mut secondary = FactBatch::new(2, 1).unwrap();
        secondary
            .push(&secondary_record, Fact::WorkflowMemberEvent(competing_fact))
            .unwrap();
        commit_object_batch(
            &mut connection,
            secondary_object,
            &secondary_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            31,
            &secondary,
        );

        let (decisive_object_id, decisive_workflow): (i64, Vec<u8>) = connection
            .query_row(
                r#"
                SELECT assertion.source_object_id, member.workflow_key
                FROM canonical_workflow_members AS member
                JOIN workflow_member_event_assertions AS assertion
                  ON assertion.fact_id = member.decisive_started_fact_id
                WHERE member.member_key = ?1
                "#,
                [member_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT identity_conflict FROM canonical_workflow_members WHERE member_key = ?1",
                    [member_key.as_bytes()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );

        let (removed_object, removed_object_ordinal, remaining_workflow) =
            if decisive_object_id == object_catalog_id(primary_object) {
                (primary_object.as_slice(), 2, secondary_workflow.as_bytes())
            } else {
                assert_eq!(decisive_object_id, object_catalog_id(secondary_object));
                (secondary_object.as_slice(), 3, primary_workflow.as_bytes())
            };
        let rewritten = object_record(
            removed_object_ordinal,
            2,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            40,
            b"decisive-member-retracted",
        );
        commit_object_batch(
            &mut connection,
            removed_object,
            &rewritten,
            1,
            SourceCursor::append_offset(1).into_bytes(),
            41,
            &FactBatch::new(1, 1).unwrap(),
        );

        let canonical_member: (Vec<u8>, i64) = connection
            .query_row(
                "SELECT workflow_key, identity_conflict FROM canonical_workflow_members WHERE member_key = ?1",
                [member_key.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(canonical_member, (remaining_workflow.to_vec(), 0));
        assert_eq!(
            connection
                .query_row(
                    "SELECT observed_member_count FROM canonical_workflows WHERE workflow_key = ?1",
                    [remaining_workflow],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM canonical_workflows WHERE workflow_key = ?1",
                    [decisive_workflow],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn session_index_first_and_transcript_first_converge_without_fabricating_history() {
        let index_object = b"session-index";
        let index_key = entity("project", PROJECT);
        let session_key = entity("session", SESSION);

        let run_index_first = || {
            let mut connection = database();
            register_object(&mut connection);
            register_object_key(&mut connection, index_object, 12);
            let index_record = object_record(
                2,
                1,
                SourceCursor::append_offset(0),
                SourceCursor::append_offset(1),
                20,
                b"session-index",
            );
            let mut index = FactBatch::new(2, 1).unwrap();
            index
                .push(
                    &index_record,
                    Fact::SessionIndexSnapshot(session_index_fact(
                        index_key.clone(),
                        PROJECT,
                        vec![session_index_entry(
                            session_key.clone(),
                            SESSION,
                            "Build the index pack",
                            Some("Index pack"),
                        )],
                    )),
                )
                .unwrap();
            commit_object_batch(
                &mut connection,
                index_object,
                &index_record,
                1,
                SourceCursor::append_offset(0).into_bytes(),
                21,
                &index,
            );
            assert_eq!(count(&connection, "canonical_sessions"), 0);
            assert_eq!(count(&connection, "canonical_messages"), 0);
            assert_eq!(
                connection
                    .query_row(
                        "SELECT transcript_status FROM canonical_session_index_entries WHERE session_key = ?1",
                        [session_key.as_bytes()],
                        |row| row.get::<_, String>(0),
                    )
                    .unwrap(),
                "missing"
            );

            let transcript_record = direct_record(1, 0, 1, 30, b"transcript-session");
            let mut transcript = FactBatch::new(2, 1).unwrap();
            transcript
                .push(
                    &transcript_record,
                    Fact::Session(SessionFact {
                        session: session_key.clone(),
                        project: index_key.clone(),
                        native_session_id: SESSION.to_string(),
                        native_project_key: PROJECT.to_string(),
                        cwd: Some("/fixture/project".to_string()),
                        git_branch: Some("main".to_string()),
                        first_prompt: Some("Transcript prompt".to_string()),
                        ai_title: None,
                        custom_title: None,
                        source_time: Some(exact("2026-02-02T00:00:30.000Z")),
                    }),
                )
                .unwrap();
            commit_direct_batch(&mut connection, &transcript_record, 1, 0, 31, &transcript);
            connection
        };

        let mut index_first = run_index_first();
        assert_eq!(
            index_first
                .query_row(
                    "SELECT transcript_status || ':' || resolution_status FROM canonical_session_index_entries WHERE session_key = ?1",
                    [session_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "present:resolved"
        );

        let mut transcript_first = database();
        register_object(&mut transcript_first);
        register_object_key(&mut transcript_first, index_object, 12);
        let transcript_record = direct_record(1, 0, 1, 20, b"transcript-session");
        let mut transcript = FactBatch::new(2, 1).unwrap();
        transcript
            .push(
                &transcript_record,
                Fact::Session(SessionFact {
                    session: session_key.clone(),
                    project: index_key.clone(),
                    native_session_id: SESSION.to_string(),
                    native_project_key: PROJECT.to_string(),
                    cwd: Some("/fixture/project".to_string()),
                    git_branch: Some("main".to_string()),
                    first_prompt: Some("Transcript prompt".to_string()),
                    ai_title: None,
                    custom_title: None,
                    source_time: Some(exact("2026-02-02T00:00:30.000Z")),
                }),
            )
            .unwrap();
        commit_direct_batch(
            &mut transcript_first,
            &transcript_record,
            1,
            0,
            21,
            &transcript,
        );
        let index_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            30,
            b"session-index",
        );
        let mut index = FactBatch::new(2, 1).unwrap();
        index
            .push(
                &index_record,
                Fact::SessionIndexSnapshot(session_index_fact(
                    index_key,
                    PROJECT,
                    vec![session_index_entry(
                        session_key.clone(),
                        SESSION,
                        "Build the index pack",
                        Some("Index pack"),
                    )],
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut transcript_first,
            index_object,
            &index_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            31,
            &index,
        );
        let canonical = |connection: &Connection| {
            connection
                .query_row(
                    r#"
                    SELECT native_session_id, first_prompt, summary,
                           transcript_status, resolution_status,
                           assertion_count, competing_entry_count, join_conflict
                    FROM canonical_session_index_entries WHERE session_key = ?1
                    "#,
                    [session_key.as_bytes()],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, i64>(5)?,
                            row.get::<_, i64>(6)?,
                            row.get::<_, i64>(7)?,
                        ))
                    },
                )
                .unwrap()
        };
        assert_eq!(canonical(&index_first), canonical(&transcript_first));

        let replacement_record = object_record(
            2,
            1,
            SourceCursor::append_offset(1),
            SourceCursor::append_offset(2),
            40,
            b"session-index-empty",
        );
        let mut replacement = FactBatch::new(2, 1).unwrap();
        replacement
            .push(
                &replacement_record,
                Fact::SessionIndexSnapshot(session_index_fact(
                    entity("project", PROJECT),
                    PROJECT,
                    Vec::new(),
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut index_first,
            index_object,
            &replacement_record,
            1,
            SourceCursor::append_offset(1).into_bytes(),
            41,
            &replacement,
        );
        assert_eq!(count(&index_first, "canonical_session_index_entries"), 0);
        assert_eq!(count(&index_first, "canonical_sessions"), 1);
        assert_eq!(
            index_first
                .query_row(
                    "SELECT entry_count FROM canonical_session_indexes WHERE project_key = ?1",
                    [entity("project", PROJECT).as_bytes()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert_eq!(
            index_first
                .query_row(
                    "SELECT COUNT(*) FROM fact_records WHERE fact_kind = 'session_index_snapshot'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
    }

    #[test]
    fn competing_session_indexes_and_cross_project_transcript_are_explicit_conflicts() {
        let mut connection = database();
        register_object(&mut connection);
        let primary_object = b"session-index-primary";
        let secondary_object = b"session-index-secondary";
        register_object_key(&mut connection, primary_object, 12);
        register_object_key(&mut connection, secondary_object, 13);
        let project_key = entity("project", PROJECT);
        let session_key = entity("session", SESSION);

        let primary_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            20,
            b"index-primary",
        );
        let mut primary = FactBatch::new(2, 1).unwrap();
        primary
            .push(
                &primary_record,
                Fact::SessionIndexSnapshot(session_index_fact(
                    project_key.clone(),
                    PROJECT,
                    vec![session_index_entry(
                        session_key.clone(),
                        SESSION,
                        "Primary prompt",
                        Some("Primary"),
                    )],
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            primary_object,
            &primary_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            21,
            &primary,
        );

        let secondary_record = object_record(
            3,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            30,
            b"index-secondary",
        );
        let mut secondary = FactBatch::new(2, 1).unwrap();
        secondary
            .push(
                &secondary_record,
                Fact::SessionIndexSnapshot(session_index_fact(
                    project_key.clone(),
                    PROJECT,
                    vec![session_index_entry(
                        session_key.clone(),
                        SESSION,
                        "Secondary prompt",
                        Some("Secondary"),
                    )],
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            secondary_object,
            &secondary_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            31,
            &secondary,
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT index_status || ':' || assertion_count || ':' || competing_snapshot_count FROM canonical_session_indexes WHERE project_key = ?1",
                    [project_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "conflicting:2:1"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT resolution_status || ':' || assertion_count || ':' || competing_entry_count FROM canonical_session_index_entries WHERE session_key = ?1",
                    [session_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "conflicting:2:1"
        );

        let transcript_record = direct_record(1, 0, 1, 40, b"wrong-project-transcript");
        let mut transcript = FactBatch::new(2, 1).unwrap();
        transcript
            .push(
                &transcript_record,
                Fact::Session(SessionFact {
                    session: session_key.clone(),
                    project: entity("project", "different-project"),
                    native_session_id: SESSION.to_string(),
                    native_project_key: "different-project".to_string(),
                    cwd: Some("/different".to_string()),
                    git_branch: None,
                    first_prompt: None,
                    ai_title: None,
                    custom_title: None,
                    source_time: None,
                }),
            )
            .unwrap();
        commit_direct_batch(&mut connection, &transcript_record, 1, 0, 41, &transcript);
        assert_eq!(
            connection
                .query_row(
                    "SELECT transcript_status || ':' || join_conflict FROM canonical_session_index_entries WHERE session_key = ?1",
                    [session_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "different_project:1"
        );

        let retracted_record = object_record(
            3,
            1,
            SourceCursor::append_offset(1),
            SourceCursor::append_offset(2),
            50,
            b"secondary-deleted",
        );
        commit_object_batch(
            &mut connection,
            secondary_object,
            &retracted_record,
            1,
            SourceCursor::append_offset(1).into_bytes(),
            51,
            &FactBatch::new(1, 1).unwrap(),
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT index_status || ':' || competing_snapshot_count FROM canonical_session_indexes WHERE project_key = ?1",
                    [project_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "resolved:0"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT resolution_status || ':' || competing_entry_count || ':' || join_conflict FROM canonical_session_index_entries WHERE session_key = ?1",
                    [session_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "conflicting:0:1"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT operation FROM change_log WHERE topic = 'diagnostic.history.session-index-conflict' AND entity_key = ?1 ORDER BY commit_seq DESC LIMIT 1",
                    [project_key.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "delete"
        );
    }

    #[test]
    fn project_memory_documents_replace_independently_conflict_and_never_create_runtime_state() {
        let mut connection = database();
        register_object(&mut connection);
        let index_object = b"memory-index";
        let topic_object = b"memory-topic";
        let competitor_object = b"memory-topic-competitor";
        register_object_key(&mut connection, index_object, 12);
        register_object_key(&mut connection, topic_object, 13);
        register_object_key(&mut connection, competitor_object, 14);

        let index_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            20,
            b"memory-index",
        );
        let mut index = FactBatch::new(2, 1).unwrap();
        index
            .push(
                &index_record,
                Fact::ProjectMemoryDocument(project_memory_fact(
                    "memory/MEMORY.md",
                    "Memory index",
                    "# Memory index\n\n- [Build](build.md)\n",
                    true,
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            index_object,
            &index_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            21,
            &index,
        );

        let topic_record = object_record(
            3,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            30,
            b"memory-topic",
        );
        let mut topic = FactBatch::new(2, 1).unwrap();
        topic
            .push(
                &topic_record,
                Fact::ProjectMemoryDocument(project_memory_fact(
                    "memory/build.md",
                    "Build",
                    "# Build\n\nUse cargo.\n",
                    false,
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            topic_object,
            &topic_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            31,
            &topic,
        );
        assert_eq!(count(&connection, "canonical_project_memory_documents"), 2);
        assert_eq!(count(&connection, "canonical_sessions"), 0);
        assert_eq!(count(&connection, "canonical_runs"), 0);
        assert_eq!(count(&connection, "run_evidence"), 0);
        assert_eq!(count(&connection, "observed_run_states"), 0);

        let replacement_record = object_record(
            3,
            1,
            SourceCursor::append_offset(1),
            SourceCursor::append_offset(2),
            40,
            b"memory-topic-replaced",
        );
        let mut replacement = FactBatch::new(2, 1).unwrap();
        replacement
            .push(
                &replacement_record,
                Fact::ProjectMemoryDocument(project_memory_fact(
                    "memory/build.md",
                    "Build v2",
                    "# Build v2\n\nUse cargo test.\n",
                    false,
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            topic_object,
            &replacement_record,
            1,
            SourceCursor::append_offset(1).into_bytes(),
            41,
            &replacement,
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT title FROM canonical_project_memory_documents WHERE native_document_path = 'memory/build.md'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "Build v2"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM fact_records WHERE fact_kind = 'project_memory_document'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            2
        );

        let competitor_record = object_record(
            4,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            50,
            b"memory-topic-competitor",
        );
        let mut competitor = FactBatch::new(2, 1).unwrap();
        competitor
            .push(
                &competitor_record,
                Fact::ProjectMemoryDocument(project_memory_fact(
                    "memory/build.md",
                    "Competing build",
                    "# Competing build\n",
                    false,
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            competitor_object,
            &competitor_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            51,
            &competitor,
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT resolution_status || ':' || assertion_count || ':' || competing_document_count FROM canonical_project_memory_documents WHERE native_document_path = 'memory/build.md'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "conflicting:2:1"
        );

        let competitor_deleted = object_record(
            4,
            1,
            SourceCursor::append_offset(1),
            SourceCursor::append_offset(2),
            60,
            b"memory-topic-competitor-deleted",
        );
        commit_object_batch(
            &mut connection,
            competitor_object,
            &competitor_deleted,
            1,
            SourceCursor::append_offset(1).into_bytes(),
            61,
            &FactBatch::new(1, 1).unwrap(),
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT resolution_status || ':' || assertion_count || ':' || competing_document_count FROM canonical_project_memory_documents WHERE native_document_path = 'memory/build.md'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "resolved:1:0"
        );

        let topic_deleted = object_record(
            3,
            1,
            SourceCursor::append_offset(2),
            SourceCursor::append_offset(3),
            70,
            b"memory-topic-deleted",
        );
        commit_object_batch(
            &mut connection,
            topic_object,
            &topic_deleted,
            1,
            SourceCursor::append_offset(2).into_bytes(),
            71,
            &FactBatch::new(1, 1).unwrap(),
        );
        assert_eq!(count(&connection, "canonical_project_memory_documents"), 1);
        assert_eq!(
            connection
                .query_row(
                    "SELECT is_index FROM canonical_project_memory_documents",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM fact_records WHERE fact_kind = 'project_memory_document'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
    }

    #[test]
    fn persisted_tool_results_late_join_replace_retract_and_never_create_history() {
        let mut connection = database();
        register_object(&mut connection);
        let sidecar_object = b"tool-result-main";
        register_object_key(&mut connection, sidecar_object, 12);
        let native_tool_id = "toolu_result_1";

        // Sidecar-first arrival is a durable result document, not fabricated
        // transcript history or run evidence.
        let initial_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            20,
            b"persisted-result-v1",
        );
        let mut initial = FactBatch::new(2, 1).unwrap();
        initial
            .push(
                &initial_record,
                Fact::PersistedToolResult(persisted_tool_result_fact(
                    native_tool_id,
                    "persisted result v1\n",
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            sidecar_object,
            &initial_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            21,
            &initial,
        );
        assert_eq!(count(&connection, "canonical_persisted_tool_results"), 1);
        assert_eq!(count(&connection, "canonical_messages"), 0);
        assert_eq!(count(&connection, "canonical_sessions"), 0);
        assert_eq!(count(&connection, "canonical_runs"), 0);
        assert_eq!(count(&connection, "run_evidence"), 0);
        assert_eq!(
            connection
                .query_row(
                    "SELECT resolution_status || ':' || correlation_status || ':' || tool_call_match_count || ':' || tool_result_match_count FROM canonical_persisted_tool_results",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "resolved:unlinked:0:0"
        );

        let call_record = object_record(
            1,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            30,
            b"tool-call-message",
        );
        let mut call = FactBatch::new(2, 1).unwrap();
        call.push(
            &call_record,
            Fact::Message(tool_message(
                "call-message",
                MessageRole::Assistant,
                vec![ContentBlock::ToolCall {
                    native_id: native_tool_id.to_string(),
                    name: "Read".to_string(),
                    input: serde_json::json!({"file_path": "/fixture/file"}),
                }],
            )),
        )
        .unwrap();
        commit_object_batch(
            &mut connection,
            b"fixture-transcript",
            &call_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            31,
            &call,
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT correlation_status || ':' || tool_call_match_count || ':' || tool_result_match_count FROM canonical_persisted_tool_results",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "call_only:1:0"
        );

        let result_record = object_record(
            1,
            1,
            SourceCursor::append_offset(1),
            SourceCursor::append_offset(2),
            40,
            b"tool-result-message",
        );
        let mut result = FactBatch::new(2, 1).unwrap();
        result
            .push(
                &result_record,
                Fact::Message(tool_message(
                    "result-message",
                    MessageRole::User,
                    vec![ContentBlock::ToolResult {
                        native_call_id: native_tool_id.to_string(),
                        content: serde_json::json!("compact inline result"),
                        is_error: false,
                    }],
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            b"fixture-transcript",
            &result_record,
            1,
            SourceCursor::append_offset(1).into_bytes(),
            41,
            &result,
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT correlation_status || ':' || tool_call_match_count || ':' || tool_result_match_count FROM canonical_persisted_tool_results",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "linked:1:1"
        );
        assert_eq!(count(&connection, "message_tool_references"), 2);
        assert_eq!(count(&connection, "canonical_message_content_blocks"), 2);
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM canonical_message_content_blocks WHERE content_kind = 'tool_call'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM canonical_message_content_blocks WHERE content_kind = 'tool_result'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );

        // Same-generation replacement owns the current file rather than
        // retaining both revisions as competing assertions.
        let replacement_record = object_record(
            2,
            1,
            SourceCursor::append_offset(1),
            SourceCursor::append_offset(2),
            50,
            b"persisted-result-v2",
        );
        let mut replacement = FactBatch::new(2, 1).unwrap();
        replacement
            .push(
                &replacement_record,
                Fact::PersistedToolResult(persisted_tool_result_fact(
                    native_tool_id,
                    "persisted result v2\n",
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            sidecar_object,
            &replacement_record,
            1,
            SourceCursor::append_offset(1).into_bytes(),
            51,
            &replacement,
        );
        assert_eq!(
            connection
                .query_row(
                    r#"
                    SELECT assertion.content
                    FROM canonical_persisted_tool_results AS canonical
                    JOIN persisted_tool_result_assertions AS assertion
                      ON assertion.fact_id = canonical.decisive_fact_id
                    "#,
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "persisted result v2\n"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT length(content) FROM canonical_persisted_tool_results",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0,
            "canonical tool results reference rather than duplicate decisive content",
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM fact_records WHERE fact_kind = 'persisted_tool_result'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );

        // A transcript generation replacement retracts both old references
        // before indexing the new message; the sidecar becomes result-only.
        let generation_record = object_record(
            1,
            2,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            60,
            b"replacement-result-message",
        );
        let mut generation = FactBatch::new(2, 1).unwrap();
        generation
            .push(
                &generation_record,
                Fact::Message(tool_message(
                    "replacement-result-message",
                    MessageRole::User,
                    vec![ContentBlock::ToolResult {
                        native_call_id: native_tool_id.to_string(),
                        content: serde_json::json!("new generation inline result"),
                        is_error: false,
                    }],
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            b"fixture-transcript",
            &generation_record,
            1,
            SourceCursor::append_offset(2).into_bytes(),
            61,
            &generation,
        );
        assert_eq!(count(&connection, "canonical_messages"), 1);
        assert_eq!(count(&connection, "message_tool_references"), 1);
        assert_eq!(count(&connection, "canonical_message_content_blocks"), 1);
        assert_eq!(
            connection
                .query_row(
                    "SELECT content_kind FROM canonical_message_content_blocks",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "tool_result"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT correlation_status || ':' || tool_call_match_count || ':' || tool_result_match_count FROM canonical_persisted_tool_results",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "result_only:0:1"
        );

        let deleted_record = object_record(
            2,
            1,
            SourceCursor::append_offset(2),
            SourceCursor::append_offset(3),
            70,
            b"persisted-result-deleted",
        );
        commit_object_batch(
            &mut connection,
            sidecar_object,
            &deleted_record,
            1,
            SourceCursor::append_offset(2).into_bytes(),
            71,
            &FactBatch::new(1, 1).unwrap(),
        );
        assert_eq!(count(&connection, "canonical_persisted_tool_results"), 0);

        // Transcript-first arrival reaches the same result-only state when a
        // file later reappears, and confirmed absence retracts it again.
        let reappeared_record = object_record(
            2,
            1,
            SourceCursor::append_offset(3),
            SourceCursor::append_offset(4),
            80,
            b"persisted-result-reappeared",
        );
        let mut reappeared = FactBatch::new(2, 1).unwrap();
        reappeared
            .push(
                &reappeared_record,
                Fact::PersistedToolResult(persisted_tool_result_fact(
                    native_tool_id,
                    "reappeared\n",
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            sidecar_object,
            &reappeared_record,
            1,
            SourceCursor::append_offset(3).into_bytes(),
            81,
            &reappeared,
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT correlation_status FROM canonical_persisted_tool_results",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "result_only"
        );
        let final_deleted_record = object_record(
            2,
            1,
            SourceCursor::append_offset(4),
            SourceCursor::append_offset(5),
            90,
            b"persisted-result-final-delete",
        );
        commit_object_batch(
            &mut connection,
            sidecar_object,
            &final_deleted_record,
            1,
            SourceCursor::append_offset(4).into_bytes(),
            91,
            &FactBatch::new(1, 1).unwrap(),
        );
        assert_eq!(count(&connection, "canonical_persisted_tool_results"), 0);
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM fact_records WHERE fact_kind = 'persisted_tool_result'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert!(
            connection
                .prepare("PRAGMA foreign_key_check")
                .unwrap()
                .query([])
                .unwrap()
                .next()
                .unwrap()
                .is_none(),
            "replacement and retraction must remain FK-complete when bootstrap enforcement is deferred"
        );
    }

    #[test]
    fn persisted_tool_result_duplicate_content_and_join_conflicts_are_explicit() {
        let mut connection = database();
        register_object(&mut connection);
        let main_object = b"tool-result-main";
        let competitor_object = b"tool-result-competitor";
        register_object_key(&mut connection, main_object, 12);
        register_object_key(&mut connection, competitor_object, 13);
        let native_tool_id = "toolu_conflict";

        let main_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            20,
            b"main",
        );
        let mut main = FactBatch::new(2, 1).unwrap();
        main.push(
            &main_record,
            Fact::PersistedToolResult(persisted_tool_result_fact(native_tool_id, "same\n")),
        )
        .unwrap();
        commit_object_batch(
            &mut connection,
            main_object,
            &main_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            21,
            &main,
        );

        let duplicate_record = object_record(
            3,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            30,
            b"duplicate",
        );
        let mut duplicate = FactBatch::new(2, 1).unwrap();
        duplicate
            .push(
                &duplicate_record,
                Fact::PersistedToolResult(persisted_tool_result_fact(native_tool_id, "same\n")),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            competitor_object,
            &duplicate_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            31,
            &duplicate,
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT resolution_status || ':' || assertion_count || ':' || competing_result_count FROM canonical_persisted_tool_results",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "resolved:2:0"
        );

        let competing_record = object_record(
            3,
            1,
            SourceCursor::append_offset(1),
            SourceCursor::append_offset(2),
            40,
            b"competing",
        );
        let mut competing = FactBatch::new(2, 1).unwrap();
        competing
            .push(
                &competing_record,
                Fact::PersistedToolResult(persisted_tool_result_fact(
                    native_tool_id,
                    "different\n",
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            competitor_object,
            &competing_record,
            1,
            SourceCursor::append_offset(1).into_bytes(),
            41,
            &competing,
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT resolution_status || ':' || competing_result_count FROM canonical_persisted_tool_results",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "conflicting:1"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT operation FROM change_log WHERE topic = 'diagnostic.history.persisted-tool-result-conflict' ORDER BY commit_seq DESC LIMIT 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "upsert"
        );

        let duplicate_calls_record = object_record(
            1,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            50,
            b"duplicate-calls",
        );
        let duplicate_call = ContentBlock::ToolCall {
            native_id: native_tool_id.to_string(),
            name: "Read".to_string(),
            input: serde_json::json!({}),
        };
        let mut duplicate_calls = FactBatch::new(2, 1).unwrap();
        duplicate_calls
            .push(
                &duplicate_calls_record,
                Fact::Message(tool_message(
                    "duplicate-call-message",
                    MessageRole::Assistant,
                    vec![duplicate_call.clone(), duplicate_call],
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            b"fixture-transcript",
            &duplicate_calls_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            51,
            &duplicate_calls,
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT correlation_status || ':' || tool_call_match_count || ':' || join_conflict FROM canonical_persisted_tool_results",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "ambiguous:2:1"
        );

        let competitor_deleted_record = object_record(
            3,
            1,
            SourceCursor::append_offset(2),
            SourceCursor::append_offset(3),
            60,
            b"competitor-deleted",
        );
        commit_object_batch(
            &mut connection,
            competitor_object,
            &competitor_deleted_record,
            1,
            SourceCursor::append_offset(2).into_bytes(),
            61,
            &FactBatch::new(1, 1).unwrap(),
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT resolution_status || ':' || competing_result_count || ':' || correlation_status FROM canonical_persisted_tool_results",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "resolved:0:ambiguous"
        );
    }

    #[test]
    fn interpretation_settings_merge_replace_invalidate_and_retract_transactionally() {
        let mut connection = database();
        let global_object = b"settings-global";
        let local_object = b"settings-local";
        register_object_key(&mut connection, global_object, 10);
        register_object_key(&mut connection, local_object, 11);

        let global_record = object_record(
            1,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            20,
            b"global-settings",
        );
        let mut global_plugins = BTreeMap::new();
        global_plugins.insert("review@official".to_string(), true);
        global_plugins.insert("local@fixture".to_string(), true);
        let mut global_hooks = BTreeMap::new();
        global_hooks.insert(
            "PreToolUse".to_string(),
            HookEventSummary {
                declared_matcher_count: 1,
                declared_hook_count: 2,
            },
        );
        let global_settings = InterpretationSettingsSnapshot {
            model: Some("sonnet".to_string()),
            effort_level: Some("medium".to_string()),
            always_thinking_enabled: Some(false),
            permission_default_mode: Some("default".to_string()),
            permission_allow: Some(vec!["Read".to_string(), "Bash(test)".to_string()]),
            permission_deny: Some(vec!["Read(.env)".to_string()]),
            enabled_plugins: Some(global_plugins),
            hook_events: Some(global_hooks),
            ..InterpretationSettingsSnapshot::default()
        };
        let mut global_batch = FactBatch::new(2, 1).unwrap();
        global_batch
            .push(
                &global_record,
                Fact::InterpretationSettings(interpretation_settings_fact(
                    InterpretationSettingsLayer::Global,
                    InterpretationSettingsDocumentStatus::Valid,
                    Some(global_settings.clone()),
                    None,
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            global_object,
            &global_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            21,
            &global_batch,
        );
        assert_eq!(
            count(&connection, "canonical_interpretation_settings_documents"),
            1
        );
        assert_eq!(
            count(&connection, "canonical_effective_interpretation_settings"),
            1
        );
        assert_eq!(count(&connection, "canonical_sessions"), 0);
        assert_eq!(count(&connection, "canonical_runs"), 0);
        assert_eq!(
            connection
                .query_row(
                    "SELECT global_document_status || ':' || local_document_status || ':' || resolution_status FROM canonical_effective_interpretation_settings",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "valid:absent:resolved"
        );

        let local_record = object_record(
            2,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            30,
            b"local-settings",
        );
        let mut local_plugins = BTreeMap::new();
        local_plugins.insert("local@fixture".to_string(), false);
        local_plugins.insert("extra@fixture".to_string(), true);
        let mut local_hooks = BTreeMap::new();
        local_hooks.insert(
            "PreToolUse".to_string(),
            HookEventSummary {
                declared_matcher_count: 2,
                declared_hook_count: 3,
            },
        );
        local_hooks.insert(
            "Stop".to_string(),
            HookEventSummary {
                declared_matcher_count: 1,
                declared_hook_count: 1,
            },
        );
        let local_settings = InterpretationSettingsSnapshot {
            model: Some("opus".to_string()),
            always_thinking_enabled: Some(true),
            permission_default_mode: Some("plan".to_string()),
            permission_allow: Some(vec!["Bash(test)".to_string(), "Edit".to_string()]),
            permission_ask: Some(Vec::new()),
            enabled_plugins: Some(local_plugins),
            hook_events: Some(local_hooks),
            ..InterpretationSettingsSnapshot::default()
        };
        let mut local_batch = FactBatch::new(2, 1).unwrap();
        local_batch
            .push(
                &local_record,
                Fact::InterpretationSettings(interpretation_settings_fact(
                    InterpretationSettingsLayer::Local,
                    InterpretationSettingsDocumentStatus::Valid,
                    Some(local_settings),
                    None,
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            local_object,
            &local_record,
            1,
            SourceCursor::append_offset(0).into_bytes(),
            31,
            &local_batch,
        );
        let effective_json: Vec<u8> = connection
            .query_row(
                "SELECT effective_settings_json FROM canonical_effective_interpretation_settings",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let effective: InterpretationSettingsSnapshot =
            serde_json::from_slice(&effective_json).unwrap();
        assert_eq!(effective.model.as_deref(), Some("opus"));
        assert_eq!(effective.effort_level.as_deref(), Some("medium"));
        assert_eq!(effective.always_thinking_enabled, Some(true));
        assert_eq!(effective.permission_default_mode.as_deref(), Some("plan"));
        assert_eq!(
            effective.permission_allow,
            Some(vec![
                "Read".to_string(),
                "Bash(test)".to_string(),
                "Edit".to_string(),
            ])
        );
        assert_eq!(effective.permission_ask, Some(Vec::new()));
        assert_eq!(effective.permission_deny, global_settings.permission_deny);
        assert!(!effective.enabled_plugins.as_ref().unwrap()["local@fixture"]);
        assert!(effective.enabled_plugins.as_ref().unwrap()["review@official"]);
        assert_eq!(
            effective.hook_events.as_ref().unwrap()["PreToolUse"],
            HookEventSummary {
                declared_matcher_count: 3,
                declared_hook_count: 5,
            }
        );

        // A malformed current local document replaces the valid layer and
        // marks effective interpretation unhealthy instead of serving stale
        // local permissions.
        let invalid_record = object_record(
            2,
            1,
            SourceCursor::append_offset(1),
            SourceCursor::append_offset(2),
            40,
            b"invalid-local-settings-secret",
        );
        let mut invalid_batch = FactBatch::new(2, 1).unwrap();
        invalid_batch
            .push(
                &invalid_record,
                Fact::InterpretationSettings(interpretation_settings_fact(
                    InterpretationSettingsLayer::Local,
                    InterpretationSettingsDocumentStatus::Invalid,
                    None,
                    Some("claude_settings_invalid_json"),
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            local_object,
            &invalid_record,
            1,
            SourceCursor::append_offset(1).into_bytes(),
            41,
            &invalid_batch,
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT global_document_status || ':' || local_document_status || ':' || resolution_status FROM canonical_effective_interpretation_settings",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "valid:invalid:invalid"
        );
        let invalid_effective_json: Vec<u8> = connection
            .query_row(
                "SELECT effective_settings_json FROM canonical_effective_interpretation_settings",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let invalid_effective: InterpretationSettingsSnapshot =
            serde_json::from_slice(&invalid_effective_json).unwrap();
        assert_eq!(invalid_effective.model.as_deref(), Some("sonnet"));
        assert_eq!(
            invalid_effective.permission_allow,
            global_settings.permission_allow
        );
        let (audit_payload, audit_codec): (Vec<u8>, String) = connection
            .query_row(
                "SELECT payload_json, payload_codec FROM fact_records WHERE fact_kind = 'interpretation_settings' AND source_object_id = ?1",
                [object_catalog_id(local_object)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let audit_payload = crate::engine::storage_codec::decode(
            &audit_codec,
            &audit_payload,
            1024 * 1024,
            "decode interpretation-settings audit payload",
        )
        .unwrap();
        assert!(!String::from_utf8(audit_payload)
            .unwrap()
            .contains("invalid-local-settings-secret"));

        // Confirmed deletion removes only the local layer and clears health.
        let deleted_record = object_record(
            2,
            1,
            SourceCursor::append_offset(2),
            SourceCursor::append_offset(3),
            50,
            b"local-deleted",
        );
        commit_object_batch(
            &mut connection,
            local_object,
            &deleted_record,
            1,
            SourceCursor::append_offset(2).into_bytes(),
            51,
            &FactBatch::new(1, 1).unwrap(),
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT global_document_status || ':' || local_document_status || ':' || resolution_status FROM canonical_effective_interpretation_settings",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "valid:absent:resolved"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM fact_records WHERE fact_kind = 'interpretation_settings'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );

        let global_deleted_record = object_record(
            1,
            1,
            SourceCursor::append_offset(1),
            SourceCursor::append_offset(2),
            60,
            b"global-deleted",
        );
        commit_object_batch(
            &mut connection,
            global_object,
            &global_deleted_record,
            1,
            SourceCursor::append_offset(1).into_bytes(),
            61,
            &FactBatch::new(1, 1).unwrap(),
        );
        assert_eq!(
            count(&connection, "canonical_effective_interpretation_settings"),
            0
        );
        assert_eq!(
            count(&connection, "canonical_interpretation_settings_documents"),
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT operation FROM change_log WHERE topic = 'configuration.interpretation-settings.changed' ORDER BY commit_seq DESC LIMIT 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "delete"
        );
    }

    #[test]
    fn interpretation_settings_duplicates_agree_or_conflict_deterministically() {
        let mut connection = database();
        let primary_object = b"settings-primary";
        let secondary_object = b"settings-secondary";
        register_object_key(&mut connection, primary_object, 10);
        register_object_key(&mut connection, secondary_object, 11);
        let settings = InterpretationSettingsSnapshot {
            model: Some("sonnet".to_string()),
            permission_allow: Some(vec!["Read".to_string()]),
            ..InterpretationSettingsSnapshot::default()
        };

        for (object_id, object_key, observed_at) in [
            (1, primary_object.as_slice(), 20),
            (2, secondary_object.as_slice(), 30),
        ] {
            let record = object_record(
                object_id,
                1,
                SourceCursor::append_offset(0),
                SourceCursor::append_offset(1),
                observed_at,
                b"same-settings",
            );
            let mut batch = FactBatch::new(2, 1).unwrap();
            batch
                .push(
                    &record,
                    Fact::InterpretationSettings(interpretation_settings_fact(
                        InterpretationSettingsLayer::Global,
                        InterpretationSettingsDocumentStatus::Valid,
                        Some(settings.clone()),
                        None,
                    )),
                )
                .unwrap();
            commit_object_batch(
                &mut connection,
                object_key,
                &record,
                1,
                SourceCursor::append_offset(0).into_bytes(),
                observed_at + 1,
                &batch,
            );
        }
        assert_eq!(
            connection
                .query_row(
                    "SELECT resolution_status || ':' || assertion_count || ':' || competing_settings_count FROM canonical_interpretation_settings_documents",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "resolved:2:0"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT global_document_status || ':' || resolution_status FROM canonical_effective_interpretation_settings",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "valid:resolved"
        );

        let competing_record = object_record(
            2,
            1,
            SourceCursor::append_offset(1),
            SourceCursor::append_offset(2),
            40,
            b"different-settings",
        );
        let mut competing_batch = FactBatch::new(2, 1).unwrap();
        competing_batch
            .push(
                &competing_record,
                Fact::InterpretationSettings(interpretation_settings_fact(
                    InterpretationSettingsLayer::Global,
                    InterpretationSettingsDocumentStatus::Valid,
                    Some(InterpretationSettingsSnapshot {
                        model: Some("opus".to_string()),
                        permission_allow: Some(vec!["Read".to_string()]),
                        ..InterpretationSettingsSnapshot::default()
                    }),
                    None,
                )),
            )
            .unwrap();
        commit_object_batch(
            &mut connection,
            secondary_object,
            &competing_record,
            1,
            SourceCursor::append_offset(1).into_bytes(),
            41,
            &competing_batch,
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT resolution_status || ':' || competing_settings_count FROM canonical_interpretation_settings_documents",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "conflicting:1"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT global_document_status || ':' || resolution_status FROM canonical_effective_interpretation_settings",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "conflicting:conflicting"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT operation FROM change_log WHERE topic = 'diagnostic.configuration.interpretation-settings-conflict' ORDER BY commit_seq DESC LIMIT 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "upsert"
        );

        let deleted_record = object_record(
            2,
            1,
            SourceCursor::append_offset(2),
            SourceCursor::append_offset(3),
            50,
            b"secondary-deleted",
        );
        commit_object_batch(
            &mut connection,
            secondary_object,
            &deleted_record,
            1,
            SourceCursor::append_offset(2).into_bytes(),
            51,
            &FactBatch::new(1, 1).unwrap(),
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT resolution_status || ':' || assertion_count || ':' || competing_settings_count FROM canonical_interpretation_settings_documents",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "resolved:1:0"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT operation FROM change_log WHERE topic = 'diagnostic.configuration.interpretation-settings-conflict' ORDER BY commit_seq DESC LIMIT 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "delete"
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
