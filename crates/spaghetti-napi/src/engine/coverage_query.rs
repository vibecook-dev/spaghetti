//! Bounded, snapshot-consistent RFC 012A fact-family coverage query.
//!
//! Public callers scope through an existing project/session identity. The
//! engine resolves that session's durable source instance and returns only
//! opaque common coverage identities—never native paths, object keys, or
//! adapter payloads.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};
use serde::{Deserialize, Serialize};

use super::query_identity::{decode_entity_id, PROJECT_ID_PREFIX, SESSION_ID_PREFIX};
use super::query_pool::read_committed_watermark;
use super::EngineError;

pub const FACT_FAMILY_COVERAGE_QUERY_CONTRACT_VERSION: u32 = 1;
pub const DEFAULT_FACT_FAMILY_COVERAGE_PAGE_LIMIT: u32 = 50;
pub const MAX_FACT_FAMILY_COVERAGE_PAGE_LIMIT: u32 = 200;
const MAX_COVERAGE_CURSOR_BYTES: usize = 32 * 1024;
const MAX_COVERAGE_IDENTIFIER_BYTES: usize = 256;
const OPAQUE_COVERAGE_REFERENCE_VERSION: &str = "v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactFamilyCoveragePageRequest {
    pub project_id: String,
    pub session_id: String,
    pub owner_id: String,
    pub family: String,
    pub family_version: u32,
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactFamilyCoverageSetSummary {
    pub coverage_set_contract_version: u32,
    pub coverage_contract_version: u32,
    pub adapter_id: String,
    pub source_instance_ref: String,
    pub support_release_id: String,
    pub declaration_ref: String,
    pub membership_revision_ref: String,
    pub completeness: String,
    pub content_digest_ref: String,
    pub last_commit_seq: u64,
    pub updated_at_unix_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactFamilyCoverageItem {
    pub kind: String,
    pub stream_ref: Option<String>,
    pub object_ref: Option<String>,
    pub generation: Option<u64>,
    pub position_kind: Option<String>,
    pub position_ref: Option<String>,
    pub monotonic_order: Option<u64>,
    pub status: Option<String>,
    pub unavailable_reason: Option<String>,
    pub source_record_ref: Option<String>,
    pub semantic_revision_ref: Option<String>,
    pub observed_at_unix_ms: Option<i64>,
    pub absence_kind: Option<String>,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactFamilyCoveragePage {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub status: String,
    pub project_id: String,
    pub session_id: String,
    pub owner_id: String,
    pub family: String,
    pub family_version: u32,
    pub coverage: Option<FactFamilyCoverageSetSummary>,
    pub items: Vec<FactFamilyCoverageItem>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FactFamilyCoverageCursor {
    version: u32,
    at_commit_seq: u64,
    project_id: String,
    session_id: String,
    owner_id: String,
    family: String,
    family_version: u32,
    last_kind: u8,
    last_stream_key: String,
    last_object_key: String,
    last_number: u64,
}

struct ValidatedCoverageRequest {
    project_key: Vec<u8>,
    session_key: Vec<u8>,
    cursor: Option<FactFamilyCoverageCursor>,
}

struct CoverageRow {
    item: FactFamilyCoverageItem,
    sort_kind: u8,
    sort_stream_key: Vec<u8>,
    sort_object_key: Vec<u8>,
    sort_number: u64,
}

pub(super) fn validate_fact_family_coverage_page(
    request: &FactFamilyCoveragePageRequest,
) -> Result<(), EngineError> {
    validate_request(request).map(|_| ())
}

pub(super) fn read_fact_family_coverage_page(
    connection: &Connection,
    request: &FactFamilyCoveragePageRequest,
) -> Result<FactFamilyCoveragePage, EngineError> {
    let validated = validate_request(request)?;
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin fact-family coverage snapshot", error))?;
    let watermark = read_committed_watermark(&transaction)?;
    if let Some(cursor) = &validated.cursor {
        if cursor.at_commit_seq != watermark {
            return Err(EngineError::InvalidQuery(format!(
                "fact-family coverage cursor expired at commit {}; current commit is {watermark}",
                cursor.at_commit_seq
            )));
        }
    }
    let (source_instance_id, owner_scope_key) =
        resolve_source_scope(&transaction, &validated.project_key, &validated.session_key)?;
    let coverage = read_coverage_set(&transaction, source_instance_id, &owner_scope_key, request)?;
    let Some((coverage_set_id, coverage)) = coverage else {
        finish_snapshot(transaction)?;
        return Ok(FactFamilyCoveragePage {
            contract_version: FACT_FAMILY_COVERAGE_QUERY_CONTRACT_VERSION,
            at_commit_seq: watermark,
            status: "not_materialized".to_string(),
            project_id: request.project_id.clone(),
            session_id: request.session_id.clone(),
            owner_id: request.owner_id.clone(),
            family: request.family.clone(),
            family_version: request.family_version,
            coverage: None,
            items: Vec::new(),
            next_cursor: None,
        });
    };

    let cursor_position = validated
        .cursor
        .as_ref()
        .map(decode_cursor_position)
        .transpose()?
        .unwrap_or((0, Vec::new(), Vec::new(), 0));
    let mut rows = read_coverage_items(
        &transaction,
        coverage_set_id,
        &cursor_position,
        request.limit,
    )?;
    let has_more = rows.len() > request.limit as usize;
    if has_more {
        rows.truncate(request.limit as usize);
    }
    let next_cursor = if has_more {
        rows.last()
            .map(|row| {
                encode_cursor(&FactFamilyCoverageCursor {
                    version: FACT_FAMILY_COVERAGE_QUERY_CONTRACT_VERSION,
                    at_commit_seq: watermark,
                    project_id: request.project_id.clone(),
                    session_id: request.session_id.clone(),
                    owner_id: request.owner_id.clone(),
                    family: request.family.clone(),
                    family_version: request.family_version,
                    last_kind: row.sort_kind,
                    last_stream_key: URL_SAFE_NO_PAD.encode(&row.sort_stream_key),
                    last_object_key: URL_SAFE_NO_PAD.encode(&row.sort_object_key),
                    last_number: row.sort_number,
                })
            })
            .transpose()?
    } else {
        None
    };
    let items = rows.into_iter().map(|row| row.item).collect();
    finish_snapshot(transaction)?;
    Ok(FactFamilyCoveragePage {
        contract_version: FACT_FAMILY_COVERAGE_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        status: "materialized".to_string(),
        project_id: request.project_id.clone(),
        session_id: request.session_id.clone(),
        owner_id: request.owner_id.clone(),
        family: request.family.clone(),
        family_version: request.family_version,
        coverage: Some(coverage),
        items,
        next_cursor,
    })
}

fn validate_request(
    request: &FactFamilyCoveragePageRequest,
) -> Result<ValidatedCoverageRequest, EngineError> {
    if !(1..=MAX_FACT_FAMILY_COVERAGE_PAGE_LIMIT).contains(&request.limit) {
        return Err(EngineError::InvalidQuery(format!(
            "fact-family coverage page limit must be between 1 and {MAX_FACT_FAMILY_COVERAGE_PAGE_LIMIT}, got {}",
            request.limit
        )));
    }
    for (label, value) in [
        ("coverage owner id", request.owner_id.as_str()),
        ("fact family", request.family.as_str()),
    ] {
        if value.trim().is_empty() || value.len() > MAX_COVERAGE_IDENTIFIER_BYTES {
            return Err(EngineError::InvalidQuery(format!(
                "{label} must be non-empty and at most {MAX_COVERAGE_IDENTIFIER_BYTES} bytes"
            )));
        }
    }
    if request.family_version == 0 {
        return Err(EngineError::InvalidQuery(
            "fact-family coverage version must be greater than zero".to_string(),
        ));
    }
    let project_key = decode_entity_id(&request.project_id, PROJECT_ID_PREFIX, "project id")?;
    let session_key = decode_entity_id(&request.session_id, SESSION_ID_PREFIX, "session id")?;
    let cursor = request.cursor.as_deref().map(decode_cursor).transpose()?;
    if let Some(cursor) = &cursor {
        if cursor.project_id != request.project_id
            || cursor.session_id != request.session_id
            || cursor.owner_id != request.owner_id
            || cursor.family != request.family
            || cursor.family_version != request.family_version
        {
            return Err(EngineError::InvalidQuery(
                "fact-family coverage cursor does not belong to this query scope".to_string(),
            ));
        }
        decode_cursor_position(cursor)?;
    }
    Ok(ValidatedCoverageRequest {
        project_key,
        session_key,
        cursor,
    })
}

fn resolve_source_scope(
    transaction: &Transaction<'_>,
    project_key: &[u8],
    session_key: &[u8],
) -> Result<(u64, Vec<u8>), EngineError> {
    transaction
        .query_row(
            r#"
            SELECT source.source_instance_id, source.stable_key
            FROM canonical_sessions AS session
            JOIN source_objects AS object
              ON object.source_object_id = session.source_object_id
            JOIN source_streams AS stream
              ON stream.source_stream_id = object.source_stream_id
            JOIN source_instances AS source
              ON source.source_instance_id = stream.source_instance_id
            WHERE session.project_key = ?1 AND session.session_key = ?2
            "#,
            params![project_key, session_key],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()
        .map_err(|error| query_sqlite_error("resolve fact-family coverage scope", error))?
        .map(|(source_instance_id, stable_key)| {
            Ok((
                nonnegative_u64(source_instance_id, "coverage source instance")?,
                stable_key,
            ))
        })
        .transpose()?
        .ok_or_else(|| {
            EngineError::InvalidQuery(
                "coverage session does not belong to the requested project".to_string(),
            )
        })
}

fn read_coverage_set(
    transaction: &Transaction<'_>,
    source_instance_id: u64,
    owner_scope_key: &[u8],
    request: &FactFamilyCoveragePageRequest,
) -> Result<Option<(i64, FactFamilyCoverageSetSummary)>, EngineError> {
    let row = transaction
        .query_row(
            r#"
            SELECT coverage_set_id, coverage_set_contract_version,
                   coverage_contract_version, adapter_id,
                   canonical_source_instance_key, support_release_id,
                   declaration_digest, membership_revision, completeness,
                   content_digest, last_commit_seq, updated_at
            FROM source_coverage_sets
            WHERE source_instance_id = ?1
              AND owner_id = ?2
              AND owner_scope_key = ?3
              AND domain_kind = 'fact_family'
              AND domain_name = ?4
              AND domain_version = ?5
              AND root_entity_key = X''
            "#,
            params![
                query_i64(source_instance_id, "coverage source instance")?,
                request.owner_id,
                owner_scope_key,
                request.family,
                i64::from(request.family_version),
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Vec<u8>>(6)?,
                    row.get::<_, Vec<u8>>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, Vec<u8>>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                ))
            },
        )
        .optional()
        .map_err(|error| query_sqlite_error("read fact-family coverage set", error))?;
    let Some((
        coverage_set_id,
        set_version,
        coverage_version,
        adapter_id,
        source_instance_key,
        support_release_id,
        declaration_digest,
        membership_revision,
        completeness,
        content_digest,
        last_commit_seq,
        updated_at,
    )) = row
    else {
        return Ok(None);
    };
    for (label, value) in [
        (
            "canonical source instance key",
            source_instance_key.as_slice(),
        ),
        ("coverage declaration digest", declaration_digest.as_slice()),
        (
            "coverage membership revision",
            membership_revision.as_slice(),
        ),
        ("coverage content digest", content_digest.as_slice()),
    ] {
        if value.len() != 32 {
            return Err(EngineError::Sqlite {
                operation: "validate fact-family coverage set",
                detail: format!("{label} is not a 32-byte common identity"),
            });
        }
    }
    if !matches!(
        completeness.as_str(),
        "complete" | "partial" | "unavailable"
    ) {
        return Err(EngineError::Sqlite {
            operation: "validate fact-family coverage set",
            detail: format!("unknown coverage completeness {completeness}"),
        });
    }
    Ok(Some((
        coverage_set_id,
        FactFamilyCoverageSetSummary {
            coverage_set_contract_version: nonnegative_u32(
                set_version,
                "coverage set contract version",
            )?,
            coverage_contract_version: nonnegative_u32(
                coverage_version,
                "coverage contract version",
            )?,
            adapter_id,
            source_instance_ref: opaque_ref(&source_instance_key),
            support_release_id,
            declaration_ref: opaque_ref(&declaration_digest),
            membership_revision_ref: opaque_ref(&membership_revision),
            completeness,
            content_digest_ref: opaque_ref(&content_digest),
            last_commit_seq: nonnegative_u64(last_commit_seq, "coverage commit sequence")?,
            updated_at_unix_ms: updated_at,
        },
    )))
}

fn read_coverage_items(
    transaction: &Transaction<'_>,
    coverage_set_id: i64,
    cursor: &(u8, Vec<u8>, Vec<u8>, u64),
    limit: u32,
) -> Result<Vec<CoverageRow>, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            WITH coverage_items AS (
              SELECT 0 AS item_kind, stream_key AS sort_stream,
                     object_key AS sort_object, generation AS sort_number,
                     stream_key, object_key, generation,
                     position_kind, position_ref, monotonic_order,
                     status, unavailable_reason, source_record_id,
                     semantic_revision_ref, observed_at,
                     NULL AS absence_kind, NULL AS error_code
              FROM source_coverage_points
              WHERE coverage_set_id = ?1
              UNION ALL
              SELECT 1 AS item_kind, stream_key AS sort_stream,
                     object_key AS sort_object, generation AS sort_number,
                     stream_key, object_key, generation,
                     NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
                     absence_kind, NULL
              FROM source_coverage_absences
              WHERE coverage_set_id = ?1
              UNION ALL
              SELECT 2 AS item_kind, COALESCE(stream_key, X'') AS sort_stream,
                     COALESCE(object_key, X'') AS sort_object,
                     error_ordinal AS sort_number,
                     stream_key, object_key, NULL,
                     NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
                     NULL, error_code
              FROM source_coverage_errors
              WHERE coverage_set_id = ?1
            )
            SELECT item_kind, sort_stream, sort_object, sort_number,
                   stream_key, object_key, generation,
                   position_kind, position_ref, monotonic_order,
                   status, unavailable_reason, source_record_id,
                   semantic_revision_ref, observed_at, absence_kind, error_code
            FROM coverage_items
            WHERE (item_kind, sort_stream, sort_object, sort_number) > (?2, ?3, ?4, ?5)
            ORDER BY item_kind, sort_stream, sort_object, sort_number
            LIMIT ?6
            "#,
        )
        .map_err(|error| query_sqlite_error("prepare fact-family coverage items", error))?;
    let rows = statement
        .query_map(
            params![
                coverage_set_id,
                i64::from(cursor.0),
                cursor.1,
                cursor.2,
                query_i64(cursor.3, "coverage cursor number")?,
                i64::from(limit.saturating_add(1)),
            ],
            coverage_row_from_sql,
        )
        .map_err(|error| query_sqlite_error("read fact-family coverage items", error))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| query_sqlite_error("decode fact-family coverage items", error))
}

