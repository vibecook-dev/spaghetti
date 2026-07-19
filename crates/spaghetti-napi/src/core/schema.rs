//! SQLite schema + migrations — ported from `packages/sdk/src/data/schema.ts`.
//!
//! This module owns:
//! - The full DDL for the Phase 3 dedicated-table schema (core entities,
//!   indexes, the `search_fts` FTS5 virtual table + content-synced triggers).
//! - The `SCHEMA_VERSION` constant and `schema_meta`-based version tracking.
//! - [`initialize_schema`] which creates the schema on a fresh database or
//!   wipes and rebuilds if the stored version is missing or stale.
//! - [`set_pragmas`] which applies the same connection-level PRAGMAs the TS
//!   [`SqliteService`](../../../../packages/sdk/src/io/sqlite-service.ts) sets
//!   on open.
//!
//! Bumping [`SCHEMA_VERSION`] forces a wipe-and-rebuild on the next warm
//! start. RFC 003 explicitly calls for wipe-on-stale rather than incremental
//! migrations, so this module mirrors that behaviour exactly.

use rusqlite::Connection;
use thiserror::Error;

/// The current schema version. Bumping this forces a wipe-and-rebuild of all
/// tables on the next call to [`initialize_schema`].
///
/// Keep in sync with `SCHEMA_VERSION` in `packages/sdk/src/data/schema.ts`.
/// v5: `source_id` on source_files/projects/sessions/messages (default
/// 'claude-code'). Writers omit the column, so the default applies and both
/// engines produce identical rows — parity stays green with no writer change.
/// v6: `projects` PK is composite `(source_id, slug)` — a slug is the encoded
/// cwd, so two sources on the same directory would collide on slug alone. The
/// Rust writer still omits `source_id` (default 'claude-code'), so claude-only
/// parity is unaffected; the conflict target just moves to `(source_id, slug)`.
/// v7: `sessions.tokens_estimated` — optional local estimate flag (TS Codex
/// path); Rust writer leaves the DEFAULT 0.
/// v8: normalized timeline projection + dirty-session triggers. Rust only
/// writes canonical messages; the shared TS query layer materializes display
/// rows lazily, keeping all source adapters in one implementation.
/// v9: source-aware, row-normalized subagent transcripts plus a lazy display
/// projection and FTS index. Rust writes canonical subagent message rows; the
/// shared TS query layer materializes display rows lazily.
/// v10: incrementally maintained main-session timeline projection. Each
/// normalized row retains its canonical raw index, and tool results are kept
/// in a small association table so later results can update earlier tool-use
/// rows without rebuilding the whole transcript.
/// v11: tokenized subagent rows and rebuildable daily token-activity buckets.
pub const SCHEMA_VERSION: u32 = 11;

/// Full DDL for the current schema — lifted verbatim from the TS `SCHEMA_SQL`
/// template literal. Whitespace differs; structure does not.
const SCHEMA_SQL: &str = r#"
-- Meta
CREATE TABLE IF NOT EXISTS schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

-- Source file tracking
CREATE TABLE IF NOT EXISTS source_files (
  path TEXT PRIMARY KEY,
  source_id TEXT NOT NULL DEFAULT 'claude-code',
  mtime_ms REAL,
  size INTEGER,
  byte_position INTEGER,
  category TEXT,
  project_slug TEXT,
  session_id TEXT
);

-- Core entities
CREATE TABLE IF NOT EXISTS projects (
  slug TEXT NOT NULL,
  source_id TEXT NOT NULL DEFAULT 'claude-code',
  original_path TEXT,
  sessions_index TEXT,
  updated_at INTEGER,
  PRIMARY KEY (source_id, slug)
);

CREATE TABLE IF NOT EXISTS project_memories (
  project_slug TEXT PRIMARY KEY,
  content TEXT,
  updated_at INTEGER
);

CREATE TABLE IF NOT EXISTS sessions (
  id TEXT PRIMARY KEY,
  source_id TEXT NOT NULL DEFAULT 'claude-code',
  project_slug TEXT,
  full_path TEXT,
  first_prompt TEXT,
  summary TEXT,
  git_branch TEXT,
  project_path TEXT,
  is_sidechain INTEGER,
  created_at TEXT,
  modified_at TEXT,
  file_mtime REAL,
  plan_slug TEXT,
  has_task INTEGER,
  updated_at INTEGER,
  tokens_estimated INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source_id TEXT NOT NULL DEFAULT 'claude-code',
  project_slug TEXT,
  session_id TEXT,
  msg_index INTEGER,
  msg_type TEXT,
  uuid TEXT,
  timestamp TEXT,
  data TEXT,
  input_tokens INTEGER DEFAULT 0,
  output_tokens INTEGER DEFAULT 0,
  cache_creation_tokens INTEGER DEFAULT 0,
  cache_read_tokens INTEGER DEFAULT 0,
  text_content TEXT DEFAULT '',
  byte_offset INTEGER,
  UNIQUE(session_id, msg_index)
);

