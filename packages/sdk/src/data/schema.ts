/**
 * Schema — DDL for the Phase 3 dedicated-table schema + migration logic
 */

import type { SqliteService } from '../io/index.js';

// v5: the source dimension. `source_id` on the four queryable entities
// (source_files, projects, sessions, messages) records which agent product a
// row came from, so multiple agents can be indexed side by side. Claude Code
// is the only source today, so every row is stamped 'claude-code'; the column
// exists so a second AgentSource needs a data change, not a schema change.
// Migration is the version bump: drop-and-rebuild re-ingests from disk (the
// index is a pure function of files on disk), stamping source_id fresh.
//
// v6: `projects` primary key is now composite `(source_id, slug)`. A project
// slug is the encoded cwd, so two sources (e.g. claude-code + codex) that
// worked the same directory derive the SAME slug — slug alone would collide and
// merge them into one row. Sessions (PK `id`) and messages (unique
// `session_id, msg_index`) don't collide (ids are globally unique per source),
// so only `projects` needs the composite key. `project_memories` stays keyed by
// `project_slug` for now — only Claude Code writes memories, so there is no
// cross-source collision there yet (see RFC 006 §8).
//
// v7: `sessions.tokens_estimated` — Codex (and future sources) may fill token
// columns via tiktoken when official usage events are missing. The flag lets
// the UI show "~" / "est" so estimates are never mistaken for API truth.
//
// v8: materialized `timeline_messages` projection. Raw agent rows are not a
// 1:1 match for visible transcript rows (assistant envelopes split into text,
// thinking and tool calls; tool results merge into calls). SQLite triggers
// mark changed sessions dirty and the query layer rebuilds the normalized
// projection atomically before serving facets or filtered cursor pages.
//
// v9: source-aware subagent threads. Whole-transcript JSON blobs are replaced
// by row-normalized raw messages plus a rebuildable display projection and
// dedicated FTS index. Threads link back to their spawning Task/Agent tool id,
// so the UI can lazily embed branches without flattening them into parent
// pagination.
//
// v10: incremental parent timeline projection. Each normalized row retains
// its canonical raw index, and tool results are stored as explicit
// associations so append-only live ingest can update one call without
// invalidating and rebuilding the entire session.
//
// v11: subagent token columns plus source-normalized daily token activity.
// Canonical message triggers mark affected project-days dirty; the shared
// query layer rebuilds only those buckets and cold ingest materializes all.
export const SCHEMA_VERSION = 11;

export const TOKEN_ACTIVITY_TRIGGER_NAMES = [
  'token_activity_messages_ai',
  'token_activity_messages_ad',
  'token_activity_messages_au',
  'token_activity_subagents_ai',
  'token_activity_subagents_ad',
  'token_activity_subagents_au',
  'token_activity_session_quality_au',
] as const;