fn coverage_row_from_sql(row: &Row<'_>) -> rusqlite::Result<CoverageRow> {
    let item_kind = row.get::<_, i64>(0)?;
    let sort_stream_key = row.get::<_, Vec<u8>>(1)?;
    let sort_object_key = row.get::<_, Vec<u8>>(2)?;
    let sort_number = row.get::<_, i64>(3)?;
    let stream_key = row.get::<_, Option<Vec<u8>>>(4)?;
    let object_key = row.get::<_, Option<Vec<u8>>>(5)?;
    let generation = row.get::<_, Option<i64>>(6)?;
    let position_kind = row.get::<_, Option<String>>(7)?;
    let position_ref = row.get::<_, Option<Vec<u8>>>(8)?;
    let monotonic_order = row.get::<_, Option<i64>>(9)?;
    let status = row.get::<_, Option<String>>(10)?;
    let unavailable_reason = row.get::<_, Option<String>>(11)?;
    let source_record_ref = row.get::<_, Option<Vec<u8>>>(12)?;
    let semantic_revision_ref = row.get::<_, Option<Vec<u8>>>(13)?;
    let observed_at_unix_ms = row.get::<_, Option<i64>>(14)?;
    let absence_kind = row.get::<_, Option<String>>(15)?;
    let error_code = row.get::<_, Option<String>>(16)?;
    let (kind, expected_key_length) = match item_kind {
        0 => ("point", true),
        1 => ("absence", true),
        2 => ("error", false),
        _ => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Integer,
                "unknown coverage item kind".into(),
            ));
        }
    };
    if expected_key_length
        && (stream_key.as_ref().is_none_or(|value| value.len() != 32)
            || object_key.as_ref().is_none_or(|value| value.len() != 32))
    {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Blob,
            "coverage point/absence has an invalid common key".into(),
        ));
    }
    for value in [
        stream_key.as_deref(),
        object_key.as_deref(),
        position_ref.as_deref(),
        source_record_ref.as_deref(),
        semantic_revision_ref.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if value.len() != 32 {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Blob,
                "coverage item has an invalid common reference".into(),
            ));
        }
    }
    Ok(CoverageRow {
        item: FactFamilyCoverageItem {
            kind: kind.to_string(),
            stream_ref: stream_key.as_deref().map(opaque_ref),
            object_ref: object_key.as_deref().map(opaque_ref),
            generation: generation
                .map(|value| nonnegative_u64_sql(value, "coverage generation"))
                .transpose()?,
            position_kind,
            position_ref: position_ref.as_deref().map(opaque_ref),
            monotonic_order: monotonic_order
                .map(|value| nonnegative_u64_sql(value, "coverage monotonic order"))
                .transpose()?,
            status,
            unavailable_reason,
            source_record_ref: source_record_ref.as_deref().map(opaque_ref),
            semantic_revision_ref: semantic_revision_ref.as_deref().map(opaque_ref),
            observed_at_unix_ms,
            absence_kind,
            error_code,
        },
        sort_kind: u8::try_from(item_kind).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Integer,
                Box::new(error),
            )
        })?,
        sort_stream_key,
        sort_object_key,
        sort_number: nonnegative_u64_sql(sort_number, "coverage sort number")?,
    })
}

