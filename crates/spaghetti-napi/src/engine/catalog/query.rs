//! Snapshot-consistent catalog reads.
//!
//! Every read opens one transaction, takes the committed watermark, and
//! answers from that snapshot. A cursor carries the watermark it was minted
//! at, so a continuation page is evaluated against the same snapshot as page
//! one — background ingestion cannot duplicate, drop, or reorder a row
//! mid-listing (RFC 012B §8). Queries are pure: nothing here schedules work.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rusqlite::{params, Connection, Row};

use super::super::query_identity::{encode_entity_id, PROJECT_ID_PREFIX, SESSION_ID_PREFIX};
use super::super::query_pool::read_committed_watermark;
use super::super::EngineError;
use super::{CatalogPageBounds, CatalogState, MAX_CATALOG_PAGE_LIMIT};

const CURSOR_PREFIX: &str = "catalog_v1_";

/// Text form of an RFC 012A `ExternalEntityRef`: the encoding version, then
/// the 32-byte canonical entity digest. Stable across restarts and machines,
/// which is what VibeField persists.
const EXTERNAL_REF_ENCODING_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogProjectPageRequest {
    pub bounds: CatalogPageBounds,
    /// Restrict to one adapter. Empty means every configured source.
    pub adapter_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSessionPageRequest {
    pub bounds: CatalogPageBounds,
    /// Restrict to one project, using the same opaque project id the history
    /// path uses.
    pub project_id: Option<String>,
    pub adapter_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogProjectRow {
    pub project_id: String,
    pub external_ref: String,
    pub adapter_id: String,
    pub native_project_key: String,
    pub display_name: Option<String>,
    pub display_path: Option<String>,
    pub catalog_state: CatalogState,
    pub degraded: bool,
    pub degraded_reason: Option<String>,
    pub session_count: u64,
    pub transcript_session_count: u64,
    pub hydrated_session_count: u64,
    pub latest_activity_at: Option<String>,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSessionRow {
    pub session_id: String,
    pub project_id: String,
    pub external_ref: String,
    pub adapter_id: String,
    pub native_session_id: Option<String>,
    pub title: Option<String>,
    pub catalog_state: CatalogState,
    pub degraded: bool,
    pub degraded_reason: Option<String>,
    pub association_basis: String,
    pub association_quality: String,
    pub association_provenance: String,
    pub native_created_at: Option<String>,
    pub native_updated_at: Option<String>,
    pub native_message_count: Option<u64>,
    pub decoded_message_count: u64,
    pub transcript_present: bool,
    pub identity_conflicts: Vec<IdentityConflict>,
    pub last_commit_seq: u64,
}

/// A competing project association that lost precedence but keeps its
/// evidence. RFC 012B forbids merging these away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityConflict {
    pub competing_native_project_key: String,
    pub basis: String,
    pub provenance: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogProjectPage {
    pub projects: Vec<CatalogProjectRow>,
    pub cursor: Option<String>,
    pub at_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSessionPage {
    pub sessions: Vec<CatalogSessionRow>,
    pub cursor: Option<String>,
    pub at_commit_seq: u64,
}

/// What a persisted external reference resolves to now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogEntityResolution {
    LiveProject(Box<CatalogProjectRow>),
    LiveSession(Box<CatalogSessionRow>),
    /// The reference was valid but its evidence has been retracted. A
    /// tombstone is never silently retargeted to a different live entity.
    Retracted,
    Unknown,
}

/// `catalog_state` for a session, derived inside the caller's snapshot.
///
/// `searchable` needs the FTS pack, which is engine state rather than a row,
/// so the caller passes it in; everything else is a join.
const SESSION_STATE_SQL: &str = r#"
    CASE
        WHEN cs.transcript_present = 0 THEN 'discovered'
        WHEN can.session_key IS NULL THEN 'discovered'
        WHEN COALESCE(msg.message_count, 0) = 0 THEN 'transcript_backed'
        WHEN ?1 = 1 THEN 'searchable'
        ELSE 'hydrated'
    END
"#;

pub fn read_project_page(
    connection: &Connection,
    request: &CatalogProjectPageRequest,
    search_ready: bool,
) -> Result<CatalogProjectPage, EngineError> {
    let limit = validate_limit(request.bounds.limit)?;
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| sqlite_error("begin catalog project snapshot", error))?;
    let watermark = read_committed_watermark(&transaction)?;
    let cursor = decode_cursor(request.bounds.cursor.as_deref(), watermark)?;
    let (cursor_sort, cursor_key) = cursor
        .map(|cursor| (cursor.sort_key, cursor.entity_key))
        .unwrap_or_default();
    let adapter_filter = adapter_filter(&request.adapter_ids);

    let sql = format!(
        r#"
        WITH session_facts AS (
            SELECT cs.project_key,
                   cs.session_key,
                   cs.sort_time,
                   {SESSION_STATE_SQL} AS state
            FROM catalog_sessions cs
            LEFT JOIN canonical_sessions can ON can.session_key = cs.session_key
            LEFT JOIN (
                SELECT session_key, COUNT(*) AS message_count
                FROM canonical_messages GROUP BY session_key
            ) msg ON msg.session_key = cs.session_key
        ),
        rollup AS (
            SELECT project_key,
                   COUNT(*) AS session_count,
                   SUM(CASE WHEN state != 'discovered' THEN 1 ELSE 0 END) AS transcript_count,
                   SUM(CASE WHEN state IN ('hydrated', 'searchable') THEN 1 ELSE 0 END) AS hydrated_count,
                   MAX(sort_time) AS latest_activity
            FROM session_facts GROUP BY project_key
        )
        SELECT p.project_key, p.external_ref, p.adapter_id, p.native_project_key,
               p.display_name, p.display_path, p.last_commit_seq,
               COALESCE(r.session_count, 0), COALESCE(r.transcript_count, 0),
               COALESCE(r.hydrated_count, 0),
               COALESCE(r.latest_activity, ''),
               COALESCE(src.degraded, 0), src.degraded_reason
        FROM catalog_projects p
        LEFT JOIN rollup r ON r.project_key = p.project_key
        LEFT JOIN catalog_sources src ON src.source_instance_id = p.source_instance_id
        WHERE p.last_commit_seq <= ?2
          AND ({adapter_filter})
          AND (
              ?3 = 0
              OR COALESCE(r.latest_activity, '') < ?4
              OR (COALESCE(r.latest_activity, '') = ?4 AND p.project_key < ?5)
          )
        ORDER BY COALESCE(r.latest_activity, '') DESC, p.project_key DESC
        LIMIT ?6
        "#
    );

    let mut statement = transaction
        .prepare(&sql)
        .map_err(|error| sqlite_error("prepare catalog project page", error))?;
    let rows = statement
        .query_map(
            params![
                i64::from(search_ready),
                to_i64(watermark)?,
                i64::from(!cursor_key.is_empty() || !cursor_sort.is_empty()),
                cursor_sort,
                cursor_key,
                i64::from(limit) + 1,
            ],
            decode_project_row,
        )
        .map_err(|error| sqlite_error("read catalog project page", error))?;

    let mut projects = Vec::with_capacity(limit as usize);
    let mut overflow = None;
    for row in rows {
        let row = row.map_err(|error| sqlite_error("decode catalog project row", error))?;
        if projects.len() == limit as usize {
            overflow = Some(row);
            break;
        }
        projects.push(row);
    }
    let cursor = overflow.and(projects.last()).map(|last| {
        encode_cursor(&Cursor {
            watermark,
            sort_key: last.latest_activity_at.clone().unwrap_or_default(),
            entity_key: decode_entity_bytes(&last.project_id),
        })
    });

    drop(statement);
    transaction
        .commit()
        .map_err(|error| sqlite_error("finish catalog project snapshot", error))?;

    Ok(CatalogProjectPage {
        projects,
        cursor,
        at_commit_seq: watermark,
    })
}

pub fn read_session_page(
    connection: &Connection,
    request: &CatalogSessionPageRequest,
    search_ready: bool,
) -> Result<CatalogSessionPage, EngineError> {
    let limit = validate_limit(request.bounds.limit)?;
    let project_key = request
        .project_id
        .as_deref()
        .map(|value| decode_entity_id(value, PROJECT_ID_PREFIX, "catalog project id"))
        .transpose()?;
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| sqlite_error("begin catalog session snapshot", error))?;
    let watermark = read_committed_watermark(&transaction)?;
    let cursor = decode_cursor(request.bounds.cursor.as_deref(), watermark)?;
    let (cursor_sort, cursor_key) = cursor
        .map(|cursor| (cursor.sort_key, cursor.entity_key))
        .unwrap_or_default();
    let adapter_filter = adapter_filter(&request.adapter_ids);

    let sql = format!(
        r#"
        SELECT cs.session_key, cs.project_key, cs.external_ref, cs.adapter_id,
               cs.native_session_id, cs.title, cs.association_basis,
               cs.association_quality, cs.association_provenance,
               cs.native_created_at, cs.native_updated_at, cs.native_message_count,
               cs.transcript_present, cs.last_commit_seq, cs.sort_time,
               {SESSION_STATE_SQL} AS state,
               COALESCE(msg.message_count, 0) AS decoded_messages,
               COALESCE(src.degraded, 0), src.degraded_reason
        FROM catalog_sessions cs
        LEFT JOIN canonical_sessions can ON can.session_key = cs.session_key
        LEFT JOIN (
            SELECT session_key, COUNT(*) AS message_count
            FROM canonical_messages GROUP BY session_key
        ) msg ON msg.session_key = cs.session_key
        LEFT JOIN catalog_sources src ON src.source_instance_id = cs.source_instance_id
        WHERE cs.last_commit_seq <= ?2
          AND (?7 IS NULL OR cs.project_key = ?7)
          AND ({adapter_filter})
          AND (
              ?3 = 0
              OR cs.sort_time < ?4
              OR (cs.sort_time = ?4 AND cs.session_key < ?5)
          )
        ORDER BY cs.sort_time DESC, cs.session_key DESC
        LIMIT ?6
        "#
    );

    let mut statement = transaction
        .prepare(&sql)
        .map_err(|error| sqlite_error("prepare catalog session page", error))?;
    let rows = statement
        .query_map(
            params![
                i64::from(search_ready),
                to_i64(watermark)?,
                i64::from(!cursor_key.is_empty() || !cursor_sort.is_empty()),
                cursor_sort,
                cursor_key,
                i64::from(limit) + 1,
                project_key,
            ],
            |row| decode_session_row(row).map(|row| (row.0, row.1)),
        )
        .map_err(|error| sqlite_error("read catalog session page", error))?;

    let mut sessions = Vec::with_capacity(limit as usize);
    let mut sort_keys = Vec::with_capacity(limit as usize);
    let mut overflow = false;
    for row in rows {
        let (row, sort_time) =
            row.map_err(|error| sqlite_error("decode catalog session row", error))?;
        if sessions.len() == limit as usize {
            overflow = true;
            break;
        }
        sessions.push(row);
        sort_keys.push(sort_time);
    }
    drop(statement);

    for session in &mut sessions {
        session.identity_conflicts =
            read_conflicts(&transaction, &decode_entity_bytes(&session.session_id))?;
    }

    let cursor = (overflow && !sessions.is_empty()).then(|| {
        encode_cursor(&Cursor {
            watermark,
            sort_key: sort_keys.last().cloned().unwrap_or_default(),
            entity_key: decode_entity_bytes(&sessions[sessions.len() - 1].session_id),
        })
    });

    transaction
        .commit()
        .map_err(|error| sqlite_error("finish catalog session snapshot", error))?;

    Ok(CatalogSessionPage {
        sessions,
        cursor,
        at_commit_seq: watermark,
    })
}

/// Resolve one persisted external reference. A reference that no longer has
/// evidence resolves to `Retracted`, never to a different live entity.
pub fn resolve_catalog_entity(
    connection: &Connection,
    external_ref: &str,
    search_ready: bool,
) -> Result<CatalogEntityResolution, EngineError> {
    let Some(digest) = decode_external_ref(external_ref) else {
        return Ok(CatalogEntityResolution::Unknown);
    };

    let project_id: Option<Vec<u8>> = connection
        .query_row(
            "SELECT project_key FROM catalog_projects WHERE external_ref = ?1",
            params![digest.as_slice()],
            |row| row.get(0),
        )
        .ok();
    if let Some(project_key) = project_id {
        let page = read_project_page(
            connection,
            &CatalogProjectPageRequest {
                bounds: CatalogPageBounds {
                    cursor: None,
                    limit: MAX_CATALOG_PAGE_LIMIT,
                },
                adapter_ids: Vec::new(),
            },
            search_ready,
        )?;
        let wanted = encode_entity_id(PROJECT_ID_PREFIX, &project_key);
        return Ok(page
            .projects
            .into_iter()
            .find(|row| row.project_id == wanted)
            .map(|row| CatalogEntityResolution::LiveProject(Box::new(row)))
            .unwrap_or(CatalogEntityResolution::Retracted));
    }

    let session_key: Option<Vec<u8>> = connection
        .query_row(
            "SELECT session_key FROM catalog_sessions WHERE external_ref = ?1",
            params![digest.as_slice()],
            |row| row.get(0),
        )
        .ok();
    let Some(session_key) = session_key else {
        return Ok(CatalogEntityResolution::Unknown);
    };
    let page = read_session_page(
        connection,
        &CatalogSessionPageRequest {
            bounds: CatalogPageBounds {
                cursor: None,
                limit: MAX_CATALOG_PAGE_LIMIT,
            },
            project_id: None,
            adapter_ids: Vec::new(),
        },
        search_ready,
    )?;
    let wanted = encode_entity_id(SESSION_ID_PREFIX, &session_key);
    Ok(page
        .sessions
        .into_iter()
        .find(|row| row.session_id == wanted)
        .map(|row| CatalogEntityResolution::LiveSession(Box::new(row)))
        .unwrap_or(CatalogEntityResolution::Retracted))
}

fn read_conflicts(
    connection: &Connection,
    session_key: &[u8],
) -> Result<Vec<IdentityConflict>, EngineError> {
    let mut statement = connection
        .prepare(
            r#"
            SELECT competing_native_project_key, basis, provenance
            FROM catalog_association_conflicts
            WHERE session_key = ?1
            ORDER BY competing_native_project_key, basis
            "#,
        )
        .map_err(|error| sqlite_error("prepare catalog conflicts", error))?;
    let rows = statement
        .query_map(params![session_key], |row| {
            Ok(IdentityConflict {
                competing_native_project_key: row.get(0)?,
                basis: row.get(1)?,
                provenance: row.get(2)?,
            })
        })
        .map_err(|error| sqlite_error("read catalog conflicts", error))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("decode catalog conflict", error))
}

