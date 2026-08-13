//! Replaceable project-memory document projection.
//!
//! Memory documents are project context, not transcript or runtime evidence.
//! Each native Markdown file is independently replaceable. Duplicate source
//! objects asserting the same stable project/path identity are retained and
//! reduced deterministically instead of overwriting by callback order.

use std::collections::BTreeSet;

use rusqlite::{params, Transaction};

use crate::adapter::{Fact, FactBatch, FactEnvelope, ProjectMemoryDocumentFact};

use super::commit::{ChangeEntry, ProjectionCommitContext};
use super::EngineError;

const CHANGE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, PartialEq, Eq)]
struct MemoryReduction {
    resolution_status: Option<String>,
    assertion_count: usize,
    competing_document_count: usize,
}

pub(super) fn apply_project_memory_facts(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    batch: &FactBatch,
) -> Result<Vec<ChangeEntry>, EngineError> {
    let object_id = sqlite_u64(context.source_object_id, "source object id")?;
    let mut affected_documents = source_object_keys(transaction, object_id)?;
    let has_memory_fact = batch
        .facts()
        .iter()
        .any(|envelope| matches!(envelope.value, Fact::ProjectMemoryDocument(_)));

    if !has_memory_fact && affected_documents.is_empty() {
        return Ok(Vec::new());
    }

    // Every memory file is one complete replace-document object. The empty
    // batch used for confirmed deletion retracts only that file's assertion.
    transaction
        .execute(
            "DELETE FROM project_memory_document_assertions WHERE source_object_id = ?1",
            [object_id],
        )
        .map_err(|error| sqlite_error("replace project memory assertion", error))?;

    for envelope in batch.facts() {
        let Fact::ProjectMemoryDocument(fact) = &envelope.value else {
            continue;
        };
        write_document(transaction, context, envelope, fact)?;
        affected_documents.insert(fact.document.as_bytes().to_vec());
    }

    let mut changes = Vec::new();
    for document_key in affected_documents {
        let reduction = reduce_document(transaction, &document_key, context.commit_seq)?;
        changes.push(memory_change(&document_key, &reduction)?);
        changes.push(memory_conflict_change(&document_key, &reduction)?);
    }

    transaction
        .execute(
            r#"
            DELETE FROM fact_records
            WHERE source_object_id = ?1
              AND fact_kind = 'project_memory_document'
              AND last_commit_seq <> ?2
            "#,
            params![
                object_id,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("retract replaced project memory facts", error))?;

    Ok(changes)
}

fn write_document(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    envelope: &FactEnvelope,
    fact: &ProjectMemoryDocumentFact,
) -> Result<(), EngineError> {
    let document_digest = digest(fact, "digest project memory document")?;
    transaction
        .execute(
            r#"
            INSERT INTO project_memory_document_assertions (
                fact_id, document_key, project_key, native_project_key,
                native_document_path, title, content, size_bytes, is_index,
                document_digest, source_object_id, source_generation,
                cursor_end, last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14
            )
            "#,
            params![
                envelope.id.as_bytes().as_slice(),
                fact.document.as_bytes(),
                fact.project.as_bytes(),
                fact.native_project_key,
                fact.native_document_path,
                fact.title,
                fact.content,
                sqlite_u64(fact.size_bytes, "project memory size")?,
                i64::from(fact.is_index),
                document_digest.as_slice(),
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
                envelope.provenance.cursor_end,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("project memory document assertion", error))?;
    Ok(())
}

fn reduce_document(
    transaction: &Transaction<'_>,
    document_key: &[u8],
    commit_seq: u64,
) -> Result<MemoryReduction, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT fact_id, document_digest
            FROM project_memory_document_assertions
            WHERE document_key = ?1
            ORDER BY fact_id
            "#,
        )
        .map_err(|error| sqlite_error("prepare project memory reduction", error))?;
    let assertions = statement
        .query_map([document_key], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .map_err(|error| sqlite_error("read project memory assertions", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("collect project memory assertions", error))?;
    let Some((decisive_fact_id, _)) = assertions.first() else {
        transaction
            .execute(
                "DELETE FROM canonical_project_memory_documents WHERE document_key = ?1",
                [document_key],
            )
            .map_err(|error| sqlite_error("remove absent project memory document", error))?;
        return Ok(MemoryReduction {
            resolution_status: None,
            assertion_count: 0,
            competing_document_count: 0,
        });
    };
    let assertion_count = assertions.len();
    let competing_document_count = assertions
        .iter()
        .map(|(_, digest)| digest)
        .collect::<BTreeSet<_>>()
        .len()
        .saturating_sub(1);
    let resolution_status = if competing_document_count > 0 {
        "conflicting"
    } else {
        "resolved"
    };

    transaction
        .execute(
            r#"
            INSERT INTO canonical_project_memory_documents (
                document_key, project_key, native_project_key,
                native_document_path, title, content, size_bytes, is_index,
                resolution_status, decisive_fact_id, assertion_count,
                competing_document_count, last_commit_seq
            )
            SELECT document_key, project_key, native_project_key,
                   native_document_path, title, content, size_bytes, is_index,
                   ?2, fact_id, ?3, ?4, ?5
            FROM project_memory_document_assertions WHERE fact_id = ?1
            ON CONFLICT(document_key) DO UPDATE SET
                project_key = excluded.project_key,
                native_project_key = excluded.native_project_key,
                native_document_path = excluded.native_document_path,
                title = excluded.title,
                content = excluded.content,
                size_bytes = excluded.size_bytes,
                is_index = excluded.is_index,
                resolution_status = excluded.resolution_status,
                decisive_fact_id = excluded.decisive_fact_id,
                assertion_count = excluded.assertion_count,
                competing_document_count = excluded.competing_document_count,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                decisive_fact_id,
                resolution_status,
                sqlite_usize(assertion_count, "project memory assertion count")?,
                sqlite_usize(
                    competing_document_count,
                    "project memory competing assertion count",
                )?,
                sqlite_u64(commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("write canonical project memory document", error))?;

    Ok(MemoryReduction {
        resolution_status: Some(resolution_status.to_string()),
        assertion_count,
        competing_document_count,
    })
}

fn source_object_keys(
    transaction: &Transaction<'_>,
    source_object_id: i64,
) -> Result<BTreeSet<Vec<u8>>, EngineError> {
    let mut statement = transaction
        .prepare(
            "SELECT DISTINCT document_key FROM project_memory_document_assertions WHERE source_object_id = ?1",
        )
        .map_err(|error| sqlite_error("prepare replaced project memory documents", error))?;
    let keys = statement
        .query_map([source_object_id], |row| row.get::<_, Vec<u8>>(0))
        .map_err(|error| sqlite_error("read replaced project memory documents", error))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| sqlite_error("collect replaced project memory documents", error))?;
    Ok(keys)
}

fn memory_change(
    document_key: &[u8],
    reduction: &MemoryReduction,
) -> Result<ChangeEntry, EngineError> {
    change(
        "context.project-memory-document.changed",
        document_key,
        reduction.resolution_status.is_some(),
        &serde_json::json!({
            "resolution_status": reduction.resolution_status,
            "assertion_count": reduction.assertion_count,
            "competing_document_count": reduction.competing_document_count,
        }),
        "serialize project memory change",
    )
}

fn memory_conflict_change(
    document_key: &[u8],
    reduction: &MemoryReduction,
) -> Result<ChangeEntry, EngineError> {
    change(
        "diagnostic.context.project-memory-document-conflict",
        document_key,
        reduction.competing_document_count > 0,
        &serde_json::json!({
            "conflicting": reduction.competing_document_count > 0,
            "competing_document_count": reduction.competing_document_count,
        }),
        "serialize project memory conflict change",
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
