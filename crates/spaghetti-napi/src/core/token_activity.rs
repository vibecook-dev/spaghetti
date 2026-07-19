//! Ingestion-owned token rollups.
//!
//! Cold/warm native ingest already streams canonical rows into SQLite. Before
//! releasing its exclusive connection, this module performs one source-scoped
//! native pass and commits compact session/day and project/day summaries.
//! Renderer/query paths never invoke this work.

use std::collections::HashMap;

use rusqlite::{params, Connection};

pub const MATERIALIZATION_NAME: &str = "token-activity";
pub const MATERIALIZATION_VERSION: i64 = 1;

#[derive(Debug, Default)]
struct SessionDayBucket {
    project_slug: String,
    session_id: String,
    activity_day: String,
    input_tokens: i64,
    output_tokens: i64,
    cache_creation_tokens: i64,
    cache_read_tokens: i64,
    total_tokens: i64,
    exact_tokens: i64,
    estimated_tokens: i64,
    message_count: i64,
    parent_message_count: i64,
}

#[derive(Debug, Default)]
struct ProjectDayBucket {
    input_tokens: i64,
    output_tokens: i64,
    cache_creation_tokens: i64,
    cache_read_tokens: i64,
    total_tokens: i64,
    exact_tokens: i64,
    estimated_tokens: i64,
    message_count: i64,
    parent_message_count: i64,
    session_count: i64,
}

#[derive(Debug, Default)]
struct SessionTotalBucket {
    project_slug: String,
    session_id: String,
    input_tokens: i64,
    output_tokens: i64,
    cache_creation_tokens: i64,
    cache_read_tokens: i64,
    parent_message_count: i64,
}

#[derive(Debug)]
struct TokenRow {
    project_slug: String,
    session_id: String,
    activity_day: Option<String>,
    input_tokens: i64,
    output_tokens: i64,
    cache_creation_tokens: i64,
    cache_read_tokens: i64,
    estimated: bool,
}

fn collect_table(
    conn: &Connection,
    source_id: &str,
    table: &str,
    parent_message: bool,
    buckets: &mut HashMap<(String, String, String), SessionDayBucket>,
    totals: &mut HashMap<(String, String), SessionTotalBucket>,
) -> rusqlite::Result<()> {
    let sql = format!(
        r#"
        SELECT r.project_slug, r.session_id, substr(r.timestamp, 1, 10),
               COALESCE(r.input_tokens, 0), COALESCE(r.output_tokens, 0),
               COALESCE(r.cache_creation_tokens, 0), COALESCE(r.cache_read_tokens, 0),
               COALESCE(s.tokens_estimated, 0)
          FROM {table} r
          LEFT JOIN sessions s ON s.id = r.session_id AND s.source_id = r.source_id
         WHERE r.source_id = ?1
           AND r.project_slug IS NOT NULL
           AND r.session_id IS NOT NULL
        "#,
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([source_id], |row| {
        Ok(TokenRow {
            project_slug: row.get(0)?,
            session_id: row.get(1)?,
            activity_day: row.get(2)?,
            input_tokens: row.get(3)?,
            output_tokens: row.get(4)?,
            cache_creation_tokens: row.get(5)?,
            cache_read_tokens: row.get(6)?,
            estimated: row.get::<_, i64>(7)? != 0,
        })
    })?;

    for row in rows {
        let row = row?;
        let total_key = (row.project_slug.clone(), row.session_id.clone());
        let total = totals
            .entry(total_key)
            .or_insert_with(|| SessionTotalBucket {
                project_slug: row.project_slug.clone(),
                session_id: row.session_id.clone(),
                ..SessionTotalBucket::default()
            });
        total.input_tokens = total.input_tokens.saturating_add(row.input_tokens);
        total.output_tokens = total.output_tokens.saturating_add(row.output_tokens);
        total.cache_creation_tokens = total
            .cache_creation_tokens
            .saturating_add(row.cache_creation_tokens);
        total.cache_read_tokens = total
            .cache_read_tokens
            .saturating_add(row.cache_read_tokens);
        if parent_message {
            total.parent_message_count = total.parent_message_count.saturating_add(1);
        }

        let Some(activity_day) = row.activity_day.filter(|day| day.len() >= 10) else {
            continue;
        };
        let key = (
            row.project_slug.clone(),
            row.session_id.clone(),
            activity_day.clone(),
        );
        let bucket = buckets.entry(key).or_insert_with(|| SessionDayBucket {
            project_slug: row.project_slug,
            session_id: row.session_id,
            activity_day,
            ..SessionDayBucket::default()
        });
        let normalized = if source_id == "codex" {
            row.input_tokens.saturating_add(row.output_tokens)
        } else {
            row.input_tokens
                .saturating_add(row.output_tokens)
                .saturating_add(row.cache_creation_tokens)
                .saturating_add(row.cache_read_tokens)
        };
        bucket.input_tokens = bucket.input_tokens.saturating_add(row.input_tokens);
        bucket.output_tokens = bucket.output_tokens.saturating_add(row.output_tokens);
        bucket.cache_creation_tokens = bucket
            .cache_creation_tokens
            .saturating_add(row.cache_creation_tokens);
        bucket.cache_read_tokens = bucket
            .cache_read_tokens
            .saturating_add(row.cache_read_tokens);
        bucket.total_tokens = bucket.total_tokens.saturating_add(normalized);
        if row.estimated {
            bucket.estimated_tokens = bucket.estimated_tokens.saturating_add(normalized);
        } else {
            bucket.exact_tokens = bucket.exact_tokens.saturating_add(normalized);
        }
        bucket.message_count = bucket.message_count.saturating_add(1);
        if parent_message {
            bucket.parent_message_count = bucket.parent_message_count.saturating_add(1);
        }
    }
    Ok(())
}

pub fn is_materialized(conn: &Connection, source_id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM source_materializations WHERE source_id = ?1 AND projection = ?2 AND version = ?3)",
        params![source_id, MATERIALIZATION_NAME, MATERIALIZATION_VERSION],
        |row| row.get(0),
    )
}

