//! Read-only RFC 011 teams and inbox capability query pack.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rusqlite::{Connection, OptionalExtension, Row, Transaction};
use serde::{Deserialize, Serialize};

use super::query_identity::{
    decode_entity_id, encode_entity_id, FACT_ID_PREFIX, SESSION_ID_PREFIX, TEAM_ID_PREFIX,
    TEAM_INBOX_ID_PREFIX, TEAM_INBOX_MESSAGE_ID_PREFIX, TEAM_MEMBER_ID_PREFIX,
};
use super::query_pool::read_committed_watermark;
use super::EngineError;
use ts_rs::TS;

pub const TEAM_QUERY_CONTRACT_VERSION: u32 = 1;
pub const DEFAULT_TEAM_PAGE_LIMIT: u32 = 50;
const MAX_TEAM_PAGE_LIMIT: u32 = 200;
const MAX_TEAM_CURSOR_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamPageRequest {
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TeamPage {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub items: Vec<TeamSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TeamSummary {
    pub team_id: String,
    pub adapter_id: String,
    pub source_instance_id: u64,
    pub native_team_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub config: Option<TeamConfigSummary>,
    pub inbox_count: u64,
    pub message_count: u64,
    pub unread_message_count: u64,
    pub conflicting_inbox_count: u64,
    pub conflicting_message_count: u64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TeamConfigSummary {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    pub created_at: String,
    pub created_at_quality: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub lead_member_id: Option<String>,
    pub lead_member_present: bool,
    pub native_lead_agent_id: String,
    pub lead_session_id: String,
    pub lead_session_present: bool,
    pub native_lead_session_id: String,
    pub config_status: String,
    pub decisive_fact_id: String,
    pub assertion_count: u64,
    pub competing_snapshot_count: u64,
    pub member_count: u64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamDetailsRequest {
    pub team_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TeamDetails {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub team: TeamSummary,
    pub members: Vec<TeamMember>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TeamMember {
    pub member_id: String,
    pub team_id: String,
    pub member_ordinal: u32,
    pub native_agent_id: String,
    pub native_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub agent_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plan_mode_required: Option<bool>,
    pub joined_at: String,
    pub joined_at_quality: String,
    pub tmux_pane_id: String,
    pub cwd: String,
    pub subscriptions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub backend_type: Option<String>,
    pub membership_status: String,
    pub decisive_fact_id: String,
    pub assertion_count: u64,
    pub competing_membership_count: u64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamInboxPageRequest {
    pub team_id: String,
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TeamInboxPage {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub team_id: String,
    pub items: Vec<TeamInboxSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TeamInboxSummary {
    pub inbox_id: String,
    pub team_id: String,
    pub recipient_id: String,
    pub recipient_present: bool,
    pub native_team_id: String,
    pub native_recipient_name: String,
    pub inbox_status: String,
    pub decisive_fact_id: String,
    pub assertion_count: u64,
    pub competing_snapshot_count: u64,
    pub message_count: u64,
    pub unread_message_count: u64,
    pub conflicting_message_count: u64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamInboxMessagePageRequest {
    pub inbox_id: String,
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TeamInboxMessagePage {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub inbox_id: String,
    pub team_id: String,
    pub native_team_id: String,
    pub native_recipient_name: String,
    pub items: Vec<TeamInboxMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TeamInboxMessage {
    pub message_id: String,
    pub inbox_id: String,
    pub sender_id: String,
    pub sender_present: bool,
    pub message_ordinal: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub native_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub native_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub native_version: Option<u32>,
    pub native_sender_name: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub color: Option<String>,
    pub source_time: String,
    pub source_time_quality: String,
    pub read: bool,
    pub message_status: String,
    pub decisive_fact_id: String,
    pub assertion_count: u64,
    pub competing_message_count: u64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TeamCursorKind {
    Teams,
    Inboxes,
    Messages,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TeamCursor {
    version: u32,
    kind: TeamCursorKind,
    at_commit_seq: u64,
    scope_id: Option<String>,
    order_text: String,
    order_number: u32,
    entity_key: String,
}

#[derive(Debug)]
struct TeamSummaryRow {
    summary: TeamSummary,
    team_key: Vec<u8>,
}

#[derive(Debug)]
struct TeamInboxRow {
    summary: TeamInboxSummary,
    inbox_key: Vec<u8>,
}

#[derive(Debug)]
struct TeamMessageRow {
    message: TeamInboxMessage,
    message_key: Vec<u8>,
}

pub(super) fn validate_team_page(request: &TeamPageRequest) -> Result<(), EngineError> {
    validate_page_limit(request.limit, "team")?;
    request
        .cursor
        .as_deref()
        .map(|value| decode_cursor(value, TeamCursorKind::Teams, None))
        .transpose()?;
    Ok(())
}

pub(super) fn validate_team_details(request: &TeamDetailsRequest) -> Result<Vec<u8>, EngineError> {
    decode_entity_id(&request.team_id, TEAM_ID_PREFIX, "team id")
}

pub(super) fn validate_team_inbox_page(
    request: &TeamInboxPageRequest,
) -> Result<Vec<u8>, EngineError> {
    validate_page_limit(request.limit, "team inbox")?;
    request
        .cursor
        .as_deref()
        .map(|value| decode_cursor(value, TeamCursorKind::Inboxes, Some(&request.team_id)))
        .transpose()?;
    decode_entity_id(&request.team_id, TEAM_ID_PREFIX, "team id")
}

pub(super) fn validate_team_message_page(
    request: &TeamInboxMessagePageRequest,
) -> Result<Vec<u8>, EngineError> {
    validate_page_limit(request.limit, "team inbox message")?;
    request
        .cursor
        .as_deref()
        .map(|value| decode_cursor(value, TeamCursorKind::Messages, Some(&request.inbox_id)))
        .transpose()?;
    decode_entity_id(&request.inbox_id, TEAM_INBOX_ID_PREFIX, "team inbox id")
}

pub(super) fn read_team_page(
    connection: &Connection,
    request: &TeamPageRequest,
) -> Result<TeamPage, EngineError> {
    validate_team_page(request)?;
    let cursor = request
        .cursor
        .as_deref()
        .map(|value| decode_cursor(value, TeamCursorKind::Teams, None))
        .transpose()?;
    let cursor_key = cursor_key(cursor.as_ref())?;
    let cursor_text = cursor
        .as_ref()
        .map(|cursor| cursor.order_text.as_str())
        .unwrap_or("");
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin team page snapshot", error))?;
    let watermark = read_committed_watermark(&transaction)?;
    validate_cursor_watermark(cursor.as_ref(), watermark)?;
    let mut rows = read_team_summaries(
        &transaction,
        Some((
            cursor.is_some(),
            cursor_text,
            &cursor_key,
            request.limit + 1,
        )),
        None,
    )?;
    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish team page snapshot", error))?;

    let has_more = rows.len() > request.limit as usize;
    if has_more {
        rows.truncate(request.limit as usize);
    }
    let next_cursor = if has_more {
        rows.last()
            .map(|row| {
                encode_cursor(&TeamCursor {
                    version: TEAM_QUERY_CONTRACT_VERSION,
                    kind: TeamCursorKind::Teams,
                    at_commit_seq: watermark,
                    scope_id: None,
                    order_text: row.summary.native_team_id.clone(),
                    order_number: 0,
                    entity_key: URL_SAFE_NO_PAD.encode(&row.team_key),
                })
            })
            .transpose()?
    } else {
        None
    };
    Ok(TeamPage {
        contract_version: TEAM_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        items: rows.into_iter().map(|row| row.summary).collect(),
        next_cursor,
    })
}

pub(super) fn read_team_details(
    connection: &Connection,
    request: &TeamDetailsRequest,
) -> Result<TeamDetails, EngineError> {
    let team_key = validate_team_details(request)?;
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin team details snapshot", error))?;
    let watermark = read_committed_watermark(&transaction)?;
    let team = read_team_summaries(&transaction, None, Some(&team_key))?
        .into_iter()
        .next()
        .ok_or_else(|| {
            EngineError::InvalidQuery("team id does not identify a current team".to_string())
        })?
        .summary;
    let members = read_team_members(&transaction, &team_key, &request.team_id)?;
    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish team details snapshot", error))?;
    Ok(TeamDetails {
        contract_version: TEAM_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        team,
        members,
    })
}

pub(super) fn read_team_inbox_page(
    connection: &Connection,
    request: &TeamInboxPageRequest,
) -> Result<TeamInboxPage, EngineError> {
    let team_key = validate_team_inbox_page(request)?;
    let cursor = request
        .cursor
        .as_deref()
        .map(|value| decode_cursor(value, TeamCursorKind::Inboxes, Some(&request.team_id)))
        .transpose()?;
    let cursor_key = cursor_key(cursor.as_ref())?;
    let cursor_text = cursor
        .as_ref()
        .map(|cursor| cursor.order_text.as_str())
        .unwrap_or("");
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin team inbox page snapshot", error))?;
    let watermark = read_committed_watermark(&transaction)?;
    validate_cursor_watermark(cursor.as_ref(), watermark)?;
    require_team(&transaction, &team_key)?;
    let mut rows = read_inbox_summaries(
        &transaction,
        &team_key,
        cursor.is_some(),
        cursor_text,
        &cursor_key,
        request.limit + 1,
    )?;
    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish team inbox page snapshot", error))?;

    let has_more = rows.len() > request.limit as usize;
    if has_more {
        rows.truncate(request.limit as usize);
    }
    let next_cursor = if has_more {
        rows.last()
            .map(|row| {
                encode_cursor(&TeamCursor {
                    version: TEAM_QUERY_CONTRACT_VERSION,
                    kind: TeamCursorKind::Inboxes,
                    at_commit_seq: watermark,
                    scope_id: Some(request.team_id.clone()),
                    order_text: row.summary.native_recipient_name.clone(),
                    order_number: 0,
                    entity_key: URL_SAFE_NO_PAD.encode(&row.inbox_key),
                })
            })
            .transpose()?
    } else {
        None
    };
    Ok(TeamInboxPage {
        contract_version: TEAM_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        team_id: request.team_id.clone(),
        items: rows.into_iter().map(|row| row.summary).collect(),
        next_cursor,
    })
}

pub(super) fn read_team_message_page(
    connection: &Connection,
    request: &TeamInboxMessagePageRequest,
) -> Result<TeamInboxMessagePage, EngineError> {
    let inbox_key = validate_team_message_page(request)?;
    let cursor = request
        .cursor
        .as_deref()
        .map(|value| decode_cursor(value, TeamCursorKind::Messages, Some(&request.inbox_id)))
        .transpose()?;
    let cursor_key = cursor_key(cursor.as_ref())?;
    let cursor_ordinal = cursor.as_ref().map_or(0, |cursor| cursor.order_number);
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin team inbox message snapshot", error))?;
    let watermark = read_committed_watermark(&transaction)?;
    validate_cursor_watermark(cursor.as_ref(), watermark)?;
    let inbox = transaction
        .query_row(
            "SELECT team_key, native_team_id, native_recipient_name FROM canonical_team_inboxes WHERE inbox_key = ?1",
            [&inbox_key],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
        )
        .optional()
        .map_err(|error| query_sqlite_error("read team inbox message scope", error))?
        .ok_or_else(|| EngineError::InvalidQuery("team inbox id does not identify a current inbox".to_string()))?;
    let team_id = encode_entity_id(TEAM_ID_PREFIX, &inbox.0);
    let mut rows = read_messages(
        &transaction,
        &inbox_key,
        &request.inbox_id,
        cursor.is_some(),
        cursor_ordinal,
        &cursor_key,
        request.limit + 1,
    )?;
    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish team inbox message snapshot", error))?;

    let has_more = rows.len() > request.limit as usize;
    if has_more {
        rows.truncate(request.limit as usize);
    }
    let next_cursor = if has_more {
        rows.last()
            .map(|row| {
                encode_cursor(&TeamCursor {
                    version: TEAM_QUERY_CONTRACT_VERSION,
                    kind: TeamCursorKind::Messages,
                    at_commit_seq: watermark,
                    scope_id: Some(request.inbox_id.clone()),
                    order_text: String::new(),
                    order_number: row.message.message_ordinal,
                    entity_key: URL_SAFE_NO_PAD.encode(&row.message_key),
                })
            })
            .transpose()?
    } else {
        None
    };
    Ok(TeamInboxMessagePage {
        contract_version: TEAM_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        inbox_id: request.inbox_id.clone(),
        team_id,
        native_team_id: inbox.1,
        native_recipient_name: inbox.2,
        items: rows.into_iter().map(|row| row.message).collect(),
        next_cursor,
    })
}

type TeamPageCursor<'a> = (bool, &'a str, &'a [u8], u32);

fn read_team_summaries(
    transaction: &Transaction<'_>,
    page: Option<TeamPageCursor<'_>>,
    exact_team_key: Option<&[u8]>,
) -> Result<Vec<TeamSummaryRow>, EngineError> {
    let (has_cursor, cursor_text, cursor_key, limit) = page.unwrap_or((false, "", &[], 1));
    let mut statement = transaction
        .prepare(
            r#"
            WITH team_keys AS (
                SELECT team_key FROM canonical_teams
                UNION
                SELECT team_key FROM canonical_team_inboxes
            ),
            inbox_stats AS (
                SELECT cti.team_key, COUNT(*) AS inbox_count,
                       COALESCE(SUM(cti.message_count), 0) AS message_count,
                       SUM(CASE WHEN cti.inbox_status = 'conflicting' THEN 1 ELSE 0 END)
                           AS conflicting_inbox_count,
                       MAX(cti.last_commit_seq) AS last_commit_seq
                FROM canonical_team_inboxes cti
                GROUP BY cti.team_key
            ),
            message_stats AS (
                SELECT cti.team_key,
                       SUM(CASE WHEN ctim.read = 0 THEN 1 ELSE 0 END)
                           AS unread_message_count,
                       SUM(CASE WHEN ctim.message_status = 'conflicting' THEN 1 ELSE 0 END)
                           AS conflicting_message_count,
                       MAX(ctim.last_commit_seq) AS last_commit_seq
                FROM canonical_team_inboxes cti
                JOIN canonical_team_inbox_messages ctim
                  ON ctim.inbox_key = cti.inbox_key
                GROUP BY cti.team_key
            ),
            team_rows AS (
                SELECT tk.team_key,
                       COALESCE(ct.native_team_id,
                           (SELECT cti.native_team_id
                            FROM canonical_team_inboxes cti
                            WHERE cti.team_key = tk.team_key
                            ORDER BY cti.inbox_key LIMIT 1)) AS native_team_id,
                       si.adapter_id, fr.source_instance_id,
                       ct.name, ct.description, ct.created_at,
                       ct.created_at_quality, ct.lead_member_key,
                       CASE WHEN ctm.member_key IS NULL THEN 0 ELSE 1 END
                           AS lead_member_present,
                       ct.native_lead_agent_id, ct.lead_session_key,
                       CASE WHEN cs.session_key IS NULL THEN 0 ELSE 1 END
                           AS lead_session_present,
                       ct.native_lead_session_id, ct.config_status,
                       ct.decisive_fact_id, ct.assertion_count,
                       ct.competing_snapshot_count, ct.member_count,
                       ct.last_commit_seq AS config_commit_seq,
                       COALESCE(ins.inbox_count, 0) AS inbox_count,
                       COALESCE(ins.message_count, 0) AS message_count,
                       COALESCE(ms.unread_message_count, 0) AS unread_message_count,
                       COALESCE(ins.conflicting_inbox_count, 0)
                           AS conflicting_inbox_count,
                       COALESCE(ms.conflicting_message_count, 0)
                           AS conflicting_message_count,
                       MAX(COALESCE(ct.last_commit_seq, 0),
                           COALESCE(ins.last_commit_seq, 0),
                           COALESCE(ms.last_commit_seq, 0)) AS last_commit_seq
                FROM team_keys tk
                LEFT JOIN canonical_teams ct ON ct.team_key = tk.team_key
                LEFT JOIN canonical_team_inboxes first_inbox
                  ON first_inbox.inbox_key = (
                      SELECT cti.inbox_key FROM canonical_team_inboxes cti
                      WHERE cti.team_key = tk.team_key
                      ORDER BY cti.inbox_key LIMIT 1
                  )
                JOIN fact_records fr
                  ON fr.fact_id = COALESCE(ct.decisive_fact_id,
                                           first_inbox.decisive_fact_id)
                JOIN source_instances si
                  ON si.source_instance_id = fr.source_instance_id
                LEFT JOIN canonical_team_members ctm
                  ON ctm.member_key = ct.lead_member_key
                LEFT JOIN canonical_sessions cs
                  ON cs.session_key = ct.lead_session_key
                LEFT JOIN inbox_stats ins ON ins.team_key = tk.team_key
                LEFT JOIN message_stats ms ON ms.team_key = tk.team_key
            )
            SELECT team_key, native_team_id, adapter_id, source_instance_id,
                   name, description, created_at, created_at_quality,
                   lead_member_key, lead_member_present, native_lead_agent_id,
                   lead_session_key, lead_session_present,
                   native_lead_session_id, config_status, decisive_fact_id,
                   assertion_count, competing_snapshot_count, member_count,
                   config_commit_seq, inbox_count, message_count,
                   unread_message_count, conflicting_inbox_count,
                   conflicting_message_count, last_commit_seq
            FROM team_rows
            WHERE (?1 IS NULL OR team_key = ?1)
              AND (?2 = 0 OR native_team_id > ?3
                   OR (native_team_id = ?3 AND team_key > ?4))
            ORDER BY native_team_id ASC, team_key ASC
            LIMIT ?5
            "#,
        )
        .map_err(|error| query_sqlite_error("prepare team summaries", error))?;
    let mut rows = statement
        .query(rusqlite::params![
            exact_team_key,
            i64::from(has_cursor),
            cursor_text,
            cursor_key,
            i64::from(limit),
        ])
        .map_err(|error| query_sqlite_error("execute team summaries", error))?;
    let mut summaries = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| query_sqlite_error("advance team summaries", error))?
    {
        summaries.push(decode_team_summary(row)?);
    }
    Ok(summaries)
}

fn decode_team_summary(row: &Row<'_>) -> Result<TeamSummaryRow, EngineError> {
    let team_key: Vec<u8> = query_get(row, 0, "decode team key")?;
    let name: Option<String> = query_get(row, 4, "decode team name")?;
    let config = name
        .map(|name| {
            let lead_member_key: Option<Vec<u8>> = query_get(row, 8, "decode team lead member")?;
            let lead_session_key: Vec<u8> = query_get(row, 11, "decode team lead session")?;
            let decisive_fact_id: Vec<u8> = query_get(row, 15, "decode team decisive fact")?;
            Ok(TeamConfigSummary {
                name,
                description: query_get(row, 5, "decode team description")?,
                created_at: query_get(row, 6, "decode team created time")?,
                created_at_quality: query_get(row, 7, "decode team created quality")?,
                lead_member_id: lead_member_key
                    .as_deref()
                    .map(|key| encode_entity_id(TEAM_MEMBER_ID_PREFIX, key)),
                lead_member_present: query_get::<i64>(row, 9, "decode team lead member presence")?
                    != 0,
                native_lead_agent_id: query_get(row, 10, "decode team native lead agent")?,
                lead_session_id: encode_entity_id(SESSION_ID_PREFIX, &lead_session_key),
                lead_session_present: query_get::<i64>(
                    row,
                    12,
                    "decode team lead session presence",
                )? != 0,
                native_lead_session_id: query_get(row, 13, "decode team native lead session")?,
                config_status: query_get(row, 14, "decode team config status")?,
                decisive_fact_id: encode_entity_id(FACT_ID_PREFIX, &decisive_fact_id),
                assertion_count: decode_nonnegative_u64(
                    query_get(row, 16, "decode team assertions")?,
                    "team assertion count",
                )?,
                competing_snapshot_count: decode_nonnegative_u64(
                    query_get(row, 17, "decode team conflicts")?,
                    "team competing snapshot count",
                )?,
                member_count: decode_nonnegative_u64(
                    query_get(row, 18, "decode team member count")?,
                    "team member count",
                )?,
                last_commit_seq: decode_nonnegative_u64(
                    query_get(row, 19, "decode team config commit")?,
                    "team config commit sequence",
                )?,
            })
        })
        .transpose()?;
    Ok(TeamSummaryRow {
        summary: TeamSummary {
            team_id: encode_entity_id(TEAM_ID_PREFIX, &team_key),
            adapter_id: query_get(row, 2, "decode team adapter")?,
            source_instance_id: decode_nonnegative_u64(
                query_get(row, 3, "decode team source instance")?,
                "team source instance id",
            )?,
            native_team_id: query_get(row, 1, "decode native team id")?,
            config,
            inbox_count: decode_nonnegative_u64(
                query_get(row, 20, "decode team inbox count")?,
                "team inbox count",
            )?,
            message_count: decode_nonnegative_u64(
                query_get(row, 21, "decode team message count")?,
                "team message count",
            )?,
            unread_message_count: decode_nonnegative_u64(
                query_get(row, 22, "decode team unread count")?,
                "team unread message count",
            )?,
            conflicting_inbox_count: decode_nonnegative_u64(
                query_get(row, 23, "decode team inbox conflicts")?,
                "team conflicting inbox count",
            )?,
            conflicting_message_count: decode_nonnegative_u64(
                query_get(row, 24, "decode team message conflicts")?,
                "team conflicting message count",
            )?,
            last_commit_seq: decode_nonnegative_u64(
                query_get(row, 25, "decode team commit")?,
                "team commit sequence",
            )?,
        },
        team_key,
    })
}

fn read_team_members(
    transaction: &Transaction<'_>,
    team_key: &[u8],
    team_id: &str,
) -> Result<Vec<TeamMember>, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT member_key, member_ordinal, native_agent_id, native_name,
                   agent_type, model, prompt, color, plan_mode_required,
                   joined_at, joined_at_quality, tmux_pane_id, cwd,
                   subscriptions_json, backend_type, membership_status,
                   decisive_fact_id, assertion_count,
                   competing_membership_count, last_commit_seq
            FROM canonical_team_members
            WHERE team_key = ?1
            ORDER BY member_ordinal ASC, member_key ASC
            "#,
        )
        .map_err(|error| query_sqlite_error("prepare team members", error))?;
    let mut rows = statement
        .query([team_key])
        .map_err(|error| query_sqlite_error("execute team members", error))?;
    let mut members = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| query_sqlite_error("advance team members", error))?
    {
        let member_key: Vec<u8> = query_get(row, 0, "decode team member key")?;
        let subscriptions_json: Vec<u8> = query_get(row, 13, "decode team subscriptions")?;
        let subscriptions =
            serde_json::from_slice::<Vec<String>>(&subscriptions_json).map_err(|error| {
                EngineError::Sqlite {
                    operation: "decode team subscriptions",
                    detail: error.to_string(),
                }
            })?;
        let decisive_fact_id: Vec<u8> = query_get(row, 16, "decode team member decisive fact")?;
        let plan_mode =
            query_get::<Option<i64>>(row, 8, "decode team plan mode")?.map(|value| value != 0);
        members.push(TeamMember {
            member_id: encode_entity_id(TEAM_MEMBER_ID_PREFIX, &member_key),
            team_id: team_id.to_string(),
            member_ordinal: decode_nonnegative_u32(
                query_get(row, 1, "decode team member ordinal")?,
                "team member ordinal",
            )?,
            native_agent_id: query_get(row, 2, "decode team member native agent")?,
            native_name: query_get(row, 3, "decode team member native name")?,
            agent_type: query_get(row, 4, "decode team member agent type")?,
            model: query_get(row, 5, "decode team member model")?,
            prompt: query_get(row, 6, "decode team member prompt")?,
            color: query_get(row, 7, "decode team member color")?,
            plan_mode_required: plan_mode,
            joined_at: query_get(row, 9, "decode team member joined time")?,
            joined_at_quality: query_get(row, 10, "decode team member joined quality")?,
            tmux_pane_id: query_get(row, 11, "decode team member pane")?,
            cwd: query_get(row, 12, "decode team member cwd")?,
            subscriptions,
            backend_type: query_get(row, 14, "decode team member backend")?,
            membership_status: query_get(row, 15, "decode team member status")?,
            decisive_fact_id: encode_entity_id(FACT_ID_PREFIX, &decisive_fact_id),
            assertion_count: decode_nonnegative_u64(
                query_get(row, 17, "decode team member assertions")?,
                "team member assertion count",
            )?,
            competing_membership_count: decode_nonnegative_u64(
                query_get(row, 18, "decode team member conflicts")?,
                "team member competing membership count",
            )?,
            last_commit_seq: decode_nonnegative_u64(
                query_get(row, 19, "decode team member commit")?,
                "team member commit sequence",
            )?,
        });
    }
    Ok(members)
}

fn read_inbox_summaries(
    transaction: &Transaction<'_>,
    team_key: &[u8],
    has_cursor: bool,
    cursor_text: &str,
    cursor_key: &[u8],
    limit: u32,
) -> Result<Vec<TeamInboxRow>, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT cti.inbox_key, cti.team_key, cti.recipient_key,
                   CASE WHEN ctm.member_key IS NULL THEN 0 ELSE 1 END,
                   cti.native_team_id, cti.native_recipient_name,
                   cti.inbox_status, cti.decisive_fact_id,
                   cti.assertion_count, cti.competing_snapshot_count,
                   cti.message_count,
                   COALESCE(SUM(CASE WHEN msg.read = 0 THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN msg.message_status = 'conflicting'
                                     THEN 1 ELSE 0 END), 0),
                   MAX(cti.last_commit_seq,
                       COALESCE(MAX(msg.last_commit_seq), 0))
            FROM canonical_team_inboxes cti
            LEFT JOIN canonical_team_members ctm
              ON ctm.member_key = cti.recipient_key
            LEFT JOIN canonical_team_inbox_messages msg
              ON msg.inbox_key = cti.inbox_key
            WHERE cti.team_key = ?1
              AND (?2 = 0 OR cti.native_recipient_name > ?3
                   OR (cti.native_recipient_name = ?3 AND cti.inbox_key > ?4))
            GROUP BY cti.inbox_key
            ORDER BY cti.native_recipient_name ASC, cti.inbox_key ASC
            LIMIT ?5
            "#,
        )
        .map_err(|error| query_sqlite_error("prepare team inbox summaries", error))?;
    let mut rows = statement
        .query(rusqlite::params![
            team_key,
            i64::from(has_cursor),
            cursor_text,
            cursor_key,
            i64::from(limit)
        ])
        .map_err(|error| query_sqlite_error("execute team inbox summaries", error))?;
    let mut inboxes = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| query_sqlite_error("advance team inbox summaries", error))?
    {
        let inbox_key: Vec<u8> = query_get(row, 0, "decode team inbox key")?;
        let row_team_key: Vec<u8> = query_get(row, 1, "decode team inbox team key")?;
        let recipient_key: Vec<u8> = query_get(row, 2, "decode team inbox recipient")?;
        let decisive_fact_id: Vec<u8> = query_get(row, 7, "decode team inbox decisive fact")?;
        inboxes.push(TeamInboxRow {
            summary: TeamInboxSummary {
                inbox_id: encode_entity_id(TEAM_INBOX_ID_PREFIX, &inbox_key),
                team_id: encode_entity_id(TEAM_ID_PREFIX, &row_team_key),
                recipient_id: encode_entity_id(TEAM_MEMBER_ID_PREFIX, &recipient_key),
                recipient_present: query_get::<i64>(row, 3, "decode team recipient presence")? != 0,
                native_team_id: query_get(row, 4, "decode team inbox native team")?,
                native_recipient_name: query_get(row, 5, "decode team inbox recipient name")?,
                inbox_status: query_get(row, 6, "decode team inbox status")?,
                decisive_fact_id: encode_entity_id(FACT_ID_PREFIX, &decisive_fact_id),
                assertion_count: decode_nonnegative_u64(
                    query_get(row, 8, "decode team inbox assertions")?,
                    "team inbox assertion count",
                )?,
                competing_snapshot_count: decode_nonnegative_u64(
                    query_get(row, 9, "decode team inbox conflicts")?,
                    "team inbox competing snapshot count",
                )?,
                message_count: decode_nonnegative_u64(
                    query_get(row, 10, "decode team inbox messages")?,
                    "team inbox message count",
                )?,
                unread_message_count: decode_nonnegative_u64(
                    query_get(row, 11, "decode team inbox unread")?,
                    "team inbox unread message count",
                )?,
                conflicting_message_count: decode_nonnegative_u64(
                    query_get(row, 12, "decode team inbox message conflicts")?,
                    "team inbox conflicting message count",
                )?,
                last_commit_seq: decode_nonnegative_u64(
                    query_get(row, 13, "decode team inbox commit")?,
                    "team inbox commit sequence",
                )?,
            },
            inbox_key,
        });
    }
    Ok(inboxes)
}

fn read_messages(
    transaction: &Transaction<'_>,
    inbox_key: &[u8],
    inbox_id: &str,
    has_cursor: bool,
    cursor_ordinal: u32,
    cursor_key: &[u8],
    limit: u32,
) -> Result<Vec<TeamMessageRow>, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT msg.message_key, msg.sender_key,
                   CASE WHEN sender.member_key IS NULL THEN 0 ELSE 1 END,
                   msg.message_ordinal, msg.native_message_id,
                   msg.native_kind, msg.native_version,
                   msg.native_sender_name, msg.text, msg.summary, msg.color,
                   msg.source_time, msg.source_time_quality, msg.read,
                   msg.message_status, msg.decisive_fact_id,
                   msg.assertion_count, msg.competing_message_count,
                   msg.last_commit_seq
            FROM canonical_team_inbox_messages msg
            LEFT JOIN canonical_team_members sender
              ON sender.member_key = msg.sender_key
            WHERE msg.inbox_key = ?1
              AND (?2 = 0 OR msg.message_ordinal > ?3
                   OR (msg.message_ordinal = ?3 AND msg.message_key > ?4))
            ORDER BY msg.message_ordinal ASC, msg.message_key ASC
            LIMIT ?5
            "#,
        )
        .map_err(|error| query_sqlite_error("prepare team inbox messages", error))?;
    let mut rows = statement
        .query(rusqlite::params![
            inbox_key,
            i64::from(has_cursor),
            i64::from(cursor_ordinal),
            cursor_key,
            i64::from(limit)
        ])
        .map_err(|error| query_sqlite_error("execute team inbox messages", error))?;
    let mut messages = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| query_sqlite_error("advance team inbox messages", error))?
    {
        let message_key: Vec<u8> = query_get(row, 0, "decode team inbox message key")?;
        let sender_key: Vec<u8> = query_get(row, 1, "decode team inbox sender key")?;
        let decisive_fact_id: Vec<u8> = query_get(row, 15, "decode team inbox message fact")?;
        messages.push(TeamMessageRow {
            message: TeamInboxMessage {
                message_id: encode_entity_id(TEAM_INBOX_MESSAGE_ID_PREFIX, &message_key),
                inbox_id: inbox_id.to_string(),
                sender_id: encode_entity_id(TEAM_MEMBER_ID_PREFIX, &sender_key),
                sender_present: query_get::<i64>(row, 2, "decode team inbox sender presence")? != 0,
                message_ordinal: decode_nonnegative_u32(
                    query_get(row, 3, "decode team inbox message ordinal")?,
                    "team inbox message ordinal",
                )?,
                native_message_id: query_get(row, 4, "decode native inbox message id")?,
                native_kind: query_get(row, 5, "decode native inbox message kind")?,
                native_version: decode_optional_u32(
                    query_get(row, 6, "decode native inbox message version")?,
                    "native inbox message version",
                )?,
                native_sender_name: query_get(row, 7, "decode native inbox sender name")?,
                text: query_get(row, 8, "decode inbox message text")?,
                summary: query_get(row, 9, "decode inbox message summary")?,
                color: query_get(row, 10, "decode inbox message color")?,
                source_time: query_get(row, 11, "decode inbox message time")?,
                source_time_quality: query_get(row, 12, "decode inbox message time quality")?,
                read: query_get::<i64>(row, 13, "decode inbox message read state")? != 0,
                message_status: query_get(row, 14, "decode inbox message status")?,
                decisive_fact_id: encode_entity_id(FACT_ID_PREFIX, &decisive_fact_id),
                assertion_count: decode_nonnegative_u64(
                    query_get(row, 16, "decode inbox message assertions")?,
                    "team inbox message assertion count",
                )?,
                competing_message_count: decode_nonnegative_u64(
                    query_get(row, 17, "decode inbox message conflicts")?,
                    "team inbox message competing count",
                )?,
                last_commit_seq: decode_nonnegative_u64(
                    query_get(row, 18, "decode inbox message commit")?,
                    "team inbox message commit sequence",
                )?,
            },
            message_key,
        });
    }
    Ok(messages)
}

fn require_team(transaction: &Transaction<'_>, team_key: &[u8]) -> Result<(), EngineError> {
    let exists = transaction
        .query_row(
            r#"
            SELECT 1 FROM (
                SELECT team_key FROM canonical_teams
                UNION SELECT team_key FROM canonical_team_inboxes
            ) WHERE team_key = ?1
            "#,
            [team_key],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| query_sqlite_error("validate current team", error))?
        .is_some();
    if !exists {
        return Err(EngineError::InvalidQuery(
            "team id does not identify a current team".to_string(),
        ));
    }
    Ok(())
}

fn validate_page_limit(limit: u32, label: &str) -> Result<(), EngineError> {
    if !(1..=MAX_TEAM_PAGE_LIMIT).contains(&limit) {
        return Err(EngineError::InvalidQuery(format!(
            "{label} page limit must be between 1 and {MAX_TEAM_PAGE_LIMIT}, got {limit}"
        )));
    }
    Ok(())
}

fn encode_cursor(cursor: &TeamCursor) -> Result<String, EngineError> {
    let json = serde_json::to_vec(cursor).map_err(|error| {
        EngineError::InvalidQuery(format!("could not encode team cursor: {error}"))
    })?;
    Ok(URL_SAFE_NO_PAD.encode(json))
}

fn decode_cursor(
    value: &str,
    kind: TeamCursorKind,
    scope_id: Option<&str>,
) -> Result<TeamCursor, EngineError> {
    if value.is_empty() || value.len() > MAX_TEAM_CURSOR_BYTES {
        return Err(EngineError::InvalidQuery(
            "team cursor is empty or exceeds the supported bound".to_string(),
        ));
    }
    let json = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| EngineError::InvalidQuery("team cursor is not valid base64url".to_string()))?;
    let cursor: TeamCursor = serde_json::from_slice(&json)
        .map_err(|_| EngineError::InvalidQuery("team cursor payload is malformed".to_string()))?;
    if cursor.version != TEAM_QUERY_CONTRACT_VERSION {
        return Err(EngineError::InvalidQuery(format!(
            "unsupported team cursor version {}",
            cursor.version
        )));
    }
    if cursor.kind != kind || cursor.scope_id.as_deref() != scope_id {
        return Err(EngineError::InvalidQuery(
            "team cursor does not belong to this query".to_string(),
        ));
    }
    if cursor.order_text.len() > MAX_TEAM_CURSOR_BYTES {
        return Err(EngineError::InvalidQuery(
            "team cursor order key exceeds the supported bound".to_string(),
        ));
    }
    cursor_key(Some(&cursor))?;
    Ok(cursor)
}

fn cursor_key(cursor: Option<&TeamCursor>) -> Result<Vec<u8>, EngineError> {
    cursor
        .map(|cursor| {
            let key = URL_SAFE_NO_PAD.decode(&cursor.entity_key).map_err(|_| {
                EngineError::InvalidQuery("team cursor entity key is malformed".to_string())
            })?;
            if key.is_empty() || key.len() > MAX_TEAM_CURSOR_BYTES {
                return Err(EngineError::InvalidQuery(
                    "team cursor entity key is empty or exceeds the supported bound".to_string(),
                ));
            }
            Ok(key)
        })
        .transpose()
        .map(|key| key.unwrap_or_default())
}

fn validate_cursor_watermark(
    cursor: Option<&TeamCursor>,
    current_watermark: u64,
) -> Result<(), EngineError> {
    if let Some(cursor) = cursor {
        if cursor.at_commit_seq != current_watermark {
            return Err(EngineError::InvalidQuery(format!(
                "team cursor expired at commit {}; current commit is {current_watermark}",
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
        operation: "decode team integer",
        detail: format!("{field} was negative: {value}"),
    })
}

fn decode_nonnegative_u32(value: i64, field: &'static str) -> Result<u32, EngineError> {
    u32::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "decode team integer",
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

    fn seed_teams(connection: &Connection) {
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
                "INSERT INTO source_streams VALUES (1, 1, 'teams', 'replace_document', 'fixture', 'available', 'none', NULL, 1)",
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

        for (ordinal, (fact_id, kind, key)) in [
            (
                1,
                (
                    b"team-fact".as_slice(),
                    "team_snapshot",
                    b"team-a".as_slice(),
                ),
            ),
            (
                2,
                (
                    b"inbox-a-fact".as_slice(),
                    "team_inbox_snapshot",
                    b"inbox-a".as_slice(),
                ),
            ),
            (
                3,
                (
                    b"inbox-b-fact".as_slice(),
                    "team_inbox_snapshot",
                    b"inbox-b".as_slice(),
                ),
            ),
        ] {
            insert_fact(connection, fact_id, kind, key, ordinal);
        }
        connection
            .execute(
                r#"
                INSERT INTO team_snapshot_assertions (
                    fact_id, team_key, native_team_id, name, description,
                    created_at, created_at_quality, lead_member_key,
                    native_lead_agent_id, lead_session_key,
                    native_lead_session_id, snapshot_digest,
                    source_object_id, source_generation, cursor_end,
                    last_commit_seq
                ) VALUES (?1, ?2, 'alpha', 'Alpha', 'fixture team',
                          '2026-08-12T00:00:00.000Z', 'native_exact', ?3,
                          'lead@alpha', ?4, 'lead-session', ?5, 1, 1, ?6, 1)
                "#,
                params![
                    b"team-fact".as_slice(),
                    b"team-a".as_slice(),
                    b"member-lead".as_slice(),
                    b"missing-session".as_slice(),
                    [1_u8; 32].as_slice(),
                    b"team-cursor".as_slice(),
                ],
            )
            .unwrap();
        connection
            .execute(
                r#"
                INSERT INTO team_member_assertions (
                    fact_id, member_key, team_key, member_ordinal,
                    native_agent_id, native_name, agent_type, model, prompt,
                    color, plan_mode_required, joined_at, joined_at_quality,
                    tmux_pane_id, cwd, subscriptions_json, backend_type,
                    member_digest
                ) VALUES (?1, ?2, ?3, 0, 'lead@alpha', 'lead',
                          'team-lead', 'test-model', 'prompt', 'blue', 1,
                          '2026-08-12T00:00:00.000Z', 'native_exact',
                          'leader', '/tmp/alpha', ?4, 'in-process', ?5)
                "#,
                params![
                    b"team-fact".as_slice(),
                    b"member-lead".as_slice(),
                    b"team-a".as_slice(),
                    br#"["changes"]"#.as_slice(),
                    [2_u8; 32].as_slice(),
                ],
            )
            .unwrap();
        connection
            .execute(
                r#"
                INSERT INTO canonical_teams (
                    team_key, native_team_id, name, description, created_at,
                    created_at_quality, lead_member_key,
                    native_lead_agent_id, lead_session_key,
                    native_lead_session_id, config_status, decisive_fact_id,
                    assertion_count, competing_snapshot_count, member_count,
                    last_commit_seq
                ) VALUES (?1, 'alpha', 'Alpha', 'fixture team',
                          '2026-08-12T00:00:00.000Z', 'native_exact', ?2,
                          'lead@alpha', ?3, 'lead-session', 'resolved', ?4,
                          1, 0, 1, 1)
                "#,
                params![
                    b"team-a".as_slice(),
                    b"member-lead".as_slice(),
                    b"missing-session".as_slice(),
                    b"team-fact".as_slice(),
                ],
            )
            .unwrap();
        connection
            .execute(
                r#"
                INSERT INTO canonical_team_members (
                    member_key, team_key, member_ordinal, native_agent_id,
                    native_name, agent_type, model, prompt, color,
                    plan_mode_required, joined_at, joined_at_quality,
                    tmux_pane_id, cwd, subscriptions_json, backend_type,
                    membership_status, decisive_fact_id, assertion_count,
                    competing_membership_count, last_commit_seq
                ) VALUES (?1, ?2, 0, 'lead@alpha', 'lead', 'team-lead',
                          'test-model', 'prompt', 'blue', 1,
                          '2026-08-12T00:00:00.000Z', 'native_exact',
                          'leader', '/tmp/alpha', ?3, 'in-process',
                          'resolved', ?4, 1, 0, 1)
                "#,
                params![
                    b"member-lead".as_slice(),
                    b"team-a".as_slice(),
                    br#"["changes"]"#.as_slice(),
                    b"team-fact".as_slice(),
                ],
            )
            .unwrap();
        insert_inbox(
            connection,
            b"inbox-a",
            b"team-a",
            b"member-lead",
            "alpha",
            "lead",
            b"inbox-a-fact",
            "resolved",
        );
        insert_inbox(
            connection,
            b"inbox-b",
            b"team-b",
            b"orphan-recipient",
            "beta",
            "orphan",
            b"inbox-b-fact",
            "conflicting",
        );
        insert_message(
            connection,
            b"message-a",
            b"inbox-a",
            b"missing-sender",
            0,
            b"inbox-a-fact",
            false,
        );
        insert_message(
            connection,
            b"message-b",
            b"inbox-a",
            b"member-lead",
            1,
            b"inbox-a-fact",
            true,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_inbox(
        connection: &Connection,
        inbox_key: &[u8],
        team_key: &[u8],
        recipient_key: &[u8],
        native_team_id: &str,
        recipient: &str,
        fact_id: &[u8],
        status: &str,
    ) {
        connection
            .execute(
                r#"
                INSERT INTO team_inbox_snapshot_assertions (
                    fact_id, inbox_key, team_key, recipient_key,
                    native_team_id, native_recipient_name, snapshot_digest,
                    source_object_id, source_generation, cursor_end,
                    last_commit_seq
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, 1, ?8, 1)
                "#,
                params![
                    fact_id,
                    inbox_key,
                    team_key,
                    recipient_key,
                    native_team_id,
                    recipient,
                    [recipient.len() as u8; 32].as_slice(),
                    format!("inbox-{recipient}").as_bytes(),
                ],
            )
            .unwrap();
        connection
            .execute(
                r#"
                INSERT INTO canonical_team_inboxes (
                    inbox_key, team_key, recipient_key, native_team_id,
                    native_recipient_name, inbox_status, decisive_fact_id,
                    assertion_count, competing_snapshot_count, message_count,
                    last_commit_seq
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7,
                          CASE WHEN ?6 = 'conflicting' THEN 2 ELSE 1 END,
                          CASE WHEN ?6 = 'conflicting' THEN 1 ELSE 0 END,
                          CASE WHEN ?4 = 'alpha' THEN 2 ELSE 0 END, 1)
                "#,
                params![
                    inbox_key,
                    team_key,
                    recipient_key,
                    native_team_id,
                    recipient,
                    status,
                    fact_id
                ],
            )
            .unwrap();
    }

    fn insert_message(
        connection: &Connection,
        message_key: &[u8],
        inbox_key: &[u8],
        sender_key: &[u8],
        ordinal: i64,
        fact_id: &[u8],
        read: bool,
    ) {
        connection
            .execute(
                r#"
                INSERT INTO team_inbox_message_assertions (
                    fact_id, message_key, inbox_key, message_ordinal,
                    sender_key, native_message_id, native_kind,
                    native_version, native_sender_name, text, summary, color,
                    source_time, source_time_quality, read, message_digest
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'message', 1,
                          'sender', ?7, 'summary', 'blue',
                          '2026-08-12T01:00:00.000Z', 'native_exact', ?8, ?9)
                "#,
                params![
                    fact_id,
                    message_key,
                    inbox_key,
                    ordinal,
                    sender_key,
                    format!("native-{ordinal}"),
                    format!("message {ordinal}"),
                    i64::from(read),
                    [ordinal as u8 + 10; 32].as_slice(),
                ],
            )
            .unwrap();
        connection
            .execute(
                r#"
                INSERT INTO canonical_team_inbox_messages (
                    message_key, inbox_key, message_ordinal, sender_key,
                    native_message_id, native_kind, native_version,
                    native_sender_name, text, summary, color, source_time,
                    source_time_quality, read, message_status,
                    decisive_fact_id, assertion_count,
                    competing_message_count, last_commit_seq
                ) VALUES (?1, ?2, ?3, ?4, ?5, 'message', 1, 'sender',
                          ?6, 'summary', 'blue', '2026-08-12T01:00:00.000Z',
                          'native_exact', ?7, 'resolved', ?8, 1, 0, 1)
                "#,
                params![
                    message_key,
                    inbox_key,
                    ordinal,
                    sender_key,
                    format!("native-{ordinal}"),
                    format!("message {ordinal}"),
                    i64::from(read),
                    fact_id,
                ],
            )
            .unwrap();
    }

    #[test]
    fn teams_preserve_config_membership_orphan_inboxes_and_paged_messages() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("teams.db");
        let connection = Connection::open(&database).unwrap();
        seed_teams(&connection);
        drop(connection);
        let mut pool = QueryPool::start(database, 1, None).unwrap();
        let client = pool.client();

        let first = client
            .teams(TeamPageRequest {
                cursor: None,
                limit: 1,
            })
            .unwrap();
        assert_eq!(first.contract_version, TEAM_QUERY_CONTRACT_VERSION);
        assert_eq!(first.items[0].native_team_id, "alpha");
        assert_eq!(first.items[0].message_count, 2);
        assert_eq!(first.items[0].unread_message_count, 1);
        let alpha_id = first.items[0].team_id.clone();
        let second = client
            .teams(TeamPageRequest {
                cursor: first.next_cursor,
                limit: 1,
            })
            .unwrap();
        assert_eq!(second.items[0].native_team_id, "beta");
        assert!(
            second.items[0].config.is_none(),
            "orphan inbox remains visible"
        );
        assert_eq!(second.items[0].conflicting_inbox_count, 1);

        let details = client
            .team_details(TeamDetailsRequest {
                team_id: alpha_id.clone(),
            })
            .unwrap();
        assert_eq!(details.team.config.as_ref().unwrap().name, "Alpha");
        assert!(!details.team.config.as_ref().unwrap().lead_session_present);
        assert_eq!(details.members.len(), 1);
        assert_eq!(details.members[0].subscriptions, ["changes"]);
        assert_eq!(details.members[0].plan_mode_required, Some(true));

        let inboxes = client
            .team_inboxes(TeamInboxPageRequest {
                team_id: alpha_id,
                cursor: None,
                limit: 10,
            })
            .unwrap();
        assert_eq!(inboxes.items.len(), 1);
        assert_eq!(inboxes.items[0].native_recipient_name, "lead");
        assert_eq!(inboxes.items[0].unread_message_count, 1);
        let inbox_id = inboxes.items[0].inbox_id.clone();

        let messages = client
            .team_inbox_messages(TeamInboxMessagePageRequest {
                inbox_id: inbox_id.clone(),
                cursor: None,
                limit: 1,
            })
            .unwrap();
        assert_eq!(messages.items.len(), 1);
        assert_eq!(messages.items[0].text, "message 0");
        assert!(!messages.items[0].sender_present);
        let final_page = client
            .team_inbox_messages(TeamInboxMessagePageRequest {
                inbox_id,
                cursor: messages.next_cursor,
                limit: 1,
            })
            .unwrap();
        assert_eq!(final_page.items[0].text, "message 1");
        assert!(final_page.items[0].sender_present);
        assert!(final_page.next_cursor.is_none());

        pool.shutdown().unwrap();
    }

    #[test]
    fn team_cursors_do_not_cross_query_scopes() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("team-cursors.db");
        let connection = Connection::open(&database).unwrap();
        seed_teams(&connection);
        drop(connection);
        let mut pool = QueryPool::start(database, 1, None).unwrap();
        let client = pool.client();
        let first = client
            .teams(TeamPageRequest {
                cursor: None,
                limit: 1,
            })
            .unwrap();
        assert!(matches!(
            client.team_inboxes(TeamInboxPageRequest {
                team_id: first.items[0].team_id.clone(),
                cursor: first.next_cursor,
                limit: 1,
            }),
            Err(EngineError::InvalidQuery(_))
        ));
        pool.shutdown().unwrap();
    }

    #[test]
    fn team_query_ordering_indexes_are_installed() {
        let connection = Connection::open_in_memory().unwrap();
        schema::initialize_schema(&connection).unwrap();
        for (table, index) in [
            ("canonical_teams", "idx_canonical_teams_native"),
            (
                "canonical_team_inboxes",
                "idx_canonical_team_inboxes_recipient",
            ),
            (
                "canonical_team_inbox_messages",
                "idx_canonical_team_inbox_messages_inbox",
            ),
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