fn encode_cursor(cursor: &FactFamilyCoverageCursor) -> Result<String, EngineError> {
    let payload = serde_json::to_vec(cursor).map_err(|error| {
        EngineError::InvalidQuery(format!(
            "could not encode fact-family coverage cursor: {error}"
        ))
    })?;
    if payload.len() > MAX_COVERAGE_CURSOR_BYTES {
        return Err(EngineError::InvalidQuery(
            "fact-family coverage cursor exceeds its encoded bound".to_string(),
        ));
    }
    Ok(URL_SAFE_NO_PAD.encode(payload))
}

fn decode_cursor(value: &str) -> Result<FactFamilyCoverageCursor, EngineError> {
    if value.is_empty() || value.len() > MAX_COVERAGE_CURSOR_BYTES * 2 {
        return Err(EngineError::InvalidQuery(
            "fact-family coverage cursor is empty or unbounded".to_string(),
        ));
    }
    let payload = URL_SAFE_NO_PAD.decode(value).map_err(|_| {
        EngineError::InvalidQuery("fact-family coverage cursor is not valid base64url".to_string())
    })?;
    if payload.len() > MAX_COVERAGE_CURSOR_BYTES {
        return Err(EngineError::InvalidQuery(
            "fact-family coverage cursor payload is unbounded".to_string(),
        ));
    }
    let cursor: FactFamilyCoverageCursor = serde_json::from_slice(&payload).map_err(|_| {
        EngineError::InvalidQuery("fact-family coverage cursor payload is invalid".to_string())
    })?;
    if cursor.version != FACT_FAMILY_COVERAGE_QUERY_CONTRACT_VERSION {
        return Err(EngineError::InvalidQuery(format!(
            "unsupported fact-family coverage cursor version {}",
            cursor.version
        )));
    }
    Ok(cursor)
}

