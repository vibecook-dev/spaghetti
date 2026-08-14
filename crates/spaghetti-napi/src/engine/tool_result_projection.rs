//! Replaceable persisted tool-result projection and transcript correlation.
//!
//! Sidecar text supplements transcript content; it never creates a message or
//! run. Native tool IDs are indexed from canonical typed content blocks so
//! transcript-first and sidecar-first commits converge without rescanning raw
//! vendor JSON. Duplicate assertions and ambiguous transcript matches remain
//! explicit rather than being resolved by callback order.

use std::collections::BTreeSet;

use rusqlite::{params, Transaction};

use crate::adapter::{ContentBlock, Fact, FactBatch, FactEnvelope, PersistedToolResultFact};

use super::commit::{ChangeEntry, ProjectionCommitContext};
use super::projection::execute_cached;
use super::EngineError;

const CHANGE_SCHEMA_VERSION: u32 = 1;

pub(super) type ToolReferenceKey = (Vec<u8>, String);
type TranscriptMatches = (Vec<Vec<u8>>, Vec<Vec<u8>>);

#[derive(Debug, PartialEq, Eq)]
struct ToolResultReduction {
    resolution_status: Option<String>,
    correlation_status: Option<String>,
    assertion_count: usize,
    competing_result_count: usize,
    tool_call_match_count: usize,
    tool_result_match_count: usize,
    join_conflict: bool,
}

pub(super) fn replaced_message_reference_keys(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
) -> Result<BTreeSet<ToolReferenceKey>, EngineError> {
    let mut statement = transaction
        .prepare_cached(
            r#"
            SELECT DISTINCT session_key, native_tool_use_id
            FROM message_tool_references
            WHERE source_object_id = ?1 AND source_generation <> ?2
            "#,
        )
        .map_err(|error| sqlite_error("prepare replaced message tool references", error))?;
    let keys = statement
        .query_map(
            params![
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
            ],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(|error| sqlite_error("read replaced message tool references", error))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| sqlite_error("collect replaced message tool references", error))?;
    Ok(keys)
}