pub fn invalidate_materialization(conn: &Connection, source_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM source_materializations WHERE source_id = ?1 AND projection = ?2",
        params![source_id, MATERIALIZATION_NAME],
    )?;
    Ok(())
}

/// Rebuild compact rollups for one source before the native writer closes.
pub fn rebuild_source(conn: &Connection, source_id: &str) -> rusqlite::Result<usize> {
    let mut sessions = HashMap::new();
    let mut totals = HashMap::new();
    collect_table(
        conn,
        source_id,
        "messages",
        true,
        &mut sessions,
        &mut totals,
    )?;
    collect_table(
        conn,
        source_id,
        "subagent_messages",
        false,
        &mut sessions,
        &mut totals,
    )?;

    let mut projects: HashMap<(String, String), ProjectDayBucket> = HashMap::new();
    for bucket in sessions.values() {
        let project = projects
            .entry((bucket.project_slug.clone(), bucket.activity_day.clone()))
            .or_default();
        project.input_tokens = project.input_tokens.saturating_add(bucket.input_tokens);
        project.output_tokens = project.output_tokens.saturating_add(bucket.output_tokens);
        project.cache_creation_tokens = project
            .cache_creation_tokens
            .saturating_add(bucket.cache_creation_tokens);
        project.cache_read_tokens = project
            .cache_read_tokens
            .saturating_add(bucket.cache_read_tokens);
        project.total_tokens = project.total_tokens.saturating_add(bucket.total_tokens);
        project.exact_tokens = project.exact_tokens.saturating_add(bucket.exact_tokens);
        project.estimated_tokens = project
            .estimated_tokens
            .saturating_add(bucket.estimated_tokens);
        project.message_count = project.message_count.saturating_add(bucket.message_count);
        project.parent_message_count = project
            .parent_message_count
            .saturating_add(bucket.parent_message_count);
        project.session_count = project.session_count.saturating_add(1);
    }

    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> rusqlite::Result<()> {
        conn.execute(
            "DELETE FROM token_activity_daily WHERE source_id = ?1",
            [source_id],
        )?;
        conn.execute(
            "DELETE FROM token_activity_session_daily WHERE source_id = ?1",
            [source_id],
        )?;
        conn.execute(
            "DELETE FROM session_summary_totals WHERE source_id = ?1",
            [source_id],
        )?;
        {
            let mut insert = conn.prepare_cached(
                r#"
                INSERT INTO session_summary_totals
                  (source_id, project_slug, session_id, input_tokens, output_tokens,
                   cache_creation_tokens, cache_read_tokens, parent_message_count)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                "#,
            )?;
            for bucket in totals.values() {
                insert.execute(params![
                    source_id,
                    bucket.project_slug,
                    bucket.session_id,
                    bucket.input_tokens,
                    bucket.output_tokens,
                    bucket.cache_creation_tokens,
                    bucket.cache_read_tokens,
                    bucket.parent_message_count,
                ])?;
            }
        }
        {
            let mut insert = conn.prepare_cached(
                r#"
                INSERT INTO token_activity_session_daily
                  (source_id, project_slug, session_id, activity_day,
                   input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
                   total_tokens, exact_tokens, estimated_tokens, message_count, parent_message_count)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                "#,
            )?;
            for bucket in sessions.values() {
                insert.execute(params![
                    source_id,
                    bucket.project_slug,
                    bucket.session_id,
                    bucket.activity_day,
                    bucket.input_tokens,
                    bucket.output_tokens,
                    bucket.cache_creation_tokens,
                    bucket.cache_read_tokens,
                    bucket.total_tokens,
                    bucket.exact_tokens,
                    bucket.estimated_tokens,
                    bucket.message_count,
                    bucket.parent_message_count,
                ])?;
            }
        }
        {
            let mut insert = conn.prepare_cached(
                r#"
                INSERT INTO token_activity_daily
                  (source_id, project_slug, activity_day,
                   input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
                   total_tokens, exact_tokens, estimated_tokens, message_count,
                   parent_message_count, session_count)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                "#,
            )?;
            for ((project_slug, activity_day), bucket) in &projects {
                insert.execute(params![
                    source_id,
                    project_slug,
                    activity_day,
                    bucket.input_tokens,
                    bucket.output_tokens,
                    bucket.cache_creation_tokens,
                    bucket.cache_read_tokens,
                    bucket.total_tokens,
                    bucket.exact_tokens,
                    bucket.estimated_tokens,
                    bucket.message_count,
                    bucket.parent_message_count,
                    bucket.session_count,
                ])?;
            }
        }
        conn.execute(
            "DELETE FROM token_activity_dirty WHERE source_id = ?1",
            [source_id],
        )?;
        conn.execute(
            "DELETE FROM session_summary_dirty WHERE source_id = ?1",
            [source_id],
        )?;
        conn.execute(
            r#"
            INSERT INTO source_materializations(source_id, projection, version, completed_at)
            VALUES (?1, ?2, ?3, CAST(unixepoch('subsec') * 1000 AS INTEGER))
            ON CONFLICT(source_id, projection) DO UPDATE SET
              version = excluded.version,
              completed_at = excluded.completed_at
            "#,
            params![source_id, MATERIALIZATION_NAME, MATERIALIZATION_VERSION],
        )?;
        Ok(())
    })();

    match result {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error);
        }
    }
    Ok(projects.len())
}

