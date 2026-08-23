//! Commit one catalog discovery pass in one transaction per source.
//!
//! Retraction is deliberate and narrow. A *complete* pass owns the source's
//! membership: rows it no longer reports are deleted, because the native
//! surface says they are gone. A *degraded* pass owns nothing: it upserts what
//! it managed to read and leaves everything else standing, so a transient
//! unreadable root can never empty the library (RFC 012B §7.3).

use rusqlite::{params, Connection, Transaction};

use super::super::EngineError;
use super::discovery::{ScannedProject, ScannedSession, SourceScan};

const SCAN_COMMIT_REASON: &str = "catalog.discovery.scanned";

/// Outcome of one committed pass, used for readiness reporting and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CatalogScanReceipt {
    pub(crate) commit_seq: u64,
    pub(crate) projects: u64,
    pub(crate) sessions: u64,
    pub(crate) retracted_sessions: u64,
    pub(crate) degraded: bool,
}

/// Write one source's catalog rows atomically.
pub(crate) fn commit_source_scan(
    connection: &mut Connection,
    scan: &SourceScan,
    now_ms: i64,
) -> Result<CatalogScanReceipt, EngineError> {
    let transaction = connection
        .transaction()
        .map_err(|error| sqlite_error("begin catalog scan transaction", error))?;

    let commit_seq = allocate_commit(&transaction, scan.source_instance_id, now_ms)?;
    let complete = scan.degraded_reason.is_none();

    for project in &scan.projects {
        upsert_project(&transaction, scan, project, commit_seq)?;
    }
    for session in &scan.sessions {
        upsert_session(&transaction, scan, session, commit_seq)?;
    }

    // Conflicts are rebuilt wholesale for the sessions this pass reported:
    // a conflict that no longer has evidence must stop being reported.
    for session in &scan.sessions {
        transaction
            .execute(
                "DELETE FROM catalog_association_conflicts WHERE session_key = ?1",
                params![session.session_key],
            )
            .map_err(|error| sqlite_error("clear catalog association conflicts", error))?;
    }
    for conflict in &scan.conflicts {
        transaction
            .execute(
                r#"
                INSERT INTO catalog_association_conflicts (
                    session_key, competing_native_project_key, basis, provenance, last_commit_seq
                ) VALUES (?1, ?2, ?3, ?4, ?5)
                ON CONFLICT(session_key, competing_native_project_key, basis) DO UPDATE SET
                    provenance = excluded.provenance,
                    last_commit_seq = excluded.last_commit_seq
                "#,
                params![
                    conflict.session_key,
                    conflict.competing_native_project_key,
                    conflict.basis,
                    conflict.provenance,
                    to_i64(commit_seq)?,
                ],
            )
            .map_err(|error| sqlite_error("record catalog association conflict", error))?;
    }

    let retracted_sessions = if complete {
        retract_stale_rows(&transaction, scan.source_instance_id, commit_seq)?
    } else {
        0
    };

    transaction
        .execute(
            r#"
            INSERT INTO catalog_sources (
                source_instance_id, adapter_id, degraded, degraded_reason,
                project_count, session_count, scanned_at_commit_seq, scanned_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(source_instance_id) DO UPDATE SET
                adapter_id = excluded.adapter_id,
                degraded = excluded.degraded,
                degraded_reason = excluded.degraded_reason,
                project_count = excluded.project_count,
                session_count = excluded.session_count,
                scanned_at_commit_seq = excluded.scanned_at_commit_seq,
                scanned_at = excluded.scanned_at
            "#,
            params![
                to_i64(scan.source_instance_id)?,
                scan.adapter_id,
                i64::from(!complete),
                scan.degraded_reason,
                count_for(&transaction, scan.source_instance_id, "catalog_projects")?,
                count_for(&transaction, scan.source_instance_id, "catalog_sessions")?,
                to_i64(commit_seq)?,
                now_ms,
            ],
        )
        .map_err(|error| sqlite_error("publish catalog source state", error))?;

    transaction
        .execute(
            "UPDATE ingest_commits SET committed_at = ?2 WHERE commit_seq = ?1",
            params![to_i64(commit_seq)?, now_ms],
        )
        .map_err(|error| sqlite_error("finish catalog scan commit", error))?;

    let projects = count_for(&transaction, scan.source_instance_id, "catalog_projects")?;
    let sessions = count_for(&transaction, scan.source_instance_id, "catalog_sessions")?;

    transaction
        .commit()
        .map_err(|error| sqlite_error("commit catalog scan", error))?;

    Ok(CatalogScanReceipt {
        commit_seq,
        projects: u64::try_from(projects).unwrap_or_default(),
        sessions: u64::try_from(sessions).unwrap_or_default(),
        retracted_sessions,
        degraded: !complete,
    })
}

fn allocate_commit(
    transaction: &Transaction<'_>,
    source_instance_id: u64,
    now_ms: i64,
) -> Result<u64, EngineError> {
    let commit_seq: i64 = transaction
        .query_row(
            r#"
            INSERT INTO ingest_commits (source_instance_id, reason, started_at, fact_count)
            VALUES (?1, ?2, ?3, 0)
            RETURNING commit_seq
            "#,
            params![to_i64(source_instance_id)?, SCAN_COMMIT_REASON, now_ms],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("allocate catalog scan commit", error))?;
    u64::try_from(commit_seq).map_err(|_| EngineError::Sqlite {
        operation: "decode catalog scan commit",
        detail: "commit sequence was negative".to_string(),
    })
}

fn upsert_project(
    transaction: &Transaction<'_>,
    scan: &SourceScan,
    project: &ScannedProject,
    commit_seq: u64,
) -> Result<(), EngineError> {
    transaction
        .execute(
            r#"
            INSERT INTO catalog_projects (
                project_key, source_instance_id, adapter_id, external_ref,
                native_project_key, display_name, display_path,
                first_seen_commit_seq, last_commit_seq
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
            ON CONFLICT(project_key) DO UPDATE SET
                adapter_id = excluded.adapter_id,
                external_ref = excluded.external_ref,
                native_project_key = excluded.native_project_key,
                display_name = COALESCE(excluded.display_name, catalog_projects.display_name),
                display_path = COALESCE(excluded.display_path, catalog_projects.display_path),
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                project.project_key,
                to_i64(scan.source_instance_id)?,
                scan.adapter_id,
                project.external_ref.as_slice(),
                project.native_project_key,
                project.display_name,
                project.display_path,
                to_i64(commit_seq)?,
            ],
        )
        .map_err(|error| sqlite_error("upsert catalog project", error))?;
    Ok(())
}

fn upsert_session(
    transaction: &Transaction<'_>,
    scan: &SourceScan,
    session: &ScannedSession,
    commit_seq: u64,
) -> Result<(), EngineError> {
    transaction
        .execute(
            r#"
            INSERT INTO catalog_sessions (
                session_key, project_key, source_instance_id, adapter_id, external_ref,
                native_session_key, native_session_id, title,
                association_basis, association_quality, association_provenance,
                native_created_at, native_updated_at, native_message_count,
                transcript_present, transcript_locator, source_size_bytes,
                source_modified_ms, sort_time, first_seen_commit_seq, last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, ?18, ?19, ?20, ?20
            )
            ON CONFLICT(session_key) DO UPDATE SET
                project_key = excluded.project_key,
                adapter_id = excluded.adapter_id,
                external_ref = excluded.external_ref,
                native_session_key = excluded.native_session_key,
                native_session_id = COALESCE(excluded.native_session_id, catalog_sessions.native_session_id),
                title = COALESCE(excluded.title, catalog_sessions.title),
                association_basis = excluded.association_basis,
                association_quality = excluded.association_quality,
                association_provenance = excluded.association_provenance,
                native_created_at = COALESCE(excluded.native_created_at, catalog_sessions.native_created_at),
                native_updated_at = COALESCE(excluded.native_updated_at, catalog_sessions.native_updated_at),
                native_message_count = COALESCE(excluded.native_message_count, catalog_sessions.native_message_count),
                transcript_present = excluded.transcript_present,
                transcript_locator = COALESCE(excluded.transcript_locator, catalog_sessions.transcript_locator),
                source_size_bytes = excluded.source_size_bytes,
                source_modified_ms = excluded.source_modified_ms,
                sort_time = excluded.sort_time,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                session.session_key,
                session.project_key,
                to_i64(scan.source_instance_id)?,
                scan.adapter_id,
                session.external_ref.as_slice(),
                session.native_session_key,
                session.native_session_id,
                session.title,
                session.association_basis,
                session.association_quality,
                session.association_provenance,
                session.native_created_at,
                session.native_updated_at,
                session.native_message_count.map(|count| count as i64),
                i64::from(session.transcript_present),
                session.transcript_locator,
                session.source_size_bytes.map(|size| size as i64),
                session.source_modified_ms,
                session.sort_time,
                to_i64(commit_seq)?,
            ],
        )
        .map_err(|error| sqlite_error("upsert catalog session", error))?;
    Ok(())
}

