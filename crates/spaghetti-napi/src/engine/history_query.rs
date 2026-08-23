//! Snapshot-consistent reads of decoded history.
//!
//! These are the RFC 011 queries the playground and CLI have always used:
//! `listProjects` / `listSessions` report what has actually been decoded, in
//! Rust-defined activity order, with an opaque keyset cursor.
//!
//! They also carry each row's catalog facts (`external_ref`, `catalog_state`).
//! Those columns come from `engine::catalog`, so the two surfaces share one
//! derivation and cannot disagree about the same entity.
//!
//! Split out of `query_pool.rs`, which owns the worker pool rather than any
//! particular query, and was well past the landing plan's file-size cap.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rusqlite::Connection;

use super::catalog::{
    encode_external_ref, history_project_catalog_cte, query_structures_deferred,
    HISTORY_PROJECT_CATALOG_COLUMNS, HISTORY_PROJECT_CATALOG_JOINS,
    HISTORY_SESSION_CATALOG_COLUMNS, HISTORY_SESSION_CATALOG_JOIN,
};
use super::query_identity::{
    decode_entity_id, encode_entity_id, PROJECT_ID_PREFIX, SESSION_ID_PREFIX,
};
use super::query_pool::{
    cursor_entity_key, decode_history_cursor, decode_nonnegative_u64, encode_history_cursor,
    query_sqlite_error, read_committed_watermark, validate_history_cursor_watermark,
    validate_history_page_limit, HistoryCursorKind, HistoryCursorPayload,
    HistoryProjectIndexSummary, HistoryProjectPage, HistoryProjectPageRequest, HistoryProjectRow,
    HistoryProjectSummary, HistorySessionIndexSummary, HistorySessionPage,
    HistorySessionPageRequest, HistorySessionRow, HistorySessionSummary,
    HISTORY_QUERY_CONTRACT_VERSION,
};
use super::EngineError;

