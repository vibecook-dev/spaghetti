//! Replaceable native presence projections.
//!
//! A canonical row proves that an agent-owned presence object currently
//! exists. Host PID probes, time-based freshness, and lifecycle conclusions
//! are deliberately outside this durable projection.

use std::collections::BTreeSet;

use rusqlite::{params, Transaction};

use crate::adapter::{Fact, FactBatch, FactEnvelope, PresenceFact, QualifiedTimestamp};

use super::commit::{ChangeEntry, ProjectionCommitContext};
use super::EngineError;

const CHANGE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, PartialEq, Eq)]
struct PresenceReduction {
    resolution_status: Option<String>,
    native_status: Option<String>,
    assertion_count: usize,
    competing_assertion_count: usize,
}

pub(super) fn apply_presence_facts(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    batch: &FactBatch,
) -> Result<Vec<ChangeEntry>, EngineError> {
    let object_id = sqlite_u64(context.source_object_id, "source object id")?;
    let mut affected = source_object_keys(transaction, object_id)?;

    // Presence objects are whole, replaceable observations. Updates and
    // confirmed absence both replace the assertion owned by this object even
    // when the common driver keeps the same generation.
    transaction
        .execute(
            "DELETE FROM presence_assertions WHERE source_object_id = ?1",
            [object_id],
        )
        .map_err(|error| sqlite_error("retract replaced presence assertions", error))?;

    for envelope in batch.facts() {
        let Fact::Presence(fact) = &envelope.value else {
            continue;
        };
        write_presence_assertion(transaction, context, envelope, fact)?;
        affected.insert(fact.presence.as_bytes().to_vec());
    }

    let mut changes = Vec::with_capacity(affected.len().saturating_mul(2));
    for presence_key in affected {
        let reduction = reduce_presence(transaction, &presence_key, context.commit_seq)?;
        changes.push(presence_change(&presence_key, &reduction)?);
        changes.push(presence_conflict_change(&presence_key, &reduction)?);
    }

    // Release replaced audit facts only after canonical foreign keys have
    // moved to the decisive current assertion or been deleted.
    transaction
        .execute(
            r#"
            DELETE FROM fact_records
            WHERE source_object_id = ?1
              AND fact_kind = 'presence'
              AND last_commit_seq <> ?2
            "#,
            params![
                object_id,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("retract replaced presence facts", error))?;

    Ok(changes)
}

fn write_presence_assertion(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    envelope: &FactEnvelope,
    fact: &PresenceFact,
) -> Result<(), EngineError> {
    let digest = serde_json::to_vec(fact)
        .map(|encoded| *blake3::hash(&encoded).as_bytes())
        .map_err(|error| {
            EngineError::InvalidCommit(format!("serialize presence assertion: {error}"))
        })?;
    transaction
        .execute(
            r#"
            INSERT INTO presence_assertions (
                fact_id, presence_key, session_key, run_key,
                native_session_id, native_pid, cwd, started_at,
                started_at_quality, native_kind, entrypoint, name,
                native_status, updated_at, updated_at_quality,
                status_updated_at, status_updated_at_quality,
                native_process_started_at, version, peer_protocol,
                name_source, bridge_session_id, messaging_socket_path,
                presence_digest, source_object_id, source_generation,
                cursor_end, last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23,
                ?24, ?25, ?26, ?27, ?28
            )
            "#,
            params![
                envelope.id.as_bytes().as_slice(),
                fact.presence.as_bytes(),
                fact.session.as_bytes(),
                fact.run.as_bytes(),
                fact.native_session_id,
                i64::from(fact.native_pid),
                fact.cwd,
                fact.started_at.value,
                timestamp_quality(&fact.started_at),
                fact.native_kind,
                fact.entrypoint,
                fact.name,
                fact.native_status,
                timestamp_value(fact.updated_at.as_ref()),
                optional_timestamp_quality(fact.updated_at.as_ref()),
                timestamp_value(fact.status_updated_at.as_ref()),
                optional_timestamp_quality(fact.status_updated_at.as_ref()),
                fact.native_process_started_at,
                fact.version,
                fact.peer_protocol.map(i64::from),
                fact.name_source,
                fact.bridge_session_id,
                fact.messaging_socket_path,
                digest.as_slice(),
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
                envelope.provenance.cursor_end,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map(|_| ())
        .map_err(|error| sqlite_error("project presence assertion", error))
}

fn reduce_presence(
    transaction: &Transaction<'_>,
    presence_key: &[u8],
    commit_seq: u64,
) -> Result<PresenceReduction, EngineError> {
    let mut statement = transaction
        .prepare(
            "SELECT fact_id, presence_digest, native_status FROM presence_assertions WHERE presence_key = ?1 ORDER BY fact_id",
        )
        .map_err(|error| sqlite_error("prepare presence assertion reduction", error))?;
    let assertions = statement
        .query_map([presence_key], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(|error| sqlite_error("read presence assertions", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("collect presence assertions", error))?;
    let Some((decisive_fact_id, _, native_status)) = assertions.first() else {
        transaction
            .execute(
                "DELETE FROM canonical_presences WHERE presence_key = ?1",
                [presence_key],
            )
            .map_err(|error| sqlite_error("remove absent canonical presence", error))?;
        return Ok(PresenceReduction {
            resolution_status: None,
            native_status: None,
            assertion_count: 0,
            competing_assertion_count: 0,
        });
    };
    let assertion_count = assertions.len();
    let competing_assertion_count = assertions
        .iter()
        .map(|(_, digest, _)| digest)
        .collect::<BTreeSet<_>>()
        .len()
        .saturating_sub(1);
    let resolution_status = if competing_assertion_count > 0 {
        "conflicting"
    } else {
        "resolved"
    };

    transaction
        .execute(
            r#"
            INSERT INTO canonical_presences (
                presence_key, session_key, run_key, native_session_id,
                native_pid, cwd, started_at, started_at_quality,
                native_kind, entrypoint, name, native_status, updated_at,
                updated_at_quality, status_updated_at,
                status_updated_at_quality, native_process_started_at,
                version, peer_protocol, name_source, bridge_session_id,
                messaging_socket_path, presence_status, decisive_fact_id,
                assertion_count, competing_assertion_count, last_commit_seq
            )
            SELECT presence_key, session_key, run_key, native_session_id,
                   native_pid, cwd, started_at, started_at_quality,
                   native_kind, entrypoint, name, native_status, updated_at,
                   updated_at_quality, status_updated_at,
                   status_updated_at_quality, native_process_started_at,
                   version, peer_protocol, name_source, bridge_session_id,
                   messaging_socket_path, ?2, fact_id, ?3, ?4, ?5
            FROM presence_assertions WHERE fact_id = ?1
            ON CONFLICT(presence_key) DO UPDATE SET
                session_key = excluded.session_key,
                run_key = excluded.run_key,
                native_session_id = excluded.native_session_id,
                native_pid = excluded.native_pid,
                cwd = excluded.cwd,
                started_at = excluded.started_at,
                started_at_quality = excluded.started_at_quality,
                native_kind = excluded.native_kind,
                entrypoint = excluded.entrypoint,
                name = excluded.name,
                native_status = excluded.native_status,
                updated_at = excluded.updated_at,
                updated_at_quality = excluded.updated_at_quality,
                status_updated_at = excluded.status_updated_at,
                status_updated_at_quality = excluded.status_updated_at_quality,
                native_process_started_at = excluded.native_process_started_at,
                version = excluded.version,
                peer_protocol = excluded.peer_protocol,
                name_source = excluded.name_source,
                bridge_session_id = excluded.bridge_session_id,
                messaging_socket_path = excluded.messaging_socket_path,
                presence_status = excluded.presence_status,
                decisive_fact_id = excluded.decisive_fact_id,
                assertion_count = excluded.assertion_count,
                competing_assertion_count = excluded.competing_assertion_count,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                decisive_fact_id,
                resolution_status,
                sqlite_usize(assertion_count, "presence assertion count")?,
                sqlite_usize(
                    competing_assertion_count,
                    "presence competing assertion count"
                )?,
                sqlite_u64(commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("write canonical presence", error))?;

    Ok(PresenceReduction {
        resolution_status: Some(resolution_status.to_string()),
        native_status: native_status.clone(),
        assertion_count,
        competing_assertion_count,
    })
}

fn source_object_keys(
    transaction: &Transaction<'_>,
    source_object_id: i64,
) -> Result<BTreeSet<Vec<u8>>, EngineError> {
    let mut statement = transaction
        .prepare(
            "SELECT DISTINCT presence_key FROM presence_assertions WHERE source_object_id = ?1",
        )
        .map_err(|error| sqlite_error("prepare replaced presence assertions", error))?;
    let keys = statement
        .query_map([source_object_id], |row| row.get::<_, Vec<u8>>(0))
        .map_err(|error| sqlite_error("read replaced presence assertions", error))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| sqlite_error("collect replaced presence assertions", error))?;
    Ok(keys)
}

fn presence_change(
    presence_key: &[u8],
    reduction: &PresenceReduction,
) -> Result<ChangeEntry, EngineError> {
    let payload = serialize(
        &serde_json::json!({
            "resolution_status": reduction.resolution_status,
            "native_status": reduction.native_status,
            "assertion_count": reduction.assertion_count,
            "competing_assertion_count": reduction.competing_assertion_count,
        }),
        "serialize presence change",
    )?;
    Ok(ChangeEntry {
        topic: "runtime.presence.changed".to_string(),
        schema_version: CHANGE_SCHEMA_VERSION,
        entity_key: presence_key.to_vec(),
        operation: if reduction.resolution_status.is_some() {
            "upsert"
        } else {
            "delete"
        }
        .to_string(),
        payload,
    })
}

fn presence_conflict_change(
    presence_key: &[u8],
    reduction: &PresenceReduction,
) -> Result<ChangeEntry, EngineError> {
    let conflicting = reduction.competing_assertion_count > 0;
    let payload = serialize(
        &serde_json::json!({
            "conflicting": conflicting,
            "competing_assertion_count": reduction.competing_assertion_count,
        }),
        "serialize presence conflict change",
    )?;
    Ok(ChangeEntry {
        topic: "diagnostic.runtime.presence-conflict".to_string(),
        schema_version: CHANGE_SCHEMA_VERSION,
        entity_key: presence_key.to_vec(),
        operation: if conflicting { "upsert" } else { "delete" }.to_string(),
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

fn timestamp_value(timestamp: Option<&QualifiedTimestamp>) -> Option<&str> {
    timestamp.map(|timestamp| timestamp.value.as_str())
}

fn optional_timestamp_quality(timestamp: Option<&QualifiedTimestamp>) -> Option<&'static str> {
    timestamp.map(timestamp_quality)
}

fn timestamp_quality(timestamp: &QualifiedTimestamp) -> &'static str {
    match timestamp.quality {
        crate::adapter::TimestampQuality::NativeExact => "native_exact",
        crate::adapter::TimestampQuality::NativeApproximate => "native_approximate",
        crate::adapter::TimestampQuality::FileMetadataFallback => "file_metadata_fallback",
        crate::adapter::TimestampQuality::Derived => "derived",
    }
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