/** Reused after bulk ingest temporarily suppresses per-row dirty markers. */
export const TOKEN_ACTIVITY_TRIGGERS_SQL = `
CREATE TRIGGER IF NOT EXISTS token_activity_messages_ai AFTER INSERT ON messages
WHEN new.timestamp IS NOT NULL AND length(new.timestamp) >= 10 BEGIN
  INSERT OR IGNORE INTO token_activity_dirty(source_id, project_slug, activity_day)
  VALUES (new.source_id, new.project_slug, substr(new.timestamp, 1, 10));
END;
CREATE TRIGGER IF NOT EXISTS token_activity_messages_ad AFTER DELETE ON messages
WHEN old.timestamp IS NOT NULL AND length(old.timestamp) >= 10 BEGIN
  INSERT OR IGNORE INTO token_activity_dirty(source_id, project_slug, activity_day)
  VALUES (old.source_id, old.project_slug, substr(old.timestamp, 1, 10));
END;
CREATE TRIGGER IF NOT EXISTS token_activity_messages_au
AFTER UPDATE OF timestamp, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, source_id, project_slug ON messages BEGIN
  INSERT OR IGNORE INTO token_activity_dirty(source_id, project_slug, activity_day)
  SELECT old.source_id, old.project_slug, substr(old.timestamp, 1, 10)
   WHERE old.timestamp IS NOT NULL AND length(old.timestamp) >= 10;
  INSERT OR IGNORE INTO token_activity_dirty(source_id, project_slug, activity_day)
  SELECT new.source_id, new.project_slug, substr(new.timestamp, 1, 10)
   WHERE new.timestamp IS NOT NULL AND length(new.timestamp) >= 10;
END;
CREATE TRIGGER IF NOT EXISTS token_activity_subagents_ai AFTER INSERT ON subagent_messages
WHEN new.timestamp IS NOT NULL AND length(new.timestamp) >= 10 BEGIN
  INSERT OR IGNORE INTO token_activity_dirty(source_id, project_slug, activity_day)
  VALUES (new.source_id, new.project_slug, substr(new.timestamp, 1, 10));
END;
CREATE TRIGGER IF NOT EXISTS token_activity_subagents_ad AFTER DELETE ON subagent_messages
WHEN old.timestamp IS NOT NULL AND length(old.timestamp) >= 10 BEGIN
  INSERT OR IGNORE INTO token_activity_dirty(source_id, project_slug, activity_day)
  VALUES (old.source_id, old.project_slug, substr(old.timestamp, 1, 10));
END;
CREATE TRIGGER IF NOT EXISTS token_activity_subagents_au
AFTER UPDATE OF timestamp, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, source_id, project_slug ON subagent_messages BEGIN
  INSERT OR IGNORE INTO token_activity_dirty(source_id, project_slug, activity_day)
  SELECT old.source_id, old.project_slug, substr(old.timestamp, 1, 10)
   WHERE old.timestamp IS NOT NULL AND length(old.timestamp) >= 10;
  INSERT OR IGNORE INTO token_activity_dirty(source_id, project_slug, activity_day)
  SELECT new.source_id, new.project_slug, substr(new.timestamp, 1, 10)
   WHERE new.timestamp IS NOT NULL AND length(new.timestamp) >= 10;
END;
CREATE TRIGGER IF NOT EXISTS token_activity_session_quality_au AFTER UPDATE OF tokens_estimated ON sessions BEGIN
  INSERT OR IGNORE INTO token_activity_dirty(source_id, project_slug, activity_day)
  SELECT source_id, project_slug, substr(timestamp, 1, 10) FROM messages
   WHERE session_id = new.id AND source_id = new.source_id AND timestamp IS NOT NULL AND length(timestamp) >= 10;
  INSERT OR IGNORE INTO token_activity_dirty(source_id, project_slug, activity_day)
  SELECT source_id, project_slug, substr(timestamp, 1, 10) FROM subagent_messages
   WHERE session_id = new.id AND source_id = new.source_id AND timestamp IS NOT NULL AND length(timestamp) >= 10;
END;
`;

export const SCHEMA_SQL = `
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
  -- 1 when message tokens were filled by a local estimate (tiktoken), not agent usage events
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

-- Raw rows are canonical. Any mutation invalidates the materialized display
-- projection for that session; QueryService refreshes it on the next read.
CREATE TRIGGER IF NOT EXISTS timeline_dirty_ai AFTER INSERT ON messages BEGIN
  INSERT INTO timeline_dirty_sessions(session_id, source_id, project_slug)
  VALUES (new.session_id, new.source_id, new.project_slug)
  ON CONFLICT(session_id) DO UPDATE SET
    source_id = excluded.source_id,
    project_slug = excluded.project_slug;
END;
CREATE TRIGGER IF NOT EXISTS timeline_dirty_ad AFTER DELETE ON messages BEGIN
  INSERT INTO timeline_dirty_sessions(session_id, source_id, project_slug)
  VALUES (old.session_id, old.source_id, old.project_slug)
  ON CONFLICT(session_id) DO UPDATE SET
    source_id = excluded.source_id,
    project_slug = excluded.project_slug;
END;
CREATE TRIGGER IF NOT EXISTS timeline_dirty_au AFTER UPDATE OF data, timestamp, msg_index, source_id, project_slug ON messages BEGIN
  INSERT INTO timeline_dirty_sessions(session_id, source_id, project_slug)
  VALUES (new.session_id, new.source_id, new.project_slug)
  ON CONFLICT(session_id) DO UPDATE SET
    source_id = excluded.source_id,
    project_slug = excluded.project_slug;
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

-- Daily token activity is derived from canonical rows. Mutations only mark
-- days dirty; recomputation avoids fragile arithmetic when rows are upserted
-- or sidecars later change token/timestamp attribution.
${TOKEN_ACTIVITY_TRIGGERS_SQL}
`;