fn decode_external_ref_column(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> Result<Option<String>, EngineError> {
    let digest: Option<Vec<u8>> = row.get(index).map_err(catalog_column_error)?;
    Ok(digest.map(|value| encode_external_ref(&value)))
}

fn catalog_column_error(error: rusqlite::Error) -> EngineError {
    query_sqlite_error("decode history catalog column", error)
}

pub(super) fn read_history_projects(
    connection: &Connection,
    request: &HistoryProjectPageRequest,
) -> Result<HistoryProjectPage, EngineError> {
    validate_history_page_limit(request.limit)?;
    let cursor = request
        .cursor
        .as_deref()
        .map(|value| decode_history_cursor(value, HistoryCursorKind::Projects, None))
        .transpose()?;
    let cursor_key = cursor_entity_key(cursor.as_ref())?;
    let cursor_time = cursor
        .as_ref()
        .map(|cursor| cursor.sort_time.as_str())
        .unwrap_or("");

    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin history project snapshot", error))?;
    let watermark = read_committed_watermark(&transaction)?;
    validate_history_cursor_watermark(cursor.as_ref(), watermark)?;
    let structures_deferred = query_structures_deferred(&transaction)
        .map_err(|error| query_sqlite_error("read deferred-structures marker", error))?;
    let catalog_cte = history_project_catalog_cte(structures_deferred);
    // While the deferred indexes are absent the message aggregate scans the
    // whole message store per query; report empty message stats until
    // finalization so the list stays bounded during background ingest.
    let message_stats = if structures_deferred {
        r#"message_stats AS (
                SELECT NULL AS project_key, 0 AS message_count,
                       NULL AS latest_message_at, 0 AS last_commit_seq
                WHERE 0
            ),"#
    } else {
        r#"message_stats AS (
                SELECT cs.project_key, COUNT(cm.message_key) AS message_count,
                       MAX(cm.source_time) AS latest_message_at,
                       MAX(cm.last_commit_seq) AS last_commit_seq
                FROM canonical_sessions cs
                JOIN canonical_messages cm ON cm.session_key = cs.session_key
                GROUP BY cs.project_key
            ),"#
    };
    let mut statement = transaction
        .prepare(&format!(
            r#"
            WITH project_evidence AS (
                SELECT cs.project_key, cs.native_project_key,
                       fr.source_instance_id, cs.last_commit_seq
                FROM canonical_sessions cs
                JOIN fact_records fr ON fr.fact_id = cs.fact_id
                UNION ALL
                SELECT ci.project_key, ci.native_project_key,
                       fr.source_instance_id, ci.last_commit_seq
                FROM canonical_session_indexes ci
                JOIN fact_records fr ON fr.fact_id = ci.decisive_fact_id
                UNION ALL
                SELECT md.project_key, md.native_project_key,
                       fr.source_instance_id, md.last_commit_seq
                FROM canonical_project_memory_documents md
                JOIN fact_records fr ON fr.fact_id = md.decisive_fact_id
            ),
            projects AS (
                SELECT project_key, MIN(native_project_key) AS native_project_key,
                       MIN(source_instance_id) AS source_instance_id,
                       MAX(last_commit_seq) AS evidence_commit_seq
                FROM project_evidence
                GROUP BY project_key
            ),
            session_stats AS (
                SELECT project_key, COUNT(*) AS session_count,
                       MAX(source_time) AS latest_session_at,
                       MAX(last_commit_seq) AS last_commit_seq
                FROM canonical_sessions
                GROUP BY project_key
            ),
            {message_stats}
            index_entry_stats AS (
                SELECT project_key,
                       MAX(CASE WHEN resolution_status = 'resolved' THEN modified_at END)
                           AS latest_index_at,
                       MAX(last_commit_seq) AS last_commit_seq
                FROM canonical_session_index_entries
                GROUP BY project_key
            ),
            memory_stats AS (
                SELECT project_key, COUNT(*) AS document_count,
                       MAX(is_index) AS has_index,
                       MAX(last_commit_seq) AS last_commit_seq
                FROM canonical_project_memory_documents
                GROUP BY project_key
            ),
            {catalog_cte}
            project_rows AS (
                SELECT p.project_key, si.adapter_id, p.source_instance_id,
                       p.native_project_key,
                       {HISTORY_PROJECT_CATALOG_COLUMNS}
                       COALESCE(ss.session_count, 0) AS session_count,
                       COALESCE(ms.message_count, 0) AS message_count,
                       COALESCE(mem.document_count, 0) AS memory_document_count,
                       COALESCE(mem.has_index, 0) AS has_memory_index,
                       MAX(
                           COALESCE(ms.latest_message_at, ''),
                           COALESCE(ss.latest_session_at, ''),
                           COALESCE(ies.latest_index_at, '')
                       ) AS activity_sort,
                       CASE
                           WHEN COALESCE(ms.latest_message_at, '') != ''
                            AND ms.latest_message_at = MAX(
                                COALESCE(ms.latest_message_at, ''),
                                COALESCE(ss.latest_session_at, ''),
                                COALESCE(ies.latest_index_at, '')
                            ) THEN 'message'
                           WHEN COALESCE(ss.latest_session_at, '') != ''
                            AND ss.latest_session_at = MAX(
                                COALESCE(ms.latest_message_at, ''),
                                COALESCE(ss.latest_session_at, ''),
                                COALESCE(ies.latest_index_at, '')
                            ) THEN 'session'
                           WHEN COALESCE(ies.latest_index_at, '') != '' THEN 'session_index'
                           ELSE NULL
                       END AS activity_source,
                       ci.index_status, ci.original_path, ci.entry_count,
                       ci.assertion_count, ci.competing_snapshot_count,
                       ci.last_commit_seq AS index_commit_seq,
                       MAX(
                           p.evidence_commit_seq,
                           COALESCE(ss.last_commit_seq, 0),
                           COALESCE(ms.last_commit_seq, 0),
                           COALESCE(ies.last_commit_seq, 0),
                           COALESCE(mem.last_commit_seq, 0),
                           COALESCE(ci.last_commit_seq, 0)
                       ) AS last_commit_seq
                FROM projects p
                JOIN source_instances si ON si.source_instance_id = p.source_instance_id
                LEFT JOIN session_stats ss ON ss.project_key = p.project_key
                LEFT JOIN message_stats ms ON ms.project_key = p.project_key
                LEFT JOIN index_entry_stats ies ON ies.project_key = p.project_key
                LEFT JOIN memory_stats mem ON mem.project_key = p.project_key
                LEFT JOIN canonical_session_indexes ci ON ci.project_key = p.project_key
                {HISTORY_PROJECT_CATALOG_JOINS}
            )
            SELECT project_key, adapter_id, source_instance_id, native_project_key,
                   session_count, message_count, memory_document_count, has_memory_index,
                   activity_sort, activity_source,
                   index_status, original_path, entry_count, assertion_count,
                   competing_snapshot_count, index_commit_seq, last_commit_seq,
                   catalog_external_ref, catalog_state
            FROM project_rows
            WHERE (?1 = 0)
               OR activity_sort < ?2
               OR (activity_sort = ?2 AND project_key < ?3)
            ORDER BY activity_sort DESC, project_key DESC
            LIMIT ?4
            "#
        ))
        .map_err(|error| query_sqlite_error("prepare history project page", error))?;
    let mut rows = statement
        .query(rusqlite::params![
            i64::from(cursor.is_some()),
            cursor_time,
            cursor_key,
            i64::from(request.limit) + 1,
        ])
        .map_err(|error| query_sqlite_error("execute history project page", error))?;
    let mut projects = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| query_sqlite_error("advance history project page", error))?
    {
        let project_key: Vec<u8> = row
            .get(0)
            .map_err(|error| query_sqlite_error("decode history project key", error))?;
        let source_instance_id = decode_nonnegative_u64(
            row.get(2)
                .map_err(|error| query_sqlite_error("decode history source instance", error))?,
            "history source instance id",
        )?;
        let activity_sort: String = row
            .get(8)
            .map_err(|error| query_sqlite_error("decode history project order", error))?;
        let index_status: Option<String> = row
            .get(10)
            .map_err(|error| query_sqlite_error("decode history project index status", error))?;
        let index = index_status
            .map(|status| {
                Ok(HistoryProjectIndexSummary {
                    status,
                    original_path: row.get(11).map_err(|error| {
                        query_sqlite_error("decode history project original path", error)
                    })?,
                    entry_count: decode_nonnegative_u64(
                        row.get(12).map_err(|error| {
                            query_sqlite_error("decode history project index entries", error)
                        })?,
                        "history project index entry count",
                    )?,
                    assertion_count: decode_nonnegative_u64(
                        row.get(13).map_err(|error| {
                            query_sqlite_error("decode history project index assertions", error)
                        })?,
                        "history project index assertion count",
                    )?,
                    competing_snapshot_count: decode_nonnegative_u64(
                        row.get(14).map_err(|error| {
                            query_sqlite_error("decode history project competing snapshots", error)
                        })?,
                        "history project competing snapshot count",
                    )?,
                    last_commit_seq: decode_nonnegative_u64(
                        row.get(15).map_err(|error| {
                            query_sqlite_error("decode history project index commit", error)
                        })?,
                        "history project index commit sequence",
                    )?,
                })
            })
            .transpose()?;
        projects.push(HistoryProjectRow {
            summary: HistoryProjectSummary {
                project_id: encode_entity_id(PROJECT_ID_PREFIX, &project_key),
                adapter_id: row
                    .get(1)
                    .map_err(|error| query_sqlite_error("decode history adapter id", error))?,
                source_instance_id,
                native_project_key: row.get(3).map_err(|error| {
                    query_sqlite_error("decode native history project key", error)
                })?,
                transcript_session_count: decode_nonnegative_u64(
                    row.get(4).map_err(|error| {
                        query_sqlite_error("decode history project session count", error)
                    })?,
                    "history project session count",
                )?,
                message_count: decode_nonnegative_u64(
                    row.get(5).map_err(|error| {
                        query_sqlite_error("decode history project message count", error)
                    })?,
                    "history project message count",
                )?,
                memory_document_count: decode_nonnegative_u64(
                    row.get(6).map_err(|error| {
                        query_sqlite_error("decode history project memory count", error)
                    })?,
                    "history project memory document count",
                )?,
                has_memory_index: row.get::<_, i64>(7).map_err(|error| {
                    query_sqlite_error("decode history project memory index flag", error)
                })? != 0,
                latest_activity_at: (!activity_sort.is_empty()).then(|| activity_sort.clone()),
                latest_activity_source: row.get(9).map_err(|error| {
                    query_sqlite_error("decode history project activity source", error)
                })?,
                index,
                external_ref: decode_external_ref_column(row, 17)?,
                catalog_state: row.get(18).map_err(catalog_column_error)?,
                last_commit_seq: decode_nonnegative_u64(
                    row.get(16).map_err(|error| {
                        query_sqlite_error("decode history project commit sequence", error)
                    })?,
                    "history project commit sequence",
                )?,
            },
            project_key,
            sort_time: activity_sort,
        });
    }
    drop(rows);
    drop(statement);
    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish history project snapshot", error))?;

    let has_more = projects.len() > request.limit as usize;
    if has_more {
        projects.truncate(request.limit as usize);
    }
    let next_cursor = if has_more {
        projects
            .last()
            .map(|row| {
                encode_history_cursor(&HistoryCursorPayload {
                    version: HISTORY_QUERY_CONTRACT_VERSION,
                    kind: HistoryCursorKind::Projects,
                    at_commit_seq: watermark,
                    sort_time: row.sort_time.clone(),
                    entity_key: URL_SAFE_NO_PAD.encode(&row.project_key),
                    project_id: None,
                })
            })
            .transpose()?
    } else {
        None
    };
    Ok(HistoryProjectPage {
        contract_version: HISTORY_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        items: projects.into_iter().map(|row| row.summary).collect(),
        next_cursor,
    })
}