fn decode_project_row(row: &Row<'_>) -> rusqlite::Result<CatalogProjectRow> {
    let project_key: Vec<u8> = row.get(0)?;
    let external_ref: Vec<u8> = row.get(1)?;
    let latest_activity: String = row.get(10)?;
    let session_count: i64 = row.get(7)?;
    let transcript_count: i64 = row.get(8)?;
    let hydrated_count: i64 = row.get(9)?;
    let degraded: i64 = row.get(11)?;
    Ok(CatalogProjectRow {
        project_id: encode_entity_id(PROJECT_ID_PREFIX, &project_key),
        external_ref: encode_external_ref(&external_ref),
        adapter_id: row.get(2)?,
        native_project_key: row.get(3)?,
        display_name: row.get(4)?,
        display_path: row.get(5)?,
        catalog_state: project_state(session_count, transcript_count, hydrated_count),
        degraded: degraded != 0,
        degraded_reason: row.get(12)?,
        session_count: session_count.max(0) as u64,
        transcript_session_count: transcript_count.max(0) as u64,
        hydrated_session_count: hydrated_count.max(0) as u64,
        latest_activity_at: (!latest_activity.is_empty()).then_some(latest_activity),
        last_commit_seq: row.get::<_, i64>(6)?.max(0) as u64,
    })
}

