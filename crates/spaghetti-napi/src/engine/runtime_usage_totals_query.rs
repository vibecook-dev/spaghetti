//! Snapshot-consistent aggregate negotiation for RFC 012C runtime usage.
//!
//! This query accepts several non-overlapping canonical scopes so a composite
//! project cannot accidentally add legacy rows from one source to usage-v2
//! response revisions from another. Selection is resolved before any aggregate
//! arm is returned.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row, Transaction};

use super::query_identity::{decode_entity_id, PROJECT_ID_PREFIX, SESSION_ID_PREFIX};
use super::query_pool::read_committed_watermark;
use super::runtime_semantic_projection::{USAGE_V2_PROJECTION_ID, USAGE_V2_PROJECTION_VERSION};
use super::runtime_usage_query::{
    bucket_aggregate_from_row, opaque_ref, read_projection_readiness, read_query_selection,
    RuntimeUsageQuerySelection, RuntimeUsageQuerySelectionValue, RuntimeUsageSourceScope,
    RuntimeUsageV2Aggregate, RuntimeUsageV2ProjectionReadiness, LEGACY_USAGE_QUERY_ID,
    RUNTIME_USAGE_V2_QUERY_CONTRACT_VERSION, RUNTIME_USAGE_V2_QUERY_ID,
};
use super::usage_query::{
    token_values_from_row, usage_aggregate, usage_coverage_from_row, UsageAggregate,
    UsageCoverageSummary, UsageMetadata, UsageScopeRequest,
};
use super::EngineError;

