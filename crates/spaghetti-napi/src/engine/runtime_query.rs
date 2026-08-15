//! Read-only RFC 011 runtime-state and presence query pack.

use std::cmp::Ordering;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rusqlite::{Connection, OptionalExtension, Row, Transaction};
use serde::{Deserialize, Serialize};

use super::query_identity::{
    decode_entity_id, encode_entity_id, FACT_ID_PREFIX, PRESENCE_ID_PREFIX, PROJECT_ID_PREFIX,
    RUN_ID_PREFIX, SESSION_ID_PREFIX,
};
use super::query_pool::read_committed_watermark;
use super::EngineError;

pub const RUNTIME_QUERY_CONTRACT_VERSION: u32 = 1;
pub const DEFAULT_RUNTIME_PAGE_LIMIT: u32 = 50;
const MAX_RUNTIME_PAGE_LIMIT: u32 = 200;
const MAX_RUNTIME_CURSOR_BYTES: usize = 32 * 1024;
const RUN_ENTRY_KIND: i64 = 1;
const PRESENCE_ENTRY_KIND: i64 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSnapshotRequest {
    pub project_id: Option<String>,
    pub session_id: Option<String>,
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStateRequest {
    pub run_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStateLookup {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub run: Option<RuntimeRunSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSnapshot {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub project_id: Option<String>,
    pub session_id: Option<String>,
    pub entries: Vec<RuntimeSnapshotEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSnapshotEntry {
    pub kind: String,
    pub run: Option<RuntimeRunSnapshot>,
    pub presence: Option<RuntimePresenceSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeRunSnapshot {
    pub run_id: String,
    pub session_id: String,
    pub project_id: Option<String>,
    pub adapter_id: String,
    pub source_instance_id: u64,
    pub native_run_id: String,
    pub parent_run_id: Option<String>,
    pub native_session_id: Option<String>,
    pub native_project_key: Option<String>,
    pub session_present: bool,
    pub state: Option<String>,
    pub decisive_evidence: Option<RuntimeRunEvidence>,
    pub evidence_count: u64,
    pub last_activity_at: Option<String>,
    pub terminal_at: Option<String>,
    pub presence_count: u64,
    pub conflicting_presence_count: u64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeRunEvidence {
    pub evidence_id: String,
    pub kind: String,
    pub strength: String,
    pub native_state: Option<String>,
    pub source_time: Option<String>,
    pub source_time_quality: Option<String>,
    pub observed_at_unix_ms: i64,
    pub source_object_id: u64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePresenceSnapshot {
    pub presence_id: String,
    pub session_id: String,
    pub run_id: String,
    pub project_id: Option<String>,
    pub adapter_id: String,
    pub source_instance_id: u64,
    pub native_session_id: String,
    pub native_pid: u32,
    pub cwd: String,
    pub started_at: String,
    pub started_at_quality: String,
    pub native_kind: Option<String>,
    pub entrypoint: Option<String>,
    pub name: Option<String>,
    pub native_status: Option<String>,
    pub updated_at: Option<String>,
    pub updated_at_quality: Option<String>,
    pub status_updated_at: Option<String>,
    pub status_updated_at_quality: Option<String>,
    pub native_process_started_at: Option<String>,
    pub version: Option<String>,
    pub peer_protocol: Option<u32>,
    pub name_source: Option<String>,
    pub bridge_session_id: Option<String>,
    pub messaging_socket_path: Option<String>,
    pub presence_status: String,
    pub decisive_fact_id: String,
    pub assertion_count: u64,
    pub competing_assertion_count: u64,
    pub observed_at_unix_ms: i64,
    pub session_present: bool,
    pub run_present: bool,
    pub last_commit_seq: u64,
}

#[derive(Debug)]
pub(super) struct ValidatedRuntimeRequest {
    project_key: Option<Vec<u8>>,
    session_key: Option<Vec<u8>>,
    cursor: Option<RuntimeCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RuntimeCursor {
    version: u32,
    at_commit_seq: u64,
    project_id: Option<String>,
    session_id: Option<String>,
    sort_rank: i64,
    sort_time: String,
    sort_kind: i64,
    entity_key: String,
}

#[derive(Debug)]
struct RuntimeEntryRow {
    entry: RuntimeSnapshotEntry,
    sort_rank: i64,
    sort_time: String,
    sort_kind: i64,
    entity_key: Vec<u8>,
}

pub(super) fn validate_runtime_request(
    request: &RuntimeSnapshotRequest,
) -> Result<ValidatedRuntimeRequest, EngineError> {
    if !(1..=MAX_RUNTIME_PAGE_LIMIT).contains(&request.limit) {
        return Err(EngineError::InvalidQuery(format!(
            "runtime page limit must be between 1 and {MAX_RUNTIME_PAGE_LIMIT}, got {}",
            request.limit
        )));
    }
    let project_key = request
        .project_id
        .as_deref()
        .map(|value| decode_entity_id(value, PROJECT_ID_PREFIX, "runtime project id"))
        .transpose()?;
    let session_key = request
        .session_id
        .as_deref()
        .map(|value| decode_entity_id(value, SESSION_ID_PREFIX, "runtime session id"))
        .transpose()?;
    let cursor = request
        .cursor
        .as_deref()
        .map(|value| decode_runtime_cursor(value, request))
        .transpose()?;
    Ok(ValidatedRuntimeRequest {
        project_key,
        session_key,
        cursor,
    })
}

pub(super) fn validate_run_state_request(
    request: &RunStateRequest,
) -> Result<Vec<u8>, EngineError> {
    decode_entity_id(&request.run_id, RUN_ID_PREFIX, "run state id")
}

pub(super) fn read_run_state(
    connection: &Connection,
    request: &RunStateRequest,
) -> Result<RunStateLookup, EngineError> {
    let run_key = validate_run_state_request(request)?;
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin run state snapshot", error))?;
    let watermark = read_committed_watermark(&transaction)?;
    let mut statement = transaction
        .prepare(
            r#"
            SELECT cr.run_key, cr.session_key, cs.project_key,
                   cr.native_run_id, cr.parent_run_key,
                   cs.native_session_id, cs.native_project_key,
                   si.adapter_id, fr.source_instance_id,
                   ors.state, ors.last_activity_at, ors.terminal_at,
                   (SELECT COALESCE(SUM(all_re.evidence_count), 0) FROM run_evidence all_re
                    WHERE all_re.run_key = cr.run_key) AS evidence_count,
                   re.fact_id, re.evidence_kind, re.evidence_strength,
                   re.native_state, re.source_time, re.source_time_quality,
                   evidence_fr.observed_at, re.source_object_id,
                   re.last_commit_seq AS evidence_commit_seq,
                   (SELECT COUNT(*) FROM canonical_presences cp
                    WHERE cp.run_key = cr.run_key) AS presence_count,
                   (SELECT COUNT(*) FROM canonical_presences cp
                    WHERE cp.run_key = cr.run_key
                      AND cp.presence_status = 'conflicting')
                       AS conflicting_presence_count,
                   MAX(cr.last_commit_seq,
                       COALESCE(ors.last_commit_seq, 0),
                       COALESCE(re.last_commit_seq, 0),
                       COALESCE((SELECT MAX(cp.last_commit_seq)
                                 FROM canonical_presences cp
                                 WHERE cp.run_key = cr.run_key), 0))
                       AS last_commit_seq,
                   MAX(cr.last_commit_seq,
                       COALESCE(ors.last_commit_seq, 0),
                       COALESCE(re.last_commit_seq, 0),
                       COALESCE((SELECT MAX(cp.last_commit_seq)
                                 FROM canonical_presences cp
                                 WHERE cp.run_key = cr.run_key), 0))
                       AS sort_rank,
                   MAX(COALESCE(ors.terminal_at, ''),
                       COALESCE(ors.last_activity_at, ''),
                       COALESCE(re.source_time, '')) AS sort_time
            FROM canonical_runs cr
            JOIN fact_records fr ON fr.fact_id = cr.fact_id
            JOIN source_instances si
              ON si.source_instance_id = fr.source_instance_id
            LEFT JOIN canonical_sessions cs
              ON cs.session_key = cr.session_key
            LEFT JOIN observed_run_states ors
              ON ors.run_key = cr.run_key
            LEFT JOIN run_evidence re
              ON re.fact_id = ors.decisive_evidence_id
            LEFT JOIN fact_records evidence_fr
              ON evidence_fr.fact_id = re.fact_id
            WHERE cr.run_key = ?1
            "#,
        )
        .map_err(|error| query_sqlite_error("prepare run state lookup", error))?;
    let run = statement
        .query_row([&run_key], |row| {
            decode_run_entry(row).map_err(to_rusqlite_query_error)
        })
        .optional()
        .map_err(|error| query_sqlite_error("read run state lookup", error))?
        .map(|row| row.entry.run.expect("run decoder always returns a run"));
    drop(statement);
    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish run state snapshot", error))?;
    Ok(RunStateLookup {
        contract_version: RUNTIME_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        run,
    })
}

pub(super) fn read_runtime_snapshot(
    connection: &Connection,
    request: &RuntimeSnapshotRequest,
) -> Result<RuntimeSnapshot, EngineError> {
    let scope = validate_runtime_request(request)?;
    let cursor_key = runtime_cursor_key(scope.cursor.as_ref())?;
    let cursor_rank = scope
        .cursor
        .as_ref()
        .map(|cursor| cursor.sort_rank)
        .unwrap_or_default();
    let cursor_time = scope
        .cursor
        .as_ref()
        .map(|cursor| cursor.sort_time.as_str())
        .unwrap_or("");
    let cursor_kind = scope
        .cursor
        .as_ref()
        .map(|cursor| cursor.sort_kind)
        .unwrap_or_default();

    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin runtime snapshot", error))?;
    let watermark = read_committed_watermark(&transaction)?;
    validate_runtime_cursor_watermark(scope.cursor.as_ref(), watermark)?;
    validate_runtime_scope_membership(&transaction, &scope)?;

    let fetch_limit = i64::from(request.limit) + 1;
    let mut entries = read_run_entries(
        &transaction,
        &scope,
        cursor_rank,
        cursor_time,
        cursor_kind,
        &cursor_key,
        fetch_limit,
    )?;
    entries.extend(read_presence_entries(
        &transaction,
        &scope,
        cursor_rank,
        cursor_time,
        cursor_kind,
        &cursor_key,
        fetch_limit,
    )?);
    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish runtime snapshot", error))?;

    entries.sort_by(compare_runtime_rows);
    let has_more = entries.len() > request.limit as usize;
    if has_more {
        entries.truncate(request.limit as usize);
    }
    let next_cursor = if has_more {
        entries
            .last()
            .map(|row| {
                encode_runtime_cursor(&RuntimeCursor {
                    version: RUNTIME_QUERY_CONTRACT_VERSION,
                    at_commit_seq: watermark,
                    project_id: request.project_id.clone(),
                    session_id: request.session_id.clone(),
                    sort_rank: row.sort_rank,
                    sort_time: row.sort_time.clone(),
                    sort_kind: row.sort_kind,
                    entity_key: URL_SAFE_NO_PAD.encode(&row.entity_key),
                })
            })
            .transpose()?
    } else {
        None
    };

    Ok(RuntimeSnapshot {
        contract_version: RUNTIME_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        project_id: request.project_id.clone(),
        session_id: request.session_id.clone(),
        entries: entries.into_iter().map(|row| row.entry).collect(),
        next_cursor,
    })
}

fn read_run_entries(
    transaction: &Transaction<'_>,
    scope: &ValidatedRuntimeRequest,
    cursor_rank: i64,
    cursor_time: &str,
    cursor_kind: i64,
    cursor_key: &[u8],
    fetch_limit: i64,
) -> Result<Vec<RuntimeEntryRow>, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            WITH run_rows AS (
                SELECT cr.run_key, cr.session_key, cs.project_key,
                       cr.native_run_id, cr.parent_run_key,
                       cs.native_session_id, cs.native_project_key,
                       si.adapter_id, fr.source_instance_id,
                       ors.state, ors.last_activity_at, ors.terminal_at,
                       (SELECT COALESCE(SUM(all_re.evidence_count), 0) FROM run_evidence all_re
                        WHERE all_re.run_key = cr.run_key) AS evidence_count,
                       re.fact_id, re.evidence_kind, re.evidence_strength,
                       re.native_state, re.source_time, re.source_time_quality,
                       evidence_fr.observed_at, re.source_object_id,
                       re.last_commit_seq AS evidence_commit_seq,
                       (SELECT COUNT(*) FROM canonical_presences cp
                        WHERE cp.run_key = cr.run_key) AS presence_count,
                       (SELECT COUNT(*) FROM canonical_presences cp
                        WHERE cp.run_key = cr.run_key
                          AND cp.presence_status = 'conflicting')
                           AS conflicting_presence_count,
                       MAX(cr.last_commit_seq,
                           COALESCE(ors.last_commit_seq, 0),
                           COALESCE(re.last_commit_seq, 0),
                           COALESCE((SELECT MAX(cp.last_commit_seq)
                                     FROM canonical_presences cp
                                     WHERE cp.run_key = cr.run_key), 0))
                           AS last_commit_seq,
                       MAX(cr.last_commit_seq,
                           COALESCE(ors.last_commit_seq, 0),
                           COALESCE(re.last_commit_seq, 0),
                           COALESCE((SELECT MAX(cp.last_commit_seq)
                                     FROM canonical_presences cp
                                     WHERE cp.run_key = cr.run_key), 0))
                           AS sort_rank,
                       MAX(COALESCE(ors.terminal_at, ''),
                           COALESCE(ors.last_activity_at, ''),
                           COALESCE(re.source_time, '')) AS sort_time
                FROM canonical_runs cr
                JOIN fact_records fr ON fr.fact_id = cr.fact_id
                JOIN source_instances si
                  ON si.source_instance_id = fr.source_instance_id
                LEFT JOIN canonical_sessions cs
                  ON cs.session_key = cr.session_key
                LEFT JOIN observed_run_states ors
                  ON ors.run_key = cr.run_key
                LEFT JOIN run_evidence re
                  ON re.fact_id = ors.decisive_evidence_id
                LEFT JOIN fact_records evidence_fr
                  ON evidence_fr.fact_id = re.fact_id
                WHERE (?1 IS NULL OR cs.project_key = ?1)
                  AND (?2 IS NULL OR cr.session_key = ?2)
            )
            SELECT run_key, session_key, project_key, native_run_id,
                   parent_run_key, native_session_id, native_project_key,
                   adapter_id, source_instance_id, state, last_activity_at,
                   terminal_at, evidence_count, fact_id, evidence_kind,
                   evidence_strength, native_state, source_time,
                   source_time_quality, observed_at, source_object_id,
                   evidence_commit_seq, presence_count,
                   conflicting_presence_count, last_commit_seq,
                   sort_rank, sort_time
            FROM run_rows
            WHERE (?3 = 0)
               OR sort_rank < ?4
               OR (sort_rank = ?4 AND sort_time < ?5)
               OR (sort_rank = ?4 AND sort_time = ?5 AND ?6 < ?7)
               OR (sort_rank = ?4 AND sort_time = ?5 AND ?6 = ?7
                   AND run_key < ?8)
            ORDER BY sort_rank DESC, sort_time DESC, run_key DESC
            LIMIT ?9
            "#,
        )
        .map_err(|error| query_sqlite_error("prepare runtime run page", error))?;
    let mut rows = statement
        .query(rusqlite::params![
            scope.project_key,
            scope.session_key,
            i64::from(scope.cursor.is_some()),
            cursor_rank,
            cursor_time,
            RUN_ENTRY_KIND,
            cursor_kind,
            cursor_key,
            fetch_limit,
        ])
        .map_err(|error| query_sqlite_error("execute runtime run page", error))?;
    let mut entries = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| query_sqlite_error("advance runtime run page", error))?
    {
        entries.push(decode_run_entry(row)?);
    }
    Ok(entries)
}

fn decode_run_entry(row: &Row<'_>) -> Result<RuntimeEntryRow, EngineError> {
    let run_key: Vec<u8> = query_get(row, 0, "decode runtime run key")?;
    let session_key: Vec<u8> = query_get(row, 1, "decode runtime run session key")?;
    let project_key: Option<Vec<u8>> = query_get(row, 2, "decode runtime run project key")?;
    let parent_run_key: Option<Vec<u8>> = query_get(row, 4, "decode runtime parent run key")?;
    let evidence_fact_id: Option<Vec<u8>> = query_get(row, 13, "decode runtime evidence id")?;
    let decisive_evidence = evidence_fact_id
        .map(|fact_id| {
            Ok(RuntimeRunEvidence {
                evidence_id: encode_entity_id(FACT_ID_PREFIX, &fact_id),
                kind: query_get(row, 14, "decode runtime evidence kind")?,
                strength: query_get(row, 15, "decode runtime evidence strength")?,
                native_state: query_get(row, 16, "decode runtime native state")?,
                source_time: query_get(row, 17, "decode runtime evidence time")?,
                source_time_quality: query_get(row, 18, "decode runtime evidence quality")?,
                observed_at_unix_ms: query_get(row, 19, "decode runtime evidence observation")?,
                source_object_id: decode_nonnegative_u64(
                    query_get(row, 20, "decode runtime evidence source object")?,
                    "runtime evidence source object id",
                )?,
                last_commit_seq: decode_nonnegative_u64(
                    query_get(row, 21, "decode runtime evidence commit")?,
                    "runtime evidence commit sequence",
                )?,
            })
        })
        .transpose()?;
    let sort_rank = query_get(row, 25, "decode runtime run rank")?;
    let sort_time = query_get(row, 26, "decode runtime run order time")?;
    let session_present = project_key.is_some();
    let run = RuntimeRunSnapshot {
        run_id: encode_entity_id(RUN_ID_PREFIX, &run_key),
        session_id: encode_entity_id(SESSION_ID_PREFIX, &session_key),
        project_id: project_key
            .as_deref()
            .map(|key| encode_entity_id(PROJECT_ID_PREFIX, key)),
        adapter_id: query_get(row, 7, "decode runtime run adapter")?,
        source_instance_id: decode_nonnegative_u64(
            query_get(row, 8, "decode runtime run source instance")?,
            "runtime run source instance id",
        )?,
        native_run_id: query_get(row, 3, "decode runtime native run id")?,
        parent_run_id: parent_run_key
            .as_deref()
            .map(|key| encode_entity_id(RUN_ID_PREFIX, key)),
        native_session_id: query_get(row, 5, "decode runtime native session id")?,
        native_project_key: query_get(row, 6, "decode runtime native project key")?,
        session_present,
        state: query_get(row, 9, "decode runtime observed state")?,
        decisive_evidence,
        evidence_count: decode_nonnegative_u64(
            query_get(row, 12, "decode runtime evidence count")?,
            "runtime evidence count",
        )?,
        last_activity_at: query_get(row, 10, "decode runtime last activity")?,
        terminal_at: query_get(row, 11, "decode runtime terminal time")?,
        presence_count: decode_nonnegative_u64(
            query_get(row, 22, "decode runtime presence count")?,
            "runtime presence count",
        )?,
        conflicting_presence_count: decode_nonnegative_u64(
            query_get(row, 23, "decode runtime conflicting presence count")?,
            "runtime conflicting presence count",
        )?,
        last_commit_seq: decode_nonnegative_u64(
            query_get(row, 24, "decode runtime run commit")?,
            "runtime run commit sequence",
        )?,
    };
    Ok(RuntimeEntryRow {
        entry: RuntimeSnapshotEntry {
            kind: "run".to_string(),
            run: Some(run),
            presence: None,
        },
        sort_rank,
        sort_time,
        sort_kind: RUN_ENTRY_KIND,
        entity_key: run_key,
    })
}

fn read_presence_entries(
    transaction: &Transaction<'_>,
    scope: &ValidatedRuntimeRequest,
    cursor_rank: i64,
    cursor_time: &str,
    cursor_kind: i64,
    cursor_key: &[u8],
    fetch_limit: i64,
) -> Result<Vec<RuntimeEntryRow>, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            WITH presence_rows AS (
                SELECT cp.presence_key, cp.session_key, cp.run_key,
                       cs.project_key, si.adapter_id, fr.source_instance_id,
                       cp.native_session_id, cp.native_pid, cp.cwd,
                       cp.started_at, cp.started_at_quality, cp.native_kind,
                       cp.entrypoint, cp.name, cp.native_status, cp.updated_at,
                       cp.updated_at_quality, cp.status_updated_at,
                       cp.status_updated_at_quality,
                       cp.native_process_started_at, cp.version,
                       cp.peer_protocol, cp.name_source, cp.bridge_session_id,
                       cp.messaging_socket_path, cp.presence_status,
                       cp.decisive_fact_id, cp.assertion_count,
                       cp.competing_assertion_count, fr.observed_at,
                       CASE WHEN cs.session_key IS NULL THEN 0 ELSE 1 END
                           AS session_present,
                       CASE WHEN cr.run_key IS NULL THEN 0 ELSE 1 END
                           AS run_present,
                       cp.last_commit_seq,
                       cp.last_commit_seq AS sort_rank,
                       MAX(COALESCE(cp.status_updated_at, ''),
                           COALESCE(cp.updated_at, ''), cp.started_at)
                           AS sort_time
                FROM canonical_presences cp
                JOIN fact_records fr ON fr.fact_id = cp.decisive_fact_id
                JOIN source_instances si
                  ON si.source_instance_id = fr.source_instance_id
                LEFT JOIN canonical_sessions cs
                  ON cs.session_key = cp.session_key
                LEFT JOIN canonical_runs cr ON cr.run_key = cp.run_key
                WHERE (?1 IS NULL OR cs.project_key = ?1)
                  AND (?2 IS NULL OR cp.session_key = ?2)
            )
            SELECT presence_key, session_key, run_key, project_key,
                   adapter_id, source_instance_id, native_session_id,
                   native_pid, cwd, started_at, started_at_quality,
                   native_kind, entrypoint, name, native_status, updated_at,
                   updated_at_quality, status_updated_at,
                   status_updated_at_quality, native_process_started_at,
                   version, peer_protocol, name_source, bridge_session_id,
                   messaging_socket_path, presence_status, decisive_fact_id,
                   assertion_count, competing_assertion_count, observed_at,
                   session_present, run_present, last_commit_seq,
                   sort_rank, sort_time
            FROM presence_rows
            WHERE (?3 = 0)
               OR sort_rank < ?4
               OR (sort_rank = ?4 AND sort_time < ?5)
               OR (sort_rank = ?4 AND sort_time = ?5 AND ?6 < ?7)
               OR (sort_rank = ?4 AND sort_time = ?5 AND ?6 = ?7
                   AND presence_key < ?8)
            ORDER BY sort_rank DESC, sort_time DESC, presence_key DESC
            LIMIT ?9
            "#,
        )
        .map_err(|error| query_sqlite_error("prepare runtime presence page", error))?;
    let mut rows = statement
        .query(rusqlite::params![
            scope.project_key,
            scope.session_key,
            i64::from(scope.cursor.is_some()),
            cursor_rank,
            cursor_time,
            PRESENCE_ENTRY_KIND,
            cursor_kind,
            cursor_key,
            fetch_limit,
        ])
        .map_err(|error| query_sqlite_error("execute runtime presence page", error))?;
    let mut entries = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| query_sqlite_error("advance runtime presence page", error))?
    {
        entries.push(decode_presence_entry(row)?);
    }
    Ok(entries)
}

fn decode_presence_entry(row: &Row<'_>) -> Result<RuntimeEntryRow, EngineError> {
    let presence_key: Vec<u8> = query_get(row, 0, "decode runtime presence key")?;
    let session_key: Vec<u8> = query_get(row, 1, "decode runtime presence session key")?;
    let run_key: Vec<u8> = query_get(row, 2, "decode runtime presence run key")?;
    let project_key: Option<Vec<u8>> = query_get(row, 3, "decode runtime presence project key")?;
    let decisive_fact_id: Vec<u8> = query_get(row, 26, "decode runtime presence decisive fact")?;
    let peer_protocol = decode_optional_u32(
        query_get(row, 21, "decode runtime presence peer protocol")?,
        "runtime presence peer protocol",
    )?;
    let sort_rank = query_get(row, 33, "decode runtime presence rank")?;
    let sort_time = query_get(row, 34, "decode runtime presence order time")?;
    let presence = RuntimePresenceSnapshot {
        presence_id: encode_entity_id(PRESENCE_ID_PREFIX, &presence_key),
        session_id: encode_entity_id(SESSION_ID_PREFIX, &session_key),
        run_id: encode_entity_id(RUN_ID_PREFIX, &run_key),
        project_id: project_key
            .as_deref()
            .map(|key| encode_entity_id(PROJECT_ID_PREFIX, key)),
        adapter_id: query_get(row, 4, "decode runtime presence adapter")?,
        source_instance_id: decode_nonnegative_u64(
            query_get(row, 5, "decode runtime presence source instance")?,
            "runtime presence source instance id",
        )?,
        native_session_id: query_get(row, 6, "decode runtime presence native session")?,
        native_pid: decode_nonnegative_u32(
            query_get(row, 7, "decode runtime presence pid")?,
            "runtime presence pid",
        )?,
        cwd: query_get(row, 8, "decode runtime presence cwd")?,
        started_at: query_get(row, 9, "decode runtime presence start")?,
        started_at_quality: query_get(row, 10, "decode runtime presence start quality")?,
        native_kind: query_get(row, 11, "decode runtime presence kind")?,
        entrypoint: query_get(row, 12, "decode runtime presence entrypoint")?,
        name: query_get(row, 13, "decode runtime presence name")?,
        native_status: query_get(row, 14, "decode runtime presence status")?,
        updated_at: query_get(row, 15, "decode runtime presence update")?,
        updated_at_quality: query_get(row, 16, "decode runtime presence update quality")?,
        status_updated_at: query_get(row, 17, "decode runtime presence status update")?,
        status_updated_at_quality: query_get(
            row,
            18,
            "decode runtime presence status update quality",
        )?,
        native_process_started_at: query_get(row, 19, "decode runtime process start")?,
        version: query_get(row, 20, "decode runtime presence version")?,
        peer_protocol,
        name_source: query_get(row, 22, "decode runtime presence name source")?,
        bridge_session_id: query_get(row, 23, "decode runtime presence bridge session")?,
        messaging_socket_path: query_get(row, 24, "decode runtime presence socket")?,
        presence_status: query_get(row, 25, "decode runtime presence resolution")?,
        decisive_fact_id: encode_entity_id(FACT_ID_PREFIX, &decisive_fact_id),
        assertion_count: decode_nonnegative_u64(
            query_get(row, 27, "decode runtime presence assertions")?,
            "runtime presence assertion count",
        )?,
        competing_assertion_count: decode_nonnegative_u64(
            query_get(row, 28, "decode runtime presence conflicts")?,
            "runtime presence competing assertion count",
        )?,
        observed_at_unix_ms: query_get(row, 29, "decode runtime presence observation")?,
        session_present: query_get::<i64>(row, 30, "decode runtime session presence")? != 0,
        run_present: query_get::<i64>(row, 31, "decode runtime run presence")? != 0,
        last_commit_seq: decode_nonnegative_u64(
            query_get(row, 32, "decode runtime presence commit")?,
            "runtime presence commit sequence",
        )?,
    };
    Ok(RuntimeEntryRow {
        entry: RuntimeSnapshotEntry {
            kind: "presence".to_string(),
            run: None,
            presence: Some(presence),
        },
        sort_rank,
        sort_time,
        sort_kind: PRESENCE_ENTRY_KIND,
        entity_key: presence_key,
    })
}

fn validate_runtime_scope_membership(
    transaction: &Transaction<'_>,
    scope: &ValidatedRuntimeRequest,
) -> Result<(), EngineError> {
    let (Some(project_key), Some(session_key)) =
        (scope.project_key.as_ref(), scope.session_key.as_ref())
    else {
        return Ok(());
    };
    let exists = transaction
        .query_row(
            "SELECT 1 FROM canonical_sessions WHERE project_key = ?1 AND session_key = ?2",
            rusqlite::params![project_key, session_key],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| query_sqlite_error("validate runtime session scope", error))?
        .is_some();
    if !exists {
        return Err(EngineError::InvalidQuery(
            "runtime session does not belong to the requested project".to_string(),
        ));
    }
    Ok(())
}

fn compare_runtime_rows(left: &RuntimeEntryRow, right: &RuntimeEntryRow) -> Ordering {
    right
        .sort_rank
        .cmp(&left.sort_rank)
        .then_with(|| right.sort_time.cmp(&left.sort_time))
        .then_with(|| right.sort_kind.cmp(&left.sort_kind))
        .then_with(|| right.entity_key.cmp(&left.entity_key))
}

fn encode_runtime_cursor(cursor: &RuntimeCursor) -> Result<String, EngineError> {
    let json = serde_json::to_vec(cursor).map_err(|error| {
        EngineError::InvalidQuery(format!("could not encode runtime cursor: {error}"))
    })?;
    Ok(URL_SAFE_NO_PAD.encode(json))
}

fn decode_runtime_cursor(
    value: &str,
    request: &RuntimeSnapshotRequest,
) -> Result<RuntimeCursor, EngineError> {
    if value.is_empty() || value.len() > MAX_RUNTIME_CURSOR_BYTES {
        return Err(EngineError::InvalidQuery(
            "runtime cursor is empty or exceeds the supported bound".to_string(),
        ));
    }
    let json = URL_SAFE_NO_PAD.decode(value).map_err(|_| {
        EngineError::InvalidQuery("runtime cursor is not valid base64url".to_string())
    })?;
    let cursor: RuntimeCursor = serde_json::from_slice(&json).map_err(|_| {
        EngineError::InvalidQuery("runtime cursor payload is malformed".to_string())
    })?;
    if cursor.version != RUNTIME_QUERY_CONTRACT_VERSION {
        return Err(EngineError::InvalidQuery(format!(
            "unsupported runtime cursor version {}",
            cursor.version
        )));
    }
    if cursor.project_id != request.project_id || cursor.session_id != request.session_id {
        return Err(EngineError::InvalidQuery(
            "runtime cursor does not belong to this query scope".to_string(),
        ));
    }
    if cursor.sort_time.len() > MAX_RUNTIME_CURSOR_BYTES
        || !matches!(cursor.sort_kind, RUN_ENTRY_KIND | PRESENCE_ENTRY_KIND)
    {
        return Err(EngineError::InvalidQuery(
            "runtime cursor contains an unsupported order key".to_string(),
        ));
    }
    runtime_cursor_key(Some(&cursor))?;
    Ok(cursor)
}

fn runtime_cursor_key(cursor: Option<&RuntimeCursor>) -> Result<Vec<u8>, EngineError> {
    cursor
        .map(|cursor| {
            let key = URL_SAFE_NO_PAD.decode(&cursor.entity_key).map_err(|_| {
                EngineError::InvalidQuery("runtime cursor entity key is malformed".to_string())
            })?;
            if key.is_empty() || key.len() > MAX_RUNTIME_CURSOR_BYTES {
                return Err(EngineError::InvalidQuery(
                    "runtime cursor entity key is empty or exceeds the supported bound".to_string(),
                ));
            }
            Ok(key)
        })
        .transpose()
        .map(|key| key.unwrap_or_default())
}

fn validate_runtime_cursor_watermark(
    cursor: Option<&RuntimeCursor>,
    current_watermark: u64,
) -> Result<(), EngineError> {
    if let Some(cursor) = cursor {
        if cursor.at_commit_seq != current_watermark {
            return Err(EngineError::InvalidQuery(format!(
                "runtime cursor expired at commit {}; current commit is {current_watermark}",
                cursor.at_commit_seq
            )));
        }
    }
    Ok(())
}

fn query_get<T: rusqlite::types::FromSql>(
    row: &Row<'_>,
    index: usize,
    operation: &'static str,
) -> Result<T, EngineError> {
    row.get(index)
        .map_err(|error| query_sqlite_error(operation, error))
}

fn decode_nonnegative_u64(value: i64, field: &'static str) -> Result<u64, EngineError> {
    u64::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "decode runtime integer",
        detail: format!("{field} was negative: {value}"),
    })
}

