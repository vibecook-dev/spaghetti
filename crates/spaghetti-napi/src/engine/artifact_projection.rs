//! File-history artifact metadata/content projection.
//!
//! Transcript checkpoints and deltas accumulate as historical metadata within
//! one append generation. Backup blobs are independently replaceable source
//! objects. The reducer joins both halves by canonical artifact key, keeps
//! missing and orphaned halves queryable, and never upgrades session capture
//! into a claim that a run produced the tracked file.

use std::collections::BTreeSet;

use rusqlite::{params, Transaction};

use crate::adapter::{
    ArtifactCapture, ArtifactContentFact, ArtifactMetadataEntry, ArtifactMetadataSnapshotFact,
    ArtifactObservationKind, Fact, FactBatch, FactEnvelope, QualifiedTimestamp,
};

use super::commit::{ChangeEntry, ProjectionCommitContext};
use super::EngineError;

const CHANGE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug)]
struct MetadataAssertion {
    fact_id: Vec<u8>,
    session_key: Vec<u8>,
    native_artifact_id: Option<String>,
    tracking_path: String,
    real_parent_dir: Option<String>,
    version: i64,
    backup_time: String,
    backup_time_quality: String,
    capture_status: String,
    metadata_digest: Vec<u8>,
}

#[derive(Debug)]
struct ContentAssertion {
    fact_id: Vec<u8>,
    session_key: Vec<u8>,
    native_artifact_id: String,
    native_file_hash: String,
    version: i64,
    size_bytes: i64,
    content_digest: Vec<u8>,
    assertion_digest: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
struct ArtifactReduction {
    resolution_status: Option<String>,
    content_status: Option<String>,
    metadata_assertion_count: usize,
    competing_metadata_count: usize,
    content_assertion_count: usize,
    competing_content_count: usize,
    join_conflict: bool,
}

pub(super) fn apply_artifact_facts(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    batch: &FactBatch,
) -> Result<Vec<ChangeEntry>, EngineError> {
    let object_id = sqlite_u64(context.source_object_id, "source object id")?;
    let generation = sqlite_u64(context.generation, "source generation")?;
    let has_metadata_fact = batch
        .facts()
        .iter()
        .any(|envelope| matches!(envelope.value, Fact::ArtifactMetadataSnapshot(_)));
    let has_content_fact = batch
        .facts()
        .iter()
        .any(|envelope| matches!(envelope.value, Fact::ArtifactContent(_)));
    if context.skip_unowned_replace_document(has_metadata_fact || has_content_fact) {
        return Ok(Vec::new());
    }
    // A source object allocated by this commit cannot own old assertions.
    // The production corpus contains tens of thousands of one-fact backup
    // documents, so avoiding two ownership probes on their first (and usually
    // only) commit removes a large number of tiny indexed reads.
    let metadata_artifacts = if context.object_is_new {
        BTreeSet::new()
    } else {
        source_object_keys(
            transaction,
            r#"
            SELECT DISTINCT metadata.artifact_key
            FROM artifact_metadata_assertions AS metadata
            JOIN artifact_snapshot_assertions AS snapshot ON snapshot.fact_id = metadata.fact_id
            WHERE snapshot.source_object_id = ?1
            "#,
            object_id,
            "read source-owned artifact metadata",
        )?
    };
    let content_artifacts = if context.object_is_new {
        BTreeSet::new()
    } else {
        source_object_keys(
            transaction,
            "SELECT DISTINCT artifact_key FROM artifact_content_assertions WHERE source_object_id = ?1",
            object_id,
            "read source-owned artifact content",
        )?
    };
    // Append metadata needs no work on a same-generation batch that contains
    // no metadata. Replaceable content still needs an empty deletion commit,
    // while old append metadata is retracted on generation replacement.
    let touches_metadata =
        has_metadata_fact || (context.replaces_prior_generation && !metadata_artifacts.is_empty());
    let touches_content = has_content_fact || !content_artifacts.is_empty();
    if !touches_metadata && !touches_content {
        return Ok(Vec::new());
    }
    let mut affected_artifacts = BTreeSet::new();
    if touches_metadata {
        affected_artifacts.extend(metadata_artifacts);
    }
    if touches_content {
        affected_artifacts.extend(content_artifacts);
    }

    // Transcript metadata is append history and accumulates inside a
    // generation. A rewrite starts a new generation and retracts the old
    // checkpoint/delta assertions. Blob documents, in contrast, replace as a
    // whole even when the common driver keeps the same generation.
    if touches_metadata && context.replaces_prior_generation {
        transaction
            .execute(
                r#"
                DELETE FROM artifact_snapshot_assertions
                WHERE source_object_id = ?1 AND source_generation <> ?2
                "#,
                params![object_id, generation],
            )
            .map_err(|error| {
                sqlite_error("retract replaced artifact metadata generation", error)
            })?;
    }
    if touches_content && !context.object_is_new {
        transaction
            .execute(
                "DELETE FROM artifact_content_assertions WHERE source_object_id = ?1",
                [object_id],
            )
            .map_err(|error| sqlite_error("retract replaced artifact content", error))?;
    }

    for envelope in batch.facts() {
        match &envelope.value {
            Fact::ArtifactMetadataSnapshot(fact) => {
                write_metadata_snapshot(transaction, context, envelope, fact)?;
                affected_artifacts.extend(
                    fact.artifacts
                        .iter()
                        .map(|artifact| artifact.artifact.as_bytes().to_vec()),
                );
            }
            Fact::ArtifactContent(fact) => {
                write_content_assertion(transaction, context, envelope, fact)?;
                affected_artifacts.insert(fact.artifact.as_bytes().to_vec());
            }
            _ => {}
        }
    }

    let mut changes = Vec::new();
    for artifact_key in affected_artifacts {
        let reduction = reduce_artifact(transaction, &artifact_key, context.commit_seq)?;
        changes.push(artifact_change(&artifact_key, &reduction)?);
        changes.push(artifact_conflict_change(&artifact_key, &reduction)?);
    }

    // Canonical rows now reference the surviving decisive assertions (or have
    // been removed), so superseded audit facts can be deleted safely.
    if touches_content && !context.object_is_new {
        transaction
            .execute(
                r#"
                DELETE FROM fact_records
                WHERE source_object_id = ?1
                  AND fact_kind = 'artifact_content'
                  AND last_commit_seq <> ?2
                "#,
                params![
                    object_id,
                    sqlite_u64(context.commit_seq, "commit sequence")?,
                ],
            )
            .map_err(|error| sqlite_error("retract replaced artifact content facts", error))?;
    }
    if touches_metadata && context.replaces_prior_generation {
        transaction
            .execute(
                r#"
                DELETE FROM fact_records
                WHERE source_object_id = ?1
                  AND source_generation <> ?2
                  AND fact_kind = 'artifact_metadata_snapshot'
                "#,
                params![object_id, generation],
            )
            .map_err(|error| sqlite_error("retract replaced artifact metadata facts", error))?;
    }

    Ok(changes)
}

fn write_metadata_snapshot(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    envelope: &FactEnvelope,
    fact: &ArtifactMetadataSnapshotFact,
) -> Result<(), EngineError> {
    if fact.native_message_id.trim().is_empty() || fact.native_snapshot_message_id.trim().is_empty()
    {
        return Err(EngineError::InvalidCommit(
            "artifact metadata message identities must not be empty".to_string(),
        ));
    }
    transaction
        .execute(
            r#"
            INSERT INTO artifact_snapshot_assertions (
                fact_id, session_key, native_message_id,
                native_snapshot_message_id, observation_kind,
                is_snapshot_update, source_time, source_time_quality,
                source_object_id, source_generation, cursor_end,
                last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12
            )
            ON CONFLICT(fact_id) DO UPDATE SET
                session_key = excluded.session_key,
                native_message_id = excluded.native_message_id,
                native_snapshot_message_id = excluded.native_snapshot_message_id,
                observation_kind = excluded.observation_kind,
                is_snapshot_update = excluded.is_snapshot_update,
                source_time = excluded.source_time,
                source_time_quality = excluded.source_time_quality,
                source_object_id = excluded.source_object_id,
                source_generation = excluded.source_generation,
                cursor_end = excluded.cursor_end,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                envelope.id.as_bytes().as_slice(),
                fact.session.as_bytes(),
                fact.native_message_id,
                fact.native_snapshot_message_id,
                observation_kind(fact.observation_kind),
                i64::from(fact.is_snapshot_update),
                timestamp_value(fact.source_time.as_ref()),
                timestamp_quality(fact.source_time.as_ref()),
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
                envelope.provenance.cursor_end,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("project artifact metadata snapshot", error))?;
    transaction
        .execute(
            "DELETE FROM artifact_metadata_assertions WHERE fact_id = ?1",
            [envelope.id.as_bytes().as_slice()],
        )
        .map_err(|error| sqlite_error("replace artifact metadata snapshot children", error))?;

    let mut artifact_keys = BTreeSet::new();
    for artifact in &fact.artifacts {
        if !artifact_keys.insert(artifact.artifact.as_bytes()) {
            return Err(EngineError::InvalidCommit(
                "artifact metadata snapshot contains a duplicate artifact key".to_string(),
            ));
        }
        write_metadata_entry(transaction, envelope, &fact.session, artifact)?;
    }
    Ok(())
}

fn write_metadata_entry(
    transaction: &Transaction<'_>,
    envelope: &FactEnvelope,
    session: &crate::adapter::EntityKey,
    artifact: &ArtifactMetadataEntry,
) -> Result<(), EngineError> {
    if artifact.version == 0
        || artifact.tracking_path.trim().is_empty()
        || artifact.backup_time.value.trim().is_empty()
    {
        return Err(EngineError::InvalidCommit(
            "artifact metadata contains an empty path/time or zero version".to_string(),
        ));
    }
    match (artifact.capture, artifact.native_artifact_id.as_deref()) {
        (ArtifactCapture::ContentExpected, Some(native_id)) if !native_id.trim().is_empty() => {}
        (ArtifactCapture::NotCaptured, None) => {}
        _ => {
            return Err(EngineError::InvalidCommit(
                "artifact capture status disagrees with native content identity".to_string(),
            ));
        }
    }
    let capture_status = capture_status(artifact.capture);
    let metadata_digest = digest(
        &(
            &artifact.artifact,
            session,
            &artifact.native_artifact_id,
            &artifact.tracking_path,
            &artifact.real_parent_dir,
            artifact.version,
            &artifact.backup_time,
            capture_status,
        ),
        "digest artifact metadata assertion",
    )?;
    transaction
        .execute(
            r#"
            INSERT INTO artifact_metadata_assertions (
                fact_id, artifact_key, session_key, native_artifact_id,
                tracking_path, real_parent_dir, version, backup_time,
                backup_time_quality, capture_status, metadata_digest
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            "#,
            params![
                envelope.id.as_bytes().as_slice(),
                artifact.artifact.as_bytes(),
                session.as_bytes(),
                artifact.native_artifact_id,
                artifact.tracking_path,
                artifact.real_parent_dir,
                sqlite_u64(artifact.version, "artifact version")?,
                artifact.backup_time.value,
                timestamp_quality(Some(&artifact.backup_time)),
                capture_status,
                metadata_digest.as_slice(),
            ],
        )
        .map(|_| ())
        .map_err(|error| sqlite_error("project artifact metadata entry", error))
}

fn write_content_assertion(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    envelope: &FactEnvelope,
    fact: &ArtifactContentFact,
) -> Result<(), EngineError> {
    if fact.version == 0
        || fact.native_artifact_id.trim().is_empty()
        || fact.native_file_hash.trim().is_empty()
        || fact.size_bytes != fact.content.len() as u64
    {
        return Err(EngineError::InvalidCommit(
            "artifact content identity, version, or byte size is invalid".to_string(),
        ));
    }
    let content_digest = *blake3::hash(&fact.content).as_bytes();
    let assertion_digest = digest(
        &(
            &fact.artifact,
            &fact.session,
            &fact.native_artifact_id,
            &fact.native_file_hash,
            fact.version,
            content_digest,
        ),
        "digest artifact content assertion",
    )?;
    transaction
        .execute(
            r#"
            INSERT INTO artifact_content_assertions (
                fact_id, artifact_key, session_key, native_artifact_id,
                native_file_hash, version, content, size_bytes,
                content_digest, assertion_digest, source_object_id,
                source_generation, cursor_end, last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14
            )
            "#,
            params![
                envelope.id.as_bytes().as_slice(),
                fact.artifact.as_bytes(),
                fact.session.as_bytes(),
                fact.native_artifact_id,
                fact.native_file_hash,
                sqlite_u64(fact.version, "artifact version")?,
                fact.content,
                sqlite_u64(fact.size_bytes, "artifact size")?,
                content_digest.as_slice(),
                assertion_digest.as_slice(),
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
                envelope.provenance.cursor_end,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map(|_| ())
        .map_err(|error| sqlite_error("project artifact content assertion", error))
}

fn reduce_artifact(
    transaction: &Transaction<'_>,
    artifact_key: &[u8],
    commit_seq: u64,
) -> Result<ArtifactReduction, EngineError> {
    let metadata = read_metadata_assertions(transaction, artifact_key)?;
    let content = read_content_assertions(transaction, artifact_key)?;
    if metadata.is_empty() && content.is_empty() {
        transaction
            .execute(
                "DELETE FROM canonical_artifacts WHERE artifact_key = ?1",
                [artifact_key],
            )
            .map_err(|error| sqlite_error("remove absent canonical artifact", error))?;
        return Ok(ArtifactReduction {
            resolution_status: None,
            content_status: None,
            metadata_assertion_count: 0,
            competing_metadata_count: 0,
            content_assertion_count: 0,
            competing_content_count: 0,
            join_conflict: false,
        });
    }

    let metadata_assertion_count = metadata.len();
    let competing_metadata_count = metadata
        .iter()
        .map(|assertion| &assertion.metadata_digest)
        .collect::<BTreeSet<_>>()
        .len()
        .saturating_sub(1);
    let content_assertion_count = content.len();
    let competing_content_count = content
        .iter()
        .map(|assertion| &assertion.assertion_digest)
        .collect::<BTreeSet<_>>()
        .len()
        .saturating_sub(1);
    let decisive_metadata = metadata.first();
    let decisive_content = content.first();
    let join_conflict =
        decisive_metadata
            .zip(decisive_content)
            .is_some_and(|(metadata, content)| {
                metadata.session_key != content.session_key
                    || metadata.native_artifact_id.as_deref()
                        != Some(content.native_artifact_id.as_str())
                    || metadata.version != content.version
                    || metadata.capture_status != "content_expected"
            });
    let content_status = match (decisive_metadata, decisive_content) {
        (None, Some(_)) => "orphan_content",
        (Some(_), Some(_)) => "captured",
        (Some(metadata), None) if metadata.capture_status == "not_captured" => "not_captured",
        (Some(_), None) => "missing_content",
        (None, None) => unreachable!("empty assertion sets returned above"),
    };
    let resolution_status =
        if competing_metadata_count > 0 || competing_content_count > 0 || join_conflict {
            "conflicting"
        } else if matches!(content_status, "missing_content" | "orphan_content") {
            "incomplete"
        } else {
            "resolved"
        };

    let session_key = decisive_metadata
        .map(|assertion| assertion.session_key.as_slice())
        .or_else(|| decisive_content.map(|assertion| assertion.session_key.as_slice()))
        .expect("at least one assertion exists");
    let native_artifact_id = decisive_metadata
        .and_then(|assertion| assertion.native_artifact_id.as_deref())
        .or_else(|| decisive_content.map(|assertion| assertion.native_artifact_id.as_str()));
    let version = decisive_metadata
        .map(|assertion| assertion.version)
        .or_else(|| decisive_content.map(|assertion| assertion.version))
        .expect("at least one assertion exists");
    let capture_status = decisive_metadata
        .map(|assertion| assertion.capture_status.as_str())
        .unwrap_or("unknown");

    transaction
        .execute(
            r#"
            INSERT INTO canonical_artifacts (
                artifact_key, session_key, native_artifact_id,
                native_file_hash, version, tracking_path, real_parent_dir,
                backup_time, backup_time_quality, capture_status, content,
                size_bytes, content_digest, content_status, resolution_status,
                decisive_metadata_fact_id, decisive_content_fact_id,
                metadata_assertion_count, competing_metadata_count,
                content_assertion_count, competing_content_count,
                join_conflict, last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23
            )
            ON CONFLICT(artifact_key) DO UPDATE SET
                session_key = excluded.session_key,
                native_artifact_id = excluded.native_artifact_id,
                native_file_hash = excluded.native_file_hash,
                version = excluded.version,
                tracking_path = excluded.tracking_path,
                real_parent_dir = excluded.real_parent_dir,
                backup_time = excluded.backup_time,
                backup_time_quality = excluded.backup_time_quality,
                capture_status = excluded.capture_status,
                content = excluded.content,
                size_bytes = excluded.size_bytes,
                content_digest = excluded.content_digest,
                content_status = excluded.content_status,
                resolution_status = excluded.resolution_status,
                decisive_metadata_fact_id = excluded.decisive_metadata_fact_id,
                decisive_content_fact_id = excluded.decisive_content_fact_id,
                metadata_assertion_count = excluded.metadata_assertion_count,
                competing_metadata_count = excluded.competing_metadata_count,
                content_assertion_count = excluded.content_assertion_count,
                competing_content_count = excluded.competing_content_count,
                join_conflict = excluded.join_conflict,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                artifact_key,
                session_key,
                native_artifact_id,
                decisive_content.map(|assertion| assertion.native_file_hash.as_str()),
                version,
                decisive_metadata.map(|assertion| assertion.tracking_path.as_str()),
                decisive_metadata.and_then(|assertion| assertion.real_parent_dir.as_deref()),
                decisive_metadata.map(|assertion| assertion.backup_time.as_str()),
                decisive_metadata.map(|assertion| assertion.backup_time_quality.as_str()),
                capture_status,
                // The decisive assertion is already the durable content
                // owner. Keep the nullable compatibility column empty on new
                // databases and have the query join through
                // decisive_content_fact_id instead of writing every backup
                // BLOB into SQLite twice.
                Option::<&[u8]>::None,
                decisive_content.map(|assertion| assertion.size_bytes),
                decisive_content.map(|assertion| assertion.content_digest.as_slice()),
                content_status,
                resolution_status,
                decisive_metadata.map(|assertion| assertion.fact_id.as_slice()),
                decisive_content.map(|assertion| assertion.fact_id.as_slice()),
                sqlite_usize(
                    metadata_assertion_count,
                    "artifact metadata assertion count"
                )?,
                sqlite_usize(
                    competing_metadata_count,
                    "artifact competing metadata count"
                )?,
                sqlite_usize(content_assertion_count, "artifact content assertion count")?,
                sqlite_usize(competing_content_count, "artifact competing content count")?,
                i64::from(join_conflict),
                sqlite_u64(commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("write canonical artifact", error))?;

    Ok(ArtifactReduction {
        resolution_status: Some(resolution_status.to_string()),
        content_status: Some(content_status.to_string()),
        metadata_assertion_count,
        competing_metadata_count,
        content_assertion_count,
        competing_content_count,
        join_conflict,
    })
}

fn read_metadata_assertions(
    transaction: &Transaction<'_>,
    artifact_key: &[u8],
) -> Result<Vec<MetadataAssertion>, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT fact_id, session_key, native_artifact_id, tracking_path,
                   real_parent_dir, version, backup_time, backup_time_quality,
                   capture_status, metadata_digest
            FROM artifact_metadata_assertions
            WHERE artifact_key = ?1
            ORDER BY fact_id
            "#,
        )
        .map_err(|error| sqlite_error("prepare artifact metadata reduction", error))?;
    let assertions = statement
        .query_map([artifact_key], |row| {
            Ok(MetadataAssertion {
                fact_id: row.get(0)?,
                session_key: row.get(1)?,
                native_artifact_id: row.get(2)?,
                tracking_path: row.get(3)?,
                real_parent_dir: row.get(4)?,
                version: row.get(5)?,
                backup_time: row.get(6)?,
                backup_time_quality: row.get(7)?,
                capture_status: row.get(8)?,
                metadata_digest: row.get(9)?,
            })
        })
        .map_err(|error| sqlite_error("read artifact metadata reduction", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("collect artifact metadata reduction", error))?;
    Ok(assertions)
}

fn read_content_assertions(
    transaction: &Transaction<'_>,
    artifact_key: &[u8],
) -> Result<Vec<ContentAssertion>, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT fact_id, session_key, native_artifact_id, native_file_hash,
                   version, size_bytes, content_digest, assertion_digest
            FROM artifact_content_assertions
            WHERE artifact_key = ?1
            ORDER BY fact_id
            "#,
        )
        .map_err(|error| sqlite_error("prepare artifact content reduction", error))?;
    let assertions = statement
        .query_map([artifact_key], |row| {
            Ok(ContentAssertion {
                fact_id: row.get(0)?,
                session_key: row.get(1)?,
                native_artifact_id: row.get(2)?,
                native_file_hash: row.get(3)?,
                version: row.get(4)?,
                size_bytes: row.get(5)?,
                content_digest: row.get(6)?,
                assertion_digest: row.get(7)?,
            })
        })
        .map_err(|error| sqlite_error("read artifact content reduction", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("collect artifact content reduction", error))?;
    Ok(assertions)
}

fn source_object_keys(
    transaction: &Transaction<'_>,
    query: &'static str,
    source_object_id: i64,
    operation: &'static str,
) -> Result<BTreeSet<Vec<u8>>, EngineError> {
    let mut statement = transaction
        .prepare(query)
        .map_err(|error| sqlite_error(operation, error))?;
    let keys = statement
        .query_map([source_object_id], |row| row.get::<_, Vec<u8>>(0))
        .map_err(|error| sqlite_error(operation, error))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| sqlite_error(operation, error))?;
    Ok(keys)
}

fn artifact_change(
    artifact_key: &[u8],
    reduction: &ArtifactReduction,
) -> Result<ChangeEntry, EngineError> {
    change(
        "runtime.artifact.changed",
        artifact_key,
        reduction.resolution_status.is_some(),
        &serde_json::json!({
            "resolution_status": reduction.resolution_status,
            "content_status": reduction.content_status,
            "metadata_assertion_count": reduction.metadata_assertion_count,
            "competing_metadata_count": reduction.competing_metadata_count,
            "content_assertion_count": reduction.content_assertion_count,
            "competing_content_count": reduction.competing_content_count,
            "join_conflict": reduction.join_conflict,
        }),
        "serialize artifact change",
    )
}

fn artifact_conflict_change(
    artifact_key: &[u8],
    reduction: &ArtifactReduction,
) -> Result<ChangeEntry, EngineError> {
    let competing_count = reduction
        .competing_metadata_count
        .saturating_add(reduction.competing_content_count)
        .saturating_add(usize::from(reduction.join_conflict));
    change(
        "diagnostic.runtime.artifact-conflict",
        artifact_key,
        competing_count > 0,
        &serde_json::json!({
            "conflicting": competing_count > 0,
            "competing_count": competing_count,
            "competing_metadata_count": reduction.competing_metadata_count,
            "competing_content_count": reduction.competing_content_count,
            "join_conflict": reduction.join_conflict,
        }),
        "serialize artifact conflict",
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

fn observation_kind(kind: ArtifactObservationKind) -> &'static str {
    match kind {
        ArtifactObservationKind::Checkpoint => "checkpoint",
        ArtifactObservationKind::Delta => "delta",
    }
}

fn capture_status(capture: ArtifactCapture) -> &'static str {
    match capture {
        ArtifactCapture::ContentExpected => "content_expected",
        ArtifactCapture::NotCaptured => "not_captured",
    }
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

fn timestamp_value(timestamp: Option<&QualifiedTimestamp>) -> Option<&str> {
    timestamp.map(|timestamp| timestamp.value.as_str())
}

fn timestamp_quality(timestamp: Option<&QualifiedTimestamp>) -> Option<&'static str> {
    timestamp.map(|timestamp| match timestamp.quality {
        crate::adapter::TimestampQuality::NativeExact => "native_exact",
        crate::adapter::TimestampQuality::NativeApproximate => "native_approximate",
        crate::adapter::TimestampQuality::FileMetadataFallback => "file_metadata_fallback",
        crate::adapter::TimestampQuality::Derived => "derived",
    })
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
