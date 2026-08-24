//! Connection-level PRAGMA policies for the sole writer.

use rusqlite::Connection;

use super::{
    SchemaError, BOOTSTRAP_CACHE_KIB, BOOTSTRAP_JOURNAL_LIMIT_BYTES, BOOTSTRAP_MMAP_BYTES,
    SQLITE_MMAP_BYTES, WAL_AUTOCHECKPOINT_PAGES, WAL_JOURNAL_LIMIT_BYTES, WRITER_CACHE_KIB,
};

/// Apply the connection-level PRAGMAs for the long-lived sole writer. The
/// cache and checkpoint ownership are explicit so performance does not
/// silently fall back to SQLite's ~2 MiB page cache and hidden 1,000-page
/// checkpoint cadence. The writer actor applies the bounded checkpoint policy.
///
/// Note: on an in-memory connection SQLite refuses WAL and reports
/// `journal_mode = memory`. Tests that need to verify WAL use a file-backed
/// connection.
pub fn set_pragmas(conn: &Connection) -> Result<(), SchemaError> {
    // `pragma_update` handles each PRAGMA as a single statement and ignores
    // the returned row that `journal_mode` produces.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "cache_size", -WRITER_CACHE_KIB)?;
    conn.pragma_update(None, "mmap_size", SQLITE_MMAP_BYTES)?;
    conn.pragma_update(None, "wal_autocheckpoint", WAL_AUTOCHECKPOINT_PAGES)?;
    conn.pragma_update(None, "journal_size_limit", WAL_JOURNAL_LIMIT_BYTES)?;
    Ok(())
}

/// Larger cache/mmap/WAL limits used only while the owner is in durable
/// query-bootstrap ingest or index finalization. `set_pragmas` restores the
/// interactive policy before readers start.
pub fn set_bootstrap_ingest_pragmas(conn: &Connection) -> Result<(), SchemaError> {
    // The cold builder is the sole trusted writer and admits no readers. Avoid
    // millions of repeated parent lookups while inserting; finalization runs
    // a complete foreign_key_check before it can clear the durable readiness
    // marker, and set_pragmas restores immediate enforcement before readers or
    // live commits are admitted.
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    // In WAL mode NORMAL's only fsyncs are the checkpoints, and on the full
    // corpus those measured 40% of cold-build wall time (fsync 73% of the
    // checkpoint windows; OFF cut the build 247.9 s → 173.4 s, +43% rec/s).
    // The database is a pure function of the source files: a power loss during
    // the build can corrupt this file, and the answer is the same rebuild the
    // interruption already forces — finalization's integrity check gates the
    // marker, and `set_pragmas` restores NORMAL before readers or live
    // commits are admitted.
    conn.pragma_update(None, "synchronous", "OFF")?;
    conn.pragma_update(None, "cache_size", -BOOTSTRAP_CACHE_KIB)?;
    conn.pragma_update(None, "mmap_size", BOOTSTRAP_MMAP_BYTES)?;
    conn.pragma_update(None, "journal_size_limit", BOOTSTRAP_JOURNAL_LIMIT_BYTES)?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    Ok(())
}