CREATE TABLE IF NOT EXISTS timeline_messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source_id TEXT NOT NULL,
  project_slug TEXT NOT NULL,
  session_id TEXT NOT NULL,
  raw_index INTEGER NOT NULL,
  timeline_index INTEGER NOT NULL,
  display_type TEXT NOT NULL,
  tool_name TEXT,
  tool_use_id TEXT,
  search_text TEXT NOT NULL DEFAULT '',
  data TEXT NOT NULL,
  UNIQUE(session_id, timeline_index)
);

CREATE TABLE IF NOT EXISTS timeline_tool_results (
  session_id TEXT NOT NULL,
  raw_index INTEGER NOT NULL,
  tool_use_id TEXT NOT NULL,
  result_data TEXT NOT NULL,
  PRIMARY KEY(session_id, raw_index, tool_use_id)
);

CREATE TABLE IF NOT EXISTS timeline_dirty_sessions (
  session_id TEXT PRIMARY KEY,
  source_id TEXT NOT NULL,
  project_slug TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS subagents (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source_id TEXT NOT NULL,
  project_slug TEXT NOT NULL,
  session_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  agent_type TEXT NOT NULL,
  file_name TEXT NOT NULL,
  message_count INTEGER NOT NULL,
  workflow_id TEXT NOT NULL DEFAULT '',
  spawn_tool_id TEXT,
  link_method TEXT NOT NULL DEFAULT 'unlinked',
  updated_at INTEGER,
  UNIQUE(source_id, project_slug, session_id, workflow_id, agent_id)
);

CREATE TABLE IF NOT EXISTS subagent_messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source_id TEXT NOT NULL,
  project_slug TEXT NOT NULL,
  session_id TEXT NOT NULL,
  workflow_id TEXT NOT NULL DEFAULT '',
  agent_id TEXT NOT NULL,
  msg_index INTEGER NOT NULL,
  timestamp TEXT,
  input_tokens INTEGER DEFAULT 0,
  output_tokens INTEGER DEFAULT 0,
  cache_creation_tokens INTEGER DEFAULT 0,
  cache_read_tokens INTEGER DEFAULT 0,
  data TEXT NOT NULL,
  UNIQUE(source_id, session_id, workflow_id, agent_id, msg_index)
);

CREATE TABLE IF NOT EXISTS subagent_timeline_messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source_id TEXT NOT NULL,
  project_slug TEXT NOT NULL,
  session_id TEXT NOT NULL,
  workflow_id TEXT NOT NULL DEFAULT '',
  agent_id TEXT NOT NULL,
  timeline_index INTEGER NOT NULL,
  display_type TEXT NOT NULL,
  tool_name TEXT,
  tool_use_id TEXT,
  search_text TEXT NOT NULL DEFAULT '',
  data TEXT NOT NULL,
  UNIQUE(source_id, session_id, workflow_id, agent_id, timeline_index)
);

CREATE TABLE IF NOT EXISTS subagent_dirty_threads (
  source_id TEXT NOT NULL,
  project_slug TEXT NOT NULL,
  session_id TEXT NOT NULL,
  workflow_id TEXT NOT NULL DEFAULT '',
  agent_id TEXT NOT NULL,
  PRIMARY KEY(source_id, session_id, workflow_id, agent_id)
);

CREATE TABLE IF NOT EXISTS token_activity_daily (
  source_id TEXT NOT NULL,
  project_slug TEXT NOT NULL,
  activity_day TEXT NOT NULL,
  input_tokens INTEGER NOT NULL DEFAULT 0,
  output_tokens INTEGER NOT NULL DEFAULT 0,
  cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
  cache_read_tokens INTEGER NOT NULL DEFAULT 0,
  total_tokens INTEGER NOT NULL DEFAULT 0,
  exact_tokens INTEGER NOT NULL DEFAULT 0,
  estimated_tokens INTEGER NOT NULL DEFAULT 0,
  message_count INTEGER NOT NULL DEFAULT 0,
  session_count INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (source_id, project_slug, activity_day)
);

CREATE TABLE IF NOT EXISTS token_activity_dirty (
  source_id TEXT NOT NULL,
  project_slug TEXT NOT NULL,
  activity_day TEXT NOT NULL,
  PRIMARY KEY (source_id, project_slug, activity_day)
);

CREATE TABLE IF NOT EXISTS workflows (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  project_slug TEXT,
  session_id TEXT,
  workflow_id TEXT,
  name TEXT,
  status TEXT,
  agent_count INTEGER,
  total_tokens INTEGER,
  total_tool_calls INTEGER,
  duration_ms INTEGER,
  subagent_count INTEGER,
  data TEXT,
  journal TEXT,
  updated_at INTEGER,
  UNIQUE(project_slug, session_id, workflow_id)
);