pub(super) fn replace_message_references(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    message_key: &[u8],
    session_key: &[u8],
    content: &[ContentBlock],
    replaces_existing: bool,
) -> Result<BTreeSet<ToolReferenceKey>, EngineError> {
    let mut affected = if replaces_existing {
        message_reference_keys(transaction, message_key)?
    } else {
        BTreeSet::new()
    };
    if replaces_existing {
        execute_cached(
            transaction,
            "DELETE FROM message_tool_references WHERE message_key = ?1",
            [message_key],
        )
        .map_err(|error| sqlite_error("replace message tool references", error))?;
    }

    for (ordinal, block) in content.iter().enumerate() {
        let (native_tool_use_id, reference_kind) = match block {
            ContentBlock::ToolCall { native_id, .. } => (native_id, "tool_call"),
            ContentBlock::ToolResult { native_call_id, .. } => (native_call_id, "tool_result"),
            _ => continue,
        };
        if native_tool_use_id.trim().is_empty() {
            continue;
        }
        execute_cached(
            transaction,
            r#"
                INSERT INTO message_tool_references (
                    message_key, session_key, native_tool_use_id,
                    reference_kind, block_ordinal, source_object_id,
                    source_generation
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                "#,
            params![
                message_key,
                session_key,
                native_tool_use_id,
                reference_kind,
                sqlite_usize(ordinal, "content block ordinal")?,
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
            ],
        )
        .map_err(|error| sqlite_error("index message tool reference", error))?;
        affected.insert((session_key.to_vec(), native_tool_use_id.clone()));
    }
    Ok(affected)
}

pub(super) fn apply_persisted_tool_result_facts(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    batch: &FactBatch,
    changed_references: &BTreeSet<ToolReferenceKey>,
) -> Result<Vec<ChangeEntry>, EngineError> {
    let object_id = sqlite_u64(context.source_object_id, "source object id")?;
    let has_result_fact = batch
        .facts()
        .iter()
        .any(|envelope| matches!(envelope.value, Fact::PersistedToolResult(_)));
    if context.skip_unowned_replace_document(has_result_fact) && changed_references.is_empty() {
        return Ok(Vec::new());
    }
    let mut affected_results = source_object_result_keys(transaction, object_id)?;
    let owns_result_document = has_result_fact || !affected_results.is_empty();
    affected_results.extend(result_keys_for_references(transaction, changed_references)?);

    // Each immediate .txt file is one complete replace-document object. An
    // empty batch on confirmed absence retracts only that object's assertion.
    if owns_result_document {
        transaction
            .execute(
                "DELETE FROM persisted_tool_result_assertions WHERE source_object_id = ?1",
                [object_id],
            )
            .map_err(|error| sqlite_error("replace persisted tool result assertion", error))?;
    }

    for envelope in batch.facts() {
        let Fact::PersistedToolResult(fact) = &envelope.value else {
            continue;
        };
        write_result(transaction, context, envelope, fact)?;
        affected_results.insert(fact.result.as_bytes().to_vec());
    }

    // A sidecar added in this commit may share a native reference that was
    // already indexed, while a transcript-only commit can affect any extant
    // sidecar for its changed reference keys.
    affected_results.extend(result_keys_for_references(transaction, changed_references)?);

    let mut changes = Vec::new();
    for result_key in affected_results {
        let reduction = reduce_result(transaction, &result_key, context.commit_seq)?;
        changes.push(tool_result_change(&result_key, &reduction)?);
        changes.push(tool_result_conflict_change(&result_key, &reduction)?);
    }

    // The canonical foreign key now points only at a surviving decisive fact.
    // Transcript-only correlation does not own a replaceable sidecar.
    if owns_result_document {
        transaction
            .execute(
                r#"
                DELETE FROM fact_records
                WHERE source_object_id = ?1
                  AND fact_kind = 'persisted_tool_result'
                  AND last_commit_seq <> ?2
                "#,
                params![
                    object_id,
                    sqlite_u64(context.commit_seq, "commit sequence")?,
                ],
            )
            .map_err(|error| sqlite_error("retract replaced persisted tool result facts", error))?;
    }

    Ok(changes)
}

fn message_reference_keys(
    transaction: &Transaction<'_>,
    message_key: &[u8],
) -> Result<BTreeSet<ToolReferenceKey>, EngineError> {
    let mut statement = transaction
        .prepare_cached(
            "SELECT DISTINCT session_key, native_tool_use_id FROM message_tool_references WHERE message_key = ?1",
        )
        .map_err(|error| sqlite_error("prepare prior message tool references", error))?;
    let keys = statement
        .query_map([message_key], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| sqlite_error("read prior message tool references", error))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| sqlite_error("collect prior message tool references", error))?;
    Ok(keys)
}

