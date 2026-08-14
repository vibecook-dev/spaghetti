//! Read-only RFC 011 usage query pack.

use rusqlite::{Connection, OptionalExtension, Row, Transaction};

use super::query_identity::{decode_entity_id, PROJECT_ID_PREFIX, SESSION_ID_PREFIX};
use super::query_pool::read_committed_watermark;
use super::EngineError;

pub const USAGE_QUERY_CONTRACT_VERSION: u32 = 1;
pub const MAX_USAGE_ACTIVITY_DAYS: u32 = 366;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageScopeRequest {
    pub project_id: String,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageActivityRequest {
    pub project_id: String,
    pub session_id: Option<String>,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageTokenValues {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    /// Arithmetic sum of the four preserved native components. This is not a
    /// provider billing normalization: some adapters may report cache input as
    /// a subset of input rather than an additive component.
    pub component_total_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageAggregate {
    pub exact: UsageTokenValues,
    pub estimated: UsageTokenValues,
    pub combined: UsageTokenValues,
    pub quality: String,
    pub exact_contribution_count: u64,
    pub estimated_contribution_count: u64,
    pub contribution_count: u64,
    pub session_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageCoverageSummary {
    pub scope: String,
    pub accounting: String,
    pub value_quality: String,
    pub quality_bucket: String,
    pub model: Option<String>,
    pub source_time_quality: Option<String>,
    pub contribution_count: u64,
    pub tokens: UsageTokenValues,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageTotalsReport {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub project_id: String,
    pub session_id: Option<String>,
    pub aggregate: UsageAggregate,
    pub coverage: Vec<UsageCoverageSummary>,
    pub first_source_time: Option<String>,
    pub last_source_time: Option<String>,
    pub first_observed_at_unix_ms: Option<i64>,
    pub last_observed_at_unix_ms: Option<i64>,
    pub last_commit_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageActivityDay {
    pub date: String,
    pub aggregate: UsageAggregate,
    pub first_source_time: String,
    pub last_source_time: String,
    pub first_observed_at_unix_ms: i64,
    pub last_observed_at_unix_ms: i64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UntimedUsageSummary {
    pub aggregate: UsageAggregate,
    pub coverage: Vec<UsageCoverageSummary>,
    pub first_observed_at_unix_ms: Option<i64>,
    pub last_observed_at_unix_ms: Option<i64>,
    pub last_commit_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageActivityReport {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub project_id: String,
    pub session_id: Option<String>,
    pub from: String,
    pub to: String,
    pub days: Vec<UsageActivityDay>,
    pub aggregate: UsageAggregate,
    pub coverage: Vec<UsageCoverageSummary>,
    /// Contributions without a structurally valid source date are never
    /// assigned to a fabricated day or silently discarded.
    pub untimed: UntimedUsageSummary,
    pub first_observed_at_unix_ms: Option<i64>,
    pub last_observed_at_unix_ms: Option<i64>,
    pub last_commit_seq: Option<u64>,
}

#[derive(Debug)]
pub(super) struct ValidatedUsageScope {
    project_key: Vec<u8>,
    session_key: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy)]
struct UsageMetadata {
    exact_contribution_count: u64,
    estimated_contribution_count: u64,
    session_count: u64,
}

#[derive(Debug)]
struct UsageWindowMetadata {
    metadata: UsageMetadata,
    first_source_time: Option<String>,
    last_source_time: Option<String>,
    first_observed_at_unix_ms: Option<i64>,
    last_observed_at_unix_ms: Option<i64>,
    last_commit_seq: Option<u64>,
}

pub(super) fn validate_usage_scope(
    request: &UsageScopeRequest,
) -> Result<ValidatedUsageScope, EngineError> {
    Ok(ValidatedUsageScope {
        project_key: decode_entity_id(&request.project_id, PROJECT_ID_PREFIX, "project id")?,
        session_key: request
            .session_id
            .as_deref()
            .map(|value| decode_entity_id(value, SESSION_ID_PREFIX, "session id"))
            .transpose()?,
    })
}

pub(super) fn validate_usage_activity(
    request: &UsageActivityRequest,
) -> Result<ValidatedUsageScope, EngineError> {
    let scope = validate_usage_scope(&UsageScopeRequest {
        project_id: request.project_id.clone(),
        session_id: request.session_id.clone(),
    })?;
    let from = parse_iso_date(&request.from).ok_or_else(|| {
        EngineError::InvalidQuery("usage activity from must be a valid YYYY-MM-DD date".to_string())
    })?;
    let to = parse_iso_date(&request.to).ok_or_else(|| {
        EngineError::InvalidQuery("usage activity to must be a valid YYYY-MM-DD date".to_string())
    })?;
    if from > to {
        return Err(EngineError::InvalidQuery(
            "usage activity from must not be after to".to_string(),
        ));
    }
    let days = to - from + 1;
    if days > i64::from(MAX_USAGE_ACTIVITY_DAYS) {
        return Err(EngineError::InvalidQuery(format!(
            "usage activity range must not exceed {MAX_USAGE_ACTIVITY_DAYS} days"
        )));
    }
    Ok(scope)
}

pub(super) fn read_usage_totals(
    connection: &Connection,
    request: &UsageScopeRequest,
) -> Result<UsageTotalsReport, EngineError> {
    let scope = validate_usage_scope(request)?;
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin usage totals snapshot", error))?;
    let watermark = read_committed_watermark(&transaction)?;
    validate_session_membership(&transaction, &scope)?;

    let exact_and_estimated = transaction
        .query_row(
            r#"
            WITH target_sessions AS (
                SELECT session_key
                FROM canonical_sessions
                WHERE project_key = ?1
                  AND (?2 IS NULL OR session_key = ?2)
            )
            SELECT COALESCE(SUM(ut.exact_input_tokens), 0),
                   COALESCE(SUM(ut.exact_output_tokens), 0),
                   COALESCE(SUM(ut.exact_cache_creation_tokens), 0),
                   COALESCE(SUM(ut.exact_cache_read_tokens), 0),
                   COALESCE(SUM(ut.estimated_input_tokens), 0),
                   COALESCE(SUM(ut.estimated_output_tokens), 0),
                   COALESCE(SUM(ut.estimated_cache_creation_tokens), 0),
                   COALESCE(SUM(ut.estimated_cache_read_tokens), 0)
            FROM target_sessions ts
            JOIN usage_totals ut ON ut.session_key = ts.session_key
            "#,
            rusqlite::params![scope.project_key, scope.session_key],
            |row| {
                Ok((
                    token_values_from_row(row, 0)?,
                    token_values_from_row(row, 4)?,
                ))
            },
        )
        .map_err(|error| query_sqlite_error("read materialized usage totals", error))?;
    let metadata = read_all_usage_metadata(&transaction, &scope)?;
    let coverage = read_all_usage_coverage(&transaction, &scope)?;
    let aggregate = usage_aggregate(
        exact_and_estimated.0,
        exact_and_estimated.1,
        metadata.metadata,
    )?;
    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish usage totals snapshot", error))?;

    Ok(UsageTotalsReport {
        contract_version: USAGE_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        project_id: request.project_id.clone(),
        session_id: request.session_id.clone(),
        aggregate,
        coverage,
        first_source_time: metadata.first_source_time,
        last_source_time: metadata.last_source_time,
        first_observed_at_unix_ms: metadata.first_observed_at_unix_ms,
        last_observed_at_unix_ms: metadata.last_observed_at_unix_ms,
        last_commit_seq: metadata.last_commit_seq,
    })
}

pub(super) fn read_usage_activity(
    connection: &Connection,
    request: &UsageActivityRequest,
) -> Result<UsageActivityReport, EngineError> {
    let scope = validate_usage_activity(request)?;
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin usage activity snapshot", error))?;
    let watermark = read_committed_watermark(&transaction)?;
    validate_session_membership(&transaction, &scope)?;

    let days = read_usage_days(&transaction, &scope, &request.from, &request.to)?;
    let range_metadata =
        read_dated_usage_metadata(&transaction, &scope, &request.from, &request.to)?;
    let (exact, estimated) = combine_day_token_values(&days)?;
    let aggregate = usage_aggregate(exact, estimated, range_metadata.metadata)?;
    let coverage = read_dated_usage_coverage(
        &transaction,
        &scope,
        Some((&request.from, &request.to)),
        false,
    )?;
    let mut untimed = read_untimed_usage(&transaction, &scope)?;
    untimed.coverage = read_dated_usage_coverage(&transaction, &scope, None, true)?;
    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish usage activity snapshot", error))?;

    Ok(UsageActivityReport {
        contract_version: USAGE_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        project_id: request.project_id.clone(),
        session_id: request.session_id.clone(),
        from: request.from.clone(),
        to: request.to.clone(),
        days,
        aggregate,
        coverage,
        untimed,
        first_observed_at_unix_ms: range_metadata.first_observed_at_unix_ms,
        last_observed_at_unix_ms: range_metadata.last_observed_at_unix_ms,
        last_commit_seq: range_metadata.last_commit_seq,
    })
}

fn validate_session_membership(
    transaction: &Transaction<'_>,
    scope: &ValidatedUsageScope,
) -> Result<(), EngineError> {
    let Some(session_key) = scope.session_key.as_ref() else {
        return Ok(());
    };
    let exists = transaction
        .query_row(
            "SELECT 1 FROM canonical_sessions WHERE session_key = ?1 AND project_key = ?2",
            rusqlite::params![session_key, scope.project_key],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| query_sqlite_error("validate usage session scope", error))?
        .is_some();
    if !exists {
        return Err(EngineError::InvalidQuery(
            "usage session does not belong to the requested project".to_string(),
        ));
    }
    Ok(())
}

fn read_all_usage_metadata(
    transaction: &Transaction<'_>,
    scope: &ValidatedUsageScope,
) -> Result<UsageWindowMetadata, EngineError> {
    transaction
        .query_row(
            r#"
            SELECT COALESCE(SUM(CASE WHEN uc.quality_bucket = 'exact' THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN uc.quality_bucket = 'estimated' THEN 1 ELSE 0 END), 0),
                   COUNT(DISTINCT uc.session_key),
                   MIN(uc.source_time), MAX(uc.source_time),
                   MIN(fr.observed_at), MAX(fr.observed_at), MAX(uc.last_commit_seq)
            FROM canonical_sessions cs
            JOIN usage_contributions uc ON uc.session_key = cs.session_key
            JOIN fact_records fr ON fr.fact_id = uc.fact_id
            WHERE cs.project_key = ?1
              AND (?2 IS NULL OR cs.session_key = ?2)
            "#,
            rusqlite::params![scope.project_key, scope.session_key],
            usage_window_metadata_from_row,
        )
        .map_err(|error| query_sqlite_error("read usage evidence metadata", error))
}

fn read_all_usage_coverage(
    transaction: &Transaction<'_>,
    scope: &ValidatedUsageScope,
) -> Result<Vec<UsageCoverageSummary>, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT uc.scope, uc.accounting, uc.quality, uc.quality_bucket,
                   uc.model, uc.source_time_quality, COUNT(*),
                   SUM(uc.input_tokens), SUM(uc.output_tokens),
                   SUM(uc.cache_creation_tokens), SUM(uc.cache_read_tokens)
            FROM canonical_sessions cs
            JOIN usage_contributions uc ON uc.session_key = cs.session_key
            WHERE cs.project_key = ?1
              AND (?2 IS NULL OR cs.session_key = ?2)
            GROUP BY uc.scope, uc.accounting, uc.quality, uc.quality_bucket,
                     uc.model, uc.source_time_quality
            ORDER BY uc.scope, uc.accounting, uc.quality, uc.model,
                     uc.source_time_quality
            "#,
        )
        .map_err(|error| query_sqlite_error("prepare usage coverage", error))?;
    let rows = statement
        .query_map(
            rusqlite::params![scope.project_key, scope.session_key],
            usage_coverage_from_row,
        )
        .map_err(|error| query_sqlite_error("read usage coverage", error))?;
    collect_rows(rows, "collect usage coverage")
}

fn read_usage_days(
    transaction: &Transaction<'_>,
    scope: &ValidatedUsageScope,
    from: &str,
    to: &str,
) -> Result<Vec<UsageActivityDay>, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT substr(uc.source_time, 1, 10) AS activity_day,
                   COALESCE(SUM(CASE WHEN uc.quality_bucket = 'exact' THEN uc.input_tokens ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN uc.quality_bucket = 'exact' THEN uc.output_tokens ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN uc.quality_bucket = 'exact' THEN uc.cache_creation_tokens ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN uc.quality_bucket = 'exact' THEN uc.cache_read_tokens ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN uc.quality_bucket = 'estimated' THEN uc.input_tokens ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN uc.quality_bucket = 'estimated' THEN uc.output_tokens ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN uc.quality_bucket = 'estimated' THEN uc.cache_creation_tokens ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN uc.quality_bucket = 'estimated' THEN uc.cache_read_tokens ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN uc.quality_bucket = 'exact' THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN uc.quality_bucket = 'estimated' THEN 1 ELSE 0 END), 0),
                   COUNT(DISTINCT uc.session_key),
                   MIN(uc.source_time), MAX(uc.source_time),
                   MIN(fr.observed_at), MAX(fr.observed_at), MAX(uc.last_commit_seq)
            FROM canonical_sessions cs
            JOIN usage_contributions uc ON uc.session_key = cs.session_key
            JOIN fact_records fr ON fr.fact_id = uc.fact_id
            WHERE cs.project_key = ?1
              AND (?2 IS NULL OR cs.session_key = ?2)
              AND COALESCE(
                  uc.source_time IS NOT NULL
                  AND length(uc.source_time) >= 10
                  AND substr(uc.source_time, 1, 10) GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
                  AND substr(uc.source_time, 1, 4) <> '0000'
                  AND strftime('%Y-%m-%d', substr(uc.source_time, 1, 10), '+0 days') = substr(uc.source_time, 1, 10),
                  0
              )
              AND uc.source_time >= ?3
              AND uc.source_time < CASE
                  WHEN ?4 = '9999-12-31' THEN '9999-12-32'
                  ELSE date(?4, '+1 day')
              END
              AND substr(uc.source_time, 1, 10) >= ?3
              AND substr(uc.source_time, 1, 10) <= ?4
            GROUP BY activity_day
            ORDER BY activity_day
            "#,
        )
        .map_err(|error| query_sqlite_error("prepare usage activity days", error))?;
    let rows = statement
        .query_map(
            rusqlite::params![scope.project_key, scope.session_key, from, to],
            usage_day_from_row,
        )
        .map_err(|error| query_sqlite_error("read usage activity days", error))?;
    collect_rows(rows, "collect usage activity days")
}

fn read_dated_usage_metadata(
    transaction: &Transaction<'_>,
    scope: &ValidatedUsageScope,
    from: &str,
    to: &str,
) -> Result<UsageWindowMetadata, EngineError> {
    transaction
        .query_row(
            r#"
            SELECT COALESCE(SUM(CASE WHEN uc.quality_bucket = 'exact' THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN uc.quality_bucket = 'estimated' THEN 1 ELSE 0 END), 0),
                   COUNT(DISTINCT uc.session_key),
                   MIN(uc.source_time), MAX(uc.source_time),
                   MIN(fr.observed_at), MAX(fr.observed_at), MAX(uc.last_commit_seq)
            FROM canonical_sessions cs
            JOIN usage_contributions uc ON uc.session_key = cs.session_key
            JOIN fact_records fr ON fr.fact_id = uc.fact_id
            WHERE cs.project_key = ?1
              AND (?2 IS NULL OR cs.session_key = ?2)
              AND COALESCE(
                  uc.source_time IS NOT NULL
                  AND length(uc.source_time) >= 10
                  AND substr(uc.source_time, 1, 10) GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
                  AND substr(uc.source_time, 1, 4) <> '0000'
                  AND strftime('%Y-%m-%d', substr(uc.source_time, 1, 10), '+0 days') = substr(uc.source_time, 1, 10),
                  0
              )
              AND uc.source_time >= ?3
              AND uc.source_time < CASE
                  WHEN ?4 = '9999-12-31' THEN '9999-12-32'
                  ELSE date(?4, '+1 day')
              END
              AND substr(uc.source_time, 1, 10) >= ?3
              AND substr(uc.source_time, 1, 10) <= ?4
            "#,
            rusqlite::params![scope.project_key, scope.session_key, from, to],
            usage_window_metadata_from_row,
        )
        .map_err(|error| query_sqlite_error("read dated usage metadata", error))
}

fn read_untimed_usage(
    transaction: &Transaction<'_>,
    scope: &ValidatedUsageScope,
) -> Result<UntimedUsageSummary, EngineError> {
    transaction
        .query_row(
            r#"
            SELECT COALESCE(SUM(CASE WHEN uc.quality_bucket = 'exact' THEN uc.input_tokens ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN uc.quality_bucket = 'exact' THEN uc.output_tokens ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN uc.quality_bucket = 'exact' THEN uc.cache_creation_tokens ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN uc.quality_bucket = 'exact' THEN uc.cache_read_tokens ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN uc.quality_bucket = 'estimated' THEN uc.input_tokens ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN uc.quality_bucket = 'estimated' THEN uc.output_tokens ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN uc.quality_bucket = 'estimated' THEN uc.cache_creation_tokens ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN uc.quality_bucket = 'estimated' THEN uc.cache_read_tokens ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN uc.quality_bucket = 'exact' THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN uc.quality_bucket = 'estimated' THEN 1 ELSE 0 END), 0),
                   COUNT(DISTINCT uc.session_key),
                   MIN(fr.observed_at), MAX(fr.observed_at), MAX(uc.last_commit_seq)
            FROM canonical_sessions cs
            JOIN usage_contributions uc ON uc.session_key = cs.session_key
            JOIN fact_records fr ON fr.fact_id = uc.fact_id
            WHERE cs.project_key = ?1
              AND (?2 IS NULL OR cs.session_key = ?2)
              AND NOT COALESCE(
                  uc.source_time IS NOT NULL
                  AND length(uc.source_time) >= 10
                  AND substr(uc.source_time, 1, 10) GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
                  AND substr(uc.source_time, 1, 4) <> '0000'
                  AND strftime('%Y-%m-%d', substr(uc.source_time, 1, 10), '+0 days') = substr(uc.source_time, 1, 10),
                  0
              )
            "#,
            rusqlite::params![scope.project_key, scope.session_key],
            |row| {
                let exact = token_values_from_row(row, 0)?;
                let estimated = token_values_from_row(row, 4)?;
                let metadata = UsageMetadata {
                    exact_contribution_count: nonnegative_u64(row.get(8)?, "exact contribution count")?,
                    estimated_contribution_count: nonnegative_u64(
                        row.get(9)?,
                        "estimated contribution count",
                    )?,
                    session_count: nonnegative_u64(row.get(10)?, "usage session count")?,
                };
                let first_observed_at_unix_ms = row.get(11)?;
                let last_observed_at_unix_ms = row.get(12)?;
                let last_commit_seq = optional_nonnegative_u64(
                    row.get(13)?,
                    "untimed usage commit sequence",
                )?;
                Ok((
                    exact,
                    estimated,
                    metadata,
                    first_observed_at_unix_ms,
                    last_observed_at_unix_ms,
                    last_commit_seq,
                ))
            },
        )
        .map_err(|error| query_sqlite_error("read untimed usage", error))
        .and_then(
            |(
                exact,
                estimated,
                metadata,
                first_observed_at_unix_ms,
                last_observed_at_unix_ms,
                last_commit_seq,
            )| {
                Ok(UntimedUsageSummary {
                    aggregate: usage_aggregate(exact, estimated, metadata)?,
                    coverage: Vec::new(),
                    first_observed_at_unix_ms,
                    last_observed_at_unix_ms,
                    last_commit_seq,
                })
            },
        )
}

fn read_dated_usage_coverage(
    transaction: &Transaction<'_>,
    scope: &ValidatedUsageScope,
    range: Option<(&str, &str)>,
    untimed: bool,
) -> Result<Vec<UsageCoverageSummary>, EngineError> {
    let (from, to) = range.unwrap_or(("", ""));
    let mut statement = transaction
        .prepare(
            r#"
            SELECT uc.scope, uc.accounting, uc.quality, uc.quality_bucket,
                   uc.model, uc.source_time_quality, COUNT(*),
                   SUM(uc.input_tokens), SUM(uc.output_tokens),
                   SUM(uc.cache_creation_tokens), SUM(uc.cache_read_tokens)
            FROM canonical_sessions cs
            JOIN usage_contributions uc ON uc.session_key = cs.session_key
            WHERE cs.project_key = ?1
              AND (?2 IS NULL OR cs.session_key = ?2)
              AND COALESCE(
                  uc.source_time IS NOT NULL
                  AND length(uc.source_time) >= 10
                  AND substr(uc.source_time, 1, 10) GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
                  AND substr(uc.source_time, 1, 4) <> '0000'
                  AND strftime('%Y-%m-%d', substr(uc.source_time, 1, 10), '+0 days') = substr(uc.source_time, 1, 10),
                  0
              ) = ?3
              AND (?3 = 0 OR (
                  uc.source_time >= ?4
                  AND uc.source_time < CASE
                      WHEN ?5 = '9999-12-31' THEN '9999-12-32'
                      ELSE date(?5, '+1 day')
                  END
                  AND
                  substr(uc.source_time, 1, 10) >= ?4
                  AND substr(uc.source_time, 1, 10) <= ?5
              ))
            GROUP BY uc.scope, uc.accounting, uc.quality, uc.quality_bucket,
                     uc.model, uc.source_time_quality
            ORDER BY uc.scope, uc.accounting, uc.quality, uc.model,
                     uc.source_time_quality
            "#,
        )
        .map_err(|error| query_sqlite_error("prepare dated usage coverage", error))?;
    let rows = statement
        .query_map(
            rusqlite::params![
                scope.project_key,
                scope.session_key,
                i64::from(!untimed),
                from,
                to
            ],
            usage_coverage_from_row,
        )
        .map_err(|error| query_sqlite_error("read dated usage coverage", error))?;
    collect_rows(rows, "collect dated usage coverage")
}

fn usage_day_from_row(row: &Row<'_>) -> rusqlite::Result<UsageActivityDay> {
    let exact = token_values_from_row(row, 1)?;
    let estimated = token_values_from_row(row, 5)?;
    let metadata = UsageMetadata {
        exact_contribution_count: nonnegative_u64(row.get(9)?, "daily exact contribution count")?,
        estimated_contribution_count: nonnegative_u64(
            row.get(10)?,
            "daily estimated contribution count",
        )?,
        session_count: nonnegative_u64(row.get(11)?, "daily usage session count")?,
    };
    Ok(UsageActivityDay {
        date: row.get(0)?,
        aggregate: usage_aggregate_sqlite(exact, estimated, metadata)?,
        first_source_time: row.get(12)?,
        last_source_time: row.get(13)?,
        first_observed_at_unix_ms: row.get(14)?,
        last_observed_at_unix_ms: row.get(15)?,
        last_commit_seq: nonnegative_u64(row.get(16)?, "daily usage commit sequence")?,
    })
}

fn usage_window_metadata_from_row(row: &Row<'_>) -> rusqlite::Result<UsageWindowMetadata> {
    Ok(UsageWindowMetadata {
        metadata: UsageMetadata {
            exact_contribution_count: nonnegative_u64(row.get(0)?, "exact contribution count")?,
            estimated_contribution_count: nonnegative_u64(
                row.get(1)?,
                "estimated contribution count",
            )?,
            session_count: nonnegative_u64(row.get(2)?, "usage session count")?,
        },
        first_source_time: row.get(3)?,
        last_source_time: row.get(4)?,
        first_observed_at_unix_ms: row.get(5)?,
        last_observed_at_unix_ms: row.get(6)?,
        last_commit_seq: optional_nonnegative_u64(row.get(7)?, "usage commit sequence")?,
    })
}

fn usage_coverage_from_row(row: &Row<'_>) -> rusqlite::Result<UsageCoverageSummary> {
    Ok(UsageCoverageSummary {
        scope: row.get(0)?,
        accounting: row.get(1)?,
        value_quality: row.get(2)?,
        quality_bucket: row.get(3)?,
        model: row.get(4)?,
        source_time_quality: row.get(5)?,
        contribution_count: nonnegative_u64(row.get(6)?, "usage coverage count")?,
        tokens: token_values_from_row(row, 7)?,
    })
}

fn token_values_from_row(row: &Row<'_>, offset: usize) -> rusqlite::Result<UsageTokenValues> {
    token_values([
        nonnegative_u64(row.get(offset)?, "input tokens")?,
        nonnegative_u64(row.get(offset + 1)?, "output tokens")?,
        nonnegative_u64(row.get(offset + 2)?, "cache creation tokens")?,
        nonnegative_u64(row.get(offset + 3)?, "cache read tokens")?,
    ])
}

fn token_values(values: [u64; 4]) -> rusqlite::Result<UsageTokenValues> {
    let component_total_tokens = values.into_iter().try_fold(0_u64, |total, value| {
        total.checked_add(value).ok_or_else(integral_overflow)
    })?;
    Ok(UsageTokenValues {
        input_tokens: values[0],
        output_tokens: values[1],
        cache_creation_tokens: values[2],
        cache_read_tokens: values[3],
        component_total_tokens,
    })
}

fn combine_tokens(
    left: UsageTokenValues,
    right: UsageTokenValues,
) -> Result<UsageTokenValues, EngineError> {
    token_values([
        checked_add(left.input_tokens, right.input_tokens)?,
        checked_add(left.output_tokens, right.output_tokens)?,
        checked_add(left.cache_creation_tokens, right.cache_creation_tokens)?,
        checked_add(left.cache_read_tokens, right.cache_read_tokens)?,
    ])
    .map_err(|error| query_sqlite_error("combine usage token values", error))
}

fn usage_aggregate(
    exact: UsageTokenValues,
    estimated: UsageTokenValues,
    metadata: UsageMetadata,
) -> Result<UsageAggregate, EngineError> {
    let contribution_count = checked_add(
        metadata.exact_contribution_count,
        metadata.estimated_contribution_count,
    )?;
    let quality = match (
        metadata.exact_contribution_count > 0,
        metadata.estimated_contribution_count > 0,
    ) {
        (true, true) => "mixed",
        (true, false) => "exact",
        (false, true) => "estimated",
        (false, false) => "unavailable",
    };
    Ok(UsageAggregate {
        exact,
        estimated,
        combined: combine_tokens(exact, estimated)?,
        quality: quality.to_string(),
        exact_contribution_count: metadata.exact_contribution_count,
        estimated_contribution_count: metadata.estimated_contribution_count,
        contribution_count,
        session_count: metadata.session_count,
    })
}

fn usage_aggregate_sqlite(
    exact: UsageTokenValues,
    estimated: UsageTokenValues,
    metadata: UsageMetadata,
) -> rusqlite::Result<UsageAggregate> {
    usage_aggregate(exact, estimated, metadata).map_err(|_| integral_overflow())
}

fn combine_day_token_values(
    days: &[UsageActivityDay],
) -> Result<(UsageTokenValues, UsageTokenValues), EngineError> {
    let mut exact = UsageTokenValues::default();
    let mut estimated = UsageTokenValues::default();
    for day in days {
        exact = combine_tokens(exact, day.aggregate.exact)?;
        estimated = combine_tokens(estimated, day.aggregate.estimated)?;
    }
    Ok((exact, estimated))
}

fn checked_add(left: u64, right: u64) -> Result<u64, EngineError> {
    left.checked_add(right).ok_or_else(|| EngineError::Sqlite {
        operation: "aggregate usage values",
        detail: "usage aggregate exceeds supported integer range".to_string(),
    })
}

fn nonnegative_u64(value: i64, field: &'static str) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|_| integral_value_error(field))
}

fn optional_nonnegative_u64(
    value: Option<i64>,
    field: &'static str,
) -> rusqlite::Result<Option<u64>> {
    value.map(|value| nonnegative_u64(value, field)).transpose()
}

fn integral_value_error(_field: &'static str) -> rusqlite::Error {
    rusqlite::Error::IntegralValueOutOfRange(0, -1)
}

fn integral_overflow() -> rusqlite::Error {
    rusqlite::Error::IntegralValueOutOfRange(0, -1)
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&Row<'_>) -> rusqlite::Result<T>>,
    operation: &'static str,
) -> Result<Vec<T>, EngineError> {
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| query_sqlite_error(operation, error))
}

fn parse_iso_date(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if bytes
        .iter()
        .enumerate()
        .any(|(index, byte)| index != 4 && index != 7 && !byte.is_ascii_digit())
    {
        return None;
    }
    let year = value[0..4].parse::<i64>().ok()?;
    let month = value[5..7].parse::<usize>().ok()?;
    let day = value[8..10].parse::<i64>().ok()?;
    if year == 0 || !(1..=12).contains(&month) {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = [
        31_i64,
        28 + i64::from(leap),
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if day == 0 || day > month_days[month - 1] {
        return None;
    }
    let years = year - 1;
    let days_before_year = years * 365 + years / 4 - years / 100 + years / 400;
    Some(days_before_year + month_days[..month - 1].iter().sum::<i64>() + day)
}

fn query_sqlite_error(operation: &'static str, error: rusqlite::Error) -> EngineError {
    EngineError::Sqlite {
        operation,
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::query_identity::encode_entity_id;
    use super::super::query_pool::QueryPool;
    use super::*;
    use crate::core::schema;
    use rusqlite::params;
    use tempfile::tempdir;

    fn insert_fact(
        connection: &Connection,
        fact_id: &[u8],
        fact_kind: &str,
        entity_key: &[u8],
        ordinal: i64,
        observed_at: i64,
    ) {
        connection
            .execute(
                r#"
                INSERT INTO fact_records (
                    fact_id, fact_kind, entity_key, source_instance_id,
                    source_stream_id, source_object_id, source_generation,
                    cursor_start, cursor_end, payload_hash, local_fact_ordinal,
                    observed_at, payload_json, last_commit_seq
                ) VALUES (?1, ?2, ?3, 1, 1, 1, 1, ?4, ?4, ?5, ?6, ?7, x'7b7d', 1)
                "#,
                params![
                    fact_id,
                    fact_kind,
                    entity_key,
                    format!("cursor-{ordinal}").as_bytes(),
                    format!("hash-{ordinal}").as_bytes(),
                    ordinal,
                    observed_at,
                ],
            )
            .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_usage(
        connection: &Connection,
        fact_id: &[u8],
        subject_key: &[u8],
        session_key: &[u8],
        ordinal: i64,
        observed_at: i64,
        scope: &str,
        quality: &str,
        quality_bucket: &str,
        input: i64,
        output: i64,
        cache_creation: i64,
        cache_read: i64,
        model: Option<&str>,
        source_time: Option<&str>,
        source_time_quality: Option<&str>,
    ) {
        insert_fact(
            connection,
            fact_id,
            "usage",
            subject_key,
            ordinal,
            observed_at,
        );
        connection
            .execute(
                r#"
                INSERT INTO usage_contributions (
                    fact_id, subject_key, session_key, series_key, scope, accounting,
                    quality, quality_bucket, input_tokens, output_tokens,
                    cache_creation_tokens, cache_read_tokens,
                    reported_input_tokens, reported_output_tokens,
                    reported_cache_creation_tokens, reported_cache_read_tokens,
                    model, source_time,
                    source_time_quality, source_object_id, source_generation,
                    cursor_end, last_commit_seq
                ) VALUES (
                    ?1, ?2, ?3, ?2, ?4, 'delta', ?5, ?6, ?7, ?8, ?9, ?10,
                    ?7, ?8, ?9, ?10, ?11, ?12, ?13, 1, 1, ?14, 1
                )
                "#,
                params![
                    fact_id,
                    subject_key,
                    session_key,
                    scope,
                    quality,
                    quality_bucket,
                    input,
                    output,
                    cache_creation,
                    cache_read,
                    model,
                    source_time,
                    source_time_quality,
                    format!("cursor-{ordinal}").as_bytes(),
                ],
            )
            .unwrap();
    }

    fn seed_usage_fixture(connection: &Connection) {
        schema::initialize_schema(connection).unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_instances VALUES (1, 'fixture', ?1, 'Fixture', '1.0.0', 1, '[]', '[]', 1, 1)",
                [b"root".as_slice()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_streams VALUES (1, 1, 'history', 'append_file', 'fixture', 'available', 'none', NULL, 1)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_objects (
                    source_object_id, source_stream_id, object_key, generation,
                    committed_cursor, decoder_contract_version, last_commit_seq, state
                ) VALUES (1, 1, ?1, 1, ?2, 1, 1, 'active')",
                params![b"object".as_slice(), b"cursor".as_slice()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO ingest_commits VALUES (1, 1, 'fixture', 1, 2, 10)",
                [],
            )
            .unwrap();

        for (ordinal, (fact_id, session_key, project_key, native_session, native_project)) in [
            (
                b"session-a-fact".as_slice(),
                b"session-a".as_slice(),
                b"project-a".as_slice(),
                "native-session-a",
                "native-project-a",
            ),
            (
                b"session-b-fact".as_slice(),
                b"session-b".as_slice(),
                b"project-a".as_slice(),
                "native-session-b",
                "native-project-a",
            ),
            (
                b"session-c-fact".as_slice(),
                b"session-c".as_slice(),
                b"project-b".as_slice(),
                "native-session-c",
                "native-project-b",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            insert_fact(
                connection,
                fact_id,
                "session",
                session_key,
                ordinal as i64,
                90 + ordinal as i64,
            );
            connection
                .execute(
                    r#"
                    INSERT INTO canonical_sessions (
                        session_key, project_key, native_session_id,
                        native_project_key, fact_id, source_object_id,
                        source_generation, cursor_end, last_commit_seq
                    ) VALUES (?1, ?2, ?3, ?4, ?5, 1, 1, ?6, 1)
                    "#,
                    params![
                        session_key,
                        project_key,
                        native_session,
                        native_project,
                        fact_id,
                        format!("session-cursor-{ordinal}").as_bytes(),
                    ],
                )
                .unwrap();
        }

        insert_usage(
            connection,
            b"usage-1",
            b"subject-1",
            b"session-a",
            10,
            100,
            "message",
            "native_exact",
            "exact",
            10,
            2,
            1,
            3,
            Some("claude-test"),
            Some("2026-08-10T01:00:00.000Z"),
            Some("native_exact"),
        );
        insert_usage(
            connection,
            b"usage-2",
            b"subject-2",
            b"session-a",
            11,
            110,
            "turn",
            "estimated",
            "estimated",
            5,
            1,
            0,
            0,
            Some("local-estimator"),
            Some("2026-08-10T02:00:00.000Z"),
            Some("derived"),
        );
        insert_usage(
            connection,
            b"usage-3",
            b"subject-3",
            b"session-a",
            12,
            120,
            "message",
            "native_exact",
            "exact",
            20,
            4,
            2,
            6,
            Some("claude-test"),
            Some("2026-08-11T01:00:00.000Z"),
            Some("native_exact"),
        );
        insert_usage(
            connection,
            b"usage-4",
            b"subject-4",
            b"session-b",
            13,
            130,
            "message",
            "derived_exact",
            "exact",
            30,
            6,
            3,
            9,
            Some("derived-model"),
            Some("2026-08-11T03:00:00.000Z"),
            Some("derived"),
        );
        insert_usage(
            connection,
            b"usage-5",
            b"subject-5",
            b"session-b",
            14,
            140,
            "session",
            "native_approximate",
            "estimated",
            7,
            1,
            0,
            0,
            Some("summary-model"),
            None,
            None,
        );
        insert_usage(
            connection,
            b"usage-6",
            b"subject-6",
            b"session-b",
            15,
            150,
            "session",
            "estimated",
            "estimated",
            11,
            2,
            0,
            0,
            Some("summary-model"),
            Some("2026-02-30T00:00:00.000Z"),
            Some("native_exact"),
        );
        insert_usage(
            connection,
            b"usage-other-project",
            b"subject-other-project",
            b"session-c",
            16,
            160,
            "message",
            "native_exact",
            "exact",
            100,
            10,
            0,
            0,
            Some("other-model"),
            Some("2026-08-10T00:00:00.000Z"),
            Some("native_exact"),
        );
        insert_usage(
            connection,
            b"usage-invalid-month",
            b"subject-invalid-month",
            b"session-b",
            17,
            170,
            "session",
            "estimated",
            "estimated",
            1,
            0,
            0,
            0,
            Some("summary-model"),
            Some("2026-13-01T00:00:00.000Z"),
            Some("native_exact"),
        );
        insert_usage(
            connection,
            b"usage-year-zero",
            b"subject-year-zero",
            b"session-b",
            18,
            180,
            "session",
            "estimated",
            "estimated",
            0,
            2,
            0,
            0,
            Some("summary-model"),
            Some("0000-01-01T00:00:00.000Z"),
            Some("native_exact"),
        );

        connection
            .execute_batch(
                r#"
                INSERT INTO usage_totals (
                    session_key, exact_input_tokens, exact_output_tokens,
                    exact_cache_creation_tokens, exact_cache_read_tokens,
                    estimated_input_tokens, estimated_output_tokens,
                    estimated_cache_creation_tokens, estimated_cache_read_tokens,
                    last_commit_seq
                )
                SELECT session_key,
                       SUM(CASE WHEN quality_bucket = 'exact' THEN input_tokens ELSE 0 END),
                       SUM(CASE WHEN quality_bucket = 'exact' THEN output_tokens ELSE 0 END),
                       SUM(CASE WHEN quality_bucket = 'exact' THEN cache_creation_tokens ELSE 0 END),
                       SUM(CASE WHEN quality_bucket = 'exact' THEN cache_read_tokens ELSE 0 END),
                       SUM(CASE WHEN quality_bucket = 'estimated' THEN input_tokens ELSE 0 END),
                       SUM(CASE WHEN quality_bucket = 'estimated' THEN output_tokens ELSE 0 END),
                       SUM(CASE WHEN quality_bucket = 'estimated' THEN cache_creation_tokens ELSE 0 END),
                       SUM(CASE WHEN quality_bucket = 'estimated' THEN cache_read_tokens ELSE 0 END),
                       MAX(last_commit_seq)
                FROM usage_contributions
                GROUP BY session_key;
                "#,
            )
            .unwrap();
    }

    #[test]
    fn usage_activity_dates_are_calendar_valid_and_bounded() {
        assert!(parse_iso_date("2024-02-29").is_some());
        assert!(parse_iso_date("2026-02-29").is_none());
        assert!(parse_iso_date("2026-13-01").is_none());
        assert!(parse_iso_date("0000-01-01").is_none());
        assert!(parse_iso_date("2026-01-1").is_none());

        let project_id = "project_v1_cHJvamVjdA".to_string();
        assert!(validate_usage_activity(&UsageActivityRequest {
            project_id: project_id.clone(),
            session_id: None,
            from: "2026-01-01".to_string(),
            to: "2026-12-31".to_string(),
        })
        .is_ok());
        assert!(matches!(
            validate_usage_activity(&UsageActivityRequest {
                project_id,
                session_id: None,
                from: "2025-01-01".to_string(),
                to: "2026-01-02".to_string(),
            }),
            Err(EngineError::InvalidQuery(_))
        ));
    }

    #[test]
    fn canonical_usage_queries_preserve_quality_scope_dates_and_identity() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("usage.db");
        let connection = Connection::open(&database).unwrap();
        seed_usage_fixture(&connection);
        drop(connection);

        let mut pool = QueryPool::start(database, 1).unwrap();
        let client = pool.client();
        let project_a = encode_entity_id(PROJECT_ID_PREFIX, b"project-a");
        let project_b = encode_entity_id(PROJECT_ID_PREFIX, b"project-b");
        let session_a = encode_entity_id(SESSION_ID_PREFIX, b"session-a");

        let totals = client
            .usage_totals(UsageScopeRequest {
                project_id: project_a.clone(),
                session_id: None,
            })
            .unwrap();
        assert_eq!(totals.contract_version, USAGE_QUERY_CONTRACT_VERSION);
        assert_eq!(totals.at_commit_seq, 1);
        assert_eq!(totals.aggregate.quality, "mixed");
        assert_eq!(totals.aggregate.exact.input_tokens, 60);
        assert_eq!(totals.aggregate.exact.component_total_tokens, 96);
        assert_eq!(totals.aggregate.estimated.input_tokens, 24);
        assert_eq!(totals.aggregate.estimated.component_total_tokens, 30);
        assert_eq!(totals.aggregate.combined.component_total_tokens, 126);
        assert_eq!(totals.aggregate.exact_contribution_count, 3);
        assert_eq!(totals.aggregate.estimated_contribution_count, 5);
        assert_eq!(totals.aggregate.session_count, 2);
        assert!(totals.coverage.iter().any(|row| {
            row.scope == "message"
                && row.value_quality == "native_exact"
                && row.model.as_deref() == Some("claude-test")
                && row.contribution_count == 2
        }));
        assert_eq!(totals.first_observed_at_unix_ms, Some(100));
        assert_eq!(totals.last_observed_at_unix_ms, Some(180));

        let session_totals = client
            .usage_totals(UsageScopeRequest {
                project_id: project_a.clone(),
                session_id: Some(session_a.clone()),
            })
            .unwrap();
        assert_eq!(session_totals.aggregate.exact.input_tokens, 30);
        assert_eq!(session_totals.aggregate.estimated.input_tokens, 5);
        assert_eq!(session_totals.aggregate.session_count, 1);

        let activity = client
            .usage_activity(UsageActivityRequest {
                project_id: project_a.clone(),
                session_id: None,
                from: "2026-08-10".to_string(),
                to: "2026-08-11".to_string(),
            })
            .unwrap();
        assert_eq!(activity.at_commit_seq, totals.at_commit_seq);
        assert_eq!(activity.days.len(), 2);
        assert_eq!(activity.days[0].date, "2026-08-10");
        assert_eq!(activity.days[0].aggregate.quality, "mixed");
        assert_eq!(
            activity.days[0].aggregate.combined.component_total_tokens,
            22
        );
        assert_eq!(activity.days[1].date, "2026-08-11");
        assert_eq!(activity.days[1].aggregate.quality, "exact");
        assert_eq!(
            activity.days[1].aggregate.combined.component_total_tokens,
            80
        );
        assert_eq!(activity.aggregate.combined.component_total_tokens, 102);
        assert_eq!(activity.aggregate.exact_contribution_count, 3);
        assert_eq!(activity.aggregate.estimated_contribution_count, 1);
        assert_eq!(activity.aggregate.session_count, 2);
        assert_eq!(activity.untimed.aggregate.quality, "estimated");
        assert_eq!(activity.untimed.aggregate.contribution_count, 4);
        assert_eq!(
            activity.untimed.aggregate.combined.component_total_tokens,
            24
        );
        assert_eq!(activity.untimed.first_observed_at_unix_ms, Some(140));
        assert_eq!(activity.untimed.last_observed_at_unix_ms, Some(180));
        assert_eq!(activity.untimed.coverage.len(), 2);
        assert_eq!(
            totals.aggregate.contribution_count,
            activity.aggregate.contribution_count + activity.untimed.aggregate.contribution_count,
            "every in-scope contribution is either dated or explicitly untimed"
        );
        assert!(activity
            .coverage
            .iter()
            .all(|row| row.model.as_deref() != Some("other-model")));

        assert!(matches!(
            client.usage_totals(UsageScopeRequest {
                project_id: project_b,
                session_id: Some(session_a),
            }),
            Err(EngineError::InvalidQuery(_))
        ));
        assert!(matches!(
            client.usage_activity(UsageActivityRequest {
                project_id: project_a,
                session_id: None,
                from: "2026-08-12".to_string(),
                to: "2026-08-10".to_string(),
            }),
            Err(EngineError::InvalidQuery(_))
        ));

        pool.shutdown().unwrap();
    }

    #[test]
    fn usage_activity_query_uses_the_session_time_index() {
        let connection = Connection::open_in_memory().unwrap();
        schema::initialize_schema(&connection).unwrap();
        let installed = connection
            .prepare("PRAGMA index_list('usage_contributions')")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(!installed
            .iter()
            .any(|index| index == "idx_usage_contributions_session"));

        let totals_plan = connection
            .prepare(
                "EXPLAIN QUERY PLAN SELECT SUM(input_tokens) FROM usage_contributions WHERE session_key = ?1",
            )
            .unwrap()
            .query_map([b"session".as_slice()], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            totals_plan.iter().any(|detail| {
                detail.contains("INDEX idx_usage_contributions_session_time")
            }),
            "session totals must retain an indexed lookup after pruning the duplicate index: {totals_plan:?}"
        );

        let mut statement = connection
            .prepare(
                r#"
                EXPLAIN QUERY PLAN
                SELECT substr(uc.source_time, 1, 10) AS activity_day,
                       SUM(uc.input_tokens), SUM(uc.output_tokens)
                FROM canonical_sessions cs
                JOIN usage_contributions uc ON uc.session_key = cs.session_key
                WHERE cs.project_key = ?1
                  AND (?2 IS NULL OR cs.session_key = ?2)
                  AND COALESCE(
                      uc.source_time IS NOT NULL
                      AND length(uc.source_time) >= 10
                      AND substr(uc.source_time, 1, 10) GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
                      AND substr(uc.source_time, 1, 4) <> '0000'
                      AND strftime('%Y-%m-%d', substr(uc.source_time, 1, 10), '+0 days') = substr(uc.source_time, 1, 10),
                      0
                  )
                  AND uc.source_time >= ?3
                  AND uc.source_time < CASE
                      WHEN ?4 = '9999-12-31' THEN '9999-12-32'
                      ELSE date(?4, '+1 day')
                  END
                  AND substr(uc.source_time, 1, 10) >= ?3
                  AND substr(uc.source_time, 1, 10) <= ?4
                GROUP BY activity_day
                "#,
            )
            .unwrap();
        let details = statement
            .query_map(
                params![
                    b"project".as_slice(),
                    Option::<&[u8]>::None,
                    "2026-08-10",
                    "2026-08-11"
                ],
                |row| row.get::<_, String>(3),
            )
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            details
                .iter()
                .any(|detail| { detail.contains("INDEX idx_usage_contributions_session_time") }),
            "production usage activity must seek by canonical session and source time: {details:?}"
        );
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("source_time>?") && detail.contains("source_time<?")),
            "usage activity must use both source-time range bounds: {details:?}"
        );
    }
}