fn decode_session_row(row: &Row<'_>) -> rusqlite::Result<(CatalogSessionRow, String)> {
    let session_key: Vec<u8> = row.get(0)?;
    let project_key: Vec<u8> = row.get(1)?;
    let external_ref: Vec<u8> = row.get(2)?;
    let transcript_present: i64 = row.get(12)?;
    let sort_time: String = row.get(14)?;
    let state: String = row.get(15)?;
    let decoded_messages: i64 = row.get(16)?;
    let degraded: i64 = row.get(17)?;
    Ok((
        CatalogSessionRow {
            session_id: encode_entity_id(SESSION_ID_PREFIX, &session_key),
            project_id: encode_entity_id(PROJECT_ID_PREFIX, &project_key),
            external_ref: encode_external_ref(&external_ref),
            adapter_id: row.get(3)?,
            native_session_id: row.get(4)?,
            title: row.get(5)?,
            catalog_state: parse_state(&state),
            degraded: degraded != 0,
            degraded_reason: row.get(18)?,
            association_basis: row.get(6)?,
            association_quality: row.get(7)?,
            association_provenance: row.get(8)?,
            native_created_at: row.get(9)?,
            native_updated_at: row.get(10)?,
            native_message_count: row
                .get::<_, Option<i64>>(11)?
                .map(|count| count.max(0) as u64),
            decoded_message_count: decoded_messages.max(0) as u64,
            transcript_present: transcript_present != 0,
            identity_conflicts: Vec::new(),
            last_commit_seq: row.get::<_, i64>(13)?.max(0) as u64,
        },
        sort_time,
    ))
}