fn write_result(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    envelope: &FactEnvelope,
    fact: &PersistedToolResultFact,
) -> Result<(), EngineError> {
    let result_digest = digest(fact, "digest persisted tool result")?;
    transaction
        .execute(
            r#"
            INSERT INTO persisted_tool_result_assertions (
                fact_id, result_key, session_key, project_key,
                native_project_key, native_session_id, native_tool_use_id,
                native_document_path, content, size_bytes, result_digest,
                source_object_id, source_generation, cursor_end,
                last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15
            )
            "#,
            params![
                envelope.id.as_bytes().as_slice(),
                fact.result.as_bytes(),
                fact.session.as_bytes(),
                fact.project.as_bytes(),
                fact.native_project_key,
                fact.native_session_id,
                fact.native_tool_use_id,
                fact.native_document_path,
                fact.content,
                sqlite_u64(fact.size_bytes, "persisted tool result size")?,
                result_digest.as_slice(),
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
                envelope.provenance.cursor_end,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("persisted tool result assertion", error))?;
    Ok(())
}

fn reduce_result(
    transaction: &Transaction<'_>,
    result_key: &[u8],
    commit_seq: u64,
) -> Result<ToolResultReduction, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT fact_id, result_digest, session_key, native_tool_use_id
            FROM persisted_tool_result_assertions
            WHERE result_key = ?1
            ORDER BY fact_id
            "#,
        )
        .map_err(|error| sqlite_error("prepare persisted tool result reduction", error))?;
    let assertions = statement
        .query_map([result_key], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|error| sqlite_error("read persisted tool result assertions", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("collect persisted tool result assertions", error))?;
    let Some((decisive_fact_id, _, session_key, native_tool_use_id)) = assertions.first() else {
        transaction
            .execute(
                "DELETE FROM canonical_persisted_tool_results WHERE result_key = ?1",
                [result_key],
            )
            .map_err(|error| sqlite_error("remove absent persisted tool result", error))?;
        return Ok(ToolResultReduction {
            resolution_status: None,
            correlation_status: None,
            assertion_count: 0,
            competing_result_count: 0,
            tool_call_match_count: 0,
            tool_result_match_count: 0,
            join_conflict: false,
        });
    };

    let assertion_count = assertions.len();
    let competing_result_count = assertions
        .iter()
        .map(|(_, digest, _, _)| digest)
        .collect::<BTreeSet<_>>()
        .len()
        .saturating_sub(1);
    let resolution_status = if competing_result_count > 0 {
        "conflicting"
    } else {
        "resolved"
    };
    let (tool_calls, tool_results) =
        transcript_matches(transaction, session_key, native_tool_use_id)?;
    let tool_call_match_count = tool_calls.len();
    let tool_result_match_count = tool_results.len();
    let join_conflict = tool_call_match_count > 1 || tool_result_match_count > 1;
    let correlation_status = if join_conflict {
        "ambiguous"
    } else {
        match (tool_call_match_count, tool_result_match_count) {
            (0, 0) => "unlinked",
            (1, 0) => "call_only",
            (0, 1) => "result_only",
            (1, 1) => "linked",
            _ => unreachable!("join conflict covers counts above one"),
        }
    };
    let tool_call_message_key = (tool_call_match_count == 1).then(|| tool_calls[0].as_slice());
    let tool_result_message_key =
        (tool_result_match_count == 1).then(|| tool_results[0].as_slice());

    transaction
        .execute(
            r#"
            INSERT INTO canonical_persisted_tool_results (
                result_key, session_key, project_key, native_project_key,
                native_session_id, native_tool_use_id, native_document_path,
                content, size_bytes, resolution_status, correlation_status,
                tool_call_message_key, tool_result_message_key,
                decisive_fact_id, assertion_count, competing_result_count,
                tool_call_match_count, tool_result_match_count, join_conflict,
                last_commit_seq
            )
            SELECT result_key, session_key, project_key, native_project_key,
                   native_session_id, native_tool_use_id, native_document_path,
                   content, size_bytes, ?2, ?3, ?4, ?5, fact_id, ?6, ?7,
                   ?8, ?9, ?10, ?11
            FROM persisted_tool_result_assertions WHERE fact_id = ?1
            ON CONFLICT(result_key) DO UPDATE SET
                session_key = excluded.session_key,
                project_key = excluded.project_key,
                native_project_key = excluded.native_project_key,
                native_session_id = excluded.native_session_id,
                native_tool_use_id = excluded.native_tool_use_id,
                native_document_path = excluded.native_document_path,
                content = excluded.content,
                size_bytes = excluded.size_bytes,
                resolution_status = excluded.resolution_status,
                correlation_status = excluded.correlation_status,
                tool_call_message_key = excluded.tool_call_message_key,
                tool_result_message_key = excluded.tool_result_message_key,
                decisive_fact_id = excluded.decisive_fact_id,
                assertion_count = excluded.assertion_count,
                competing_result_count = excluded.competing_result_count,
                tool_call_match_count = excluded.tool_call_match_count,
                tool_result_match_count = excluded.tool_result_match_count,
                join_conflict = excluded.join_conflict,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                decisive_fact_id,
                resolution_status,
                correlation_status,
                tool_call_message_key,
                tool_result_message_key,
                sqlite_usize(assertion_count, "persisted tool result assertion count")?,
                sqlite_usize(
                    competing_result_count,
                    "persisted tool result competing assertion count",
                )?,
                sqlite_usize(tool_call_match_count, "tool call match count")?,
                sqlite_usize(tool_result_match_count, "tool result match count")?,
                i64::from(join_conflict),
                sqlite_u64(commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("write canonical persisted tool result", error))?;

    Ok(ToolResultReduction {
        resolution_status: Some(resolution_status.to_string()),
        correlation_status: Some(correlation_status.to_string()),
        assertion_count,
        competing_result_count,
        tool_call_match_count,
        tool_result_match_count,
        join_conflict,
    })
}

fn transcript_matches(
    transaction: &Transaction<'_>,
    session_key: &[u8],
    native_tool_use_id: &str,
) -> Result<TranscriptMatches, EngineError> {
    fn kind_matches(
        transaction: &Transaction<'_>,
        session_key: &[u8],
        native_tool_use_id: &str,
        reference_kind: &str,
    ) -> Result<Vec<Vec<u8>>, EngineError> {
        let mut statement = transaction
            .prepare(
                r#"
                SELECT message_key
                FROM message_tool_references
                WHERE session_key = ?1 AND native_tool_use_id = ?2
                  AND reference_kind = ?3
                ORDER BY message_key, block_ordinal
                "#,
            )
            .map_err(|error| sqlite_error("prepare transcript tool matches", error))?;
        let keys = statement
            .query_map(
                params![session_key, native_tool_use_id, reference_kind],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .map_err(|error| sqlite_error("read transcript tool matches", error))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| sqlite_error("collect transcript tool matches", error))?;
        Ok(keys)
    }

    Ok((
        kind_matches(transaction, session_key, native_tool_use_id, "tool_call")?,
        kind_matches(transaction, session_key, native_tool_use_id, "tool_result")?,
    ))
}

fn source_object_result_keys(
    transaction: &Transaction<'_>,
    source_object_id: i64,
) -> Result<BTreeSet<Vec<u8>>, EngineError> {
    let mut statement = transaction
        .prepare(
            "SELECT DISTINCT result_key FROM persisted_tool_result_assertions WHERE source_object_id = ?1",
        )
        .map_err(|error| sqlite_error("prepare replaced persisted tool results", error))?;
    let keys = statement
        .query_map([source_object_id], |row| row.get::<_, Vec<u8>>(0))
        .map_err(|error| sqlite_error("read replaced persisted tool results", error))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| sqlite_error("collect replaced persisted tool results", error))?;
    Ok(keys)
}

