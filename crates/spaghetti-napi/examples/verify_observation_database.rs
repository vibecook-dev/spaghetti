//! Offline integrity verifier for an observation database.
//!
//! Run only after the sole engine owner has disposed. FTS5 integrity commands
//! are special INSERT statements, so the verifier opens the database
//! read-write but executes every FTS check inside an explicitly rolled-back
//! transaction.

use std::env;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OpenFlags};

const FTS_TABLES: &[&str] = &[
    "search_fts",
    "subagent_search_fts",
    "canonical_message_search_fts",
];

fn main() -> Result<()> {
    let mut arguments = env::args_os().skip(1);
    let database_path = arguments.next().context(
        "usage: cargo run -p spaghetti-napi --example verify_observation_database -- <database> [--repair-canonical-fts]",
    )?;
    let repair_canonical_fts = match arguments.next() {
        None => false,
        Some(flag) if flag == "--repair-canonical-fts" => true,
        Some(flag) => bail!("unknown option: {}", flag.to_string_lossy()),
    };
    if let Some(extra) = arguments.next() {
        bail!("unexpected argument: {}", extra.to_string_lossy());
    }
    let database_path = Path::new(&database_path);
    if !database_path.is_file() {
        bail!("database does not exist: {}", database_path.display());
    }

    let mut connection = Connection::open_with_flags(
        database_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("open database: {}", database_path.display()))?;
    connection.busy_timeout(Duration::from_secs(5))?;

    let sqlite_version: String =
        connection.query_row("SELECT sqlite_version()", [], |row| row.get(0))?;
    println!("sqlite_version={sqlite_version}");

    if repair_canonical_fts {
        connection.execute_batch(
            "INSERT INTO canonical_message_search_fts(canonical_message_search_fts) VALUES('rebuild')",
        )?;
        println!("fts.canonical_message_search_fts.rebuilt=true");
    }

    let quick_check = connection
        .prepare("PRAGMA quick_check")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if quick_check.as_slice() != ["ok"] {
        bail!("quick_check failed: {quick_check:?}");
    }
    println!("quick_check=ok");

    let foreign_key_violations = {
        let mut statement = connection.prepare("PRAGMA foreign_key_check")?;
        let mut rows = statement.query([])?;
        let mut count = 0_u64;
        while rows.next()?.is_some() {
            count = count.saturating_add(1);
        }
        count
    };
    println!("foreign_key_violations={foreign_key_violations}");
    if foreign_key_violations != 0 {
        bail!("foreign_key_check found {foreign_key_violations} violations");
    }

    let mut failed = false;
    for table in FTS_TABLES {
        for content_check in [false, true] {
            let kind = if content_check { "content" } else { "internal" };
            match check_fts(&mut connection, table, content_check) {
                Ok(()) => println!("fts.{table}.{kind}=ok"),
                Err(error) => {
                    failed = true;
                    eprintln!("fts.{table}.{kind}=failed: {error:#}");
                    if content_check {
                        match check_fts_after_rebuild(&mut connection, table) {
                            Ok(()) => eprintln!("fts.{table}.content_after_rebuild=ok"),
                            Err(rebuild_error) => eprintln!(
                                "fts.{table}.content_after_rebuild=failed: {rebuild_error:#}"
                            ),
                        }
                    }
                }
            }
        }
    }
    if failed {
        bail!("one or more FTS5 integrity checks failed");
    }
    Ok(())
}

fn check_fts(connection: &mut Connection, table: &str, content_check: bool) -> Result<()> {
    let transaction = connection.transaction()?;
    let sql = if content_check {
        format!("INSERT INTO {table}({table}, rank) VALUES('integrity-check', 1)")
    } else {
        format!("INSERT INTO {table}({table}) VALUES('integrity-check')")
    };
    let result = transaction
        .execute_batch(&sql)
        .with_context(|| format!("run FTS5 integrity check for {table}"));
    transaction.rollback()?;
    result
}

fn check_fts_after_rebuild(connection: &mut Connection, table: &str) -> Result<()> {
    let transaction = connection.transaction()?;
    let rebuild = format!("INSERT INTO {table}({table}) VALUES('rebuild')");
    let integrity = format!("INSERT INTO {table}({table}, rank) VALUES('integrity-check', 1)");
    let result = transaction
        .execute_batch(&rebuild)
        .with_context(|| format!("rebuild FTS5 index {table}"))
        .and_then(|()| {
            transaction
                .execute_batch(&integrity)
                .with_context(|| format!("check rebuilt FTS5 index {table}"))
        });
    transaction.rollback()?;
    result
}
