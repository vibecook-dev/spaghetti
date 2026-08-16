//! Read-only RFC 012C usage-v2 shadow query pack.
//!
//! The pack pages canonical response revisions and their current actor and
//! affiliation context. It never reads legacy additive usage rows and never
//! copies a response to materialize a team/workflow total.

use std::collections::{BTreeMap, BTreeSet};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row, Transaction};
use serde::{Deserialize, Serialize};

use super::query_identity::{decode_entity_id, PROJECT_ID_PREFIX, SESSION_ID_PREFIX};
use super::query_pool::read_committed_watermark;
use super::EngineError;

pub const RUNTIME_USAGE_V2_QUERY_CONTRACT_VERSION: u32 = 1;
pub const DEFAULT_RUNTIME_USAGE_V2_PAGE_LIMIT: u32 = 50;
pub const MAX_RUNTIME_USAGE_V2_PAGE_LIMIT: u32 = 200;
const MAX_RUNTIME_USAGE_V2_CURSOR_BYTES: usize = 32 * 1024;
const MAX_RUNTIME_USAGE_V2_AFFILIATIONS_PER_PAGE: usize = 6_400;
const OPAQUE_REFERENCE_VERSION: &str = "v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeUsageV2PageRequest {
    pub project_id: String,
    pub session_id: String,
    pub actor_run_ref: Option<String>,
    pub affiliation_dimension: Option<String>,
    pub affiliation_target_ref: Option<String>,
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeUsageV2ExternalEntityRef {
    pub external_entity_reference_version: u32,
    pub entity_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeUsageV2SemanticRevisionRef {
    pub semantic_reference_contract_version: u32,
    pub fact_revision_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeUsageV2ValueProvenance {
    pub native_field: String,
    pub normalization_contract_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeUsageV2TokenValue {
    pub value: Option<u64>,
    pub quality: String,
    pub authority: String,
    pub completeness: String,
    pub unknown_reason: Option<String>,
    pub effective_at: Option<i64>,
    pub provenance: RuntimeUsageV2ValueProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeUsageV2TextValue {
    pub value: Option<String>,
    pub quality: String,
    pub authority: String,
    pub completeness: String,
    pub unknown_reason: Option<String>,
    pub effective_at: Option<i64>,
    pub provenance: RuntimeUsageV2ValueProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeUsageV2Response {
    pub usage_key: String,
    pub semantic_revision_ref: RuntimeUsageV2SemanticRevisionRef,
    pub source_record_ref: String,
    pub session_ref: RuntimeUsageV2ExternalEntityRef,
    pub actor_run_ref: RuntimeUsageV2ExternalEntityRef,
    pub response_key_base64: String,
    pub response_identity: String,
    pub native_message_id: Option<String>,
    pub request_id: Option<String>,
    pub input_tokens: RuntimeUsageV2TokenValue,
    pub output_tokens: RuntimeUsageV2TokenValue,
    pub cache_creation_input_tokens: RuntimeUsageV2TokenValue,
    pub cache_read_input_tokens: RuntimeUsageV2TokenValue,
    pub model: Option<RuntimeUsageV2TextValue>,
    pub effort: Option<RuntimeUsageV2TextValue>,
    pub source_time: Option<String>,
    pub source_time_quality: Option<String>,
    pub observed_at_unix_ms: i64,
    pub source_generation: u64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeUsageV2Affiliation {
    pub affiliation_ref: RuntimeUsageV2ExternalEntityRef,
    pub semantic_revision_ref: RuntimeUsageV2SemanticRevisionRef,
    pub dimension: String,
    pub target_ref: RuntimeUsageV2ExternalEntityRef,
    pub member_ref: Option<RuntimeUsageV2ExternalEntityRef>,
    pub native_target_id: Option<String>,
    pub native_member_id: Option<String>,
    pub state: String,
    pub effective_at: Option<String>,
    pub effective_at_quality: Option<String>,
    pub observed_at_unix_ms: i64,
    pub source_generation: u64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeUsageV2ActorContext {
    pub actor_run_ref: RuntimeUsageV2ExternalEntityRef,
    pub semantic_revision_ref: RuntimeUsageV2SemanticRevisionRef,
    pub session_ref: RuntimeUsageV2ExternalEntityRef,
    pub role: String,
    pub parent_actor_run_ref: Option<RuntimeUsageV2ExternalEntityRef>,
    pub native_session_id: Option<String>,
    pub native_actor_id: Option<String>,
    pub native_actor_type: Option<String>,
    pub affiliations: Vec<RuntimeUsageV2Affiliation>,
    pub observed_at_unix_ms: i64,
    pub source_generation: u64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeUsageV2BucketAggregate {
    pub known_tokens: u64,
    pub known_response_count: u64,
    pub exact_response_count: u64,
    pub non_exact_response_count: u64,
    pub unknown_response_count: u64,
    pub completeness: &'static str,
}

impl Default for RuntimeUsageV2BucketAggregate {
    fn default() -> Self {
        Self {
            known_tokens: 0,
            known_response_count: 0,
            exact_response_count: 0,
            non_exact_response_count: 0,
            unknown_response_count: 0,
            completeness: "unknown",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeUsageV2Aggregate {
    pub response_count: u64,
    pub actor_count: u64,
    pub input_tokens: RuntimeUsageV2BucketAggregate,
    pub output_tokens: RuntimeUsageV2BucketAggregate,
    pub cache_creation_input_tokens: RuntimeUsageV2BucketAggregate,
    pub cache_read_input_tokens: RuntimeUsageV2BucketAggregate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeUsageV2Page {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub projection_status: String,
    pub project_id: String,
    pub session_id: String,
    pub session_ref: Option<RuntimeUsageV2ExternalEntityRef>,
    pub actor_run_ref: Option<String>,
    pub affiliation_dimension: Option<String>,
    pub affiliation_target_ref: Option<String>,
    pub aggregate: RuntimeUsageV2Aggregate,
    pub items: Vec<RuntimeUsageV2Response>,
    pub actors: Vec<RuntimeUsageV2ActorContext>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RuntimeUsageV2Cursor {
    version: u32,
    at_commit_seq: u64,
    project_id: String,
    session_id: String,
    actor_run_ref: Option<String>,
    affiliation_dimension: Option<String>,
    affiliation_target_ref: Option<String>,
    last_usage_key: String,
}

struct ValidatedRuntimeUsageV2Request {
    project_key: Vec<u8>,
    session_key: Vec<u8>,
    actor_run_key: Option<Vec<u8>>,
    affiliation_dimension: Option<String>,
    affiliation_target_key: Option<Vec<u8>>,
    cursor: Option<RuntimeUsageV2Cursor>,
}

struct RuntimeUsageV2Row {
    usage_key: Vec<u8>,
    actor_run_key: Vec<u8>,
    response: RuntimeUsageV2Response,
}

pub(super) fn validate_runtime_usage_v2_page(
    request: &RuntimeUsageV2PageRequest,
) -> Result<(), EngineError> {
    validate_request(request).map(|_| ())
}

pub(super) fn read_runtime_usage_v2_page(
    connection: &Connection,
    request: &RuntimeUsageV2PageRequest,
) -> Result<RuntimeUsageV2Page, EngineError> {
    let validated = validate_request(request)?;
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin runtime usage-v2 snapshot", error))?;
    let watermark = read_committed_watermark(&transaction)?;
    if let Some(cursor) = &validated.cursor {
        if cursor.at_commit_seq != watermark {
            return Err(EngineError::InvalidQuery(format!(
                "runtime usage-v2 cursor expired at commit {}; current commit is {watermark}",
                cursor.at_commit_seq
            )));
        }
    }
    require_session_membership(&transaction, &validated.project_key, &validated.session_key)?;
    let canonical_session_key = resolve_canonical_session_key(
        &transaction,
        &validated.project_key,
        &validated.session_key,
    )?;

    let Some(canonical_session_key) = canonical_session_key else {
        finish_snapshot(transaction)?;
        return Ok(RuntimeUsageV2Page {
            contract_version: RUNTIME_USAGE_V2_QUERY_CONTRACT_VERSION,
            at_commit_seq: watermark,
            projection_status: "not_materialized".to_string(),
            project_id: request.project_id.clone(),
            session_id: request.session_id.clone(),
            session_ref: None,
            actor_run_ref: request.actor_run_ref.clone(),
            affiliation_dimension: request.affiliation_dimension.clone(),
            affiliation_target_ref: request.affiliation_target_ref.clone(),
            aggregate: RuntimeUsageV2Aggregate::default(),
            items: Vec::new(),
            actors: Vec::new(),
            next_cursor: None,
        });
    };

    let aggregate = read_aggregate(&transaction, &canonical_session_key, &validated)?;
    let cursor_key = validated
        .cursor
        .as_ref()
        .map(|cursor| decode_cursor_usage_key(&cursor.last_usage_key))
        .transpose()?
        .unwrap_or_default();
    let mut rows = read_response_rows(
        &transaction,
        &canonical_session_key,
        &validated,
        &cursor_key,
        request.limit,
    )?;
    let has_more = rows.len() > request.limit as usize;
    if has_more {
        rows.truncate(request.limit as usize);
    }
    let actor_keys = rows
        .iter()
        .map(|row| row.actor_run_key.clone())
        .collect::<BTreeSet<_>>();
    let actors = read_actor_contexts(&transaction, &actor_keys)?;
    let next_cursor = if has_more {
        rows.last()
            .map(|row| {
                encode_cursor(&RuntimeUsageV2Cursor {
                    version: RUNTIME_USAGE_V2_QUERY_CONTRACT_VERSION,
                    at_commit_seq: watermark,
                    project_id: request.project_id.clone(),
                    session_id: request.session_id.clone(),
                    actor_run_ref: request.actor_run_ref.clone(),
                    affiliation_dimension: request.affiliation_dimension.clone(),
                    affiliation_target_ref: request.affiliation_target_ref.clone(),
                    last_usage_key: URL_SAFE_NO_PAD.encode(&row.usage_key),
                })
            })
            .transpose()?
    } else {
        None
    };
    let items = rows.into_iter().map(|row| row.response).collect();
    let session_ref = external_entity_ref(&canonical_session_key)
        .map_err(|error| query_sqlite_error("encode runtime usage-v2 session reference", error))?;
    finish_snapshot(transaction)?;

    Ok(RuntimeUsageV2Page {
        contract_version: RUNTIME_USAGE_V2_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        projection_status: "shadow".to_string(),
        project_id: request.project_id.clone(),
        session_id: request.session_id.clone(),
        session_ref: Some(session_ref),
        actor_run_ref: request.actor_run_ref.clone(),
        affiliation_dimension: request.affiliation_dimension.clone(),
        affiliation_target_ref: request.affiliation_target_ref.clone(),
        aggregate,
        items,
        actors,
        next_cursor,
    })
}

fn validate_request(
    request: &RuntimeUsageV2PageRequest,
) -> Result<ValidatedRuntimeUsageV2Request, EngineError> {
    if !(1..=MAX_RUNTIME_USAGE_V2_PAGE_LIMIT).contains(&request.limit) {
        return Err(EngineError::InvalidQuery(format!(
            "runtime usage-v2 page limit must be between 1 and {MAX_RUNTIME_USAGE_V2_PAGE_LIMIT}, got {}",
            request.limit
        )));
    }
    let project_key = decode_entity_id(&request.project_id, PROJECT_ID_PREFIX, "project id")?;
    let session_key = decode_entity_id(&request.session_id, SESSION_ID_PREFIX, "session id")?;
    let actor_run_key = request
        .actor_run_ref
        .as_deref()
        .map(|value| decode_opaque_reference(value, "actor run reference"))
        .transpose()?;
    let (affiliation_dimension, affiliation_target_key) = match (
        request.affiliation_dimension.as_deref(),
        request.affiliation_target_ref.as_deref(),
    ) {
        (None, None) => (None, None),
        (Some(dimension @ ("team" | "workflow")), Some(target)) => (
            Some(dimension.to_string()),
            Some(decode_opaque_reference(
                target,
                "affiliation target reference",
            )?),
        ),
        (Some(_), Some(_)) => {
            return Err(EngineError::InvalidQuery(
                "runtime usage-v2 affiliation dimension must be team or workflow".to_string(),
            ));
        }
        _ => {
            return Err(EngineError::InvalidQuery(
                "runtime usage-v2 affiliation dimension and target must be supplied together"
                    .to_string(),
            ));
        }
    };
    let cursor = request.cursor.as_deref().map(decode_cursor).transpose()?;
    if let Some(cursor) = &cursor {
        if cursor.project_id != request.project_id
            || cursor.session_id != request.session_id
            || cursor.actor_run_ref != request.actor_run_ref
            || cursor.affiliation_dimension != request.affiliation_dimension
            || cursor.affiliation_target_ref != request.affiliation_target_ref
        {
            return Err(EngineError::InvalidQuery(
                "runtime usage-v2 cursor does not belong to this query scope".to_string(),
            ));
        }
        decode_cursor_usage_key(&cursor.last_usage_key)?;
    }
    Ok(ValidatedRuntimeUsageV2Request {
        project_key,
        session_key,
        actor_run_key,
        affiliation_dimension,
        affiliation_target_key,
        cursor,
    })
}

fn require_session_membership(
    transaction: &Transaction<'_>,
    project_key: &[u8],
    session_key: &[u8],
) -> Result<(), EngineError> {
    let exists = transaction
        .query_row(
            "SELECT 1 FROM canonical_sessions WHERE project_key = ?1 AND session_key = ?2",
            params![project_key, session_key],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| query_sqlite_error("validate runtime usage-v2 session scope", error))?
        .is_some();
    if !exists {
        return Err(EngineError::InvalidQuery(
            "runtime usage-v2 session does not belong to the requested project".to_string(),
        ));
    }
    Ok(())
}

fn resolve_canonical_session_key(
    transaction: &Transaction<'_>,
    project_key: &[u8],
    session_key: &[u8],
) -> Result<Option<Vec<u8>>, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT DISTINCT actor.session_key
            FROM canonical_sessions AS session
            JOIN source_objects AS session_object
              ON session_object.source_object_id = session.source_object_id
            JOIN source_streams AS session_stream
              ON session_stream.source_stream_id = session_object.source_stream_id
            JOIN runtime_actor_runs_v2 AS actor
              ON actor.native_session_id = session.native_session_id
            JOIN source_objects AS actor_object
              ON actor_object.source_object_id = actor.source_object_id
            JOIN source_streams AS actor_stream
              ON actor_stream.source_stream_id = actor_object.source_stream_id
             AND actor_stream.source_instance_id = session_stream.source_instance_id
            WHERE session.project_key = ?1
              AND session.session_key = ?2
            ORDER BY actor.session_key
            LIMIT 2
            "#,
        )
        .map_err(|error| {
            query_sqlite_error("prepare canonical runtime session resolution", error)
        })?;
    let keys = statement
        .query_map(params![project_key, session_key], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .map_err(|error| query_sqlite_error("resolve canonical runtime session", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| query_sqlite_error("collect canonical runtime session", error))?;
    match keys.as_slice() {
        [] => Ok(None),
        [key] => Ok(Some(key.clone())),
        _ => Err(EngineError::InvalidQuery(
            "runtime usage-v2 session identity is ambiguous".to_string(),
        )),
    }
}

fn read_aggregate(
    transaction: &Transaction<'_>,
    canonical_session_key: &[u8],
    request: &ValidatedRuntimeUsageV2Request,
) -> Result<RuntimeUsageV2Aggregate, EngineError> {
    transaction
        .query_row(
            r#"
            SELECT COUNT(*), COUNT(DISTINCT usage.actor_run_key),
                   COALESCE(SUM(usage.input_tokens), 0),
                   COUNT(usage.input_tokens),
                   COALESCE(SUM(CASE WHEN input_q.quality = 'exact' AND usage.input_tokens IS NOT NULL THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(usage.output_tokens), 0),
                   COUNT(usage.output_tokens),
                   COALESCE(SUM(CASE WHEN output_q.quality = 'exact' AND usage.output_tokens IS NOT NULL THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(usage.cache_creation_input_tokens), 0),
                   COUNT(usage.cache_creation_input_tokens),
                   COALESCE(SUM(CASE WHEN cache_create_q.quality = 'exact' AND usage.cache_creation_input_tokens IS NOT NULL THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(usage.cache_read_input_tokens), 0),
                   COUNT(usage.cache_read_input_tokens),
                   COALESCE(SUM(CASE WHEN cache_read_q.quality = 'exact' AND usage.cache_read_input_tokens IS NOT NULL THEN 1 ELSE 0 END), 0)
            FROM usage_v2_response_contributions AS usage
            JOIN runtime_actor_runs_v2 AS actor
              ON actor.actor_run_key = usage.actor_run_key
             AND actor.session_key = usage.session_key
            JOIN usage_v2_qualification_specs AS input_q
              ON input_q.qualification_key = usage.input_qualification_key
            JOIN usage_v2_qualification_specs AS output_q
              ON output_q.qualification_key = usage.output_qualification_key
            JOIN usage_v2_qualification_specs AS cache_create_q
              ON cache_create_q.qualification_key = usage.cache_creation_qualification_key
            JOIN usage_v2_qualification_specs AS cache_read_q
              ON cache_read_q.qualification_key = usage.cache_read_qualification_key
            WHERE usage.session_key = ?1
              AND (?2 IS NULL OR usage.actor_run_key = ?2)
              AND (
                ?3 IS NULL OR EXISTS (
                  SELECT 1
                  FROM runtime_actor_affiliations_v2 AS affiliation
                  WHERE affiliation.actor_run_key = usage.actor_run_key
                    AND affiliation.session_key = usage.session_key
                    AND affiliation.dimension = ?3
                    AND affiliation.target_key = ?4
                    AND affiliation.state = 'present'
                )
              )
            "#,
            params![
                canonical_session_key,
                request.actor_run_key,
                request.affiliation_dimension,
                request.affiliation_target_key,
            ],
            |row| {
                let response_count = nonnegative_u64(row.get(0)?, "usage-v2 response count")?;
                Ok(RuntimeUsageV2Aggregate {
                    response_count,
                    actor_count: nonnegative_u64(row.get(1)?, "usage-v2 actor count")?,
                    input_tokens: bucket_aggregate_from_row(row, 2, response_count)?,
                    output_tokens: bucket_aggregate_from_row(row, 5, response_count)?,
                    cache_creation_input_tokens: bucket_aggregate_from_row(
                        row,
                        8,
                        response_count,
                    )?,
                    cache_read_input_tokens: bucket_aggregate_from_row(
                        row,
                        11,
                        response_count,
                    )?,
                })
            },
        )
        .map_err(|error| query_sqlite_error("read runtime usage-v2 aggregate", error))
}

fn bucket_aggregate_from_row(
    row: &Row<'_>,
    offset: usize,
    response_count: u64,
) -> rusqlite::Result<RuntimeUsageV2BucketAggregate> {
    let known_tokens = nonnegative_u64(row.get(offset)?, "usage-v2 known token total")?;
    let known_response_count =
        nonnegative_u64(row.get(offset + 1)?, "usage-v2 known response count")?;
    let exact_response_count =
        nonnegative_u64(row.get(offset + 2)?, "usage-v2 exact response count")?;
    let unknown_response_count = response_count
        .checked_sub(known_response_count)
        .ok_or_else(integral_value_error)?;
    let non_exact_response_count = known_response_count
        .checked_sub(exact_response_count)
        .ok_or_else(integral_value_error)?;
    let completeness = if response_count == 0 || known_response_count == response_count {
        "complete"
    } else if known_response_count == 0 {
        "unknown"
    } else {
        "partial"
    };
    Ok(RuntimeUsageV2BucketAggregate {
        known_tokens,
        known_response_count,
        exact_response_count,
        non_exact_response_count,
        unknown_response_count,
        completeness,
    })
}

fn read_response_rows(
    transaction: &Transaction<'_>,
    canonical_session_key: &[u8],
    request: &ValidatedRuntimeUsageV2Request,
    cursor_key: &[u8],
    limit: u32,
) -> Result<Vec<RuntimeUsageV2Row>, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT usage.usage_key, usage.fact_revision_id,
                   usage.source_record_id, usage.session_key,
                   usage.actor_run_key, usage.response_key,
                   usage.response_identity, usage.native_message_id,
                   usage.request_id,
                   usage.input_tokens, input_q.quality,
                   input_q.completeness, input_q.unknown_reason,
                   input_q.authority, input_q.native_field,
                   input_q.normalization_contract_version,
                   usage.input_effective_at,
                   usage.output_tokens, output_q.quality,
                   output_q.completeness, output_q.unknown_reason,
                   output_q.authority, output_q.native_field,
                   output_q.normalization_contract_version,
                   usage.output_effective_at,
                   usage.cache_creation_input_tokens, cache_create_q.quality,
                   cache_create_q.completeness, cache_create_q.unknown_reason,
                   cache_create_q.authority, cache_create_q.native_field,
                   cache_create_q.normalization_contract_version,
                   usage.cache_creation_effective_at,
                   usage.cache_read_input_tokens, cache_read_q.quality,
                   cache_read_q.completeness, cache_read_q.unknown_reason,
                   cache_read_q.authority, cache_read_q.native_field,
                   cache_read_q.normalization_contract_version,
                   usage.cache_read_effective_at,
                   usage.model, model_q.quality, model_q.completeness,
                   model_q.unknown_reason, model_q.authority,
                   model_q.native_field, model_q.normalization_contract_version,
                   usage.model_effective_at,
                   usage.effort, effort_q.quality, effort_q.completeness,
                   effort_q.unknown_reason, effort_q.authority,
                   effort_q.native_field, effort_q.normalization_contract_version,
                   usage.effort_effective_at,
                   usage.source_time, usage.source_time_quality,
                   fact.observed_at, usage.source_generation,
                   usage.last_commit_seq
            FROM usage_v2_response_contributions AS usage
            JOIN runtime_actor_runs_v2 AS actor
              ON actor.actor_run_key = usage.actor_run_key
             AND actor.session_key = usage.session_key
            JOIN fact_records AS fact ON fact.fact_id = usage.fact_id
            JOIN usage_v2_qualification_specs AS input_q
              ON input_q.qualification_key = usage.input_qualification_key
            JOIN usage_v2_qualification_specs AS output_q
              ON output_q.qualification_key = usage.output_qualification_key
            JOIN usage_v2_qualification_specs AS cache_create_q
              ON cache_create_q.qualification_key = usage.cache_creation_qualification_key
            JOIN usage_v2_qualification_specs AS cache_read_q
              ON cache_read_q.qualification_key = usage.cache_read_qualification_key
            LEFT JOIN usage_v2_qualification_specs AS model_q
              ON model_q.qualification_key = usage.model_qualification_key
            LEFT JOIN usage_v2_qualification_specs AS effort_q
              ON effort_q.qualification_key = usage.effort_qualification_key
            WHERE usage.session_key = ?1
              AND (?2 IS NULL OR usage.actor_run_key = ?2)
              AND (
                ?3 IS NULL OR EXISTS (
                  SELECT 1
                  FROM runtime_actor_affiliations_v2 AS affiliation
                  WHERE affiliation.actor_run_key = usage.actor_run_key
                    AND affiliation.session_key = usage.session_key
                    AND affiliation.dimension = ?3
                    AND affiliation.target_key = ?4
                    AND affiliation.state = 'present'
                )
              )
              AND usage.usage_key > ?5
            ORDER BY usage.usage_key
            LIMIT ?6
            "#,
        )
        .map_err(|error| query_sqlite_error("prepare runtime usage-v2 page", error))?;
    let rows = statement
        .query_map(
            params![
                canonical_session_key,
                request.actor_run_key,
                request.affiliation_dimension,
                request.affiliation_target_key,
                cursor_key,
                i64::from(limit) + 1,
            ],
            runtime_usage_v2_row_from_sql,
        )
        .map_err(|error| query_sqlite_error("read runtime usage-v2 page", error))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| query_sqlite_error("collect runtime usage-v2 page", error))
}

fn runtime_usage_v2_row_from_sql(row: &Row<'_>) -> rusqlite::Result<RuntimeUsageV2Row> {
    let usage_key = digest(row, 0, "usage key")?;
    let fact_revision_id = digest(row, 1, "usage fact revision")?;
    let source_record_id = digest(row, 2, "usage source record")?;
    let session_key = digest(row, 3, "usage session")?;
    let actor_run_key = digest(row, 4, "usage actor run")?;
    let response_key = row.get::<_, Vec<u8>>(5)?;
    if response_key.is_empty() {
        return Err(integral_value_error());
    }
    let response = RuntimeUsageV2Response {
        usage_key: encode_opaque_reference(&usage_key)?,
        semantic_revision_ref: semantic_revision_ref(&fact_revision_id)?,
        source_record_ref: encode_opaque_reference(&source_record_id)?,
        session_ref: external_entity_ref(&session_key)?,
        actor_run_ref: external_entity_ref(&actor_run_key)?,
        response_key_base64: URL_SAFE_NO_PAD.encode(response_key),
        response_identity: row.get(6)?,
        native_message_id: row.get(7)?,
        request_id: row.get(8)?,
        input_tokens: token_value_from_row(row, 9)?,
        output_tokens: token_value_from_row(row, 17)?,
        cache_creation_input_tokens: token_value_from_row(row, 25)?,
        cache_read_input_tokens: token_value_from_row(row, 33)?,
        model: text_value_from_row(row, 41)?,
        effort: text_value_from_row(row, 49)?,
        source_time: row.get(57)?,
        source_time_quality: row.get(58)?,
        observed_at_unix_ms: row.get(59)?,
        source_generation: nonnegative_u64(row.get(60)?, "usage source generation")?,
        last_commit_seq: nonnegative_u64(row.get(61)?, "usage last commit sequence")?,
    };
    Ok(RuntimeUsageV2Row {
        usage_key,
        actor_run_key,
        response,
    })
}

fn token_value_from_row(
    row: &Row<'_>,
    offset: usize,
) -> rusqlite::Result<RuntimeUsageV2TokenValue> {
    Ok(RuntimeUsageV2TokenValue {
        value: optional_nonnegative_u64(row.get(offset)?, "qualified usage token value")?,
        quality: row.get(offset + 1)?,
        completeness: row.get(offset + 2)?,
        unknown_reason: row.get(offset + 3)?,
        authority: row.get(offset + 4)?,
        provenance: RuntimeUsageV2ValueProvenance {
            native_field: row.get(offset + 5)?,
            normalization_contract_version: nonnegative_u32(
                row.get(offset + 6)?,
                "usage normalization contract version",
            )?,
        },
        effective_at: row.get(offset + 7)?,
    })
}

fn text_value_from_row(
    row: &Row<'_>,
    offset: usize,
) -> rusqlite::Result<Option<RuntimeUsageV2TextValue>> {
    let quality = row.get::<_, Option<String>>(offset + 1)?;
    let Some(quality) = quality else {
        return Ok(None);
    };
    Ok(Some(RuntimeUsageV2TextValue {
        value: row.get(offset)?,
        quality,
        completeness: row.get(offset + 2)?,
        unknown_reason: row.get(offset + 3)?,
        authority: row.get(offset + 4)?,
        provenance: RuntimeUsageV2ValueProvenance {
            native_field: row.get(offset + 5)?,
            normalization_contract_version: nonnegative_u32(
                row.get(offset + 6)?,
                "usage normalization contract version",
            )?,
        },
        effective_at: row.get(offset + 7)?,
    }))
}

fn read_actor_contexts(
    transaction: &Transaction<'_>,
    actor_keys: &BTreeSet<Vec<u8>>,
) -> Result<Vec<RuntimeUsageV2ActorContext>, EngineError> {
    if actor_keys.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = std::iter::repeat_n("?", actor_keys.len())
        .collect::<Vec<_>>()
        .join(",");
    let actor_sql = format!(
        r#"
        SELECT actor.actor_run_key, actor.fact_revision_id, actor.session_key,
               actor.role, actor.parent_actor_run_key, actor.native_session_id,
               actor.native_actor_id, actor.native_actor_type, fact.observed_at,
               actor.source_generation, actor.last_commit_seq
        FROM runtime_actor_runs_v2 AS actor
        JOIN fact_records AS fact ON fact.fact_id = actor.fact_id
        WHERE actor.actor_run_key IN ({placeholders})
        ORDER BY actor.actor_run_key
        "#
    );
    let mut actor_statement = transaction
        .prepare(&actor_sql)
        .map_err(|error| query_sqlite_error("prepare runtime usage-v2 actors", error))?;
    let actor_rows = actor_statement
        .query_map(params_from_iter(actor_keys.iter()), |row| {
            let actor_run_key = digest(row, 0, "actor run key")?;
            let fact_revision_id = digest(row, 1, "actor fact revision")?;
            let session_key = digest(row, 2, "actor session key")?;
            let parent_actor_run_key = row.get::<_, Option<Vec<u8>>>(4)?;
            if parent_actor_run_key
                .as_ref()
                .is_some_and(|key| key.len() != 32)
            {
                return Err(integral_value_error());
            }
            Ok((
                actor_run_key.clone(),
                RuntimeUsageV2ActorContext {
                    actor_run_ref: external_entity_ref(&actor_run_key)?,
                    semantic_revision_ref: semantic_revision_ref(&fact_revision_id)?,
                    session_ref: external_entity_ref(&session_key)?,
                    role: row.get(3)?,
                    parent_actor_run_ref: parent_actor_run_key
                        .as_deref()
                        .map(external_entity_ref)
                        .transpose()?,
                    native_session_id: row.get(5)?,
                    native_actor_id: row.get(6)?,
                    native_actor_type: row.get(7)?,
                    affiliations: Vec::new(),
                    observed_at_unix_ms: row.get(8)?,
                    source_generation: nonnegative_u64(row.get(9)?, "actor source generation")?,
                    last_commit_seq: nonnegative_u64(row.get(10)?, "actor last commit sequence")?,
                },
            ))
        })
        .map_err(|error| query_sqlite_error("read runtime usage-v2 actors", error))?
        .collect::<Result<BTreeMap<_, _>, _>>()
        .map_err(|error| query_sqlite_error("collect runtime usage-v2 actors", error))?;
    if actor_rows.len() != actor_keys.len() {
        return Err(EngineError::InvalidQuery(
            "runtime usage-v2 response references a missing actor context".to_string(),
        ));
    }

    let affiliation_sql = format!(
        r#"
        SELECT affiliation.actor_run_key, affiliation.affiliation_key,
               affiliation.fact_revision_id, affiliation.dimension,
               affiliation.target_key, affiliation.member_key,
               affiliation.native_target_id, affiliation.native_member_id,
               affiliation.state, affiliation.effective_at,
               affiliation.effective_at_quality, fact.observed_at,
               affiliation.source_generation, affiliation.last_commit_seq
        FROM runtime_actor_affiliations_v2 AS affiliation
        JOIN fact_records AS fact ON fact.fact_id = affiliation.fact_id
        WHERE affiliation.actor_run_key IN ({placeholders})
        ORDER BY affiliation.actor_run_key, affiliation.dimension,
                 affiliation.target_key, affiliation.affiliation_key
        LIMIT ?
        "#
    );
    let mut affiliation_params = actor_keys
        .iter()
        .map(|key| rusqlite::types::Value::Blob(key.clone()))
        .collect::<Vec<_>>();
    affiliation_params.push(rusqlite::types::Value::Integer(
        i64::try_from(MAX_RUNTIME_USAGE_V2_AFFILIATIONS_PER_PAGE + 1)
            .map_err(|_| EngineError::InvalidQuery("affiliation bound overflowed".to_string()))?,
    ));
    let mut affiliation_statement = transaction
        .prepare(&affiliation_sql)
        .map_err(|error| query_sqlite_error("prepare runtime usage-v2 affiliations", error))?;
    let affiliations = affiliation_statement
        .query_map(params_from_iter(affiliation_params), |row| {
            let actor_run_key = digest(row, 0, "affiliation actor key")?;
            let affiliation_key = digest(row, 1, "affiliation key")?;
            let fact_revision_id = digest(row, 2, "affiliation fact revision")?;
            let target_key = digest(row, 4, "affiliation target key")?;
            let member_key = row.get::<_, Option<Vec<u8>>>(5)?;
            if member_key.as_ref().is_some_and(|key| key.len() != 32) {
                return Err(integral_value_error());
            }
            Ok((
                actor_run_key,
                RuntimeUsageV2Affiliation {
                    affiliation_ref: external_entity_ref(&affiliation_key)?,
                    semantic_revision_ref: semantic_revision_ref(&fact_revision_id)?,
                    dimension: row.get(3)?,
                    target_ref: external_entity_ref(&target_key)?,
                    member_ref: member_key.as_deref().map(external_entity_ref).transpose()?,
                    native_target_id: row.get(6)?,
                    native_member_id: row.get(7)?,
                    state: row.get(8)?,
                    effective_at: row.get(9)?,
                    effective_at_quality: row.get(10)?,
                    observed_at_unix_ms: row.get(11)?,
                    source_generation: nonnegative_u64(
                        row.get(12)?,
                        "affiliation source generation",
                    )?,
                    last_commit_seq: nonnegative_u64(
                        row.get(13)?,
                        "affiliation last commit sequence",
                    )?,
                },
            ))
        })
        .map_err(|error| query_sqlite_error("read runtime usage-v2 affiliations", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| query_sqlite_error("collect runtime usage-v2 affiliations", error))?;
    if affiliations.len() > MAX_RUNTIME_USAGE_V2_AFFILIATIONS_PER_PAGE {
        return Err(EngineError::InvalidQuery(format!(
            "runtime usage-v2 page exceeds {MAX_RUNTIME_USAGE_V2_AFFILIATIONS_PER_PAGE} actor affiliations"
        )));
    }
    let mut actor_rows = actor_rows;
    for (actor_key, affiliation) in affiliations {
        let actor = actor_rows.get_mut(&actor_key).ok_or_else(|| {
            EngineError::InvalidQuery(
                "runtime usage-v2 affiliation references an unpaged actor".to_string(),
            )
        })?;
        actor.affiliations.push(affiliation);
    }
    Ok(actor_rows.into_values().collect())
}

fn external_entity_ref(bytes: &[u8]) -> rusqlite::Result<RuntimeUsageV2ExternalEntityRef> {
    Ok(RuntimeUsageV2ExternalEntityRef {
        external_entity_reference_version: 1,
        entity_key: encode_opaque_reference(bytes)?,
    })
}

fn semantic_revision_ref(bytes: &[u8]) -> rusqlite::Result<RuntimeUsageV2SemanticRevisionRef> {
    Ok(RuntimeUsageV2SemanticRevisionRef {
        semantic_reference_contract_version: 1,
        fact_revision_id: encode_opaque_reference(bytes)?,
    })
}

fn encode_opaque_reference(bytes: &[u8]) -> rusqlite::Result<String> {
    if bytes.len() != 32 {
        return Err(integral_value_error());
    }
    Ok(format!(
        "{OPAQUE_REFERENCE_VERSION}:{}",
        URL_SAFE_NO_PAD.encode(bytes)
    ))
}

fn decode_opaque_reference(value: &str, label: &str) -> Result<Vec<u8>, EngineError> {
    let prefix = format!("{OPAQUE_REFERENCE_VERSION}:");
    let payload = value.strip_prefix(&prefix).ok_or_else(|| {
        EngineError::InvalidQuery(format!("{label} has an unsupported encoding version"))
    })?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| EngineError::InvalidQuery(format!("{label} is malformed")))?;
    if bytes.len() != 32 {
        return Err(EngineError::InvalidQuery(format!(
            "{label} must contain exactly 32 digest bytes"
        )));
    }
    Ok(bytes)
}

fn digest(row: &Row<'_>, index: usize, _label: &'static str) -> rusqlite::Result<Vec<u8>> {
    let value = row.get::<_, Vec<u8>>(index)?;
    if value.len() != 32 {
        return Err(integral_value_error());
    }
    Ok(value)
}

fn encode_cursor(cursor: &RuntimeUsageV2Cursor) -> Result<String, EngineError> {
    let bytes = serde_json::to_vec(cursor).map_err(|error| {
        EngineError::InvalidQuery(format!("could not encode runtime usage-v2 cursor: {error}"))
    })?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_cursor(value: &str) -> Result<RuntimeUsageV2Cursor, EngineError> {
    if value.len() > MAX_RUNTIME_USAGE_V2_CURSOR_BYTES {
        return Err(EngineError::InvalidQuery(
            "runtime usage-v2 cursor exceeds the supported bound".to_string(),
        ));
    }
    let bytes = URL_SAFE_NO_PAD.decode(value).map_err(|_| {
        EngineError::InvalidQuery("runtime usage-v2 cursor is malformed".to_string())
    })?;
    let cursor: RuntimeUsageV2Cursor = serde_json::from_slice(&bytes).map_err(|_| {
        EngineError::InvalidQuery("runtime usage-v2 cursor payload is malformed".to_string())
    })?;
    if cursor.version != RUNTIME_USAGE_V2_QUERY_CONTRACT_VERSION {
        return Err(EngineError::InvalidQuery(format!(
            "unsupported runtime usage-v2 cursor version {}",
            cursor.version
        )));
    }
    Ok(cursor)
}

fn decode_cursor_usage_key(value: &str) -> Result<Vec<u8>, EngineError> {
    let bytes = URL_SAFE_NO_PAD.decode(value).map_err(|_| {
        EngineError::InvalidQuery("runtime usage-v2 cursor usage key is malformed".to_string())
    })?;
    if bytes.len() != 32 {
        return Err(EngineError::InvalidQuery(
            "runtime usage-v2 cursor usage key must contain 32 bytes".to_string(),
        ));
    }
    Ok(bytes)
}

fn finish_snapshot(transaction: Transaction<'_>) -> Result<(), EngineError> {
    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish runtime usage-v2 snapshot", error))
}

fn nonnegative_u64(value: i64, _field: &'static str) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|_| integral_value_error())
}

fn optional_nonnegative_u64(
    value: Option<i64>,
    field: &'static str,
) -> rusqlite::Result<Option<u64>> {
    value.map(|value| nonnegative_u64(value, field)).transpose()
}

fn nonnegative_u32(value: i64, _field: &'static str) -> rusqlite::Result<u32> {
    u32::try_from(value).map_err(|_| integral_value_error())
}

fn integral_value_error() -> rusqlite::Error {
    rusqlite::Error::IntegralValueOutOfRange(0, -1)
}

fn query_sqlite_error(operation: &'static str, error: rusqlite::Error) -> EngineError {
    EngineError::Sqlite {
        operation,
        detail: error.to_string(),
    }
}