fn result_keys_for_references(
    transaction: &Transaction<'_>,
    references: &BTreeSet<ToolReferenceKey>,
) -> Result<BTreeSet<Vec<u8>>, EngineError> {
    let mut keys = BTreeSet::new();
    let mut statement = transaction
        .prepare(
            r#"
            SELECT DISTINCT result_key
            FROM persisted_tool_result_assertions
            WHERE session_key = ?1 AND native_tool_use_id = ?2
            "#,
        )
        .map_err(|error| sqlite_error("prepare sidecars for tool references", error))?;
    for (session_key, native_tool_use_id) in references {
        keys.extend(
            statement
                .query_map(params![session_key, native_tool_use_id], |row| {
                    row.get::<_, Vec<u8>>(0)
                })
                .map_err(|error| sqlite_error("read sidecars for tool references", error))?
                .collect::<Result<BTreeSet<_>, _>>()
                .map_err(|error| sqlite_error("collect sidecars for tool references", error))?,
        );
    }
    Ok(keys)
}

fn tool_result_change(
    result_key: &[u8],
    reduction: &ToolResultReduction,
) -> Result<ChangeEntry, EngineError> {
    change(
        "history.persisted-tool-result.changed",
        result_key,
        reduction.resolution_status.is_some(),
        &serde_json::json!({
            "resolution_status": reduction.resolution_status,
            "correlation_status": reduction.correlation_status,
            "assertion_count": reduction.assertion_count,
            "competing_result_count": reduction.competing_result_count,
            "tool_call_match_count": reduction.tool_call_match_count,
            "tool_result_match_count": reduction.tool_result_match_count,
        }),
        "serialize persisted tool result change",
    )
}

fn tool_result_conflict_change(
    result_key: &[u8],
    reduction: &ToolResultReduction,
) -> Result<ChangeEntry, EngineError> {
    let conflicting = reduction.competing_result_count > 0 || reduction.join_conflict;
    change(
        "diagnostic.history.persisted-tool-result-conflict",
        result_key,
        conflicting,
        &serde_json::json!({
            "conflicting": conflicting,
            "competing_result_count": reduction.competing_result_count,
            "join_conflict": reduction.join_conflict,
            "tool_call_match_count": reduction.tool_call_match_count,
            "tool_result_match_count": reduction.tool_result_match_count,
        }),
        "serialize persisted tool result conflict change",
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