CREATE TABLE IF NOT EXISTS tool_results (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  project_slug TEXT,
  session_id TEXT,
  tool_use_id TEXT,
  content TEXT,
  updated_at INTEGER,
  UNIQUE(project_slug, session_id, tool_use_id)
);

CREATE TABLE IF NOT EXISTS todos (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT,
  agent_id TEXT,
  items TEXT,
  updated_at INTEGER,
  UNIQUE(session_id, agent_id)
);

CREATE TABLE IF NOT EXISTS tasks (
  session_id TEXT PRIMARY KEY,
  has_highwatermark INTEGER,
  highwatermark INTEGER,
  lock_exists INTEGER,
  updated_at INTEGER
);

CREATE TABLE IF NOT EXISTS plans (
  slug TEXT PRIMARY KEY,
  title TEXT,
  content TEXT,
  size INTEGER,
  updated_at INTEGER
);

CREATE TABLE IF NOT EXISTS config (
  key TEXT PRIMARY KEY,
  data TEXT,
  updated_at INTEGER
);

CREATE TABLE IF NOT EXISTS analytics (
  key TEXT PRIMARY KEY,
  data TEXT,
  updated_at INTEGER
);

CREATE TABLE IF NOT EXISTS file_history (
  session_id TEXT PRIMARY KEY,
  data TEXT,
  updated_at INTEGER
);

-- Indexes for common queries
CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(project_slug);
CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(project_slug, session_id);
CREATE INDEX IF NOT EXISTS idx_messages_session_idx ON messages(session_id, msg_index);
CREATE INDEX IF NOT EXISTS idx_messages_activity_day ON messages(source_id, project_slug, substr(timestamp, 1, 10));
CREATE INDEX IF NOT EXISTS idx_timeline_session_idx ON timeline_messages(session_id, timeline_index);
CREATE INDEX IF NOT EXISTS idx_timeline_session_raw ON timeline_messages(session_id, raw_index);
CREATE INDEX IF NOT EXISTS idx_timeline_session_type ON timeline_messages(session_id, display_type, timeline_index);
CREATE INDEX IF NOT EXISTS idx_timeline_session_tool ON timeline_messages(session_id, tool_name, timeline_index);
CREATE INDEX IF NOT EXISTS idx_timeline_session_tool_id ON timeline_messages(session_id, tool_use_id);
CREATE INDEX IF NOT EXISTS idx_timeline_results_tool ON timeline_tool_results(session_id, tool_use_id, raw_index);
CREATE INDEX IF NOT EXISTS idx_subagents_session ON subagents(source_id, project_slug, session_id);
CREATE INDEX IF NOT EXISTS idx_subagent_messages_thread ON subagent_messages(source_id, session_id, workflow_id, agent_id, msg_index);
CREATE INDEX IF NOT EXISTS idx_subagent_messages_activity_day ON subagent_messages(source_id, project_slug, substr(timestamp, 1, 10));
CREATE INDEX IF NOT EXISTS idx_token_activity_project_day ON token_activity_daily(project_slug, activity_day, source_id);
CREATE INDEX IF NOT EXISTS idx_subagent_timeline_thread ON subagent_timeline_messages(source_id, session_id, workflow_id, agent_id, timeline_index);
CREATE INDEX IF NOT EXISTS idx_subagent_timeline_type ON subagent_timeline_messages(source_id, session_id, display_type);
CREATE INDEX IF NOT EXISTS idx_subagent_timeline_tool ON subagent_timeline_messages(source_id, session_id, tool_name);
CREATE INDEX IF NOT EXISTS idx_tool_results_session ON tool_results(project_slug, session_id);
CREATE INDEX IF NOT EXISTS idx_todos_session ON todos(session_id);

-- Persistent FTS5 (content-synced with messages)
CREATE VIRTUAL TABLE IF NOT EXISTS search_fts USING fts5(text_content, content='messages', content_rowid='id');
CREATE VIRTUAL TABLE IF NOT EXISTS subagent_search_fts USING fts5(search_text, content='subagent_timeline_messages', content_rowid='id');

-- Auto-sync triggers
CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
  INSERT INTO search_fts(rowid, text_content) VALUES (new.id, new.text_content);
END;
CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
  INSERT INTO search_fts(search_fts, rowid, text_content) VALUES ('delete', old.id, old.text_content);
END;
CREATE TRIGGER IF NOT EXISTS messages_au AFTER UPDATE ON messages BEGIN
  INSERT INTO search_fts(search_fts, rowid, text_content) VALUES ('delete', old.id, old.text_content);
  INSERT INTO search_fts(rowid, text_content) VALUES (new.id, new.text_content);
