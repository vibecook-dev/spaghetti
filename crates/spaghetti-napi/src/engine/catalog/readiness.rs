//! The readiness vector.
//!
//! One struct replaces the five overlapping readiness surfaces the 012B
//! implementation grew. Every field is derived from committed rows inside one
//! snapshot — there is no durable state machine, no epoch, and no publication
//! record to fall out of step with the data.
//!
//! The fields are independent on purpose: `catalog` can be ready while
//! `history` is still indexing and `search` is unavailable. That is the whole
//! point of catalog-first startup.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::super::query_pool::read_committed_watermark;
use super::super::runtime_semantic_projection::USAGE_V2_PROJECTION_ID;
use super::super::EngineError;

/// State of one readiness field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReadinessState {
    /// No work has been committed yet.
    Pending,
    /// Committed and converging. `detail` says how far.
    Indexing,
    /// Complete for everything the catalog knows about.
    Ready,
    /// Usable but knowingly incomplete; `detail` says why.
    Degraded,
    /// Not available in this build or configuration.
    Unavailable,
}

impl ReadinessState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Indexing => "indexing",
            Self::Ready => "ready",
            Self::Degraded => "degraded",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ReadinessField {
    pub state: ReadinessState,
    /// Commit sequence the evidence for this field was read at.
    pub committed_at_seq: u64,
    #[ts(optional)]
    pub detail: Option<String>,
}

impl ReadinessField {
    fn new(state: ReadinessState, committed_at_seq: u64, detail: Option<String>) -> Self {
        Self {
            state,
            committed_at_seq,
            detail,
        }
    }
}

/// What the host, `spag doctor`, and the playground library screen all read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Readiness {
    pub catalog: ReadinessField,
    pub history: ReadinessField,
    pub usage: ReadinessField,
    pub capabilities: ReadinessField,
    pub artifacts: ReadinessField,
    pub search: ReadinessField,
    pub at_commit_seq: u64,
}

/// Row counts one snapshot needs to answer every field.
struct Evidence {
    sources: i64,
    degraded_sources: i64,
    degraded_reason: Option<String>,
    catalog_projects: i64,
    catalog_sessions: i64,
    transcript_sessions: i64,
    canonical_sessions: i64,
    hydrated_sessions: i64,
    scanned_at_commit_seq: i64,
    /// Read from the same durable marker the query SQL uses.
    search_ready: bool,
}

pub fn read_readiness(connection: &Connection) -> Result<Readiness, EngineError> {
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| sqlite_error("begin readiness snapshot", error))?;
    let watermark = read_committed_watermark(&transaction)?;
    let evidence = read_evidence(&transaction)?;
    let usage = read_usage_field(&transaction, watermark)?;
    transaction
        .commit()
        .map_err(|error| sqlite_error("finish readiness snapshot", error))?;

    let scanned_at = evidence.scanned_at_commit_seq.max(0) as u64;
    let catalog = catalog_field(&evidence, scanned_at);
    let history = convergence_field(
        &evidence,
        evidence.canonical_sessions,
        watermark,
        "transcripts decoded",
    );
    // Capability and artifact facts are emitted by the same decode pass that
    // produces messages, so they converge with hydration rather than on their
    // own schedule. Reporting them from one number is the honest description
    // of how the decode spine actually works.
    let capabilities = convergence_field(
        &evidence,
        evidence.hydrated_sessions,
        watermark,
        "sessions decoded for capability facts",
    );
    let artifacts = convergence_field(
        &evidence,
        evidence.hydrated_sessions,
        watermark,
        "sessions decoded for artifact facts",
    );
    let search = if evidence.search_ready {
        convergence_field(
            &evidence,
            evidence.hydrated_sessions,
            watermark,
            "sessions indexed for search",
        )
    } else {
        ReadinessField::new(
            ReadinessState::Pending,
            watermark,
            Some("full-text bootstrap has not finished".to_string()),
        )
    };

    Ok(Readiness {
        catalog,
        history,
        usage,
        capabilities,
        artifacts,
        search,
        at_commit_seq: watermark,
    })
}

fn catalog_field(evidence: &Evidence, scanned_at: u64) -> ReadinessField {
    if evidence.sources == 0 {
        return ReadinessField::new(
            ReadinessState::Pending,
            scanned_at,
            Some("no configured source has been scanned yet".to_string()),
        );
    }
    let summary = format!(
        "{} projects, {} sessions",
        evidence.catalog_projects, evidence.catalog_sessions
    );
    if evidence.degraded_sources > 0 {
        let reason = evidence
            .degraded_reason
            .clone()
            .unwrap_or_else(|| "a configured source could not be read completely".to_string());
        return ReadinessField::new(
            ReadinessState::Degraded,
            scanned_at,
            Some(format!("{summary}; {reason}")),
        );
    }
    ReadinessField::new(ReadinessState::Ready, scanned_at, Some(summary))
}