/**
 * Tables from previous schema versions that should be dropped during migration.
 */
const LEGACY_TABLES = ['segments', 'search_index', 'schema_version'];

/**
 * All tables in the current schema (used for drop-and-recreate).
 */
const CURRENT_TABLES = [
  'search_fts',
  'subagent_search_fts',
  'source_files',
  'projects',
  'project_memories',
  'sessions',
  'messages',
  'timeline_messages',
  'timeline_tool_results',
  'timeline_dirty_sessions',
  'subagents',
  'subagent_messages',
  'subagent_timeline_messages',
  'subagent_dirty_threads',
  'token_activity_daily',
  'token_activity_dirty',
  'workflows',
  'tool_results',
  'todos',
  'tasks',
  'plans',
  'config',
  'analytics',
  'file_history',
  'schema_meta',
];

/**
 * Initialize the database schema, migrating from older versions if necessary.
 *
 * - Creates `schema_meta` table if it doesn't exist
 * - If the stored version !== SCHEMA_VERSION, drops ALL old + current tables and recreates
 * - Inserts / updates the version to SCHEMA_VERSION
 */
export function initializeSchema(db: SqliteService): void {
  // Ensure schema_meta exists so we can read the version
  db.exec('CREATE TABLE IF NOT EXISTS schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)');

  const row = db.get<{ value: string }>(`SELECT value FROM schema_meta WHERE key = 'version'`);
  const currentVersion = row ? parseInt(row.value, 10) : 0;

  if (currentVersion !== SCHEMA_VERSION) {
    // Drop all legacy tables from previous schema versions
    for (const table of LEGACY_TABLES) {
      try {
        db.exec(`DROP TABLE IF EXISTS ${table}`);
      } catch {
        /* ignore */
      }
    }

    // Drop all current-schema tables (including triggers & virtual tables)
    for (const table of CURRENT_TABLES) {
      try {
        db.exec(`DROP TABLE IF EXISTS ${table}`);
      } catch {
        /* ignore */
      }
    }

    // Also drop triggers explicitly (some may survive the table drops)
    try {
      db.exec('DROP TRIGGER IF EXISTS messages_ai');
    } catch {
      /* ignore */
    }
    try {
      db.exec('DROP TRIGGER IF EXISTS messages_ad');
    } catch {
      /* ignore */
    }
    try {
      db.exec('DROP TRIGGER IF EXISTS messages_au');
    } catch {
      /* ignore */
    }
    for (const trigger of [
      'timeline_dirty_ai',
      'timeline_dirty_ad',
      'timeline_dirty_au',
      'subagent_timeline_ai',
      'subagent_timeline_ad',
      'subagent_timeline_au',
      'subagent_dirty_ai',
      'subagent_dirty_ad',
      'subagent_dirty_au',
      'token_activity_messages_ai',
      'token_activity_messages_ad',
      'token_activity_messages_au',
      'token_activity_subagents_ai',
      'token_activity_subagents_ad',
      'token_activity_subagents_au',
      'token_activity_session_quality_au',
    ]) {
      try {
        db.exec(`DROP TRIGGER IF EXISTS ${trigger}`);
      } catch {
        /* ignore */
      }
    }

    // Recreate schema_meta
    db.exec('CREATE TABLE IF NOT EXISTS schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)');

    // Create all tables
    db.exec(SCHEMA_SQL);

    // Set version
    db.run(
      `INSERT INTO schema_meta (key, value) VALUES ('version', ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
      String(SCHEMA_VERSION),
    );
  } else {
    // Version matches — ensure all tables exist (IF NOT EXISTS makes this safe)
    db.exec(SCHEMA_SQL);
  }
}