END;
CREATE TRIGGER IF NOT EXISTS subagent_timeline_ai AFTER INSERT ON subagent_timeline_messages BEGIN
  INSERT INTO subagent_search_fts(rowid, search_text) VALUES (new.id, new.search_text);
END;
CREATE TRIGGER IF NOT EXISTS subagent_timeline_ad AFTER DELETE ON subagent_timeline_messages BEGIN
  INSERT INTO subagent_search_fts(subagent_search_fts, rowid, search_text) VALUES ('delete', old.id, old.search_text);
END;
CREATE TRIGGER IF NOT EXISTS subagent_timeline_au AFTER UPDATE ON subagent_timeline_messages BEGIN
  INSERT INTO subagent_search_fts(subagent_search_fts, rowid, search_text) VALUES ('delete', old.id, old.search_text);
  INSERT INTO subagent_search_fts(rowid, search_text) VALUES (new.id, new.search_text);
END;
CREATE TRIGGER IF NOT EXISTS timeline_dirty_ai AFTER INSERT ON messages BEGIN
  INSERT INTO timeline_dirty_sessions(session_id, source_id, project_slug)
  VALUES (new.session_id, new.source_id, new.project_slug)
  ON CONFLICT(session_id) DO UPDATE SET source_id = excluded.source_id, project_slug = excluded.project_slug;
END;
CREATE TRIGGER IF NOT EXISTS timeline_dirty_ad AFTER DELETE ON messages BEGIN
  INSERT INTO timeline_dirty_sessions(session_id, source_id, project_slug)
  VALUES (old.session_id, old.source_id, old.project_slug)
  ON CONFLICT(session_id) DO UPDATE SET source_id = excluded.source_id, project_slug = excluded.project_slug;
END;
CREATE TRIGGER IF NOT EXISTS timeline_dirty_au AFTER UPDATE OF data, timestamp, msg_index, source_id, project_slug ON messages BEGIN
  INSERT INTO timeline_dirty_sessions(session_id, source_id, project_slug)
  VALUES (new.session_id, new.source_id, new.project_slug)
  ON CONFLICT(session_id) DO UPDATE SET source_id = excluded.source_id, project_slug = excluded.project_slug;
END;
CREATE TRIGGER IF NOT EXISTS subagent_dirty_ai AFTER INSERT ON subagent_messages BEGIN
  INSERT INTO subagent_dirty_threads(source_id, project_slug, session_id, workflow_id, agent_id)
  VALUES (new.source_id, new.project_slug, new.session_id, new.workflow_id, new.agent_id)
  ON CONFLICT(source_id, session_id, workflow_id, agent_id) DO UPDATE SET project_slug = excluded.project_slug;
END;
CREATE TRIGGER IF NOT EXISTS subagent_dirty_ad AFTER DELETE ON subagent_messages BEGIN
  INSERT INTO subagent_dirty_threads(source_id, project_slug, session_id, workflow_id, agent_id)
  VALUES (old.source_id, old.project_slug, old.session_id, old.workflow_id, old.agent_id)
  ON CONFLICT(source_id, session_id, workflow_id, agent_id) DO UPDATE SET project_slug = excluded.project_slug;
END;
CREATE TRIGGER IF NOT EXISTS subagent_dirty_au AFTER UPDATE ON subagent_messages BEGIN
  INSERT INTO subagent_dirty_threads(source_id, project_slug, session_id, workflow_id, agent_id)
  VALUES (new.source_id, new.project_slug, new.session_id, new.workflow_id, new.agent_id)
  ON CONFLICT(source_id, session_id, workflow_id, agent_id) DO UPDATE SET project_slug = excluded.project_slug;
END;