fn decode_cursor_position(
    cursor: &FactFamilyCoverageCursor,
) -> Result<(u8, Vec<u8>, Vec<u8>, u64), EngineError> {
    if cursor.last_kind > 2 {
        return Err(EngineError::InvalidQuery(
            "fact-family coverage cursor has an invalid item kind".to_string(),
        ));
    }
    let stream_key = URL_SAFE_NO_PAD
        .decode(&cursor.last_stream_key)
        .map_err(|_| {
            EngineError::InvalidQuery("coverage cursor stream key is invalid".to_string())
        })?;
    let object_key = URL_SAFE_NO_PAD
        .decode(&cursor.last_object_key)
        .map_err(|_| {
            EngineError::InvalidQuery("coverage cursor object key is invalid".to_string())
        })?;
    if stream_key.len() > 32 || object_key.len() > 32 {
        return Err(EngineError::InvalidQuery(
            "coverage cursor contains an unbounded common key".to_string(),
        ));
    }
    Ok((cursor.last_kind, stream_key, object_key, cursor.last_number))
}

fn opaque_ref(value: &[u8]) -> String {
    format!(
        "{OPAQUE_COVERAGE_REFERENCE_VERSION}:{}",
        URL_SAFE_NO_PAD.encode(value)
    )
}

fn finish_snapshot(transaction: Transaction<'_>) -> Result<(), EngineError> {
    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish fact-family coverage snapshot", error))
}

