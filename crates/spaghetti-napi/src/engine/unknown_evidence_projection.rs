//! Durable RFC 012C unknown-native-evidence projection.
//!
//! Record mappings are produced by the common decode boundary and arrive in
//! the same `FactBatch` as their retained audit fact. This projector owns the
//! bounded current-generation SQL state; aggregate and sample semantics stay
//! in the topology-neutral reducer shared with scoped observation.

use std::collections::BTreeMap;

use rusqlite::{params, Transaction};
#[cfg(test)]
use rusqlite::{Connection, OptionalExtension};

#[cfg(test)]
use crate::adapter::BoundedNativeEvidence;
use crate::adapter::{Fact, FactBatch, RecordMappingDisposition, SourceRecordId};
use crate::unknown_evidence_reducer::{
    UnknownEvidenceOccurrence, UnknownEvidenceReductionError, MAX_UNKNOWN_EVIDENCE_OCCURRENCES,
};

use super::commit::ProjectionCommitContext;
use super::EngineError;

pub(super) fn apply_unknown_evidence_mappings(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    batch: &FactBatch,
) -> Result<(), EngineError> {
    let occurrences = retained_unknown_occurrences(batch)?;

    if context.consistency == crate::adapter::ConsistencyPolicy::SnapshotReplace {
        transaction
            .execute(
                "DELETE FROM unknown_native_evidence WHERE source_object_id = ?1",
                [sqlite_u64(context.source_object_id, "source object id")?],
            )
            .map_err(|error| sqlite_error("replace durable unknown evidence", error))?;
    } else if context.replaces_prior_generation {
        transaction
            .execute(
                "DELETE FROM unknown_native_evidence \
                 WHERE source_object_id = ?1 AND source_generation <> ?2",
                params![
                    sqlite_u64(context.source_object_id, "source object id")?,
                    sqlite_u64(context.generation, "source generation")?,
                ],
            )
            .map_err(|error| sqlite_error("retract prior unknown-evidence generation", error))?;
    }

    for occurrence in occurrences.values() {
        let changed = transaction
            .execute(
                r#"
                INSERT INTO unknown_native_evidence (
                  source_record_id,
                  source_instance_id,
                  source_stream_id,
                  source_object_id,
                  source_generation,
                  family_hint,
                  observed_bytes,
                  payload_digest,
                  sanitized_excerpt,
                  last_commit_seq
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                ON CONFLICT(source_record_id) DO UPDATE SET
                  family_hint = excluded.family_hint,
                  observed_bytes = excluded.observed_bytes,
                  payload_digest = excluded.payload_digest,
                  sanitized_excerpt = excluded.sanitized_excerpt,
                  last_commit_seq = excluded.last_commit_seq
                WHERE unknown_native_evidence.source_instance_id = excluded.source_instance_id
                  AND unknown_native_evidence.source_stream_id = excluded.source_stream_id
                  AND unknown_native_evidence.source_object_id = excluded.source_object_id
                  AND unknown_native_evidence.source_generation = excluded.source_generation
                "#,
                params![
                    occurrence.evidence.source_record_id.as_bytes().as_slice(),
                    sqlite_u64(context.source_instance_id, "source instance id")?,
                    sqlite_u64(context.source_stream_id, "source stream id")?,
                    sqlite_u64(context.source_object_id, "source object id")?,
                    sqlite_u64(context.generation, "source generation")?,
                    occurrence.family_hint.as_deref(),
                    sqlite_u64(occurrence.evidence.observed_bytes, "unknown evidence bytes")?,
                    occurrence.evidence.payload_digest.as_slice(),
                    occurrence.evidence.sanitized_excerpt.as_slice(),
                    sqlite_u64(context.commit_seq, "commit sequence")?,
                ],
            )
            .map_err(|error| sqlite_error("upsert durable unknown evidence", error))?;
        if changed != 1 {
            return Err(EngineError::InvalidCommit(
                "unknown-evidence identity crossed durable ownership".to_string(),
            ));
        }
    }

    let count = transaction
        .query_row(
            "SELECT COUNT(*) FROM unknown_native_evidence \
             WHERE source_object_id = ?1 AND source_generation = ?2",
            params![
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
            ],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| sqlite_error("count durable unknown evidence", error))?;
    if usize::try_from(count)
        .ok()
        .is_none_or(|count| count > MAX_UNKNOWN_EVIDENCE_OCCURRENCES)
    {
        return Err(EngineError::InvalidCommit(
            "unknown-evidence reduction capacity exhausted".to_string(),
        ));
    }
    Ok(())
}

fn retained_unknown_occurrences(
    batch: &FactBatch,
) -> Result<BTreeMap<SourceRecordId, UnknownEvidenceOccurrence>, EngineError> {
    let mut retained_fact_hashes = BTreeMap::<[u8; 32], usize>::new();
    for envelope in batch.facts() {
        if matches!(envelope.value, Fact::UnknownRecord { .. }) {
            *retained_fact_hashes
                .entry(envelope.provenance.record_hash)
                .or_default() += 1;
        }
    }

    let mut occurrences = BTreeMap::new();
    for mapping in batch.record_mapping_dispositions() {
        match mapping {
            RecordMappingDisposition::RetainedUnknown {
                family_hint,
                bounded_evidence,
            } => {
                let matching_fact_count = retained_fact_hashes
                    .get_mut(&bounded_evidence.payload_digest)
                    .ok_or_else(|| {
                        EngineError::InvalidCommit(
                            "unknown mapping has no retained audit fact".to_string(),
                        )
                    })?;
                if *matching_fact_count == 0 {
                    return Err(EngineError::InvalidCommit(
                        "unknown mapping has no retained audit fact".to_string(),
                    ));
                }
                *matching_fact_count -= 1;
                let occurrence =
                    UnknownEvidenceOccurrence::new(family_hint.clone(), bounded_evidence.clone())
                        .map_err(reduction_error)?;
                if occurrences
                    .insert(bounded_evidence.source_record_id, occurrence)
                    .is_some()
                {
                    return Err(EngineError::InvalidCommit(
                        "duplicate unknown-evidence source identity".to_string(),
                    ));
                }
            }
            RecordMappingDisposition::BufferedIncomplete => {
                return Err(EngineError::InvalidCommit(
                    "buffered-incomplete mapping cannot be committed".to_string(),
                ));
            }
            RecordMappingDisposition::Mapped { .. }
            | RecordMappingDisposition::IgnoredKnown { .. } => {}
        }
    }
    Ok(occurrences)
}

/// The retained current-generation unknown-evidence set for one object,
/// ordered by source identity so a comparison is independent of arrival order.
#[cfg(test)]
pub(super) fn read_unknown_evidence_snapshot(
    connection: &Connection,
    source_object_id: u64,
    generation: u64,
) -> Result<Vec<UnknownEvidenceOccurrence>, EngineError> {
    let mut statement = connection
        .prepare(
            r#"
            SELECT source_record_id, family_hint, observed_bytes,
                   payload_digest, sanitized_excerpt
            FROM unknown_native_evidence
            WHERE source_object_id = ?1 AND source_generation = ?2
            ORDER BY source_record_id
            "#,
        )
        .map_err(|error| sqlite_error("prepare durable unknown evidence", error))?;
    let rows = statement
        .query_map(
            params![
                sqlite_u64(source_object_id, "source object id")?,
                sqlite_u64(generation, "source generation")?,
            ],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                ))
            },
        )
        .map_err(|error| sqlite_error("read durable unknown evidence", error))?;

    let mut retained = Vec::new();
    for row in rows {
        let (source_record_id, family_hint, observed_bytes, payload_digest, sanitized_excerpt) =
            row.map_err(|error| sqlite_error("decode durable unknown evidence", error))?;
        let source_record_id =
            SourceRecordId::from_digest(source_record_id.try_into().map_err(|_| {
                EngineError::InvalidCommit(
                    "durable unknown-evidence source identity is invalid".to_string(),
                )
            })?);
        let payload_digest = payload_digest.try_into().map_err(|_| {
            EngineError::InvalidCommit(
                "durable unknown-evidence payload digest is invalid".to_string(),
            )
        })?;
        let observed_bytes = u64::try_from(observed_bytes).map_err(|_| {
            EngineError::InvalidCommit("durable unknown-evidence byte count is invalid".to_string())
        })?;
        retained.push(
            UnknownEvidenceOccurrence::new(
                family_hint,
                BoundedNativeEvidence {
                    source_record_id,
                    observed_bytes,
                    payload_digest,
                    sanitized_excerpt,
                },
            )
            .map_err(reduction_error)?,
        );
    }
    Ok(retained)
}

#[cfg(test)]
pub(super) fn unknown_evidence_owner(
    connection: &Connection,
    source_record_id: &SourceRecordId,
) -> Result<Option<(u64, u64)>, EngineError> {
    connection
        .query_row(
            "SELECT source_object_id, source_generation FROM unknown_native_evidence \
             WHERE source_record_id = ?1",
            [source_record_id.as_bytes().as_slice()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(|error| sqlite_error("read durable unknown-evidence owner", error))?
        .map(|(object, generation)| {
            Ok((
                u64::try_from(object).map_err(|_| {
                    EngineError::InvalidCommit(
                        "durable unknown-evidence owner is invalid".to_string(),
                    )
                })?,
                u64::try_from(generation).map_err(|_| {
                    EngineError::InvalidCommit(
                        "durable unknown-evidence generation is invalid".to_string(),
                    )
                })?,
            ))
        })
        .transpose()
}

fn reduction_error(error: UnknownEvidenceReductionError) -> EngineError {
    EngineError::InvalidCommit(error.to_string())
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