fn decode_nonnegative_u32(value: i64, field: &'static str) -> Result<u32, EngineError> {
    u32::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "decode runtime integer",
        detail: format!("{field} was outside u32: {value}"),
    })
}

fn decode_optional_u32(
    value: Option<i64>,
    field: &'static str,
) -> Result<Option<u32>, EngineError> {
    value
        .map(|value| decode_nonnegative_u32(value, field))
        .transpose()
}

fn query_sqlite_error(operation: &'static str, error: rusqlite::Error) -> EngineError {
    EngineError::Sqlite {
        operation,
        detail: error.to_string(),
    }
}

fn to_rusqlite_query_error(error: EngineError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Blob, Box::new(error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::schema;
    use crate::engine::query_pool::QueryPool;
    use rusqlite::params;
    use tempfile::tempdir;

    fn insert_fact(
        connection: &Connection,
        fact_id: &[u8],
        kind: &str,
        entity_key: &[u8],
        ordinal: i64,
    ) {
        connection
            .execute(
                r#"
                INSERT INTO fact_records (
                    fact_id, fact_kind, entity_key, source_instance_id,
                    source_stream_id, source_object_id, source_generation,
                    cursor_start, cursor_end, payload_hash,
                    local_fact_ordinal, observed_at, payload_json,
                    last_commit_seq
                ) VALUES (?1, ?2, ?3, 1, 1, 1, 1, ?4, ?5, ?6, ?7, ?8, ?9, 1)
                "#,
                params![
                    fact_id,
                    kind,
                    entity_key,
                    format!("start-{ordinal}").as_bytes(),
                    format!("end-{ordinal}").as_bytes(),
                    [ordinal as u8; 32].as_slice(),
                    ordinal,
                    1_786_507_200_000_i64 + ordinal,
                    b"{}".as_slice(),
                ],
            )
            .unwrap();
    }

    fn seed_runtime(connection: &Connection) {
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
                "INSERT INTO source_streams VALUES (1, 1, 'runtime', 'presence_object', 'fixture', 'available', 'none', NULL, 1)",
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
                "INSERT INTO ingest_commits VALUES (1, 1, 'fixture', 1, 2, 5)",
                [],
            )
            .unwrap();

        insert_fact(connection, b"session-fact", "session", b"session", 1);
        insert_fact(connection, b"run-fact", "run", b"run", 2);
        insert_fact(connection, b"evidence-fact", "run_evidence", b"run", 3);
        insert_fact(connection, b"presence-fact", "presence", b"presence", 4);
        insert_fact(
            connection,
            b"orphan-presence-fact",
            "presence",
            b"orphan-presence",
            5,
        );
        connection
            .execute(
                r#"
                INSERT INTO canonical_sessions (
                    session_key, project_key, native_session_id,
                    native_project_key, source_time, source_time_quality,
                    fact_id, source_object_id, source_generation, cursor_end,
                    last_commit_seq
                ) VALUES (?1, ?2, 'native-session', 'native-project',
                          '2026-08-12T01:00:00.000Z', 'native_exact', ?3,
                          1, 1, ?4, 1)
                "#,
                params![
                    b"session".as_slice(),
                    b"project".as_slice(),
                    b"session-fact".as_slice(),
                    b"cursor-session".as_slice(),
                ],
            )
            .unwrap();
        connection
            .execute(
                r#"
                INSERT INTO canonical_runs (
                    run_key, session_key, native_run_id, parent_run_key,
                    fact_id, source_object_id, source_generation, cursor_end,
                    last_commit_seq
                ) VALUES (?1, ?2, 'root', NULL, ?3, 1, 1, ?4, 1)
                "#,
                params![
                    b"run".as_slice(),
                    b"session".as_slice(),
                    b"run-fact".as_slice(),
                    b"cursor-run".as_slice(),
                ],
            )
            .unwrap();
        connection
            .execute(
                r#"
                INSERT INTO run_evidence (
                    fact_id, run_key, evidence_kind, evidence_strength,
                    native_state, source_time, source_time_quality,
                    source_object_id, source_generation, cursor_end,
                    last_commit_seq, evidence_count, last_activity_at
                ) VALUES (?1, ?2, 'activity_observed', 'native_activity',
                          'working', '2026-08-12T02:00:00.000Z',
                          'native_exact', 1, 1, ?3, 1, 7,
                          '2026-08-12T02:00:00.000Z')
                "#,
                params![
                    b"evidence-fact".as_slice(),
                    b"run".as_slice(),
                    b"cursor-evidence".as_slice(),
                ],
            )
            .unwrap();
        connection
            .execute(
                r#"
                INSERT INTO observed_run_states (
                    run_key, state, decisive_evidence_id, last_activity_at,
                    terminal_at, last_commit_seq
                ) VALUES (?1, 'active', ?2, '2026-08-12T02:00:00.000Z', NULL, 1)
                "#,
                params![b"run".as_slice(), b"evidence-fact".as_slice()],
            )
            .unwrap();
        insert_presence(
            connection,
            b"presence",
            b"session",
            b"run",
            "native-session",
            42,
            b"presence-fact",
            "resolved",
            1,
        );
        insert_presence(
            connection,
            b"orphan-presence",
            b"orphan-session",
            b"orphan-run",
            "orphan-native-session",
            43,
            b"orphan-presence-fact",
            "conflicting",
            2,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_presence(
        connection: &Connection,
        presence_key: &[u8],
        session_key: &[u8],
        run_key: &[u8],
        native_session_id: &str,
        pid: i64,
        fact_id: &[u8],
        status: &str,
        assertions: i64,
    ) {
        connection
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
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, '/tmp/project',
                          '2026-08-12T00:00:00.000Z', 'native_exact',
                          'local', 'cli', 'fixture', 'working',
                          '2026-08-12T03:00:00.000Z', 'native_exact',
                          '2026-08-12T03:00:00.000Z', 'native_exact',
                          'process-start', '1.0.0', 7, 'native', 'bridge',
                          '/tmp/socket', ?7, 1, 1, ?8, 1)
                "#,
                params![
                    fact_id,
                    presence_key,
                    session_key,
                    run_key,
                    native_session_id,
                    pid,
                    [pid as u8; 32].as_slice(),
                    format!("presence-cursor-{pid}").as_bytes(),
                ],
            )
            .unwrap();
        connection
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
                    assertion_count, competing_assertion_count,
                    last_commit_seq
                ) VALUES (?1, ?2, ?3, ?4, ?5, '/tmp/project',
                          '2026-08-12T00:00:00.000Z', 'native_exact',
                          'local', 'cli', 'fixture', 'working',
                          '2026-08-12T03:00:00.000Z', 'native_exact',
                          '2026-08-12T03:00:00.000Z', 'native_exact',
                          'process-start', '1.0.0', 7, 'native', 'bridge',
                          '/tmp/socket', ?6, ?7, ?8, ?9, 1)
                "#,
                params![
                    presence_key,
                    session_key,
                    run_key,
                    native_session_id,
                    pid,
                    status,
                    fact_id,
                    assertions,
                    assertions - 1,
                ],
            )
            .unwrap();
    }

    #[test]
    fn snapshot_pages_runs_and_presence_without_inventing_liveness() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("runtime.db");
        let connection = Connection::open(&database).unwrap();
        seed_runtime(&connection);
        drop(connection);
        let mut pool = QueryPool::start(database, 1).unwrap();
        let client = pool.client();

        let first = client
            .runtime_snapshot(RuntimeSnapshotRequest {
                project_id: None,
                session_id: None,
                cursor: None,
                limit: 1,
            })
            .unwrap();
        assert_eq!(first.contract_version, RUNTIME_QUERY_CONTRACT_VERSION);
        assert_eq!(first.at_commit_seq, 1);
        assert_eq!(first.entries.len(), 1);
        assert_eq!(first.entries[0].kind, "presence");
        let present = first.entries[0].presence.as_ref().unwrap();
        assert_eq!(present.native_session_id, "native-session");
        assert!(present.session_present);
        assert!(present.run_present);
        assert!(first.next_cursor.is_some());

        let second = client
            .runtime_snapshot(RuntimeSnapshotRequest {
                project_id: None,
                session_id: None,
                cursor: first.next_cursor,
                limit: 2,
            })
            .unwrap();
        assert_eq!(second.entries.len(), 2);
        let kinds = second
            .entries
            .iter()
            .map(|entry| entry.kind.as_str())
            .collect::<Vec<_>>();
        assert_eq!(kinds, ["presence", "run"]);
        let orphan = second.entries[0].presence.as_ref().unwrap();
        assert_eq!(orphan.native_session_id, "orphan-native-session");
        assert!(!orphan.session_present);
        assert!(!orphan.run_present);
        assert_eq!(orphan.presence_status, "conflicting");
        assert_eq!(orphan.competing_assertion_count, 1);
        let run = second.entries[1].run.as_ref().unwrap();
        assert_eq!(run.state.as_deref(), Some("active"));
        assert_eq!(
            run.decisive_evidence.as_ref().unwrap().kind,
            "activity_observed"
        );
        assert_eq!(
            run.decisive_evidence
                .as_ref()
                .unwrap()
                .native_state
                .as_deref(),
            Some("working")
        );
        assert_eq!(run.presence_count, 1);
        assert_eq!(run.conflicting_presence_count, 0);
        assert_eq!(run.evidence_count, 7);
        assert!(second.next_cursor.is_none());

        let exact = client
            .run_state(RunStateRequest {
                run_id: encode_entity_id(RUN_ID_PREFIX, b"run"),
            })
            .unwrap();
        assert_eq!(exact.contract_version, RUNTIME_QUERY_CONTRACT_VERSION);
        assert_eq!(exact.at_commit_seq, 1);
        let exact_run = exact.run.unwrap();
        assert_eq!(exact_run.native_run_id, "root");
        assert_eq!(exact_run.state.as_deref(), Some("active"));
        assert_eq!(exact_run.evidence_count, 7);
        assert_eq!(exact_run.presence_count, 1);

        let unknown = client
            .run_state(RunStateRequest {
                run_id: encode_entity_id(RUN_ID_PREFIX, b"unknown"),
            })
            .unwrap();
        assert!(unknown.run.is_none());
        assert!(matches!(
            client.run_state(RunStateRequest {
                run_id: "native-run-id".to_string(),
            }),
            Err(EngineError::InvalidQuery(_))
        ));

        let scoped = client
            .runtime_snapshot(RuntimeSnapshotRequest {
                project_id: Some(encode_entity_id(PROJECT_ID_PREFIX, b"project")),
                session_id: Some(encode_entity_id(SESSION_ID_PREFIX, b"session")),
                cursor: None,
                limit: 10,
            })
            .unwrap();
        assert_eq!(
            scoped.entries.len(),
            2,
            "project scope excludes orphan evidence"
        );
        assert!(scoped.entries.iter().all(|entry| {
            entry
                .run
                .as_ref()
                .and_then(|run| run.project_id.as_ref())
                .or_else(|| {
                    entry
                        .presence
                        .as_ref()
                        .and_then(|presence| presence.project_id.as_ref())
                })
                .is_some()
        }));

        pool.shutdown().unwrap();
    }

    #[test]
    fn runtime_cursor_is_scope_and_watermark_bound() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("runtime-cursor.db");
        let connection = Connection::open(&database).unwrap();
        seed_runtime(&connection);
        drop(connection);
        let mut pool = QueryPool::start(database, 1).unwrap();
        let client = pool.client();
        let page = client
            .runtime_snapshot(RuntimeSnapshotRequest {
                project_id: None,
                session_id: None,
                cursor: None,
                limit: 1,
            })
            .unwrap();
        let cursor = page.next_cursor.unwrap();
        assert!(matches!(
            client.runtime_snapshot(RuntimeSnapshotRequest {
                project_id: Some(encode_entity_id(PROJECT_ID_PREFIX, b"project")),
                session_id: None,
                cursor: Some(cursor),
                limit: 1,
            }),
            Err(EngineError::InvalidQuery(_))
        ));
        assert!(matches!(
            client.runtime_snapshot(RuntimeSnapshotRequest {
                project_id: None,
                session_id: None,
                cursor: None,
                limit: 0,
            }),
            Err(EngineError::InvalidQuery(_))
        ));
        pool.shutdown().unwrap();
    }

    #[test]
    fn runtime_query_ordering_indexes_are_installed() {
        let connection = Connection::open_in_memory().unwrap();
        schema::initialize_schema(&connection).unwrap();
        for (table, index) in [
            ("canonical_runs", "idx_canonical_runs_commit"),
            ("canonical_presences", "idx_canonical_presences_commit"),
        ] {
            let mut statement = connection
                .prepare(&format!("PRAGMA index_list('{table}')"))
                .unwrap();
            let names = statement
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert!(
                names.iter().any(|name| name == index),
                "missing {index}: {names:?}"
            );
        }
    }
}