/// Refresh live-write dirty keys inside the caller's existing transaction.
pub fn rebuild_dirty_in_transaction(conn: &Connection, source_id: &str) -> rusqlite::Result<usize> {
    let dirty = {
        let mut stmt = conn.prepare(
            "SELECT project_slug, activity_day FROM token_activity_dirty WHERE source_id = ?1",
        )?;
        let rows = stmt.query_map([source_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    let dirty_sessions = {
        let mut stmt = conn.prepare(
            "SELECT project_slug, session_id FROM session_summary_dirty WHERE source_id = ?1",
        )?;
        let rows = stmt.query_map([source_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    for (project_slug, session_id) in &dirty_sessions {
        conn.execute(
            "DELETE FROM session_summary_totals WHERE source_id = ?1 AND session_id = ?2",
            params![source_id, session_id],
        )?;
        conn.execute(
            r#"
            INSERT INTO session_summary_totals
              (source_id, project_slug, session_id, input_tokens, output_tokens,
               cache_creation_tokens, cache_read_tokens, parent_message_count)
            WITH token_rows AS (
              SELECT source_id, project_slug, session_id,
                     COALESCE(input_tokens, 0) AS input_tokens,
                     COALESCE(output_tokens, 0) AS output_tokens,
                     COALESCE(cache_creation_tokens, 0) AS cache_creation_tokens,
                     COALESCE(cache_read_tokens, 0) AS cache_read_tokens,
                     1 AS parent_message
                FROM messages
               WHERE source_id = ?1 AND project_slug = ?2 AND session_id = ?3
              UNION ALL
              SELECT source_id, project_slug, session_id,
                     COALESCE(input_tokens, 0), COALESCE(output_tokens, 0),
                     COALESCE(cache_creation_tokens, 0), COALESCE(cache_read_tokens, 0), 0
                FROM subagent_messages
               WHERE source_id = ?1 AND project_slug = ?2 AND session_id = ?3
            )
            SELECT source_id, project_slug, session_id,
                   SUM(input_tokens), SUM(output_tokens),
                   SUM(cache_creation_tokens), SUM(cache_read_tokens),
                   SUM(parent_message)
              FROM token_rows
             GROUP BY source_id, project_slug, session_id
            "#,
            params![source_id, project_slug, session_id],
        )?;
        conn.execute(
            "DELETE FROM session_summary_dirty WHERE source_id = ?1 AND session_id = ?2",
            params![source_id, session_id],
        )?;
    }

    for (project_slug, activity_day) in &dirty {
        conn.execute(
            "DELETE FROM token_activity_session_daily WHERE source_id = ?1 AND project_slug = ?2 AND activity_day = ?3",
            params![source_id, project_slug, activity_day],
        )?;
        conn.execute(
            r#"
            INSERT INTO token_activity_session_daily
              (source_id, project_slug, session_id, activity_day,
               input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
               total_tokens, exact_tokens, estimated_tokens, message_count, parent_message_count)
            WITH token_rows AS (
              SELECT source_id, project_slug, session_id, timestamp,
                     COALESCE(input_tokens, 0) AS input_tokens,
                     COALESCE(output_tokens, 0) AS output_tokens,
                     COALESCE(cache_creation_tokens, 0) AS cache_creation_tokens,
                     COALESCE(cache_read_tokens, 0) AS cache_read_tokens,
                     1 AS parent_message
                FROM messages
               WHERE source_id = ?1 AND project_slug = ?2 AND substr(timestamp, 1, 10) = ?3
              UNION ALL
              SELECT source_id, project_slug, session_id, timestamp,
                     COALESCE(input_tokens, 0), COALESCE(output_tokens, 0),
                     COALESCE(cache_creation_tokens, 0), COALESCE(cache_read_tokens, 0), 0
                FROM subagent_messages
               WHERE source_id = ?1 AND project_slug = ?2 AND substr(timestamp, 1, 10) = ?3
            )
            SELECT r.source_id, r.project_slug, r.session_id, substr(r.timestamp, 1, 10),
                   SUM(r.input_tokens), SUM(r.output_tokens),
                   SUM(r.cache_creation_tokens), SUM(r.cache_read_tokens),
                   SUM(CASE WHEN r.source_id = 'codex'
                            THEN r.input_tokens + r.output_tokens
                            ELSE r.input_tokens + r.output_tokens + r.cache_creation_tokens + r.cache_read_tokens END),
                   SUM(CASE WHEN COALESCE(s.tokens_estimated, 0) = 0
                            THEN CASE WHEN r.source_id = 'codex'
                                      THEN r.input_tokens + r.output_tokens
                                      ELSE r.input_tokens + r.output_tokens + r.cache_creation_tokens + r.cache_read_tokens END
                            ELSE 0 END),
                   SUM(CASE WHEN COALESCE(s.tokens_estimated, 0) = 1
                            THEN CASE WHEN r.source_id = 'codex'
                                      THEN r.input_tokens + r.output_tokens
                                      ELSE r.input_tokens + r.output_tokens + r.cache_creation_tokens + r.cache_read_tokens END
                            ELSE 0 END),
                   COUNT(*), SUM(r.parent_message)
              FROM token_rows r
              LEFT JOIN sessions s ON s.id = r.session_id AND s.source_id = r.source_id
             GROUP BY r.source_id, r.project_slug, r.session_id, substr(r.timestamp, 1, 10)
            "#,
            params![source_id, project_slug, activity_day],
        )?;
        conn.execute(
            "DELETE FROM token_activity_daily WHERE source_id = ?1 AND project_slug = ?2 AND activity_day = ?3",
            params![source_id, project_slug, activity_day],
        )?;
        conn.execute(
            r#"
            INSERT INTO token_activity_daily
              (source_id, project_slug, activity_day,
               input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
               total_tokens, exact_tokens, estimated_tokens, message_count,
               parent_message_count, session_count)
            SELECT source_id, project_slug, activity_day,
                   SUM(input_tokens), SUM(output_tokens), SUM(cache_creation_tokens), SUM(cache_read_tokens),
                   SUM(total_tokens), SUM(exact_tokens), SUM(estimated_tokens),
                   SUM(message_count), SUM(parent_message_count), COUNT(*)
              FROM token_activity_session_daily
             WHERE source_id = ?1 AND project_slug = ?2 AND activity_day = ?3
             GROUP BY source_id, project_slug, activity_day
            "#,
            params![source_id, project_slug, activity_day],
        )?;
        conn.execute(
            "DELETE FROM token_activity_dirty WHERE source_id = ?1 AND project_slug = ?2 AND activity_day = ?3",
            params![source_id, project_slug, activity_day],
        )?;
    }
    Ok(dirty.len().saturating_add(dirty_sessions.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::schema;

    fn seeded() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        schema::initialize_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO sessions(id, source_id, project_slug, tokens_estimated) VALUES ('s1', 'claude-code', 'p1', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages(source_id, project_slug, session_id, msg_index, timestamp, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, data)
             VALUES ('claude-code', 'p1', 's1', 0, '2026-07-19T01:00:00Z', 10, 2, 3, 5, '{}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO subagent_messages(source_id, project_slug, session_id, workflow_id, agent_id, msg_index, timestamp, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, data)
             VALUES ('claude-code', 'p1', 's1', '', 'a1', 0, '2026-07-19T01:01:00Z', 4, 6, 1, 2, '{}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages(source_id, project_slug, session_id, msg_index, timestamp, input_tokens, data)
             VALUES ('claude-code', 'p1', 's1', 1, NULL, 7, '{}')",
            [],
        )
        .unwrap();
        conn
    }

    #[test]
    fn full_source_rebuild_materializes_both_rollup_levels() {
        let conn = seeded();
        assert_eq!(rebuild_source(&conn, "claude-code").unwrap(), 1);
        let project: (i64, i64, i64) = conn
            .query_row(
                "SELECT total_tokens, message_count, parent_message_count FROM token_activity_daily",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(project, (33, 2, 1));
        let session: (i64, i64) = conn
            .query_row(
                "SELECT total_tokens, parent_message_count FROM token_activity_session_daily",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(session, (33, 1));
        let summary: (i64, i64) = conn
            .query_row(
                "SELECT input_tokens + output_tokens + cache_creation_tokens + cache_read_tokens, parent_message_count FROM session_summary_totals",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(summary, (40, 2));
        assert!(is_materialized(&conn, "claude-code").unwrap());
        let dirty: i64 = conn
            .query_row("SELECT COUNT(*) FROM token_activity_dirty", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(dirty, 0);
    }

    #[test]
    fn completion_marker_can_force_warm_repair() {
        let conn = seeded();
        rebuild_source(&conn, "claude-code").unwrap();
        assert!(is_materialized(&conn, "claude-code").unwrap());
        invalidate_materialization(&conn, "claude-code").unwrap();
        assert!(!is_materialized(&conn, "claude-code").unwrap());
    }

    #[test]
    fn live_rebuild_refreshes_only_dirty_days_inside_the_write_transaction() {
        let conn = seeded();
        rebuild_source(&conn, "claude-code").unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        conn.execute(
            "UPDATE messages SET input_tokens = 20, timestamp = '2026-07-20T01:00:00Z' WHERE session_id = 's1' AND msg_index = 0",
            [],
        )
        .unwrap();
        assert_eq!(
            rebuild_dirty_in_transaction(&conn, "claude-code").unwrap(),
            3
        );
        conn.execute_batch("COMMIT").unwrap();

        let days: Vec<(String, i64)> = conn
            .prepare(
                "SELECT activity_day, total_tokens FROM token_activity_daily ORDER BY activity_day",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            days,
            vec![("2026-07-19".into(), 13), ("2026-07-20".into(), 30)]
        );
        let summary: i64 = conn
            .query_row(
                "SELECT input_tokens + output_tokens + cache_creation_tokens + cache_read_tokens FROM session_summary_totals",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(summary, 50);
        let dirty: i64 = conn
            .query_row("SELECT COUNT(*) FROM token_activity_dirty", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(dirty, 0);
    }
}
