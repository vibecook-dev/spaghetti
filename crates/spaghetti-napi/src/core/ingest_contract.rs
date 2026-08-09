//! Per-source ingest-contract marker (RFC 008 Phase 1.3).
//!
//! Rust counterpart of `packages/sdk/src/data/ingest-contract.ts`. Records
//! which version of the Rust bulk-ingest contract last completed for a source,
//! so a warm run can tell "unchanged since a build that was correct" from
//! "unchanged since a build that predates a fix".
//!
//! Historical builds can leave rows no fingerprint diff reveals — a
//! parent-less sidecar written before a project rolled back, for instance.
//! Those never surface from an ordinary file change, so the version bump is the
//! only thing that forces them out.
//!
//! Stored in `source_materializations` under a per-source projection key. A
//! global key would be wrong: repairing Claude Code must not re-ingest Codex.

use rusqlite::{params, Connection};

/// Projection key. Per-source by construction — the table's PK is
/// `(source_id, projection)`.
///
/// Keep in sync with `RUST_INGEST_CONTRACT` in `ingest-contract.ts`.
pub const RUST_INGEST_CONTRACT: &str = "rust-ingest-contract";

/// Bump when a Rust ingest fix leaves previously-written rows wrong in a way
/// fingerprints cannot detect. A bump forces one full clear-and-reingest for
/// every source whose stored version is older.
///
/// Keep in sync with `RUST_INGEST_CONTRACT_VERSION` in `ingest-contract.ts`.
pub const RUST_INGEST_CONTRACT_VERSION: u32 = 1;

/// True when this source completed under the current contract version.
///
/// Deliberately strict: an older *or* absent version both read as "not
/// current", so the safe answer on unknown state is to do the work again.
pub fn is_source_contract_current(conn: &Connection, source_id: &str) -> rusqlite::Result<bool> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM source_materializations \
             WHERE source_id = ?1 AND projection = ?2 AND version = ?3",
            params![
                source_id,
                RUST_INGEST_CONTRACT,
                RUST_INGEST_CONTRACT_VERSION
            ],
            |row| row.get(0),
        )
        .ok();
    Ok(found.is_some())
}

/// Publish the current contract version for one source.
///
/// **Success-last.** Call this only after entity writes, derived rebuilds, and
/// fingerprint publication have all finished without an omitted-fingerprint
/// error. Publishing early is the one way to make a failed repair look
/// complete, which the next warm run would then skip.
pub fn mark_source_contract_current(conn: &Connection, source_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO source_materializations(source_id, projection, version, completed_at) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(source_id, projection) DO UPDATE SET \
           version = excluded.version, \
           completed_at = excluded.completed_at",
        params![
            source_id,
            RUST_INGEST_CONTRACT,
            RUST_INGEST_CONTRACT_VERSION,
            now_ms()
        ],
    )?;
    Ok(())
}

/// Drop the marker for one source.
///
/// Runs inside the atomic source clear, so a clear that rolls back cannot leave
/// a source looking repaired.
pub fn invalidate_source_contract(conn: &Connection, source_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM source_materializations WHERE source_id = ?1 AND projection = ?2",
        params![source_id, RUST_INGEST_CONTRACT],
    )?;
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().expect("open mem db");
        conn.execute_batch(
            "CREATE TABLE source_materializations (\
               source_id TEXT NOT NULL,\
               projection TEXT NOT NULL,\
               version INTEGER NOT NULL,\
               completed_at INTEGER NOT NULL,\
               PRIMARY KEY (source_id, projection)\
             )",
        )
        .expect("create table");
        conn
    }

    #[test]
    fn absent_marker_is_not_current() {
        let conn = db();
        assert!(!is_source_contract_current(&conn, "claude-code").unwrap());
    }

    #[test]
    fn marks_and_reads_back_one_source() {
        let conn = db();
        mark_source_contract_current(&conn, "claude-code").unwrap();

        assert!(is_source_contract_current(&conn, "claude-code").unwrap());
        // Per-source: repairing Claude must not mark Codex repaired.
        assert!(!is_source_contract_current(&conn, "codex").unwrap());
    }

    #[test]
    fn stale_version_is_not_current() {
        let conn = db();
        conn.execute(
            "INSERT INTO source_materializations(source_id, projection, version, completed_at) \
             VALUES (?1, ?2, ?3, 0)",
            params![
                "claude-code",
                RUST_INGEST_CONTRACT,
                RUST_INGEST_CONTRACT_VERSION - 1
            ],
        )
        .unwrap();

        assert!(!is_source_contract_current(&conn, "claude-code").unwrap());
    }

    #[test]
    fn invalidation_drops_only_the_named_source() {
        let conn = db();
        mark_source_contract_current(&conn, "claude-code").unwrap();
        mark_source_contract_current(&conn, "codex").unwrap();

        invalidate_source_contract(&conn, "claude-code").unwrap();

        assert!(!is_source_contract_current(&conn, "claude-code").unwrap());
        assert!(is_source_contract_current(&conn, "codex").unwrap());
    }

    #[test]
    fn token_activity_rows_do_not_satisfy_the_ingest_contract() {
        let conn = db();
        conn.execute(
            "INSERT INTO source_materializations(source_id, projection, version, completed_at) \
             VALUES ('claude-code', 'token-activity', 1, 0)",
            [],
        )
        .unwrap();

        assert!(!is_source_contract_current(&conn, "claude-code").unwrap());
    }
}