CREATE TRIGGER IF NOT EXISTS token_activity_messages_ai AFTER INSERT ON messages
WHEN new.timestamp IS NOT NULL AND length(new.timestamp) >= 10 BEGIN
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  VALUES (new.source_id, new.project_slug, substr(new.timestamp, 1, 10))
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
END;
CREATE TRIGGER IF NOT EXISTS token_activity_messages_ad AFTER DELETE ON messages
WHEN old.timestamp IS NOT NULL AND length(old.timestamp) >= 10 BEGIN
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  VALUES (old.source_id, old.project_slug, substr(old.timestamp, 1, 10))
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
END;
CREATE TRIGGER IF NOT EXISTS token_activity_messages_au
AFTER UPDATE OF timestamp, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, source_id, project_slug ON messages BEGIN
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  SELECT old.source_id, old.project_slug, substr(old.timestamp, 1, 10)
   WHERE old.timestamp IS NOT NULL AND length(old.timestamp) >= 10
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  SELECT new.source_id, new.project_slug, substr(new.timestamp, 1, 10)
   WHERE new.timestamp IS NOT NULL AND length(new.timestamp) >= 10
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
END;
CREATE TRIGGER IF NOT EXISTS token_activity_subagents_ai AFTER INSERT ON subagent_messages
WHEN new.timestamp IS NOT NULL AND length(new.timestamp) >= 10 BEGIN
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  VALUES (new.source_id, new.project_slug, substr(new.timestamp, 1, 10))
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
END;
CREATE TRIGGER IF NOT EXISTS token_activity_subagents_ad AFTER DELETE ON subagent_messages
WHEN old.timestamp IS NOT NULL AND length(old.timestamp) >= 10 BEGIN
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  VALUES (old.source_id, old.project_slug, substr(old.timestamp, 1, 10))
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
END;
CREATE TRIGGER IF NOT EXISTS token_activity_subagents_au
AFTER UPDATE OF timestamp, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, source_id, project_slug ON subagent_messages BEGIN
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  SELECT old.source_id, old.project_slug, substr(old.timestamp, 1, 10)
   WHERE old.timestamp IS NOT NULL AND length(old.timestamp) >= 10
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  SELECT new.source_id, new.project_slug, substr(new.timestamp, 1, 10)
   WHERE new.timestamp IS NOT NULL AND length(new.timestamp) >= 10
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
END;
CREATE TRIGGER IF NOT EXISTS token_activity_session_quality_au AFTER UPDATE OF tokens_estimated ON sessions BEGIN
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  SELECT source_id, project_slug, substr(timestamp, 1, 10) FROM messages
   WHERE session_id = new.id AND source_id = new.source_id AND timestamp IS NOT NULL AND length(timestamp) >= 10
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  SELECT source_id, project_slug, substr(timestamp, 1, 10) FROM subagent_messages
   WHERE session_id = new.id AND source_id = new.source_id AND timestamp IS NOT NULL AND length(timestamp) >= 10
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
END;
"#;

/// Tables from previous schema versions that should be dropped during
/// migration. Kept verbatim with the TS `LEGACY_TABLES` list.
const LEGACY_TABLES: &[&str] = &["segments", "search_index", "schema_version"];

/// All tables in the current schema, used for drop-and-recreate. Kept verbatim
/// with the TS `CURRENT_TABLES` list (same order).
const CURRENT_TABLES: &[&str] = &[
    "search_fts",
    "subagent_search_fts",
    "source_files",
    "projects",
    "project_memories",
    "sessions",
    "messages",
    "timeline_messages",
    "timeline_tool_results",
    "timeline_dirty_sessions",
    "subagents",
    "subagent_messages",
    "subagent_timeline_messages",
    "subagent_dirty_threads",
    "token_activity_daily",
    "token_activity_dirty",
    "workflows",
    "tool_results",
    "todos",
    "tasks",
    "plans",
    "config",
    "analytics",
    "file_history",
    "schema_meta",
];

/// Triggers that are explicitly dropped during a wipe. `DROP TABLE` on their
/// owning table removes them, but we drop defensively in case the table is
/// already gone from a partial legacy state.
const CURRENT_TRIGGERS: &[&str] = &["messages_ai", "messages_ad", "messages_au"];

const TOKEN_ACTIVITY_TRIGGERS: &[&str] = &[
    "token_activity_messages_ai",
    "token_activity_messages_ad",
    "token_activity_messages_au",
    "token_activity_subagents_ai",
    "token_activity_subagents_ad",
    "token_activity_subagents_au",
    "token_activity_session_quality_au",
];

/// FTS auto-sync trigger DDL, extracted so bulk-ingest can drop and
/// recreate them around a high-volume INSERT run. Must stay byte-identical
/// to the trigger block embedded in [`SCHEMA_SQL`] above.
const FTS_TRIGGERS_SQL: &str = r#"
CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
  INSERT INTO search_fts(rowid, text_content) VALUES (new.id, new.text_content);
END;
CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
  INSERT INTO search_fts(search_fts, rowid, text_content) VALUES ('delete', old.id, old.text_content);
END;
CREATE TRIGGER IF NOT EXISTS messages_au AFTER UPDATE ON messages BEGIN
  INSERT INTO search_fts(search_fts, rowid, text_content) VALUES ('delete', old.id, old.text_content);
  INSERT INTO search_fts(rowid, text_content) VALUES (new.id, new.text_content);
END;
"#;

