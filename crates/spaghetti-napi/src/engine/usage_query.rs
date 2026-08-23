//! Canonical usage queries over RFC 012C response-level contributions.
//!
//! One response contributes at most once to its session and to its project, no
//! matter how many native rows revised it. Every bucket carries its own
//! qualification, so a bucket that asserts nothing is reported unknown instead
//! of being summed as zero.

use rusqlite::{Connection, OptionalExtension, Row, Transaction};

use super::query_identity::{decode_entity_id, PROJECT_ID_PREFIX, SESSION_ID_PREFIX};
use super::query_pool::read_committed_watermark;
use super::EngineError;

/// Every query pack shares one negotiated contract version with the client, so
/// this tracks that protocol number rather than versioning usage on its own.
/// The response-level change is carried by `SCHEMA_VERSION`, which rebuilds.
pub const USAGE_QUERY_CONTRACT_VERSION: u32 = 1;
pub const MAX_USAGE_WINDOW_DAYS: u32 = 366;

/// Bucket qualities that assert a number. `unknown` asserts nothing and is
/// never folded into a total.
const KNOWN_QUALITIES: &str = "('exact', 'native_claimed', 'derived', 'estimated')";

/// Bucket value column paired with the alias of its resolved qualification.
const BUCKETS: [(&str, &str, &str); 4] = [
    ("input", "input_tokens", "qi"),
    ("output", "output_tokens", "qo"),
    ("cache_creation", "cache_creation_input_tokens", "qc"),
    ("cache_read", "cache_read_input_tokens", "qr"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRequest {
    pub project_id: String,
    pub session_id: Option<String>,
    /// Optional inclusive calendar window. When present the report also carries
    /// a per-day series plus the contributions no day can own.
    pub window: Option<UsageWindow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageWindow {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageTokenValues {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    /// Sum of the four buckets. Buckets that assert nothing add nothing.
    pub component_total_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageAggregate {
    pub exact: UsageTokenValues,
    pub estimated: UsageTokenValues,
    pub combined: UsageTokenValues,
    pub quality: String,
    /// Responses whose four buckets are all exact.
    pub exact_contribution_count: u64,
    /// Responses with at least one known bucket that are not fully exact.
    pub estimated_contribution_count: u64,
    /// Responses that assert no bucket at all. They are counted, never summed.
    pub unknown_contribution_count: u64,
    /// Distinct responses, not native rows. This is the RFC 012C correction.
    pub contribution_count: u64,
    pub session_count: u64,
}

/// One qualified bucket population: how a group of responses described the same
/// bucket with the same quality, authority, and native provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageCoverageSummary {
    pub bucket: String,
    pub value_quality: String,
    pub completeness: String,
    pub unknown_reason: Option<String>,
    pub authority: String,
    pub native_field: String,
    pub model: Option<String>,
    pub source_time_quality: Option<String>,
    pub contribution_count: u64,
    pub tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageDay {
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
    pub first_observed_at_unix_ms: Option<i64>,
    pub last_observed_at_unix_ms: Option<i64>,
    pub last_commit_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageWindowReport {
    pub from: String,
    pub to: String,
    pub days: Vec<UsageDay>,
    /// Contributions without a structurally valid source date are never
    /// assigned to a fabricated day or silently discarded.
    pub untimed: UntimedUsageSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageReport {
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
    pub window: Option<UsageWindowReport>,
}

#[derive(Debug)]
struct ValidatedUsageScope {
    project_key: Vec<u8>,
    session_key: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, Default)]
struct UsageCounts {
    exact_contribution_count: u64,
    estimated_contribution_count: u64,
    unknown_contribution_count: u64,
    session_count: u64,
}

#[derive(Debug)]
struct UsageScopeMetadata {
    first_source_time: Option<String>,
    last_source_time: Option<String>,
    first_observed_at_unix_ms: Option<i64>,
    last_observed_at_unix_ms: Option<i64>,
    last_commit_seq: Option<u64>,
}

/// A source timestamp that names a real calendar day. Anything else is untimed
/// evidence and is surfaced separately rather than bucketed into a guess.
const DATED: &str = r#"COALESCE(
        u.source_time IS NOT NULL
        AND length(u.source_time) >= 10
        AND substr(u.source_time, 1, 10) GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
        AND substr(u.source_time, 1, 4) <> '0000'
        AND strftime('%Y-%m-%d', substr(u.source_time, 1, 10), '+0 days') = substr(u.source_time, 1, 10),
        0
    )"#;

/// RFC 012C identities are topology-neutral and deliberately independent of the
/// catalog's own session keys, so a scope request has to cross that boundary.
/// The actor-run revision carries the native session id under one source
/// instance, which is the only evidence that joins the two key spaces.
const SCOPED_SESSIONS: &str = r#"
        SELECT DISTINCT actor.session_key AS runtime_session_key
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
          AND (?2 IS NULL OR session.session_key = ?2)
"#;

/// Scoped response contributions with every bucket's qualification resolved.
/// Each qualification join is a primary-key lookup into the interned
/// specification table, so the row cost stays flat as the corpus grows.
fn scoped_responses(extra: &str) -> String {
    format!(
        r#"
        FROM scope
        JOIN usage_v2_response_contributions u ON u.session_key = scope.runtime_session_key
        JOIN fact_records fr ON fr.fact_id = u.fact_id
        JOIN usage_v2_qualification_specs qi ON qi.qualification_key = u.input_qualification_key
        JOIN usage_v2_qualification_specs qo ON qo.qualification_key = u.output_qualification_key
        JOIN usage_v2_qualification_specs qc ON qc.qualification_key = u.cache_creation_qualification_key
        JOIN usage_v2_qualification_specs qr ON qr.qualification_key = u.cache_read_qualification_key
        WHERE 1 = 1
          {extra}
        "#
    )
}

/// Per-bucket sums split by exact versus merely known quality, followed by the
/// response classification counts and the distinct session count.
fn aggregate_projection() -> String {
    let mut projection = Vec::new();
    for (_, column, alias) in BUCKETS {
        projection.push(format!(
            "COALESCE(SUM(CASE WHEN {alias}.quality = 'exact' THEN u.{column} END), 0)"
        ));
    }
    for (_, column, alias) in BUCKETS {
        projection.push(format!(
            "COALESCE(SUM(CASE WHEN {alias}.quality <> 'exact' AND {alias}.quality IN {KNOWN_QUALITIES} THEN u.{column} END), 0)"
        ));
    }
    let exact_all = BUCKETS
        .iter()
        .map(|(_, _, alias)| format!("{alias}.quality = 'exact'"))
        .collect::<Vec<_>>()
        .join(" AND ");
    let any_known = BUCKETS
        .iter()
        .map(|(_, _, alias)| format!("{alias}.quality IN {KNOWN_QUALITIES}"))
        .collect::<Vec<_>>()
        .join(" OR ");
    projection.push(format!(
        "COALESCE(SUM(CASE WHEN {exact_all} THEN 1 ELSE 0 END), 0)"
    ));
    projection.push(format!(
        "COALESCE(SUM(CASE WHEN NOT ({exact_all}) AND ({any_known}) THEN 1 ELSE 0 END), 0)"
    ));
    projection.push(format!(
        "COALESCE(SUM(CASE WHEN NOT ({any_known}) THEN 1 ELSE 0 END), 0)"
    ));
    projection.push("COUNT(DISTINCT u.session_key)".to_string());
    projection.join(",\n               ")
}

const METADATA_PROJECTION: &str = "MIN(u.source_time), MAX(u.source_time),
               MIN(fr.observed_at), MAX(fr.observed_at), MAX(u.last_commit_seq)";

fn validate_usage_request(request: &UsageRequest) -> Result<ValidatedUsageScope, EngineError> {
    let scope = ValidatedUsageScope {
        project_key: decode_entity_id(&request.project_id, PROJECT_ID_PREFIX, "project id")?,
        session_key: request
            .session_id
            .as_deref()
            .map(|value| decode_entity_id(value, SESSION_ID_PREFIX, "session id"))
            .transpose()?,
    };
    let Some(window) = request.window.as_ref() else {
        return Ok(scope);
    };
    let from = parse_iso_date(&window.from).ok_or_else(|| {
        EngineError::InvalidQuery("usage window from must be a valid YYYY-MM-DD date".to_string())
    })?;
    let to = parse_iso_date(&window.to).ok_or_else(|| {
        EngineError::InvalidQuery("usage window to must be a valid YYYY-MM-DD date".to_string())
    })?;
    if from > to {
        return Err(EngineError::InvalidQuery(
            "usage window from must not be after to".to_string(),
        ));
    }
    if to - from + 1 > i64::from(MAX_USAGE_WINDOW_DAYS) {
        return Err(EngineError::InvalidQuery(format!(
            "usage window must not exceed {MAX_USAGE_WINDOW_DAYS} days"
        )));
    }
    Ok(scope)
}

pub(super) fn read_usage(
    connection: &Connection,
    request: &UsageRequest,
) -> Result<UsageReport, EngineError> {
    let scope = validate_usage_request(request)?;
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin usage snapshot", error))?;
    let watermark = read_committed_watermark(&transaction)?;
    validate_session_membership(&transaction, &scope)?;

    let (aggregate, metadata) = read_scope_aggregate(&transaction, &scope, "")?;
    let coverage = read_coverage(&transaction, &scope)?;
    let window = request
        .window
        .as_ref()
        .map(|window| read_window(&transaction, &scope, window))
        .transpose()?;
    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish usage snapshot", error))?;

    Ok(UsageReport {
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
        window,
    })
}

fn read_window(
    transaction: &Transaction<'_>,
    scope: &ValidatedUsageScope,
    window: &UsageWindow,
) -> Result<UsageWindowReport, EngineError> {
    let days = read_usage_days(transaction, scope, window)?;
    let (aggregate, metadata) =
        read_scope_aggregate(transaction, scope, &format!("AND NOT {DATED}"))?;
    Ok(UsageWindowReport {
        from: window.from.clone(),
        to: window.to.clone(),
        days,
        untimed: UntimedUsageSummary {
            aggregate,
            first_observed_at_unix_ms: metadata.first_observed_at_unix_ms,
            last_observed_at_unix_ms: metadata.last_observed_at_unix_ms,
            last_commit_seq: metadata.last_commit_seq,
        },
    })
}

fn read_scope_aggregate(
    transaction: &Transaction<'_>,
    scope: &ValidatedUsageScope,
    extra: &str,
) -> Result<(UsageAggregate, UsageScopeMetadata), EngineError> {
    let sql = format!(
        "WITH scope AS MATERIALIZED ({SCOPED_SESSIONS})\n         SELECT {projection},\n               {METADATA_PROJECTION}{source}",
        projection = aggregate_projection(),
        source = scoped_responses(extra),
    );
    transaction
        .query_row(
            &sql,
            rusqlite::params![scope.project_key, scope.session_key],
            |row| {
                Ok((
                    usage_aggregate_sqlite(
                        token_values_from_row(row, 0)?,
                        token_values_from_row(row, 4)?,
                        usage_counts_from_row(row, 8)?,
                    )?,
                    UsageScopeMetadata {
                        first_source_time: row.get(12)?,
                        last_source_time: row.get(13)?,
                        first_observed_at_unix_ms: row.get(14)?,
                        last_observed_at_unix_ms: row.get(15)?,
                        last_commit_seq: optional_nonnegative_u64(
                            row.get(16)?,
                            "usage commit sequence",
                        )?,
                    },
                ))
            },
        )
        .map_err(|error| query_sqlite_error("read usage aggregate", error))
}

fn read_usage_days(
    transaction: &Transaction<'_>,
    scope: &ValidatedUsageScope,
    window: &UsageWindow,
) -> Result<Vec<UsageDay>, EngineError> {
    let sql = format!(
        r#"
        WITH scope AS MATERIALIZED ({SCOPED_SESSIONS})
        SELECT substr(u.source_time, 1, 10) AS activity_day,
               {projection},
               {METADATA_PROJECTION}{source}
        GROUP BY activity_day
        ORDER BY activity_day
        "#,
        projection = aggregate_projection(),
        source = scoped_responses(
            &format!(
                "AND {DATED}\n          AND substr(u.source_time, 1, 10) >= ?3\n          AND substr(u.source_time, 1, 10) <= ?4"
            )
        ),
    );
    let mut statement = transaction
        .prepare(&sql)
        .map_err(|error| query_sqlite_error("prepare usage days", error))?;
    let rows = statement
        .query_map(
            rusqlite::params![scope.project_key, scope.session_key, window.from, window.to],
            usage_day_from_row,
        )
        .map_err(|error| query_sqlite_error("read usage days", error))?;
    collect_rows(rows, "collect usage days")
}

fn read_coverage(
    transaction: &Transaction<'_>,
    scope: &ValidatedUsageScope,
) -> Result<Vec<UsageCoverageSummary>, EngineError> {
    // One materialized pass over the scoped contributions feeds all four bucket
    // arms. Repeating the identity bridge per arm made this the slowest query in
    // the pack; SQLite has no reason to hoist it on its own.
    let arms = BUCKETS
        .iter()
        .map(|(bucket, column, _)| {
            format!(
                r#"
                SELECT '{bucket}' AS bucket, q.quality, q.completeness, q.unknown_reason,
                       q.authority, q.native_field, s.model, s.source_time_quality,
                       s.{column} AS token_value
                FROM scoped AS s
                JOIN usage_v2_qualification_specs q ON q.qualification_key = s.{column_key}
                "#,
                column_key = qualification_column(column),
            )
        })
        .collect::<Vec<_>>()
        .join("                UNION ALL");
    let columns = BUCKETS
        .iter()
        .map(|(_, column, _)| format!("u.{column}"))
        .collect::<Vec<_>>()
        .join(", ");
    let qualification_columns = BUCKETS
        .iter()
        .map(|(_, column, _)| format!("u.{}", qualification_column(column)))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        r#"
        WITH scope AS MATERIALIZED ({SCOPED_SESSIONS}),
        scoped AS MATERIALIZED (
            SELECT {columns}, {qualification_columns}, u.model, u.source_time_quality
            FROM scope
            JOIN usage_v2_response_contributions u ON u.session_key = scope.runtime_session_key
        )
        SELECT bucket, quality, completeness, unknown_reason, authority, native_field,
               model, source_time_quality, COUNT(*), COALESCE(SUM(token_value), 0)
        FROM ({arms})
        GROUP BY bucket, quality, completeness, unknown_reason, authority, native_field,
                 model, source_time_quality
        ORDER BY bucket, quality, authority, native_field, model, source_time_quality
        "#
    );
    let mut statement = transaction
        .prepare(&sql)
        .map_err(|error| query_sqlite_error("prepare usage coverage", error))?;
    let rows = statement
        .query_map(
            rusqlite::params![scope.project_key, scope.session_key],
            usage_coverage_from_row,
        )
        .map_err(|error| query_sqlite_error("read usage coverage", error))?;
    collect_rows(rows, "collect usage coverage")
}

fn qualification_column(value_column: &str) -> &'static str {
    match value_column {
        "input_tokens" => "input_qualification_key",
        "output_tokens" => "output_qualification_key",
        "cache_creation_input_tokens" => "cache_creation_qualification_key",
        _ => "cache_read_qualification_key",
    }
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

fn usage_day_from_row(row: &Row<'_>) -> rusqlite::Result<UsageDay> {
    Ok(UsageDay {
        date: row.get(0)?,
        aggregate: usage_aggregate_sqlite(
            token_values_from_row(row, 1)?,
            token_values_from_row(row, 5)?,
            usage_counts_from_row(row, 9)?,
        )?,
        first_source_time: row.get(13)?,
        last_source_time: row.get(14)?,
        first_observed_at_unix_ms: row.get(15)?,
        last_observed_at_unix_ms: row.get(16)?,
        last_commit_seq: nonnegative_u64(row.get(17)?, "daily usage commit sequence")?,
    })
}

fn usage_coverage_from_row(row: &Row<'_>) -> rusqlite::Result<UsageCoverageSummary> {
    Ok(UsageCoverageSummary {
        bucket: row.get(0)?,
        value_quality: row.get(1)?,
        completeness: row.get(2)?,
        unknown_reason: row.get(3)?,
        authority: row.get(4)?,
        native_field: row.get(5)?,
        model: row.get(6)?,
        source_time_quality: row.get(7)?,
        contribution_count: nonnegative_u64(row.get(8)?, "usage coverage count")?,
        tokens: nonnegative_u64(row.get(9)?, "usage coverage tokens")?,
    })
}

fn usage_counts_from_row(row: &Row<'_>, offset: usize) -> rusqlite::Result<UsageCounts> {
    Ok(UsageCounts {
        exact_contribution_count: nonnegative_u64(row.get(offset)?, "exact responses")?,
        estimated_contribution_count: nonnegative_u64(row.get(offset + 1)?, "estimated responses")?,
        unknown_contribution_count: nonnegative_u64(row.get(offset + 2)?, "unknown responses")?,
        session_count: nonnegative_u64(row.get(offset + 3)?, "usage session count")?,
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
    counts: UsageCounts,
) -> Result<UsageAggregate, EngineError> {
    let contribution_count = checked_add(
        checked_add(
            counts.exact_contribution_count,
            counts.estimated_contribution_count,
        )?,
        counts.unknown_contribution_count,
    )?;
    let quality = match (
        counts.exact_contribution_count > 0,
        counts.estimated_contribution_count > 0,
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
        exact_contribution_count: counts.exact_contribution_count,
        estimated_contribution_count: counts.estimated_contribution_count,
        unknown_contribution_count: counts.unknown_contribution_count,
        contribution_count,
        session_count: counts.session_count,
    })
}

fn usage_aggregate_sqlite(
    exact: UsageTokenValues,
    estimated: UsageTokenValues,
    counts: UsageCounts,
) -> rusqlite::Result<UsageAggregate> {
    usage_aggregate(exact, estimated, counts).map_err(|_| integral_overflow())
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
mod tests;