pub(super) fn read_history_sessions(
    connection: &Connection,
    request: &HistorySessionPageRequest,
) -> Result<HistorySessionPage, EngineError> {
    validate_history_page_limit(request.limit)?;
    let project_key = decode_entity_id(&request.project_id, PROJECT_ID_PREFIX, "project id")?;
    let cursor = request
        .cursor
        .as_deref()
        .map(|value| {
            decode_history_cursor(
                value,
                HistoryCursorKind::Sessions,
                Some(request.project_id.as_str()),
            )
        })
        .transpose()?;
    let cursor_key = cursor_entity_key(cursor.as_ref())?;
    let cursor_time = cursor
        .as_ref()
        .map(|cursor| cursor.sort_time.as_str())
        .unwrap_or("");

    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin history session snapshot", error))?;
    let watermark = read_committed_watermark(&transaction)?;
    validate_history_cursor_watermark(cursor.as_ref(), watermark)?;
    let structures_deferred = query_structures_deferred(&transaction)
        .map_err(|error| query_sqlite_error("read deferred-structures marker", error))?;
    // Same deferred-index hazard as the project list: without the session
    // activity index every message aggregate and correlated quality probe
    // scans the message store. Report empty stats until finalization.
    let message_stats = if structures_deferred {
        r#"message_stats AS (
                SELECT NULL AS session_key, 0 AS message_count,
                       NULL AS first_message_at, NULL AS last_message_at,
                       0 AS last_commit_seq
                WHERE 0
            ),"#
    } else {
        r#"message_stats AS (
                SELECT cm.session_key, COUNT(*) AS message_count,
                       MIN(cm.source_time) AS first_message_at,
                       MAX(cm.source_time) AS last_message_at,
                       MAX(cm.last_commit_seq) AS last_commit_seq
                FROM canonical_messages cm
                JOIN target_sessions target ON target.session_key = cm.session_key
                GROUP BY cm.session_key
            ),"#
    };
    let first_message_quality = if structures_deferred {
        "NULL"
    } else {
        r#"(
                           SELECT cm.source_time_quality
                           FROM canonical_messages cm
                           WHERE cm.session_key = cs.session_key
                             AND cm.source_time = ms.first_message_at
                           ORDER BY cm.message_key ASC
                           LIMIT 1
                       )"#
    };
    let last_message_quality = if structures_deferred {
        "NULL"
    } else {
        r#"(
                           SELECT cm.source_time_quality
                           FROM canonical_messages cm
                           WHERE cm.session_key = cs.session_key
                             AND cm.source_time = ms.last_message_at
                           ORDER BY cm.message_key DESC
                           LIMIT 1
                       )"#
    };
    let mut statement = transaction
        .prepare(&format!(
            r#"
            WITH target_sessions AS (
                SELECT *
                FROM canonical_sessions
                WHERE project_key = ?1
            ),
            {message_stats}
            session_rows AS (
                SELECT cs.session_key, cs.project_key, cs.native_session_id,
                       cs.native_project_key, cs.cwd, cs.git_branch,
                       cs.first_prompt, cs.ai_title, cs.custom_title,
                       COALESCE(ms.message_count, 0) AS message_count,
                       ms.first_message_at,
                       {first_message_quality} AS first_message_quality,
                       ms.last_message_at,
                       {last_message_quality} AS last_message_quality,
                       MAX(
                           COALESCE(ms.last_message_at, ''),
                           COALESCE(cs.source_time, ''),
                           CASE
                               WHEN si.transcript_status = 'present'
                                AND si.resolution_status = 'resolved'
                               THEN COALESCE(si.modified_at, '')
                               ELSE ''
                           END
                       ) AS activity_sort,
                       CASE
                           WHEN COALESCE(ms.last_message_at, '') != ''
                            AND ms.last_message_at = MAX(
                                COALESCE(ms.last_message_at, ''),
                                COALESCE(cs.source_time, ''),
                                CASE
                                    WHEN si.transcript_status = 'present'
                                     AND si.resolution_status = 'resolved'
                                    THEN COALESCE(si.modified_at, '')
                                    ELSE ''
                                END
                            ) THEN 'message'
                           WHEN COALESCE(cs.source_time, '') != ''
                            AND cs.source_time = MAX(
                                COALESCE(ms.last_message_at, ''),
                                COALESCE(cs.source_time, ''),
                                CASE
                                    WHEN si.transcript_status = 'present'
                                     AND si.resolution_status = 'resolved'
                                    THEN COALESCE(si.modified_at, '')
                                    ELSE ''
                                END
                            ) THEN 'session'
                           WHEN si.transcript_status = 'present'
                            AND si.resolution_status = 'resolved'
                            AND COALESCE(si.modified_at, '') != '' THEN 'session_index'
                           ELSE NULL
                       END AS activity_source,
                       si.full_path, si.file_mtime_ms, si.first_prompt AS index_first_prompt,
                       si.summary, si.message_count AS index_message_count,
                       si.created_at, si.created_at_quality, si.modified_at,
                       si.modified_at_quality, si.git_branch AS index_git_branch,
                       si.project_path, si.is_sidechain, si.transcript_status,
                       si.resolution_status, si.assertion_count,
                       si.competing_entry_count, si.identity_conflict,
                       si.join_conflict, si.last_commit_seq AS index_commit_seq,
                       {HISTORY_SESSION_CATALOG_COLUMNS}
                       MAX(
                           cs.last_commit_seq,
                           COALESCE(ms.last_commit_seq, 0),
                           COALESCE(si.last_commit_seq, 0)
                       ) AS last_commit_seq
                FROM target_sessions cs
                LEFT JOIN message_stats ms ON ms.session_key = cs.session_key
                LEFT JOIN canonical_session_index_entries si ON si.session_key = cs.session_key
                {HISTORY_SESSION_CATALOG_JOIN}
            )
            SELECT session_key, project_key, native_session_id, native_project_key,
                   cwd, git_branch, first_prompt, ai_title, custom_title,
                   message_count, first_message_at, first_message_quality,
                   last_message_at, last_message_quality, activity_sort,
                   activity_source, full_path, file_mtime_ms, index_first_prompt,
                   summary, index_message_count, created_at, created_at_quality,
                   modified_at, modified_at_quality, index_git_branch, project_path,
                   is_sidechain, transcript_status, resolution_status,
                   assertion_count, competing_entry_count, identity_conflict,
                   join_conflict, index_commit_seq, last_commit_seq,
                   catalog_external_ref, catalog_state
            FROM session_rows
            WHERE (?2 = 0)
               OR activity_sort < ?3
               OR (activity_sort = ?3 AND session_key < ?4)
            ORDER BY activity_sort DESC, session_key DESC
            LIMIT ?5
            "#
        ))
        .map_err(|error| query_sqlite_error("prepare history session page", error))?;
    let mut rows = statement
        .query(rusqlite::params![
            project_key,
            i64::from(cursor.is_some()),
            cursor_time,
            cursor_key,
            i64::from(request.limit) + 1,
        ])
        .map_err(|error| query_sqlite_error("execute history session page", error))?;
    let mut sessions = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| query_sqlite_error("advance history session page", error))?
    {
        let session_key: Vec<u8> = row
            .get(0)
            .map_err(|error| query_sqlite_error("decode history session key", error))?;
        let row_project_key: Vec<u8> = row
            .get(1)
            .map_err(|error| query_sqlite_error("decode history session project key", error))?;
        let activity_sort: String = row
            .get(14)
            .map_err(|error| query_sqlite_error("decode history session order", error))?;
        let index_full_path: Option<String> = row
            .get(16)
            .map_err(|error| query_sqlite_error("decode history session index path", error))?;
        let index = index_full_path
            .map(|full_path| {
                Ok(HistorySessionIndexSummary {
                    full_path,
                    file_mtime_ms: decode_nonnegative_u64(
                        row.get(17).map_err(|error| {
                            query_sqlite_error("decode history session file mtime", error)
                        })?,
                        "history session file mtime",
                    )?,
                    first_prompt: row.get(18).map_err(|error| {
                        query_sqlite_error("decode history session index prompt", error)
                    })?,
                    summary: row.get(19).map_err(|error| {
                        query_sqlite_error("decode history session index summary", error)
                    })?,
                    message_count: decode_nonnegative_u64(
                        row.get(20).map_err(|error| {
                            query_sqlite_error("decode history session index message count", error)
                        })?,
                        "history session index message count",
                    )?,
                    created_at: row.get(21).map_err(|error| {
                        query_sqlite_error("decode history session created time", error)
                    })?,
                    created_at_quality: row.get(22).map_err(|error| {
                        query_sqlite_error("decode history session created quality", error)
                    })?,
                    modified_at: row.get(23).map_err(|error| {
                        query_sqlite_error("decode history session modified time", error)
                    })?,
                    modified_at_quality: row.get(24).map_err(|error| {
                        query_sqlite_error("decode history session modified quality", error)
                    })?,
                    git_branch: row.get(25).map_err(|error| {
                        query_sqlite_error("decode history session index branch", error)
                    })?,
                    project_path: row.get(26).map_err(|error| {
                        query_sqlite_error("decode history session project path", error)
                    })?,
                    is_sidechain: row.get::<_, i64>(27).map_err(|error| {
                        query_sqlite_error("decode history session sidechain flag", error)
                    })? != 0,
                    transcript_status: row.get(28).map_err(|error| {
                        query_sqlite_error("decode history session transcript status", error)
                    })?,
                    resolution_status: row.get(29).map_err(|error| {
                        query_sqlite_error("decode history session resolution status", error)
                    })?,
                    assertion_count: decode_nonnegative_u64(
                        row.get(30).map_err(|error| {
                            query_sqlite_error("decode history session index assertions", error)
                        })?,
                        "history session index assertion count",
                    )?,
                    competing_entry_count: decode_nonnegative_u64(
                        row.get(31).map_err(|error| {
                            query_sqlite_error("decode history session competing entries", error)
                        })?,
                        "history session competing entry count",
                    )?,
                    identity_conflict: row.get::<_, i64>(32).map_err(|error| {
                        query_sqlite_error("decode history session identity conflict", error)
                    })? != 0,
                    join_conflict: row.get::<_, i64>(33).map_err(|error| {
                        query_sqlite_error("decode history session join conflict", error)
                    })? != 0,
                    last_commit_seq: decode_nonnegative_u64(
                        row.get(34).map_err(|error| {
                            query_sqlite_error("decode history session index commit", error)
                        })?,
                        "history session index commit sequence",
                    )?,
                })
            })
            .transpose()?;
        sessions.push(HistorySessionRow {
            summary: HistorySessionSummary {
                session_id: encode_entity_id(SESSION_ID_PREFIX, &session_key),
                project_id: encode_entity_id(PROJECT_ID_PREFIX, &row_project_key),
                native_session_id: row.get(2).map_err(|error| {
                    query_sqlite_error("decode native history session id", error)
                })?,
                native_project_key: row.get(3).map_err(|error| {
                    query_sqlite_error("decode native history project key", error)
                })?,
                cwd: row
                    .get(4)
                    .map_err(|error| query_sqlite_error("decode history session cwd", error))?,
                git_branch: row.get(5).map_err(|error| {
                    query_sqlite_error("decode history session git branch", error)
                })?,
                first_prompt: row.get(6).map_err(|error| {
                    query_sqlite_error("decode history session first prompt", error)
                })?,
                ai_title: row.get(7).map_err(|error| {
                    query_sqlite_error("decode history session AI title", error)
                })?,
                custom_title: row.get(8).map_err(|error| {
                    query_sqlite_error("decode history session custom title", error)
                })?,
                message_count: decode_nonnegative_u64(
                    row.get(9).map_err(|error| {
                        query_sqlite_error("decode history session message count", error)
                    })?,
                    "history session message count",
                )?,
                first_message_at: row.get(10).map_err(|error| {
                    query_sqlite_error("decode history first message time", error)
                })?,
                first_message_time_quality: row.get(11).map_err(|error| {
                    query_sqlite_error("decode history first message quality", error)
                })?,
                last_message_at: row.get(12).map_err(|error| {
                    query_sqlite_error("decode history last message time", error)
                })?,
                last_message_time_quality: row.get(13).map_err(|error| {
                    query_sqlite_error("decode history last message quality", error)
                })?,
                latest_activity_at: (!activity_sort.is_empty()).then(|| activity_sort.clone()),
                latest_activity_source: row.get(15).map_err(|error| {
                    query_sqlite_error("decode history session activity source", error)
                })?,
                index,
                external_ref: decode_external_ref_column(row, 36)?,
                catalog_state: row.get(37).map_err(catalog_column_error)?,
                last_commit_seq: decode_nonnegative_u64(
                    row.get(35).map_err(|error| {
                        query_sqlite_error("decode history session commit sequence", error)
                    })?,
                    "history session commit sequence",
                )?,
            },
            session_key,
            sort_time: activity_sort,
        });
    }
    drop(rows);
    drop(statement);
    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish history session snapshot", error))?;

    let has_more = sessions.len() > request.limit as usize;
    if has_more {
        sessions.truncate(request.limit as usize);
    }
    let next_cursor = if has_more {
        sessions
            .last()
            .map(|row| {
                encode_history_cursor(&HistoryCursorPayload {
                    version: HISTORY_QUERY_CONTRACT_VERSION,
                    kind: HistoryCursorKind::Sessions,
                    at_commit_seq: watermark,
                    sort_time: row.sort_time.clone(),
                    entity_key: URL_SAFE_NO_PAD.encode(&row.session_key),
                    project_id: Some(request.project_id.clone()),
                })
            })
            .transpose()?
    } else {
        None
    };
    Ok(HistorySessionPage {
        contract_version: HISTORY_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        project_id: request.project_id.clone(),
        items: sessions.into_iter().map(|row| row.summary).collect(),
        next_cursor,
    })
}