/// Delete rows this source no longer reports. Only a complete pass may call
/// this: it is the one place where absence is treated as evidence.
fn retract_stale_rows(
    transaction: &Transaction<'_>,
    source_instance_id: u64,
    commit_seq: u64,
) -> Result<u64, EngineError> {
    let sessions = transaction
        .execute(
            "DELETE FROM catalog_sessions WHERE source_instance_id = ?1 AND last_commit_seq < ?2",
            params![to_i64(source_instance_id)?, to_i64(commit_seq)?],
        )
        .map_err(|error| sqlite_error("retract stale catalog sessions", error))?;
    transaction
        .execute(
            "DELETE FROM catalog_projects WHERE source_instance_id = ?1 AND last_commit_seq < ?2",
            params![to_i64(source_instance_id)?, to_i64(commit_seq)?],
        )
        .map_err(|error| sqlite_error("retract stale catalog projects", error))?;
    Ok(u64::try_from(sessions).unwrap_or_default())
}

fn count_for(
    transaction: &Transaction<'_>,
    source_instance_id: u64,
    table: &'static str,
) -> Result<i64, EngineError> {
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE source_instance_id = ?1");
    transaction
        .query_row(&sql, params![to_i64(source_instance_id)?], |row| row.get(0))
        .map_err(|error| sqlite_error("count catalog rows", error))
}

fn to_i64(value: u64) -> Result<i64, EngineError> {
    i64::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "encode catalog identifier",
        detail: "value exceeds the SQLite integer range".to_string(),
    })
}

fn sqlite_error(operation: &'static str, error: rusqlite::Error) -> EngineError {
    EngineError::Sqlite {
        operation,
        detail: error.to_string(),
    }
}