/// A background projection is `ready` once it covers every transcript-backed
/// catalog session, `indexing` while it is behind, and `pending` before the
/// catalog itself knows what there is to converge on.
fn convergence_field(
    evidence: &Evidence,
    converged: i64,
    watermark: u64,
    label: &str,
) -> ReadinessField {
    if evidence.sources == 0 {
        return ReadinessField::new(ReadinessState::Pending, watermark, None);
    }
    let expected = evidence.transcript_sessions;
    if expected == 0 {
        return ReadinessField::new(
            ReadinessState::Ready,
            watermark,
            Some(format!("0 of 0 {label}")),
        );
    }
    let state = if converged >= expected {
        ReadinessState::Ready
    } else {
        ReadinessState::Indexing
    };
    ReadinessField::new(
        state,
        watermark,
        Some(format!("{converged} of {expected} {label}")),
    )
}

/// Usage readiness is owned by the usage lane: it is whatever the usage-v2
/// projection has published. This reads that row rather than reinterpreting
/// it, so the two cannot disagree.
fn read_usage_field(
    connection: &Connection,
    watermark: u64,
) -> Result<ReadinessField, EngineError> {
    let mut statement = connection
        .prepare(
            r#"
            SELECT readiness, COUNT(*), MAX(COALESCE(last_commit_seq, 0)), MAX(detail)
            FROM projection_versions
            WHERE projection_id = ?1
            GROUP BY readiness
            ORDER BY readiness
            "#,
        )
        .map_err(|error| sqlite_error("prepare usage readiness", error))?;
    let rows = statement
        .query_map(params![USAGE_V2_PROJECTION_ID], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(|error| sqlite_error("read usage readiness", error))?;

    let mut total = 0_i64;
    let mut ready = 0_i64;
    let mut pending_detail = None;
    let mut unavailable = 0_i64;
    let mut commit_seq = 0_i64;
    for row in rows {
        let (readiness, count, last_commit, detail) =
            row.map_err(|error| sqlite_error("decode usage readiness", error))?;
        total += count;
        commit_seq = commit_seq.max(last_commit);
        match readiness.as_str() {
            "ready" => ready += count,
            "unavailable" => {
                unavailable += count;
                pending_detail = pending_detail.or(detail);
            }
            _ => pending_detail = pending_detail.or(detail),
        }
    }

    let at = if commit_seq > 0 {
        commit_seq.max(0) as u64
    } else {
        watermark
    };
    if total == 0 {
        return Ok(ReadinessField::new(
            ReadinessState::Pending,
            at,
            Some("no usage projection has been committed yet".to_string()),
        ));
    }
    if unavailable > 0 {
        return Ok(ReadinessField::new(
            ReadinessState::Degraded,
            at,
            pending_detail
                .or_else(|| Some("a source cannot produce response-level usage".to_string())),
        ));
    }
    if ready >= total {
        return Ok(ReadinessField::new(
            ReadinessState::Ready,
            at,
            Some(format!("{ready} of {total} sources")),
        ));
    }
    Ok(ReadinessField::new(
        ReadinessState::Indexing,
        at,
        Some(format!("{ready} of {total} sources")),
    ))
}

fn read_evidence(connection: &Connection) -> Result<Evidence, EngineError> {
    let (sources, degraded_sources, degraded_reason, scanned_at_commit_seq) = connection
        .query_row(
            r#"
            SELECT COUNT(*),
                   COALESCE(SUM(degraded), 0),
                   MAX(degraded_reason),
                   COALESCE(MAX(scanned_at_commit_seq), 0)
            FROM catalog_sources
            "#,
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .map_err(|error| sqlite_error("read catalog source evidence", error))?;

    let (catalog_projects, catalog_sessions, transcript_sessions) = connection
        .query_row(
            r#"
            SELECT (SELECT COUNT(*) FROM catalog_projects),
                   (SELECT COUNT(*) FROM catalog_sessions),
                   (SELECT COUNT(*) FROM catalog_sessions WHERE transcript_present = 1)
            "#,
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .map_err(|error| sqlite_error("read catalog row counts", error))?;

    let (canonical_sessions, hydrated_sessions) = connection
        .query_row(
            concat!(
                r#"
            SELECT (
                SELECT COUNT(*) FROM catalog_sessions cs
                JOIN canonical_sessions can ON can.session_key = cs.session_key
                WHERE cs.transcript_present = 1
            ), (
                SELECT COUNT(*) FROM catalog_sessions cs
                WHERE cs.transcript_present = 1 AND "#,
                session_hydrated_sql!(),
                r#"
            )
            "#
            ),
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .map_err(|error| sqlite_error("read catalog convergence evidence", error))?;

    let search_ready: bool = connection
        .query_row(concat!("SELECT ", search_ready_sql!()), [], |row| {
            row.get(0)
        })
        .map_err(|error| sqlite_error("read durable search readiness", error))?;

    Ok(Evidence {
        sources,
        degraded_sources,
        degraded_reason,
        catalog_projects,
        catalog_sessions,
        transcript_sessions,
        canonical_sessions,
        hydrated_sessions,
        scanned_at_commit_seq,
        search_ready,
    })
}

fn sqlite_error(operation: &'static str, error: rusqlite::Error) -> EngineError {
    EngineError::Sqlite {
        operation,
        detail: error.to_string(),
    }
}