/// A project is only as complete as its weakest evidence: it is `hydrated`
/// when every session is, `transcript_backed` when every session has a
/// canonical row, and `discovered` otherwise. An empty project is
/// `discovered`; it has evidence of itself and nothing more.
fn project_state(session_count: i64, transcript_count: i64, hydrated_count: i64) -> CatalogState {
    if session_count <= 0 {
        return CatalogState::Discovered;
    }
    if hydrated_count >= session_count {
        CatalogState::Hydrated
    } else if transcript_count >= session_count {
        CatalogState::TranscriptBacked
    } else {
        CatalogState::Discovered
    }
}

fn parse_state(value: &str) -> CatalogState {
    match value {
        "searchable" => CatalogState::Searchable,
        "hydrated" => CatalogState::Hydrated,
        "transcript_backed" => CatalogState::TranscriptBacked,
        _ => CatalogState::Discovered,
    }
}

/// SQL fragment restricting a page to the requested adapters. Adapter ids are
/// validated identifiers, so inlining them cannot inject.
fn adapter_filter(adapter_ids: &[String]) -> String {
    if adapter_ids.is_empty() {
        return "1 = 1".to_string();
    }
    let list = adapter_ids
        .iter()
        .filter(|id| {
            !id.is_empty()
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        })
        .map(|id| format!("'{id}'"))
        .collect::<Vec<_>>()
        .join(", ");
    if list.is_empty() {
        "1 = 0".to_string()
    } else {
        format!("adapter_id IN ({list})")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Cursor {
    watermark: u64,
    sort_key: String,
    entity_key: Vec<u8>,
}

fn encode_cursor(cursor: &Cursor) -> String {
    let mut payload = Vec::new();
    payload.extend_from_slice(&cursor.watermark.to_be_bytes());
    let sort = cursor.sort_key.as_bytes();
    payload.extend_from_slice(&(sort.len() as u32).to_be_bytes());
    payload.extend_from_slice(sort);
    payload.extend_from_slice(&cursor.entity_key);
    format!("{CURSOR_PREFIX}{}", URL_SAFE_NO_PAD.encode(payload))
}

/// Decode a continuation cursor and refuse one minted at a different
/// snapshot: continuing at a newer commit is exactly the duplicate/omission
/// bug snapshot-consistent pagination exists to prevent.
fn decode_cursor(value: Option<&str>, watermark: u64) -> Result<Option<Cursor>, EngineError> {
    let Some(value) = value else { return Ok(None) };
    let invalid = || EngineError::InvalidQuery("catalog cursor is not valid".to_string());
    let payload = value.strip_prefix(CURSOR_PREFIX).ok_or_else(invalid)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).map_err(|_| invalid())?;
    if bytes.len() < 12 {
        return Err(invalid());
    }
    let cursor_watermark = u64::from_be_bytes(bytes[0..8].try_into().map_err(|_| invalid())?);
    let sort_len = u32::from_be_bytes(bytes[8..12].try_into().map_err(|_| invalid())?) as usize;
    if bytes.len() < 12 + sort_len {
        return Err(invalid());
    }
    let sort_key = String::from_utf8(bytes[12..12 + sort_len].to_vec()).map_err(|_| invalid())?;
    let entity_key = bytes[12 + sort_len..].to_vec();
    if cursor_watermark > watermark {
        return Err(EngineError::InvalidQuery(
            "catalog cursor was issued for a newer snapshot".to_string(),
        ));
    }
    Ok(Some(Cursor {
        watermark: cursor_watermark,
        sort_key,
        entity_key,
    }))
}

fn encode_external_ref(digest: &[u8]) -> String {
    format!(
        "{EXTERNAL_REF_ENCODING_VERSION}:{}",
        URL_SAFE_NO_PAD.encode(digest)
    )
}

fn decode_external_ref(value: &str) -> Option<Vec<u8>> {
    let payload = value.strip_prefix(&format!("{EXTERNAL_REF_ENCODING_VERSION}:"))?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    (decoded.len() == 32).then_some(decoded)
}

fn decode_entity_bytes(entity_id: &str) -> Vec<u8> {
    entity_id
        .rsplit_once('_')
        .and_then(|(_, payload)| URL_SAFE_NO_PAD.decode(payload).ok())
        .unwrap_or_default()
}

fn decode_entity_id(
    value: &str,
    prefix: &str,
    label: &'static str,
) -> Result<Vec<u8>, EngineError> {
    let payload = value
        .strip_prefix(prefix)
        .ok_or_else(|| EngineError::InvalidQuery(format!("{label} is not valid")))?;
    URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| EngineError::InvalidQuery(format!("{label} is not valid")))
}

fn validate_limit(limit: u32) -> Result<u32, EngineError> {
    if limit == 0 || limit > MAX_CATALOG_PAGE_LIMIT {
        return Err(EngineError::InvalidQuery(format!(
            "catalog page limit must be between 1 and {MAX_CATALOG_PAGE_LIMIT}"
        )));
    }
    Ok(limit)
}

fn to_i64(value: u64) -> Result<i64, EngineError> {
    i64::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "encode catalog watermark",
        detail: "value exceeds the SQLite integer range".to_string(),
    })
}

fn sqlite_error(operation: &'static str, error: rusqlite::Error) -> EngineError {
    EngineError::Sqlite {
        operation,
        detail: error.to_string(),
    }
}
