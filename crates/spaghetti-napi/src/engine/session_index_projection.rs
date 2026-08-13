//! Replaceable Claude project session-index projection.
//!
//! A native index is useful metadata, but it is not transcript evidence. Index
//! entries therefore remain queryable when their JSONL is absent and join to
//! canonical transcript sessions only when an independently committed
//! `SessionFact` exists. Complete project snapshots compete deterministically;
//! child metadata and cross-project identity disagreement remain explicit.

use std::collections::BTreeSet;

use rusqlite::{params, OptionalExtension, Transaction};

use crate::adapter::{
    Fact, FactBatch, FactEnvelope, QualifiedTimestamp, SessionIndexEntrySnapshot,
    SessionIndexSnapshotFact, TimestampQuality,
};

use super::commit::{ChangeEntry, ProjectionCommitContext};
use super::EngineError;

const CHANGE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, PartialEq, Eq)]
struct ProjectReduction {
    status: Option<String>,
    assertion_count: usize,
    competing_snapshot_count: usize,
    entry_count: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct EntryReduction {
    resolution_status: Option<String>,
    transcript_status: Option<String>,
    assertion_count: usize,
    competing_entry_count: usize,
    identity_conflict: bool,
    join_conflict: bool,
}

pub(super) fn apply_session_index_facts(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    batch: &FactBatch,
    changed_session_keys: &BTreeSet<Vec<u8>>,
) -> Result<Vec<ChangeEntry>, EngineError> {
    let object_id = sqlite_u64(context.source_object_id, "source object id")?;
    let has_snapshot_fact = batch
        .facts()
        .iter()
        .any(|envelope| matches!(envelope.value, Fact::SessionIndexSnapshot(_)));
    let mut affected_projects = source_object_keys(
        transaction,
        "SELECT DISTINCT project_key FROM session_index_snapshot_assertions WHERE source_object_id = ?1",
        object_id,
        "read replaced session indexes",
    )?;
    let mut affected_sessions = source_object_keys(
        transaction,
        r#"
        SELECT DISTINCT entry.session_key
        FROM session_index_entry_assertions AS entry
        JOIN session_index_snapshot_assertions AS snapshot ON snapshot.fact_id = entry.fact_id
        WHERE snapshot.source_object_id = ?1
        "#,
        object_id,
        "read replaced session index entries",
    )?;
    let owns_snapshot =
        has_snapshot_fact || !affected_projects.is_empty() || !affected_sessions.is_empty();
    affected_sessions.extend(indexed_candidates(transaction, changed_session_keys)?);
    affected_sessions.extend(children_for_projects(
        transaction,
        "SELECT session_key FROM canonical_session_index_entries WHERE project_key = ?1",
        &affected_projects,
        "read prior canonical session index entries",
    )?);

    // A transcript commit may still change a session-index join, but it does
    // not own the index document. Only replace assertions when this object
    // supplied (or previously supplied) the replace-document snapshot.
    if owns_snapshot {
        transaction
            .execute(
                "DELETE FROM session_index_snapshot_assertions WHERE source_object_id = ?1",
                [object_id],
            )
            .map_err(|error| sqlite_error("replace session index snapshot", error))?;
    }

    for envelope in batch.facts() {
        let Fact::SessionIndexSnapshot(fact) = &envelope.value else {
            continue;
        };
        write_snapshot(transaction, context, envelope, fact)?;
        affected_projects.insert(fact.project.as_bytes().to_vec());
        affected_sessions.extend(
            fact.entries
                .iter()
                .map(|entry| entry.session.as_bytes().to_vec()),
        );
    }

    // Transcript-only commits can change an entry's join without changing the
    // project snapshot. `changed_session_keys` already queues those entries;
    // do not publish an unrelated project-index change for every transcript
    // record.
    affected_sessions.extend(children_for_projects(
        transaction,
        "SELECT session_key FROM session_index_entry_assertions WHERE project_key = ?1",
        &affected_projects,
        "read current session index entries",
    )?);

    let mut changes = Vec::new();
    for project_key in &affected_projects {
        let reduction = reduce_project(transaction, project_key, context.commit_seq)?;
        changes.push(project_change(project_key, &reduction)?);
        changes.push(project_conflict_change(project_key, &reduction)?);
    }
    for session_key in affected_sessions {
        let reduction = reduce_entry(transaction, &session_key, context.commit_seq)?;
        changes.push(entry_change(&session_key, &reduction)?);
        changes.push(entry_conflict_change(&session_key, &reduction)?);
    }

    // Reducers now reference only surviving decisive assertions. Avoid this
    // source-object fact scan for transcript-only join maintenance.
    if owns_snapshot {
        transaction
            .execute(
                r#"
                DELETE FROM fact_records
                WHERE source_object_id = ?1
                  AND fact_kind = 'session_index_snapshot'
                  AND last_commit_seq <> ?2
                "#,
                params![
                    object_id,
                    sqlite_u64(context.commit_seq, "commit sequence")?,
                ],
            )
            .map_err(|error| sqlite_error("retract replaced session index facts", error))?;
    }

    Ok(changes)
}

fn write_snapshot(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    envelope: &FactEnvelope,
    fact: &SessionIndexSnapshotFact,
) -> Result<(), EngineError> {
    let native_snapshot_json = serialize(&fact.native_snapshot, "serialize native session index")?;
    let snapshot_digest = digest(
        &(
            &fact.project,
            &fact.native_project_key,
            fact.native_version,
            &fact.original_path,
            &fact.entries,
        ),
        "digest session index snapshot",
    )?;
    transaction
        .execute(
            r#"
            INSERT INTO session_index_snapshot_assertions (
                fact_id, project_key, native_project_key, native_version,
                original_path, native_snapshot_json, snapshot_digest,
                source_object_id, source_generation, cursor_end,
                last_commit_seq
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            "#,
            params![
                envelope.id.as_bytes().as_slice(),
                fact.project.as_bytes(),
                fact.native_project_key,
                sqlite_u64(fact.native_version, "session index version")?,
                fact.original_path,
                native_snapshot_json,
                snapshot_digest.as_slice(),
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
                envelope.provenance.cursor_end,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("project session index snapshot assertion", error))?;

    let mut session_keys = BTreeSet::new();
    for (ordinal, entry) in fact.entries.iter().enumerate() {
        if !session_keys.insert(entry.session.as_bytes().to_vec()) {
            return Err(EngineError::InvalidCommit(
                "session index snapshot contains duplicate session keys".to_string(),
            ));
        }
        write_entry(transaction, envelope, fact, entry, ordinal)?;
    }
    Ok(())
}

fn write_entry(
    transaction: &Transaction<'_>,
    envelope: &FactEnvelope,
    snapshot: &SessionIndexSnapshotFact,
    entry: &SessionIndexEntrySnapshot,
    ordinal: usize,
) -> Result<(), EngineError> {
    let entry_digest = digest(
        &(snapshot.project.as_bytes(), entry),
        "digest session index entry",
    )?;
    transaction
        .execute(
            r#"
            INSERT INTO session_index_entry_assertions (
                fact_id, session_key, project_key, entry_ordinal,
                native_session_id, full_path, file_mtime_ms, first_prompt,
                summary, message_count, created_at, created_at_quality,
                modified_at, modified_at_quality, git_branch, project_path,
                is_sidechain, entry_digest
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17, ?18
            )
            "#,
            params![
                envelope.id.as_bytes().as_slice(),
                entry.session.as_bytes(),
                snapshot.project.as_bytes(),
                sqlite_usize(ordinal, "session index entry ordinal")?,
                entry.native_session_id,
                entry.full_path,
                sqlite_u64(entry.file_mtime_ms, "session index file mtime")?,
                entry.first_prompt,
                entry.summary,
                sqlite_u64(entry.message_count, "session index message count")?,
                entry.created_at.value,
                timestamp_quality(&entry.created_at),
                entry.modified_at.value,
                timestamp_quality(&entry.modified_at),
                entry.git_branch,
                entry.project_path,
                i64::from(entry.is_sidechain),
                entry_digest.as_slice(),
            ],
        )
        .map_err(|error| sqlite_error("project session index entry assertion", error))?;
    Ok(())
}

fn reduce_project(
    transaction: &Transaction<'_>,
    project_key: &[u8],
    commit_seq: u64,
) -> Result<ProjectReduction, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT fact_id, snapshot_digest
            FROM session_index_snapshot_assertions
            WHERE project_key = ?1
            ORDER BY fact_id
            "#,
        )
        .map_err(|error| sqlite_error("prepare session index reduction", error))?;
    let assertions = statement
        .query_map([project_key], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .map_err(|error| sqlite_error("read session index assertions", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("collect session index assertions", error))?;
    let Some((decisive_fact_id, _)) = assertions.first() else {
        transaction
            .execute(
                "DELETE FROM canonical_session_indexes WHERE project_key = ?1",
                [project_key],
            )
            .map_err(|error| sqlite_error("remove absent canonical session index", error))?;
        return Ok(ProjectReduction {
            status: None,
            assertion_count: 0,
            competing_snapshot_count: 0,
            entry_count: 0,
        });
    };
    let assertion_count = assertions.len();
    let competing_snapshot_count = assertions
        .iter()
        .map(|(_, digest)| digest)
        .collect::<BTreeSet<_>>()
        .len()
        .saturating_sub(1);
    let status = if competing_snapshot_count > 0 {
        "conflicting"
    } else {
        "resolved"
    };
    let entry_count = transaction
        .query_row(
            "SELECT COUNT(*) FROM session_index_entry_assertions WHERE fact_id = ?1",
            [decisive_fact_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| sqlite_error("count decisive session index entries", error))?;
    transaction
        .execute(
            r#"
            INSERT INTO canonical_session_indexes (
                project_key, native_project_key, native_version,
                original_path, native_snapshot_json, index_status,
                decisive_fact_id, assertion_count, competing_snapshot_count,
                entry_count, last_commit_seq
            )
            SELECT project_key, native_project_key, native_version,
                   original_path, native_snapshot_json, ?2, fact_id, ?3, ?4,
                   ?5, ?6
            FROM session_index_snapshot_assertions WHERE fact_id = ?1
            ON CONFLICT(project_key) DO UPDATE SET
                native_project_key = excluded.native_project_key,
                native_version = excluded.native_version,
                original_path = excluded.original_path,
                native_snapshot_json = excluded.native_snapshot_json,
                index_status = excluded.index_status,
                decisive_fact_id = excluded.decisive_fact_id,
                assertion_count = excluded.assertion_count,
                competing_snapshot_count = excluded.competing_snapshot_count,
                entry_count = excluded.entry_count,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                decisive_fact_id,
                status,
                sqlite_usize(assertion_count, "session index assertion count")?,
                sqlite_usize(
                    competing_snapshot_count,
                    "session index competing snapshot count",
                )?,
                entry_count,
                sqlite_u64(commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("write canonical session index", error))?;

    Ok(ProjectReduction {
        status: Some(status.to_string()),
        assertion_count,
        competing_snapshot_count,
        entry_count: sqlite_i64_usize(entry_count, "session index entry count")?,
    })
}

fn reduce_entry(
    transaction: &Transaction<'_>,
    session_key: &[u8],
    commit_seq: u64,
) -> Result<EntryReduction, EngineError> {
    let mut decisive_statement = transaction
        .prepare(
            r#"
            SELECT entry.fact_id, entry.project_key
            FROM session_index_entry_assertions AS entry
            JOIN canonical_session_indexes AS project
              ON project.decisive_fact_id = entry.fact_id
            WHERE entry.session_key = ?1
            ORDER BY entry.fact_id
            "#,
        )
        .map_err(|error| sqlite_error("prepare session index entry reduction", error))?;
    let decisive = decisive_statement
        .query_map([session_key], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .map_err(|error| sqlite_error("read decisive session index entry", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("collect decisive session index entry", error))?;
    let Some((decisive_fact_id, decisive_project_key)) = decisive.first() else {
        transaction
            .execute(
                "DELETE FROM canonical_session_index_entries WHERE session_key = ?1",
                [session_key],
            )
            .map_err(|error| sqlite_error("remove absent session index entry", error))?;
        return Ok(EntryReduction {
            resolution_status: None,
            transcript_status: None,
            assertion_count: 0,
            competing_entry_count: 0,
            identity_conflict: false,
            join_conflict: false,
        });
    };
    let identity_conflict = decisive
        .iter()
        .map(|(_, project_key)| project_key)
        .collect::<BTreeSet<_>>()
        .len()
        > 1;
    let mut statement = transaction
        .prepare(
            r#"
            SELECT entry_digest
            FROM session_index_entry_assertions
            WHERE session_key = ?1 AND project_key = ?2
            ORDER BY fact_id
            "#,
        )
        .map_err(|error| sqlite_error("prepare competing session index entries", error))?;
    let assertions = statement
        .query_map(params![session_key, decisive_project_key], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .map_err(|error| sqlite_error("read competing session index entries", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("collect competing session index entries", error))?;
    let assertion_count = assertions.len();
    let project_snapshot_count = transaction
        .query_row(
            "SELECT COUNT(*) FROM session_index_snapshot_assertions WHERE project_key = ?1",
            [decisive_project_key],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| sqlite_error("count session index project assertions", error))?;
    let project_snapshot_count = sqlite_i64_usize(
        project_snapshot_count,
        "session index project assertion count",
    )?;
    let competing_entry_count = assertions
        .iter()
        .collect::<BTreeSet<_>>()
        .len()
        .saturating_sub(1)
        .saturating_add(usize::from(assertion_count < project_snapshot_count));
    let transcript_project = transaction
        .query_row(
            "SELECT project_key FROM canonical_sessions WHERE session_key = ?1",
            [session_key],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .map_err(|error| sqlite_error("join session index entry to transcript", error))?;
    let (transcript_status, join_conflict) = match transcript_project {
        Some(project_key) if project_key != *decisive_project_key => ("different_project", true),
        Some(_) => ("present", false),
        None => ("missing", false),
    };
    let resolution_status = if competing_entry_count > 0 || identity_conflict || join_conflict {
        "conflicting"
    } else {
        "resolved"
    };
    transaction
        .execute(
            r#"
            INSERT INTO canonical_session_index_entries (
                session_key, project_key, entry_ordinal, native_session_id,
                full_path, file_mtime_ms, first_prompt, summary,
                message_count, created_at, created_at_quality, modified_at,
                modified_at_quality, git_branch, project_path, is_sidechain,
                transcript_status, resolution_status, decisive_fact_id,
                assertion_count, competing_entry_count, identity_conflict,
                join_conflict, last_commit_seq
            )
            SELECT session_key, project_key, entry_ordinal, native_session_id,
                   full_path, file_mtime_ms, first_prompt, summary,
                   message_count, created_at, created_at_quality, modified_at,
                   modified_at_quality, git_branch, project_path, is_sidechain,
                   ?2, ?3, fact_id, ?4, ?5, ?6, ?7, ?8
            FROM session_index_entry_assertions
            WHERE fact_id = ?1 AND session_key = ?9
            ON CONFLICT(session_key) DO UPDATE SET
                project_key = excluded.project_key,
                entry_ordinal = excluded.entry_ordinal,
                native_session_id = excluded.native_session_id,
                full_path = excluded.full_path,
                file_mtime_ms = excluded.file_mtime_ms,
                first_prompt = excluded.first_prompt,
                summary = excluded.summary,
                message_count = excluded.message_count,
                created_at = excluded.created_at,
                created_at_quality = excluded.created_at_quality,
                modified_at = excluded.modified_at,
                modified_at_quality = excluded.modified_at_quality,
                git_branch = excluded.git_branch,
                project_path = excluded.project_path,
                is_sidechain = excluded.is_sidechain,
                transcript_status = excluded.transcript_status,
                resolution_status = excluded.resolution_status,
                decisive_fact_id = excluded.decisive_fact_id,
                assertion_count = excluded.assertion_count,
                competing_entry_count = excluded.competing_entry_count,
                identity_conflict = excluded.identity_conflict,
                join_conflict = excluded.join_conflict,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                decisive_fact_id,
                transcript_status,
                resolution_status,
                sqlite_usize(assertion_count, "session index entry assertion count")?,
                sqlite_usize(competing_entry_count, "session index competing entry count",)?,
                i64::from(identity_conflict),
                i64::from(join_conflict),
                sqlite_u64(commit_seq, "commit sequence")?,
                session_key,
            ],
        )
        .map_err(|error| sqlite_error("write canonical session index entry", error))?;

    Ok(EntryReduction {
        resolution_status: Some(resolution_status.to_string()),
        transcript_status: Some(transcript_status.to_string()),
        assertion_count,
        competing_entry_count,
        identity_conflict,
        join_conflict,
    })
}

fn project_change(
    project_key: &[u8],
    reduction: &ProjectReduction,
) -> Result<ChangeEntry, EngineError> {
    change(
        "history.session-index.changed",
        project_key,
        reduction.status.is_some(),
        &serde_json::json!({
            "status": reduction.status,
            "assertion_count": reduction.assertion_count,
            "competing_snapshot_count": reduction.competing_snapshot_count,
            "entry_count": reduction.entry_count,
        }),
        "serialize session index change",
    )
}

fn project_conflict_change(
    project_key: &[u8],
    reduction: &ProjectReduction,
) -> Result<ChangeEntry, EngineError> {
    let conflicting = reduction.competing_snapshot_count > 0;
    change(
        "diagnostic.history.session-index-conflict",
        project_key,
        conflicting,
        &serde_json::json!({
            "conflicting": conflicting,
            "competing_snapshot_count": reduction.competing_snapshot_count,
        }),
        "serialize session index conflict change",
    )
}

fn entry_change(
    session_key: &[u8],
    reduction: &EntryReduction,
) -> Result<ChangeEntry, EngineError> {
    change(
        "history.session-index-entry.changed",
        session_key,
        reduction.resolution_status.is_some(),
        &serde_json::json!({
            "resolution_status": reduction.resolution_status,
            "transcript_status": reduction.transcript_status,
            "assertion_count": reduction.assertion_count,
            "competing_entry_count": reduction.competing_entry_count,
            "identity_conflict": reduction.identity_conflict,
            "join_conflict": reduction.join_conflict,
        }),
        "serialize session index entry change",
    )
}

fn entry_conflict_change(
    session_key: &[u8],
    reduction: &EntryReduction,
) -> Result<ChangeEntry, EngineError> {
    let conflicting = reduction.competing_entry_count > 0
        || reduction.identity_conflict
        || reduction.join_conflict;
    change(
        "diagnostic.history.session-index-entry-conflict",
        session_key,
        conflicting,
        &serde_json::json!({
            "conflicting": conflicting,
            "competing_entry_count": reduction.competing_entry_count,
            "identity_conflict": reduction.identity_conflict,
            "join_conflict": reduction.join_conflict,
        }),
        "serialize session index entry conflict change",
    )
}

fn change<T: serde::Serialize>(
    topic: &str,
    entity_key: &[u8],
    present: bool,
    payload: &T,
    operation: &'static str,
) -> Result<ChangeEntry, EngineError> {
    Ok(ChangeEntry {
        topic: topic.to_string(),
        schema_version: CHANGE_SCHEMA_VERSION,
        entity_key: entity_key.to_vec(),
        operation: if present { "upsert" } else { "delete" }.to_string(),
        payload: serialize(payload, operation)?,
    })
}

fn source_object_keys(
    transaction: &Transaction<'_>,
    sql: &str,
    object_id: i64,
    operation: &'static str,
) -> Result<BTreeSet<Vec<u8>>, EngineError> {
    let mut statement = transaction
        .prepare(sql)
        .map_err(|error| sqlite_error(operation, error))?;
    let keys = statement
        .query_map([object_id], |row| row.get::<_, Vec<u8>>(0))
        .map_err(|error| sqlite_error(operation, error))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| sqlite_error(operation, error))?;
    Ok(keys)
}

fn children_for_projects(
    transaction: &Transaction<'_>,
    sql: &str,
    project_keys: &BTreeSet<Vec<u8>>,
    operation: &'static str,
) -> Result<BTreeSet<Vec<u8>>, EngineError> {
    let mut children = BTreeSet::new();
    let mut statement = transaction
        .prepare(sql)
        .map_err(|error| sqlite_error(operation, error))?;
    for project_key in project_keys {
        children.extend(
            statement
                .query_map([project_key], |row| row.get::<_, Vec<u8>>(0))
                .map_err(|error| sqlite_error(operation, error))?
                .collect::<Result<BTreeSet<_>, _>>()
                .map_err(|error| sqlite_error(operation, error))?,
        );
    }
    Ok(children)
}

fn indexed_candidates(
    transaction: &Transaction<'_>,
    candidates: &BTreeSet<Vec<u8>>,
) -> Result<BTreeSet<Vec<u8>>, EngineError> {
    let mut indexed = BTreeSet::new();
    let mut statement = transaction
        .prepare(
            r#"
            SELECT 1
            FROM session_index_entry_assertions
            WHERE session_key = ?1
            LIMIT 1
            "#,
        )
        .map_err(|error| sqlite_error("prepare indexed session candidates", error))?;
    for session_key in candidates {
        if statement
            .query_row([session_key], |_| Ok(()))
            .optional()
            .map_err(|error| sqlite_error("read indexed session candidate", error))?
            .is_some()
        {
            indexed.insert(session_key.clone());
        }
    }
    Ok(indexed)
}

fn digest<T: serde::Serialize>(
    value: &T,
    operation: &'static str,
) -> Result<[u8; 32], EngineError> {
    serialize(value, operation).map(|encoded| *blake3::hash(&encoded).as_bytes())
}

fn serialize<T: serde::Serialize>(
    value: &T,
    operation: &'static str,
) -> Result<Vec<u8>, EngineError> {
    serde_json::to_vec(value)
        .map_err(|error| EngineError::InvalidCommit(format!("{operation}: {error}")))
}

fn timestamp_quality(timestamp: &QualifiedTimestamp) -> &'static str {
    match timestamp.quality {
        TimestampQuality::NativeExact => "native_exact",
        TimestampQuality::NativeApproximate => "native_approximate",
        TimestampQuality::FileMetadataFallback => "file_metadata_fallback",
        TimestampQuality::Derived => "derived",
    }
}

fn sqlite_i64_usize(value: i64, field: &'static str) -> Result<usize, EngineError> {
    usize::try_from(value)
        .map_err(|_| EngineError::InvalidCommit(format!("{field} is outside the usize range")))
}

fn sqlite_usize(value: usize, field: &'static str) -> Result<i64, EngineError> {
    i64::try_from(value)
        .map_err(|_| EngineError::InvalidCommit(format!("{field} exceeds SQLite integer range")))
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