pub const RUNTIME_USAGE_TOTALS_QUERY_CONTRACT_VERSION: u32 = 1;
pub const RUNTIME_USAGE_COMPATIBILITY_QUERY_CONTRACT_VERSION: u32 = 1;
pub const MAX_RUNTIME_USAGE_TOTALS_SCOPES: usize = 128;
pub const SELECTED_RUNTIME_USAGE_QUERY_ID: &str = "selected";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeUsageTotalsRequest {
    pub scopes: Vec<UsageScopeRequest>,
    pub requested_query_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeUsageTotalsSelectionScope {
    /// Query-local opaque identity for one vector member. This is deliberately
    /// distinct from RFC 012A's canonical source-instance reference.
    pub selection_scope_ref: String,
    pub adapter_id: String,
    pub session_count: u64,
    pub query_selection: RuntimeUsageQuerySelection,
    pub projection_readiness: RuntimeUsageV2ProjectionReadiness,
    pub coverage_status: String,
    pub v2_eligible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeUsageLegacyTotals {
    pub aggregate: UsageAggregate,
    pub coverage: Vec<UsageCoverageSummary>,
    pub first_source_time: Option<String>,
    pub last_source_time: Option<String>,
    pub first_observed_at_unix_ms: Option<i64>,
    pub last_observed_at_unix_ms: Option<i64>,
    pub last_commit_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeUsageTotalsReport {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub requested_query_id: String,
    pub status: String,
    pub resolved_query: Option<RuntimeUsageQuerySelectionValue>,
    pub scopes: Vec<UsageScopeRequest>,
    pub selection_vector: Vec<RuntimeUsageTotalsSelectionScope>,
    pub legacy: Option<RuntimeUsageLegacyTotals>,
    pub usage_v2: Option<RuntimeUsageV2Aggregate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeUsageCompatibilityRequest {
    pub scopes: Vec<UsageScopeRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeUsageCompatibilityBucket {
    pub legacy_exact_tokens: u64,
    pub legacy_estimated_tokens: u64,
    pub legacy_combined_tokens: u64,
    pub v2_known_tokens: u64,
    pub v2_unknown_response_count: u64,
    pub v2_completeness: String,
    pub relation: String,
    pub absolute_delta_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeUsageCompatibilityReport {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    /// Stable for the same unordered scope set at the same commit watermark.
    pub comparison_ref: String,
    pub status: String,
    pub comparison_status: String,
    pub scopes: Vec<UsageScopeRequest>,
    pub selection_vector: Vec<RuntimeUsageTotalsSelectionScope>,
    pub legacy: UsageAggregate,
    pub usage_v2: Option<RuntimeUsageV2Aggregate>,
    pub input_tokens: Option<RuntimeUsageCompatibilityBucket>,
    pub output_tokens: Option<RuntimeUsageCompatibilityBucket>,
    pub cache_creation_input_tokens: Option<RuntimeUsageCompatibilityBucket>,
    pub cache_read_input_tokens: Option<RuntimeUsageCompatibilityBucket>,
}

#[derive(Debug)]
struct ValidatedScope {
    project_key: Vec<u8>,
    session_key: Option<Vec<u8>>,
}

#[derive(Debug)]
struct ValidatedRequest {
    scopes: Vec<ValidatedScope>,
}

#[derive(Debug)]
struct SourceVectorRow {
    source_instance_id: u64,
    adapter_id: String,
    stable_key: Vec<u8>,
    session_count: u64,
}

#[derive(Debug)]
struct CoverageAssessment {
    status: String,
    v2_eligible: bool,
}

#[derive(Debug)]
struct LegacyMetadata {
    usage: UsageMetadata,
    first_source_time: Option<String>,
    last_source_time: Option<String>,
    first_observed_at_unix_ms: Option<i64>,
    last_observed_at_unix_ms: Option<i64>,
    last_commit_seq: Option<u64>,
}

pub(super) fn validate_runtime_usage_totals(
    request: &RuntimeUsageTotalsRequest,
) -> Result<(), EngineError> {
    validate_request(request).map(|_| ())
}

pub(super) fn validate_runtime_usage_compatibility(
    request: &RuntimeUsageCompatibilityRequest,
) -> Result<(), EngineError> {
    validate_request(&RuntimeUsageTotalsRequest {
        scopes: request.scopes.clone(),
        requested_query_id: RUNTIME_USAGE_V2_QUERY_ID.to_string(),
    })
    .map(|_| ())
}

pub(super) fn read_runtime_usage_totals(
    connection: &Connection,
    request: &RuntimeUsageTotalsRequest,
) -> Result<RuntimeUsageTotalsReport, EngineError> {
    let validated = validate_request(request)?;
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin runtime usage totals snapshot", error))?;
    let at_commit_seq = read_committed_watermark(&transaction)?;
    validate_session_membership(&transaction, &validated)?;
    let selection_vector = read_selection_vector(&transaction, &validated)?;
    let (status, resolved_query) = resolve_query(request, &selection_vector);

    let mut legacy = None;
    let mut usage_v2 = None;
    if status == "resolved" {
        match resolved_query.as_ref().map(|value| value.query_id.as_str()) {
            Some(LEGACY_USAGE_QUERY_ID) => {
                legacy = Some(read_legacy_totals(&transaction, &validated)?);
            }
            Some(RUNTIME_USAGE_V2_QUERY_ID) => {
                usage_v2 = Some(read_usage_v2_totals(&transaction, &validated)?);
            }
            _ => {
                return Err(EngineError::Sqlite {
                    operation: "resolve runtime usage totals",
                    detail: "resolved query has no supported aggregate arm".to_string(),
                });
            }
        }
    }

    finish_snapshot(transaction)?;
    Ok(RuntimeUsageTotalsReport {
        contract_version: RUNTIME_USAGE_TOTALS_QUERY_CONTRACT_VERSION,
        at_commit_seq,
        requested_query_id: request.requested_query_id.clone(),
        status,
        resolved_query,
        scopes: request.scopes.clone(),
        selection_vector,
        legacy,
        usage_v2,
    })
}

pub(super) fn read_runtime_usage_compatibility(
    connection: &Connection,
    request: &RuntimeUsageCompatibilityRequest,
) -> Result<RuntimeUsageCompatibilityReport, EngineError> {
    let totals_request = RuntimeUsageTotalsRequest {
        scopes: request.scopes.clone(),
        requested_query_id: RUNTIME_USAGE_V2_QUERY_ID.to_string(),
    };
    let validated = validate_request(&totals_request)?;
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin runtime usage compatibility snapshot", error))?;
    let at_commit_seq = read_committed_watermark(&transaction)?;
    validate_session_membership(&transaction, &validated)?;
    let selection_vector = read_selection_vector(&transaction, &validated)?;
    let legacy_totals = read_legacy_totals(&transaction, &validated)?;
    let comparison_ref = comparison_ref(&validated, at_commit_seq);

    if selection_vector.is_empty() || !selection_vector.iter().all(|scope| scope.v2_eligible) {
        finish_snapshot(transaction)?;
        return Ok(RuntimeUsageCompatibilityReport {
            contract_version: RUNTIME_USAGE_COMPATIBILITY_QUERY_CONTRACT_VERSION,
            at_commit_seq,
            comparison_ref,
            status: "not_ready".to_string(),
            comparison_status: "not_ready".to_string(),
            scopes: request.scopes.clone(),
            selection_vector,
            legacy: legacy_totals.aggregate,
            usage_v2: None,
            input_tokens: None,
            output_tokens: None,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
    }

    let usage_v2 = read_usage_v2_totals(&transaction, &validated)?;
    let input_tokens = compatibility_bucket(
        legacy_totals.aggregate.exact.input_tokens,
        legacy_totals.aggregate.estimated.input_tokens,
        legacy_totals.aggregate.combined.input_tokens,
        usage_v2.input_tokens,
    );
    let output_tokens = compatibility_bucket(
        legacy_totals.aggregate.exact.output_tokens,
        legacy_totals.aggregate.estimated.output_tokens,
        legacy_totals.aggregate.combined.output_tokens,
        usage_v2.output_tokens,
    );
    let cache_creation_input_tokens = compatibility_bucket(
        legacy_totals.aggregate.exact.cache_creation_tokens,
        legacy_totals.aggregate.estimated.cache_creation_tokens,
        legacy_totals.aggregate.combined.cache_creation_tokens,
        usage_v2.cache_creation_input_tokens,
    );
    let cache_read_input_tokens = compatibility_bucket(
        legacy_totals.aggregate.exact.cache_read_tokens,
        legacy_totals.aggregate.estimated.cache_read_tokens,
        legacy_totals.aggregate.combined.cache_read_tokens,
        usage_v2.cache_read_input_tokens,
    );
    let relations = [
        input_tokens.relation.as_str(),
        output_tokens.relation.as_str(),
        cache_creation_input_tokens.relation.as_str(),
        cache_read_input_tokens.relation.as_str(),
    ];
    let comparison_status = if relations.contains(&"incomparable") {
        "incomparable"
    } else if relations.iter().all(|relation| *relation == "equal") {
        "equal"
    } else {
        "different"
    };
    finish_snapshot(transaction)?;
    Ok(RuntimeUsageCompatibilityReport {
        contract_version: RUNTIME_USAGE_COMPATIBILITY_QUERY_CONTRACT_VERSION,
        at_commit_seq,
        comparison_ref,
        status: "ready".to_string(),
        comparison_status: comparison_status.to_string(),
        scopes: request.scopes.clone(),
        selection_vector,
        legacy: legacy_totals.aggregate,
        usage_v2: Some(usage_v2),
        input_tokens: Some(input_tokens),
        output_tokens: Some(output_tokens),
        cache_creation_input_tokens: Some(cache_creation_input_tokens),
        cache_read_input_tokens: Some(cache_read_input_tokens),
    })
}

fn validate_request(request: &RuntimeUsageTotalsRequest) -> Result<ValidatedRequest, EngineError> {
    if request.scopes.is_empty() || request.scopes.len() > MAX_RUNTIME_USAGE_TOTALS_SCOPES {
        return Err(EngineError::InvalidQuery(format!(
            "runtime usage totals requires between 1 and {MAX_RUNTIME_USAGE_TOTALS_SCOPES} scopes"
        )));
    }
    if !matches!(
        request.requested_query_id.as_str(),
        SELECTED_RUNTIME_USAGE_QUERY_ID | LEGACY_USAGE_QUERY_ID | RUNTIME_USAGE_V2_QUERY_ID
    ) {
        return Err(EngineError::InvalidQuery(
            "runtime usage totals query must be selected, legacy.usage, or runtime.usage-v2"
                .to_string(),
        ));
    }

    let mut scopes_by_project = BTreeMap::<Vec<u8>, BTreeSet<Option<Vec<u8>>>>::new();
    let mut scopes = Vec::with_capacity(request.scopes.len());
    for scope in &request.scopes {
        let project_key = decode_entity_id(&scope.project_id, PROJECT_ID_PREFIX, "project id")?;
        let session_key = scope
            .session_id
            .as_deref()
            .map(|value| decode_entity_id(value, SESSION_ID_PREFIX, "session id"))
            .transpose()?;
        let project_scopes = scopes_by_project.entry(project_key.clone()).or_default();
        if !project_scopes.insert(session_key.clone()) {
            return Err(EngineError::InvalidQuery(
                "runtime usage totals contains a duplicate scope".to_string(),
            ));
        }
        if project_scopes.contains(&None) && project_scopes.len() > 1 {
            return Err(EngineError::InvalidQuery(
                "runtime usage totals project and session scopes must not overlap".to_string(),
            ));
        }
        scopes.push(ValidatedScope {
            project_key,
            session_key,
        });
    }
    Ok(ValidatedRequest { scopes })
}

fn validate_session_membership(
    transaction: &Transaction<'_>,
    request: &ValidatedRequest,
) -> Result<(), EngineError> {
    for scope in &request.scopes {
        let Some(session_key) = scope.session_key.as_ref() else {
            continue;
        };
        let exists = transaction
            .query_row(
                "SELECT 1 FROM canonical_sessions WHERE project_key = ?1 AND session_key = ?2",
                params![scope.project_key, session_key],
                |_| Ok(()),
            )
            .optional()
            .map_err(|error| {
                query_sqlite_error("validate runtime usage totals session scope", error)
            })?
            .is_some();
        if !exists {
            return Err(EngineError::InvalidQuery(
                "runtime usage totals session does not belong to the requested project".to_string(),
            ));
        }
    }
    Ok(())
}

fn read_selection_vector(
    transaction: &Transaction<'_>,
    request: &ValidatedRequest,
) -> Result<Vec<RuntimeUsageTotalsSelectionScope>, EngineError> {
    let sql = format!(
        r#"
        WITH requested_scopes(project_key, session_key) AS (VALUES {}),
        target_sessions AS (
            SELECT DISTINCT session.session_key, stream.source_instance_id
            FROM requested_scopes AS requested
            JOIN canonical_sessions AS session
              ON session.project_key = requested.project_key
             AND (requested.session_key IS NULL OR session.session_key = requested.session_key)
            JOIN source_objects AS object
              ON object.source_object_id = session.source_object_id
            JOIN source_streams AS stream
              ON stream.source_stream_id = object.source_stream_id
        )
        SELECT source.source_instance_id, source.adapter_id, source.stable_key,
               COUNT(target.session_key)
        FROM target_sessions AS target
        JOIN source_instances AS source
          ON source.source_instance_id = target.source_instance_id
        GROUP BY source.source_instance_id, source.adapter_id, source.stable_key
        ORDER BY source.adapter_id, source.stable_key
        "#,
        scope_values_sql(request.scopes.len())
    );
    let parameters = scope_parameters(request);
    let mut statement = transaction
        .prepare(&sql)
        .map_err(|error| query_sqlite_error("prepare runtime usage selection vector", error))?;
    let rows = statement
        .query_map(params_from_iter(parameters.iter()), |row| {
            Ok(SourceVectorRow {
                source_instance_id: nonnegative_u64(row.get(0)?)?,
                adapter_id: row.get(1)?,
                stable_key: row.get(2)?,
                session_count: nonnegative_u64(row.get(3)?)?,
            })
        })
        .map_err(|error| query_sqlite_error("read runtime usage selection vector", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| query_sqlite_error("collect runtime usage selection vector", error))?;
    drop(statement);

    rows.into_iter()
        .map(|row| {
            let source_scope = RuntimeUsageSourceScope {
                source_instance_id: row.source_instance_id,
                stable_key: row.stable_key.clone(),
            };
            let query_selection = read_query_selection(transaction, &source_scope)?;
            let projection_readiness =
                read_projection_readiness(transaction, &source_scope.stable_key)?;
            let coverage =
                read_coverage_assessment(transaction, &source_scope, &projection_readiness)?;
            Ok(RuntimeUsageTotalsSelectionScope {
                selection_scope_ref: selection_scope_ref(&row.adapter_id, &row.stable_key),
                adapter_id: row.adapter_id,
                session_count: row.session_count,
                query_selection,
                projection_readiness,
                coverage_status: coverage.status,
                v2_eligible: coverage.v2_eligible,
            })
        })
        .collect()
}

fn read_coverage_assessment(
    transaction: &Transaction<'_>,
    source_scope: &RuntimeUsageSourceScope,
    readiness: &RuntimeUsageV2ProjectionReadiness,
) -> Result<CoverageAssessment, EngineError> {
    let source_instance_id =
        i64::try_from(source_scope.source_instance_id).map_err(|_| EngineError::Sqlite {
            operation: "read runtime usage coverage assessment",
            detail: "source instance id exceeds SQLite integer range".to_string(),
        })?;
    let row = transaction
        .query_row(
            r#"
            SELECT coverage.completeness, coverage.last_commit_seq,
                   EXISTS (
                     SELECT 1 FROM source_coverage_errors AS errors
                     WHERE errors.coverage_set_id = coverage.coverage_set_id
                   ),
                   EXISTS (
                     SELECT 1 FROM source_coverage_points AS points
                     WHERE points.coverage_set_id = coverage.coverage_set_id
                       AND points.status NOT IN ('complete_through', 'exact_snapshot')
                   )
            FROM source_coverage_sets AS coverage
            WHERE coverage.source_instance_id = ?1
              AND coverage.owner_id = ?2
              AND coverage.owner_scope_key = ?3
              AND coverage.domain_kind = 'fact_family'
              AND coverage.domain_name = ?2
              AND coverage.domain_version = ?4
              AND length(coverage.root_entity_key) = 0
            "#,
            params![
                source_instance_id,
                USAGE_V2_PROJECTION_ID,
                source_scope.stable_key,
                i64::from(USAGE_V2_PROJECTION_VERSION),
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    nonnegative_u64(row.get(1)?)?,
                    row.get::<_, i64>(2)? != 0,
                    row.get::<_, i64>(3)? != 0,
                ))
            },
        )
        .optional()
        .map_err(|error| query_sqlite_error("read runtime usage coverage assessment", error))?;
    let Some((completeness, coverage_commit_seq, has_errors, has_incomplete_points)) = row else {
        return Ok(CoverageAssessment {
            status: "not_materialized".to_string(),
            v2_eligible: false,
        });
    };
    let coverage_status = if has_errors || has_incomplete_points {
        "inconsistent".to_string()
    } else {
        completeness.clone()
    };
    let v2_eligible = completeness == "complete"
        && !has_errors
        && !has_incomplete_points
        && readiness.state == "ready"
        && readiness.desired_version == USAGE_V2_PROJECTION_VERSION
        && readiness.completed_version == Some(USAGE_V2_PROJECTION_VERSION)
        && readiness.last_commit_seq == Some(coverage_commit_seq);
    Ok(CoverageAssessment {
        status: coverage_status,
        v2_eligible,
    })
}

fn resolve_query(
    request: &RuntimeUsageTotalsRequest,
    vector: &[RuntimeUsageTotalsSelectionScope],
) -> (String, Option<RuntimeUsageQuerySelectionValue>) {
    let legacy = || RuntimeUsageQuerySelectionValue {
        query_id: LEGACY_USAGE_QUERY_ID.to_string(),
        contract_version: 1,
    };
    let usage_v2 = || RuntimeUsageQuerySelectionValue {
        query_id: RUNTIME_USAGE_V2_QUERY_ID.to_string(),
        contract_version: RUNTIME_USAGE_V2_QUERY_CONTRACT_VERSION,
    };

    match request.requested_query_id.as_str() {
        LEGACY_USAGE_QUERY_ID => ("resolved".to_string(), Some(legacy())),
        RUNTIME_USAGE_V2_QUERY_ID => {
            let resolved = usage_v2();
            if vector.iter().all(|scope| scope.v2_eligible) {
                ("resolved".to_string(), Some(resolved))
            } else {
                ("not_ready".to_string(), Some(resolved))
            }
        }
        SELECTED_RUNTIME_USAGE_QUERY_ID => {
            let Some(first) = vector.first() else {
                return ("resolved".to_string(), Some(legacy()));
            };
            let selected = first.query_selection.selected.clone();
            if vector
                .iter()
                .any(|scope| scope.query_selection.selected != selected)
            {
                return ("mixed_selection".to_string(), None);
            }
            match (selected.query_id.as_str(), selected.contract_version) {
                (LEGACY_USAGE_QUERY_ID, 1) => ("resolved".to_string(), Some(selected)),
                (RUNTIME_USAGE_V2_QUERY_ID, RUNTIME_USAGE_V2_QUERY_CONTRACT_VERSION) => {
                    if vector.iter().all(|scope| scope.v2_eligible) {
                        ("resolved".to_string(), Some(selected))
                    } else {
                        ("not_ready".to_string(), Some(selected))
                    }
                }
                _ => ("unsupported_selection".to_string(), Some(selected)),
            }
        }
        _ => unreachable!("requested query id was validated"),
    }
}

fn read_legacy_totals(
    transaction: &Transaction<'_>,
    request: &ValidatedRequest,
) -> Result<RuntimeUsageLegacyTotals, EngineError> {
    let target_sessions = target_sessions_cte(request.scopes.len());
    let parameters = scope_parameters(request);
    let totals_sql = format!(
        r#"
        {target_sessions}
        SELECT COALESCE(SUM(usage.exact_input_tokens), 0),
               COALESCE(SUM(usage.exact_output_tokens), 0),
               COALESCE(SUM(usage.exact_cache_creation_tokens), 0),
               COALESCE(SUM(usage.exact_cache_read_tokens), 0),
               COALESCE(SUM(usage.estimated_input_tokens), 0),
               COALESCE(SUM(usage.estimated_output_tokens), 0),
               COALESCE(SUM(usage.estimated_cache_creation_tokens), 0),
               COALESCE(SUM(usage.estimated_cache_read_tokens), 0)
        FROM target_sessions AS target
        JOIN usage_totals AS usage ON usage.session_key = target.session_key
        "#
    );
    let (exact, estimated) = transaction
        .query_row(&totals_sql, params_from_iter(parameters.iter()), |row| {
            Ok((
                token_values_from_row(row, 0)?,
                token_values_from_row(row, 4)?,
            ))
        })
        .map_err(|error| query_sqlite_error("read selected legacy usage totals", error))?;

    let metadata_sql = format!(
        r#"
        {target_sessions}
        SELECT COALESCE(SUM(CASE WHEN usage.quality_bucket = 'exact' THEN 1 ELSE 0 END), 0),
               COALESCE(SUM(CASE WHEN usage.quality_bucket = 'estimated' THEN 1 ELSE 0 END), 0),
               COUNT(DISTINCT usage.session_key),
               MIN(usage.source_time), MAX(usage.source_time),
               MIN(fact.observed_at), MAX(fact.observed_at), MAX(usage.last_commit_seq)
        FROM target_sessions AS target
        JOIN usage_contributions AS usage ON usage.session_key = target.session_key
        JOIN fact_records AS fact ON fact.fact_id = usage.fact_id
        "#
    );
    let metadata = transaction
        .query_row(
            &metadata_sql,
            params_from_iter(parameters.iter()),
            legacy_metadata_from_row,
        )
        .map_err(|error| query_sqlite_error("read selected legacy usage metadata", error))?;

    let coverage_sql = format!(
        r#"
        {target_sessions}
        SELECT usage.scope, usage.accounting, usage.quality, usage.quality_bucket,
               usage.model, usage.source_time_quality, COUNT(*),
               SUM(usage.input_tokens), SUM(usage.output_tokens),
               SUM(usage.cache_creation_tokens), SUM(usage.cache_read_tokens)
        FROM target_sessions AS target
        JOIN usage_contributions AS usage ON usage.session_key = target.session_key
        GROUP BY usage.scope, usage.accounting, usage.quality, usage.quality_bucket,
                 usage.model, usage.source_time_quality
        ORDER BY usage.scope, usage.accounting, usage.quality, usage.model,
                 usage.source_time_quality
        "#
    );
    let mut statement = transaction
        .prepare(&coverage_sql)
        .map_err(|error| query_sqlite_error("prepare selected legacy usage coverage", error))?;
    let coverage = statement
        .query_map(params_from_iter(parameters.iter()), usage_coverage_from_row)
        .map_err(|error| query_sqlite_error("read selected legacy usage coverage", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| query_sqlite_error("collect selected legacy usage coverage", error))?;

    Ok(RuntimeUsageLegacyTotals {
        aggregate: usage_aggregate(exact, estimated, metadata.usage)?,
        coverage,
        first_source_time: metadata.first_source_time,
        last_source_time: metadata.last_source_time,
        first_observed_at_unix_ms: metadata.first_observed_at_unix_ms,
        last_observed_at_unix_ms: metadata.last_observed_at_unix_ms,
        last_commit_seq: metadata.last_commit_seq,
    })
}

fn read_usage_v2_totals(
    transaction: &Transaction<'_>,
    request: &ValidatedRequest,
) -> Result<RuntimeUsageV2Aggregate, EngineError> {
    let sql = format!(
        r#"
        {},
        target_runtime_sessions AS (
            SELECT DISTINCT actor.session_key
            FROM target_sessions AS target
            JOIN runtime_actor_runs_v2 AS actor
              ON actor.native_session_id = target.native_session_id
            JOIN source_objects AS actor_object
              ON actor_object.source_object_id = actor.source_object_id
            JOIN source_streams AS actor_stream
              ON actor_stream.source_stream_id = actor_object.source_stream_id
             AND actor_stream.source_instance_id = target.source_instance_id
        )
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
        JOIN target_runtime_sessions AS target ON target.session_key = usage.session_key
        JOIN usage_v2_qualification_specs AS input_q
          ON input_q.qualification_key = usage.input_qualification_key
        JOIN usage_v2_qualification_specs AS output_q
          ON output_q.qualification_key = usage.output_qualification_key
        JOIN usage_v2_qualification_specs AS cache_create_q
          ON cache_create_q.qualification_key = usage.cache_creation_qualification_key
        JOIN usage_v2_qualification_specs AS cache_read_q
          ON cache_read_q.qualification_key = usage.cache_read_qualification_key
        "#,
        target_sessions_cte(request.scopes.len())
    );
    let parameters = scope_parameters(request);
    transaction
        .query_row(&sql, params_from_iter(parameters.iter()), |row| {
            let response_count = nonnegative_u64(row.get(0)?)?;
            Ok(RuntimeUsageV2Aggregate {
                response_count,
                actor_count: nonnegative_u64(row.get(1)?)?,
                input_tokens: bucket_aggregate_from_row(row, 2, response_count)?,
                output_tokens: bucket_aggregate_from_row(row, 5, response_count)?,
                cache_creation_input_tokens: bucket_aggregate_from_row(row, 8, response_count)?,
                cache_read_input_tokens: bucket_aggregate_from_row(row, 11, response_count)?,
            })
        })
        .map_err(|error| query_sqlite_error("read selected usage-v2 totals", error))
}

fn target_sessions_cte(scope_count: usize) -> String {
    format!(
        r#"
        WITH requested_scopes(project_key, session_key) AS (VALUES {}),
        target_sessions AS (
            SELECT DISTINCT session.session_key, session.native_session_id,
                            stream.source_instance_id
            FROM requested_scopes AS requested
            JOIN canonical_sessions AS session
              ON session.project_key = requested.project_key
             AND (requested.session_key IS NULL OR session.session_key = requested.session_key)
            JOIN source_objects AS object
              ON object.source_object_id = session.source_object_id
            JOIN source_streams AS stream
              ON stream.source_stream_id = object.source_stream_id
        )
        "#,
        scope_values_sql(scope_count)
    )
}

fn scope_values_sql(scope_count: usize) -> String {
    (0..scope_count)
        .map(|index| format!("(?{}, ?{})", index * 2 + 1, index * 2 + 2))
        .collect::<Vec<_>>()
        .join(", ")
}

fn scope_parameters(request: &ValidatedRequest) -> Vec<Value> {
    request
        .scopes
        .iter()
        .flat_map(|scope| {
            [
                Value::Blob(scope.project_key.clone()),
                scope
                    .session_key
                    .as_ref()
                    .map_or(Value::Null, |value| Value::Blob(value.clone())),
            ]
        })
        .collect()
}

fn selection_scope_ref(adapter_id: &str, stable_key: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/runtime-usage-selection-scope/v1\0");
    hasher.update(&(adapter_id.len() as u64).to_be_bytes());
    hasher.update(adapter_id.as_bytes());
    hasher.update(&(stable_key.len() as u64).to_be_bytes());
    hasher.update(stable_key);
    opaque_ref(hasher.finalize().as_bytes())
}

fn comparison_ref(request: &ValidatedRequest, at_commit_seq: u64) -> String {
    let mut scopes = request
        .scopes
        .iter()
        .map(|scope| (scope.project_key.as_slice(), scope.session_key.as_deref()))
        .collect::<Vec<_>>();
    scopes.sort();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/runtime-usage-compatibility/v1\0");
    hasher.update(&at_commit_seq.to_be_bytes());
    for (project_key, session_key) in scopes {
        hasher.update(&(project_key.len() as u64).to_be_bytes());
        hasher.update(project_key);
        match session_key {
            Some(session_key) => {
                hasher.update(&[1]);
                hasher.update(&(session_key.len() as u64).to_be_bytes());
                hasher.update(session_key);
            }
            None => {
                hasher.update(&[0]);
            }
        }
    }
    opaque_ref(hasher.finalize().as_bytes())
}

fn compatibility_bucket(
    legacy_exact_tokens: u64,
    legacy_estimated_tokens: u64,
    legacy_combined_tokens: u64,
    usage_v2: super::runtime_usage_query::RuntimeUsageV2BucketAggregate,
) -> RuntimeUsageCompatibilityBucket {
    let (relation, absolute_delta_tokens) = if usage_v2.completeness != "complete" {
        ("incomparable", None)
    } else if legacy_combined_tokens == usage_v2.known_tokens {
        ("equal", Some(0))
    } else if legacy_combined_tokens > usage_v2.known_tokens {
        (
            "legacy_higher",
            Some(legacy_combined_tokens - usage_v2.known_tokens),
        )
    } else {
        (
            "v2_higher",
            Some(usage_v2.known_tokens - legacy_combined_tokens),
        )
    };
    RuntimeUsageCompatibilityBucket {
        legacy_exact_tokens,
        legacy_estimated_tokens,
        legacy_combined_tokens,
        v2_known_tokens: usage_v2.known_tokens,
        v2_unknown_response_count: usage_v2.unknown_response_count,
        v2_completeness: usage_v2.completeness.to_string(),
        relation: relation.to_string(),
        absolute_delta_tokens,
    }
}

fn legacy_metadata_from_row(row: &Row<'_>) -> rusqlite::Result<LegacyMetadata> {
    Ok(LegacyMetadata {
        usage: UsageMetadata {
            exact_contribution_count: nonnegative_u64(row.get(0)?)?,
            estimated_contribution_count: nonnegative_u64(row.get(1)?)?,
            session_count: nonnegative_u64(row.get(2)?)?,
        },
        first_source_time: row.get(3)?,
        last_source_time: row.get(4)?,
        first_observed_at_unix_ms: row.get(5)?,
        last_observed_at_unix_ms: row.get(6)?,
        last_commit_seq: row
            .get::<_, Option<i64>>(7)?
            .map(nonnegative_u64)
            .transpose()?,
    })
}

fn finish_snapshot(transaction: Transaction<'_>) -> Result<(), EngineError> {
    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish runtime usage totals snapshot", error))
}

fn nonnegative_u64(value: i64) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))
}

fn query_sqlite_error(operation: &'static str, error: rusqlite::Error) -> EngineError {
    EngineError::Sqlite {
        operation,
        detail: error.to_string(),
    }
}