const TOKEN_ACTIVITY_TRIGGERS_SQL: &str = r#"
CREATE TRIGGER IF NOT EXISTS token_activity_messages_ai AFTER INSERT ON messages
WHEN new.timestamp IS NOT NULL AND length(new.timestamp) >= 10 BEGIN
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  VALUES (new.source_id, new.project_slug, substr(new.timestamp, 1, 10))
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
END;
CREATE TRIGGER IF NOT EXISTS token_activity_messages_ad AFTER DELETE ON messages
WHEN old.timestamp IS NOT NULL AND length(old.timestamp) >= 10 BEGIN
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  VALUES (old.source_id, old.project_slug, substr(old.timestamp, 1, 10))
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
END;
CREATE TRIGGER IF NOT EXISTS token_activity_messages_au
AFTER UPDATE OF timestamp, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, source_id, project_slug ON messages BEGIN
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  SELECT old.source_id, old.project_slug, substr(old.timestamp, 1, 10)
   WHERE old.timestamp IS NOT NULL AND length(old.timestamp) >= 10
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  SELECT new.source_id, new.project_slug, substr(new.timestamp, 1, 10)
   WHERE new.timestamp IS NOT NULL AND length(new.timestamp) >= 10
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
END;
CREATE TRIGGER IF NOT EXISTS token_activity_subagents_ai AFTER INSERT ON subagent_messages
WHEN new.timestamp IS NOT NULL AND length(new.timestamp) >= 10 BEGIN
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  VALUES (new.source_id, new.project_slug, substr(new.timestamp, 1, 10))
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
END;
CREATE TRIGGER IF NOT EXISTS token_activity_subagents_ad AFTER DELETE ON subagent_messages
WHEN old.timestamp IS NOT NULL AND length(old.timestamp) >= 10 BEGIN
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  VALUES (old.source_id, old.project_slug, substr(old.timestamp, 1, 10))
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
END;
CREATE TRIGGER IF NOT EXISTS token_activity_subagents_au
AFTER UPDATE OF timestamp, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, source_id, project_slug ON subagent_messages BEGIN
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  SELECT old.source_id, old.project_slug, substr(old.timestamp, 1, 10)
   WHERE old.timestamp IS NOT NULL AND length(old.timestamp) >= 10
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  SELECT new.source_id, new.project_slug, substr(new.timestamp, 1, 10)
   WHERE new.timestamp IS NOT NULL AND length(new.timestamp) >= 10
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
END;
CREATE TRIGGER IF NOT EXISTS token_activity_session_quality_au AFTER UPDATE OF tokens_estimated ON sessions BEGIN
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  SELECT source_id, project_slug, substr(timestamp, 1, 10) FROM messages
   WHERE session_id = new.id AND source_id = new.source_id AND timestamp IS NOT NULL AND length(timestamp) >= 10
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  SELECT source_id, project_slug, substr(timestamp, 1, 10) FROM subagent_messages
   WHERE session_id = new.id AND source_id = new.source_id AND timestamp IS NOT NULL AND length(timestamp) >= 10
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
END;
"#;

/// Errors produced by the schema module.
#[derive(Debug, Error)]
pub enum SchemaError {
    /// An underlying SQLite error occurred.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Drop per-row FTS and activity triggers during a high-volume import.
pub fn drop_fts_triggers(conn: &Connection) -> Result<(), SchemaError> {
    for trigger in CURRENT_TRIGGERS {
        conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {trigger}"))?;
    }
    for trigger in TOKEN_ACTIVITY_TRIGGERS {
        conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {trigger}"))?;
    }
    Ok(())
}

fn refresh_token_activity_triggers(conn: &Connection) -> Result<(), SchemaError> {
    for trigger in TOKEN_ACTIVITY_TRIGGERS {
        conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {trigger}"))?;
    }
    conn.execute_batch(TOKEN_ACTIVITY_TRIGGERS_SQL)?;
    Ok(())
}

/// Rebuild `search_fts` from its content table (`messages`) via the FTS5
/// `'rebuild'` command, then recreate the auto-sync triggers so warm-start
/// incremental writes stay in sync. Pairs with [`drop_fts_triggers`] —
/// every bulk ingest that drops triggers must call this before releasing
/// the connection, otherwise the FTS index will silently diverge from
/// `messages`.
pub fn rebuild_fts_and_recreate_triggers(conn: &Connection) -> Result<(), SchemaError> {
    conn.execute_batch("INSERT INTO search_fts(search_fts) VALUES('rebuild')")?;
    conn.execute_batch(FTS_TRIGGERS_SQL)?;
    conn.execute_batch(TOKEN_ACTIVITY_TRIGGERS_SQL)?;
    conn.execute_batch(
        r#"
        INSERT OR IGNORE INTO token_activity_dirty(source_id, project_slug, activity_day)
        SELECT DISTINCT source_id, project_slug, substr(timestamp, 1, 10)
          FROM messages WHERE timestamp IS NOT NULL AND length(timestamp) >= 10;
        INSERT OR IGNORE INTO token_activity_dirty(source_id, project_slug, activity_day)
        SELECT DISTINCT source_id, project_slug, substr(timestamp, 1, 10)
          FROM subagent_messages WHERE timestamp IS NOT NULL AND length(timestamp) >= 10;
        "#,
    )?;
    Ok(())
}

/// Apply the connection-level PRAGMAs that the TS `SqliteService` sets on
/// every open: WAL journal mode, NORMAL synchronous, foreign keys on.
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
    Ok(())
}

