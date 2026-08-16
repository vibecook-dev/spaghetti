//! Durable RFC 012A source/fact-family coverage storage.
//!
//! Coverage is normalized instead of retained as one unbounded JSON blob so a
//! later public query can page points without decoding or copying an entire
//! source instance. Projection-pack readiness remains a separate owner: one
//! administrative commit atomically advances readiness and replaces the
//! coverage set that justified that transition.

use std::collections::BTreeSet;

use rusqlite::{params, OptionalExtension, Transaction};

use crate::adapter::{
    CoverageDomain, CoveragePositionKind, CoverageSetCompleteness, CoverageStatus,
    SourceCoverageSet,
};

use super::EngineError;

const MAX_COVERAGE_SET_UPDATES: usize = 16;
const MAX_COVERAGE_OWNER_ID_BYTES: usize = 256;
const MAX_COVERAGE_OWNER_SCOPE_BYTES: usize = 4 * 1024;
const MAX_COVERAGE_POINTS_PER_SET: usize = 250_000;
const MAX_COVERAGE_ABSENCES_PER_SET: usize = 250_000;
const MAX_COVERAGE_ERRORS_PER_SET: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableCoverageSetUpdate {
    pub owner_id: String,
    pub owner_scope_key: Vec<u8>,
    pub set: SourceCoverageSet,
}

/// Compare-and-set guard for an administrative transition justified by one
/// previously inspected coverage set. The writer evaluates this inside the
/// same immediate transaction that publishes the transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableCoverageSetPrecondition {
    pub owner_id: String,
    pub owner_scope_key: Vec<u8>,
    pub family: String,
    pub family_version: u32,
    pub adapter_id: String,
    pub canonical_source_instance_key: Vec<u8>,
    pub expected_content_digest: Vec<u8>,
    pub expected_last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CoverageStorageIdentity {
    owner_id: String,
    owner_scope_key: Vec<u8>,
    domain_kind: &'static str,
    domain_name: String,
    domain_version: u32,
    root_entity_key: Vec<u8>,
}

impl DurableCoverageSetUpdate {
    fn identity(&self) -> CoverageStorageIdentity {
        let (domain_kind, domain_name, domain_version) = domain_parts(&self.set.coverage_domain);
        CoverageStorageIdentity {
            owner_id: self.owner_id.clone(),
            owner_scope_key: self.owner_scope_key.clone(),
            domain_kind,
            domain_name,
            domain_version,
            root_entity_key: self
                .set
                .scope
                .root_entity_key
                .as_ref()
                .map_or_else(Vec::new, |key| key.as_bytes().to_vec()),
        }
    }
}