fn query_i64(value: u64, label: &'static str) -> Result<i64, EngineError> {
    i64::try_from(value).map_err(|_| EngineError::InvalidQuery(format!("{label} exceeds SQLite")))
}

fn nonnegative_u64(value: i64, label: &'static str) -> Result<u64, EngineError> {
    u64::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "decode fact-family coverage",
        detail: format!("{label} is negative"),
    })
}

fn nonnegative_u32(value: i64, label: &'static str) -> Result<u32, EngineError> {
    u32::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "decode fact-family coverage",
        detail: format!("{label} is negative or unbounded"),
    })
}

fn nonnegative_u64_sql(value: i64, label: &'static str) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Integer,
            format!("{label}: {error}").into(),
        )
    })
}

fn query_sqlite_error(operation: &'static str, error: rusqlite::Error) -> EngineError {
    EngineError::Sqlite {
        operation,
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::query_identity::encode_entity_id;

    fn request() -> FactFamilyCoveragePageRequest {
        FactFamilyCoveragePageRequest {
            project_id: encode_entity_id(PROJECT_ID_PREFIX, b"project-key"),
            session_id: encode_entity_id(SESSION_ID_PREFIX, b"session-key"),
            owner_id: "runtime.usage-v2".to_string(),
            family: "runtime.usage-v2".to_string(),
            family_version: 1,
            cursor: None,
            limit: DEFAULT_FACT_FAMILY_COVERAGE_PAGE_LIMIT,
        }
    }

    #[test]
    fn request_validation_rejects_unbounded_and_incompatible_cursor_scope() {
        let mut invalid = request();
        invalid.limit = 0;
        assert!(matches!(
            validate_fact_family_coverage_page(&invalid),
            Err(EngineError::InvalidQuery(_))
        ));

        let cursor = FactFamilyCoverageCursor {
            version: FACT_FAMILY_COVERAGE_QUERY_CONTRACT_VERSION,
            at_commit_seq: 1,
            project_id: invalid.project_id.clone(),
            session_id: invalid.session_id.clone(),
            owner_id: invalid.owner_id.clone(),
            family: invalid.family.clone(),
            family_version: 1,
            last_kind: 0,
            last_stream_key: URL_SAFE_NO_PAD.encode([0_u8; 32]),
            last_object_key: URL_SAFE_NO_PAD.encode([1_u8; 32]),
            last_number: 1,
        };
        let mut cross_scope = request();
        cross_scope.owner_id = "another-owner".to_string();
        cross_scope.cursor = Some(encode_cursor(&cursor).unwrap());
        assert!(matches!(
            validate_fact_family_coverage_page(&cross_scope),
            Err(EngineError::InvalidQuery(_))
        ));
    }
}