/// Read the currently-persisted schema version from `schema_meta`.
///
/// Returns `Ok(None)` if `schema_meta` does not yet exist or the `version`
/// row has not been written. Returns `Ok(Some(v))` with the parsed `u32`
/// otherwise. A row whose value fails to parse is treated as "missing" and
/// returns `Ok(None)`, matching the TS `parseInt` fallback behaviour.
pub fn current_schema_version(conn: &Connection) -> Result<Option<u32>, SchemaError> {
    // If schema_meta doesn't exist, we have no stored version.
    let meta_exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_meta'",
        [],
        |row| row.get(0),
    )?;
    if meta_exists == 0 {
        return Ok(None);
    }

    let row: Option<String> = conn
        .query_row(
            "SELECT value FROM schema_meta WHERE key = 'version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;

    Ok(row.and_then(|v| v.parse::<u32>().ok()))
}

/// Initialize the database schema, migrating from older versions if
/// necessary.
///
/// - Ensures `schema_meta` exists so the version can be read.
/// - If the stored version is missing or `!= SCHEMA_VERSION`, drops all
///   legacy + current tables (and their triggers) and rebuilds from
///   [`SCHEMA_SQL`].
/// - Otherwise, reruns [`SCHEMA_SQL`] (every statement is `IF NOT EXISTS`,
///   so it is safe and idempotent when the version already matches).
/// - Writes the current [`SCHEMA_VERSION`] into `schema_meta` after a wipe.
///
/// This mirrors the behaviour of `initializeSchema` in
/// `packages/sdk/src/data/schema.ts` — wipe-on-stale, never incremental.
pub fn initialize_schema(conn: &Connection) -> Result<(), SchemaError> {
    // Ensure schema_meta exists so we can read the version.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
    )?;

    let current = current_schema_version(conn)?;

    if current != Some(SCHEMA_VERSION) {
        // Drop legacy tables from previous schema versions. Errors here are
        // deliberately ignored (match TS try/catch with empty catch) so a
        // partially-broken legacy state still migrates.
        for table in LEGACY_TABLES {
            let _ = conn.execute_batch(&format!("DROP TABLE IF EXISTS {table}"));
        }

        // Drop current-schema tables (including the FTS5 virtual table).
        for table in CURRENT_TABLES {
            let _ = conn.execute_batch(&format!("DROP TABLE IF EXISTS {table}"));
        }

        // Explicitly drop triggers; `DROP TABLE messages` already removes
        // them, but be defensive in case the table is missing.
        for trigger in CURRENT_TRIGGERS {
            let _ = conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {trigger}"));
        }

        // Recreate schema_meta (it was dropped above as part of CURRENT_TABLES).
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        )?;

        // Create all tables / indexes / FTS / triggers.
        conn.execute_batch(SCHEMA_SQL)?;

        // Record the new version.
        conn.execute(
            "INSERT INTO schema_meta (key, value) VALUES ('version', ?1) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [SCHEMA_VERSION.to_string()],
        )?;
    } else {
        // Version matches — make sure all tables exist. Every statement in
        // SCHEMA_SQL is IF NOT EXISTS so this is a no-op on a healthy DB.
        conn.execute_batch(SCHEMA_SQL)?;
        // Refresh derived-index trigger bodies: IF NOT EXISTS cannot replace
        // a correctness-fixed definition from the same schema version.
        refresh_token_activity_triggers(conn)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Count rows in `sqlite_master` matching a given type + name, used to
    /// assert the presence of tables / triggers after operations.
    fn object_exists(conn: &Connection, obj_type: &str, name: &str) -> bool {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = ?1 AND name = ?2",
                [obj_type, name],
                |row| row.get(0),
            )
            .unwrap();
        count > 0
    }

    #[test]
    fn initialize_schema_on_fresh_db_sets_version() {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        initialize_schema(&conn).expect("initialize_schema");

        let version = current_schema_version(&conn).expect("read version");
        assert_eq!(version, Some(SCHEMA_VERSION));

        // Spot-check a handful of objects from every category.
        assert!(object_exists(&conn, "table", "schema_meta"));
        assert!(object_exists(&conn, "table", "projects"));
        assert!(object_exists(&conn, "table", "messages"));
        assert!(object_exists(&conn, "table", "timeline_tool_results"));
        assert!(object_exists(&conn, "table", "source_files"));
        assert!(object_exists(&conn, "table", "search_fts")); // FTS5 virtual table
        assert!(object_exists(&conn, "index", "idx_messages_session"));
        assert!(object_exists(&conn, "trigger", "messages_ai"));
        assert!(object_exists(&conn, "trigger", "messages_ad"));
        assert!(object_exists(&conn, "trigger", "messages_au"));

        let raw_index_columns: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('timeline_messages') WHERE name = 'raw_index'",
                [],
                |row| row.get(0),
            )
            .expect("inspect timeline schema");
        assert_eq!(raw_index_columns, 1);
    }

    #[test]
    fn initialize_schema_is_idempotent() {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        initialize_schema(&conn).expect("first init");

        // Insert a row we expect to survive the second init, since the
        // version already matches and no wipe should occur.
        conn.execute(
            "INSERT INTO projects (slug, original_path, sessions_index, updated_at) \
             VALUES ('canary', '/tmp/canary', '[]', 123)",
            [],
        )
        .expect("insert canary");

        initialize_schema(&conn).expect("second init");

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM projects WHERE slug = 'canary'",
                [],
                |row| row.get(0),
            )
            .expect("count canary");
        assert_eq!(count, 1, "second initialize_schema should not wipe data");

        let version = current_schema_version(&conn).expect("read version");
        assert_eq!(version, Some(SCHEMA_VERSION));
    }

    #[test]
    fn same_version_attach_refreshes_activity_triggers_and_upserts_stay_safe() {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        initialize_schema(&conn).expect("first init");
        conn.execute_batch(
            r#"
            DROP TRIGGER token_activity_messages_ai;
            CREATE TRIGGER token_activity_messages_ai AFTER INSERT ON messages BEGIN SELECT 1; END;
            "#,
        )
        .expect("install stale trigger");

        initialize_schema(&conn).expect("refresh triggers");
        let trigger_sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='trigger' AND name='token_activity_messages_ai'",
                [],
                |row| row.get(0),
            )
            .expect("read trigger body");
        assert!(trigger_sql.contains("DO NOTHING"));

        conn.execute_batch(
            r#"
            INSERT INTO messages(project_slug, session_id, msg_index, timestamp, data)
            VALUES ('p', 's', 0, '2026-07-19T00:00:00Z', '{}');
            INSERT INTO messages(project_slug, session_id, msg_index, timestamp, data)
            VALUES ('p', 's', 0, '2026-07-19T00:00:00Z', '{}')
            ON CONFLICT(session_id, msg_index) DO UPDATE SET data=excluded.data;
            "#,
        )
        .expect("outer message upsert must not override dirty-marker conflict handling");
    }

    #[test]
    fn stale_schema_triggers_wipe_and_rebuild() {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        initialize_schema(&conn).expect("first init");

        // Insert a dummy row we expect to be wiped.
        conn.execute(
            "INSERT INTO projects (slug, original_path, sessions_index, updated_at) \
             VALUES ('doomed', '/tmp/doomed', '[]', 456)",
            [],
        )
        .expect("insert doomed");

        // Pretend the stored schema is one version behind.
        let stale_version = SCHEMA_VERSION - 1;
        conn.execute(
            "INSERT INTO schema_meta (key, value) VALUES ('version', ?1) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [stale_version.to_string()],
        )
        .expect("set stale version");

        // Sanity: version really is stale.
        assert_eq!(
            current_schema_version(&conn).expect("read stale"),
            Some(stale_version)
        );

        initialize_schema(&conn).expect("migrate");

        // Version should now be current.
        assert_eq!(
            current_schema_version(&conn).expect("read after migrate"),
            Some(SCHEMA_VERSION)
        );

        // The doomed row must be gone — wipe-and-rebuild happened.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM projects WHERE slug = 'doomed'",
                [],
                |row| row.get(0),
            )
            .expect("count doomed");
        assert_eq!(count, 0, "stale migration should drop all data");

        // Schema objects should still exist.
        assert!(object_exists(&conn, "table", "messages"));
        assert!(object_exists(&conn, "table", "search_fts"));
        assert!(object_exists(&conn, "trigger", "messages_ai"));
    }

    #[test]
    fn set_pragmas_enables_wal_on_file_db() {
        // `PRAGMA journal_mode = WAL` is persisted as `memory` on in-memory
        // connections; use a tempfile-backed DB so WAL is actually applied.
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("pragma-test.sqlite");
        let conn = Connection::open(&db_path).expect("open file db");

        set_pragmas(&conn).expect("set pragmas");

        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("read journal_mode");
        assert_eq!(mode.to_lowercase(), "wal");

        // synchronous = NORMAL (1)
        let sync: i64 = conn
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .expect("read synchronous");
        assert_eq!(sync, 1);

        // foreign_keys = ON (1)
        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("read foreign_keys");
        assert_eq!(fk, 1);
    }
}