pub(crate) fn validate_updates(updates: &[DurableCoverageSetUpdate]) -> Result<(), EngineError> {
    if updates.len() > MAX_COVERAGE_SET_UPDATES {
        return Err(invalid(format!(
            "coverage set update count exceeds {MAX_COVERAGE_SET_UPDATES}"
        )));
    }
    let mut identities = BTreeSet::new();
    for update in updates {
        if update.owner_id.trim().is_empty() || update.owner_id.len() > MAX_COVERAGE_OWNER_ID_BYTES
        {
            return Err(invalid("coverage owner id is empty or unbounded"));
        }
        if update.owner_scope_key.is_empty()
            || update.owner_scope_key.len() > MAX_COVERAGE_OWNER_SCOPE_BYTES
        {
            return Err(invalid("coverage owner scope key is empty or unbounded"));
        }
        update
            .set
            .validate()
            .map_err(|error| invalid(format!("invalid source coverage set: {error}")))?;
        if update.set.points.len() > MAX_COVERAGE_POINTS_PER_SET {
            return Err(invalid(format!(
                "coverage point count exceeds {MAX_COVERAGE_POINTS_PER_SET}"
            )));
        }
        if update.set.explicit_absence_or_deletion.len() > MAX_COVERAGE_ABSENCES_PER_SET {
            return Err(invalid(format!(
                "coverage absence count exceeds {MAX_COVERAGE_ABSENCES_PER_SET}"
            )));
        }
        if update.set.explicit_errors.len() > MAX_COVERAGE_ERRORS_PER_SET {
            return Err(invalid(format!(
                "coverage error count exceeds {MAX_COVERAGE_ERRORS_PER_SET}"
            )));
        }
        if !strictly_sorted_by(&update.set.points, |point| {
            (point.stream_key, point.object_key, point.generation)
        }) {
            return Err(invalid(
                "coverage points must be strictly ordered by stream, object, and generation",
            ));
        }
        if !strictly_sorted_by(&update.set.explicit_absence_or_deletion, |absence| {
            (absence.stream_key, absence.object_key, absence.generation)
        }) {
            return Err(invalid(
                "coverage absences must be strictly ordered by stream, object, and generation",
            ));
        }
        if !update
            .set
            .explicit_errors
            .windows(2)
            .all(|pair| pair[0] < pair[1])
        {
            return Err(invalid("coverage errors must be strictly ordered"));
        }
        if !identities.insert(update.identity()) {
            return Err(invalid(
                "coverage commit replaces the same owner/domain/scope more than once",
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_preconditions(
    preconditions: &[DurableCoverageSetPrecondition],
) -> Result<(), EngineError> {
    if preconditions.len() > MAX_COVERAGE_SET_UPDATES {
        return Err(invalid(format!(
            "coverage precondition count exceeds {MAX_COVERAGE_SET_UPDATES}"
        )));
    }
    let mut identities = BTreeSet::new();
    for precondition in preconditions {
        if precondition.owner_id.trim().is_empty()
            || precondition.owner_id.len() > MAX_COVERAGE_OWNER_ID_BYTES
            || precondition.owner_scope_key.is_empty()
            || precondition.owner_scope_key.len() > MAX_COVERAGE_OWNER_SCOPE_BYTES
            || precondition.family.trim().is_empty()
            || precondition.family.len() > MAX_COVERAGE_OWNER_ID_BYTES
            || precondition.family_version == 0
            || precondition.adapter_id.trim().is_empty()
            || precondition.adapter_id.len() > MAX_COVERAGE_OWNER_ID_BYTES
            || precondition.canonical_source_instance_key.len() != 32
            || precondition.expected_content_digest.len() != 32
            || precondition.expected_last_commit_seq == 0
        {
            return Err(invalid(
                "coverage precondition has an empty, unbounded, or invalid field",
            ));
        }
        if !identities.insert((
            precondition.owner_id.as_str(),
            precondition.owner_scope_key.as_slice(),
            precondition.family.as_str(),
            precondition.family_version,
        )) {
            return Err(invalid("coverage precondition repeats one family scope"));
        }
    }
    Ok(())
}

pub(crate) fn assert_preconditions(
    transaction: &Transaction<'_>,
    source_instance_id: u64,
    preconditions: &[DurableCoverageSetPrecondition],
) -> Result<(), EngineError> {
    for precondition in preconditions {
        let matches = transaction
            .query_row(
                r#"
                SELECT 1
                FROM source_coverage_sets
                WHERE source_instance_id = ?1
                  AND owner_id = ?2
                  AND owner_scope_key = ?3
                  AND domain_kind = 'fact_family'
                  AND domain_name = ?4
                  AND domain_version = ?5
                  AND root_entity_key = X''
                  AND adapter_id = ?6
                  AND canonical_source_instance_key = ?7
                  AND content_digest = ?8
                  AND last_commit_seq = ?9
                "#,
                params![
                    sqlite_u64(source_instance_id, "coverage precondition source instance")?,
                    precondition.owner_id,
                    precondition.owner_scope_key,
                    precondition.family,
                    i64::from(precondition.family_version),
                    precondition.adapter_id,
                    precondition.canonical_source_instance_key,
                    precondition.expected_content_digest,
                    sqlite_u64(
                        precondition.expected_last_commit_seq,
                        "coverage precondition commit sequence"
                    )?,
                ],
                |_| Ok(()),
            )
            .optional()
            .map_err(|error| sqlite_error("check source coverage precondition", error))?
            .is_some();
        if !matches {
            return Err(EngineError::InvalidCommit(
                "fact-family replay authorization is stale or belongs to another scope".to_string(),
            ));
        }
    }
    Ok(())
}

fn strictly_sorted_by<T, K: Ord>(values: &[T], key: impl Fn(&T) -> K) -> bool {
    values.windows(2).all(|pair| key(&pair[0]) < key(&pair[1]))
}

pub(crate) fn updates_changed(
    transaction: &Transaction<'_>,
    updates: &[DurableCoverageSetUpdate],
) -> Result<bool, EngineError> {
    let mut statement = transaction
        .prepare_cached(
            r#"
            SELECT content_digest
            FROM source_coverage_sets
            WHERE owner_id = ?1
              AND owner_scope_key = ?2
              AND domain_kind = ?3
              AND domain_name = ?4
              AND domain_version = ?5
              AND root_entity_key = ?6
            "#,
        )
        .map_err(|error| sqlite_error("prepare source coverage comparison", error))?;
    for update in updates {
        let identity = update.identity();
        let current = statement
            .query_row(
                params![
                    identity.owner_id,
                    identity.owner_scope_key,
                    identity.domain_kind,
                    identity.domain_name,
                    i64::from(identity.domain_version),
                    identity.root_entity_key,
                ],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(|error| sqlite_error("compare source coverage set", error))?;
        if current.as_deref() != Some(content_digest(&update.set)?.as_slice()) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn replace_sets(
    transaction: &Transaction<'_>,
    source_instance_id: u64,
    commit_seq: u64,
    updated_at: i64,
    updates: &[DurableCoverageSetUpdate],
) -> Result<(), EngineError> {
    for update in updates {
        replace_set(
            transaction,
            source_instance_id,
            commit_seq,
            updated_at,
            update,
        )?;
    }
    Ok(())
}

fn replace_set(
    transaction: &Transaction<'_>,
    source_instance_id: u64,
    commit_seq: u64,
    updated_at: i64,
    update: &DurableCoverageSetUpdate,
) -> Result<(), EngineError> {
    let identity = update.identity();
    let set = &update.set;
    let content_digest = content_digest(set)?;
    let coverage_set_id: i64 = transaction
        .query_row(
            r#"
            INSERT INTO source_coverage_sets (
                source_instance_id, owner_id, owner_scope_key,
                coverage_set_contract_version, coverage_contract_version,
                domain_kind, domain_name, domain_version,
                adapter_id, canonical_source_instance_key, root_entity_key,
                support_release_id, declaration_digest, membership_revision,
                completeness, content_digest, last_commit_seq, updated_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18
            )
            ON CONFLICT(
                owner_id, owner_scope_key, domain_kind, domain_name,
                domain_version, root_entity_key
            ) DO UPDATE SET
                source_instance_id = excluded.source_instance_id,
                coverage_set_contract_version = excluded.coverage_set_contract_version,
                coverage_contract_version = excluded.coverage_contract_version,
                adapter_id = excluded.adapter_id,
                canonical_source_instance_key = excluded.canonical_source_instance_key,
                support_release_id = excluded.support_release_id,
                declaration_digest = excluded.declaration_digest,
                membership_revision = excluded.membership_revision,
                completeness = excluded.completeness,
                content_digest = excluded.content_digest,
                last_commit_seq = excluded.last_commit_seq,
                updated_at = excluded.updated_at
            RETURNING coverage_set_id
            "#,
            params![
                sqlite_u64(source_instance_id, "source coverage instance")?,
                identity.owner_id,
                identity.owner_scope_key,
                i64::from(set.coverage_set_contract_version),
                coverage_contract_version(set),
                identity.domain_kind,
                identity.domain_name,
                i64::from(identity.domain_version),
                set.scope.adapter_id,
                set.scope.source_instance_key.as_bytes().as_slice(),
                identity.root_entity_key,
                set.scope.support_release_id,
                set.scope
                    .source_or_scope_declaration_digest
                    .as_bytes()
                    .as_slice(),
                set.membership_revision.as_bytes().as_slice(),
                completeness_name(set.completeness),
                content_digest,
                sqlite_u64(commit_seq, "source coverage commit")?,
                updated_at,
            ],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("upsert source coverage set", error))?;

    transaction
        .execute(
            "DELETE FROM source_coverage_points WHERE coverage_set_id = ?1",
            [coverage_set_id],
        )
        .map_err(|error| sqlite_error("replace source coverage points", error))?;
    transaction
        .execute(
            "DELETE FROM source_coverage_absences WHERE coverage_set_id = ?1",
            [coverage_set_id],
        )
        .map_err(|error| sqlite_error("replace source coverage absences", error))?;
    transaction
        .execute(
            "DELETE FROM source_coverage_errors WHERE coverage_set_id = ?1",
            [coverage_set_id],
        )
        .map_err(|error| sqlite_error("replace source coverage errors", error))?;

    let mut point_statement = transaction
        .prepare_cached(
            r#"
            INSERT INTO source_coverage_points (
                coverage_set_id, stream_key, object_key, generation,
                position_kind, position_ref, monotonic_order,
                status, unavailable_reason, source_record_id,
                semantic_revision_ref, observed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            "#,
        )
        .map_err(|error| sqlite_error("prepare source coverage point", error))?;
    for point in &set.points {
        let (position_kind, position_ref, monotonic_order) = point
            .position
            .as_ref()
            .map(|position| {
                Ok((
                    Some(position_kind_name(position.kind)),
                    Some(position.opaque.as_bytes().as_slice()),
                    position
                        .monotonic_order
                        .map(|value| sqlite_u64(value, "coverage monotonic order"))
                        .transpose()?,
                ))
            })
            .transpose()?
            .unwrap_or((None, None, None));
        let (status, unavailable_reason) = coverage_status(&point.status);
        point_statement
            .execute(params![
                coverage_set_id,
                point.stream_key.as_bytes().as_slice(),
                point.object_key.as_bytes().as_slice(),
                sqlite_u64(point.generation, "coverage generation")?,
                position_kind,
                position_ref,
                monotonic_order,
                status,
                unavailable_reason,
                point
                    .provenance
                    .source_record_id
                    .as_ref()
                    .map(|value| value.as_bytes().as_slice()),
                point
                    .provenance
                    .semantic_revision_ref
                    .as_ref()
                    .map(|value| value.fact_revision_id.as_bytes().as_slice()),
                point.provenance.observed_at,
            ])
            .map_err(|error| sqlite_error("insert source coverage point", error))?;
    }

    let mut absence_statement = transaction
        .prepare_cached(
            r#"
            INSERT INTO source_coverage_absences (
                coverage_set_id, stream_key, object_key, generation, absence_kind
            ) VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
        )
        .map_err(|error| sqlite_error("prepare source coverage absence", error))?;
    for absence in &set.explicit_absence_or_deletion {
        absence_statement
            .execute(params![
                coverage_set_id,
                absence.stream_key.as_bytes().as_slice(),
                absence.object_key.as_bytes().as_slice(),
                sqlite_u64(absence.generation, "coverage absence generation")?,
                match absence.kind {
                    crate::adapter::CoverageAbsenceKind::Absent => "absent",
                    crate::adapter::CoverageAbsenceKind::Deleted => "deleted",
                },
            ])
            .map_err(|error| sqlite_error("insert source coverage absence", error))?;
    }

    let mut error_statement = transaction
        .prepare_cached(
            r#"
            INSERT INTO source_coverage_errors (
                coverage_set_id, error_ordinal, stream_key, object_key, error_code
            ) VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
        )
        .map_err(|error| sqlite_error("prepare source coverage error", error))?;
    for (ordinal, error) in set.explicit_errors.iter().enumerate() {
        let ordinal =
            u64::try_from(ordinal).map_err(|_| invalid("coverage error ordinal exceeds u64"))?;
        error_statement
            .execute(params![
                coverage_set_id,
                sqlite_u64(ordinal, "coverage error ordinal")?,
                error
                    .stream_key
                    .as_ref()
                    .map(|value| value.as_bytes().as_slice()),
                error
                    .object_key
                    .as_ref()
                    .map(|value| value.as_bytes().as_slice()),
                error.code,
            ])
            .map_err(|error| sqlite_error("insert source coverage error", error))?;
    }
    Ok(())
}

fn content_digest(set: &SourceCoverageSet) -> Result<[u8; 32], EngineError> {
    let payload = serde_json::to_vec(set).map_err(|error| {
        EngineError::InvalidCommit(format!("serialize source coverage set: {error}"))
    })?;
    Ok(*blake3::hash(&payload).as_bytes())
}

fn domain_parts(domain: &CoverageDomain) -> (&'static str, String, u32) {
    match domain {
        CoverageDomain::Decode => ("decode", String::new(), 0),
        CoverageDomain::FactFamily { family, version } => ("fact_family", family.clone(), *version),
        CoverageDomain::ProjectionPack { pack, version } => {
            ("projection_pack", pack.clone(), *version)
        }
    }
}

fn coverage_contract_version(set: &SourceCoverageSet) -> i64 {
    set.points.first().map_or(
        i64::from(crate::adapter::SOURCE_COVERAGE_CONTRACT_VERSION),
        |point| i64::from(point.coverage_contract_version),
    )
}

fn completeness_name(completeness: CoverageSetCompleteness) -> &'static str {
    match completeness {
        CoverageSetCompleteness::Complete => "complete",
        CoverageSetCompleteness::Partial => "partial",
        CoverageSetCompleteness::Unavailable => "unavailable",
    }
}

fn position_kind_name(kind: CoveragePositionKind) -> &'static str {
    match kind {
        CoveragePositionKind::AppendCursor => "append_cursor",
        CoveragePositionKind::DocumentRevision => "document_revision",
        CoveragePositionKind::SnapshotRevision => "snapshot_revision",
        CoveragePositionKind::DatabaseWatermark => "database_watermark",
        CoveragePositionKind::KeyRangeToken => "key_range_token",
    }
}

fn coverage_status(status: &CoverageStatus) -> (&'static str, Option<&str>) {
    match status {
        CoverageStatus::CompleteThrough => ("complete_through", None),
        CoverageStatus::ExactSnapshot => ("exact_snapshot", None),
        CoverageStatus::Partial => ("partial", None),
        CoverageStatus::Unavailable { reason } => ("unavailable", Some(reason.as_str())),
    }
}

fn sqlite_u64(value: u64, label: &'static str) -> Result<i64, EngineError> {
    i64::try_from(value)
        .map_err(|_| invalid(format!("{label} exceeds SQLite signed integer range")))
}

fn invalid(message: impl Into<String>) -> EngineError {
    EngineError::InvalidCommit(message.into())
}

fn sqlite_error(operation: &'static str, error: rusqlite::Error) -> EngineError {
    EngineError::Sqlite {
        operation,
        detail: error.to_string(),
    }
}
