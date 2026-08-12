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
//
// v12: session/day token rollups. Native/TS ingestion owns materialization;
// query APIs are read-only and never scan canonical message tables.
//
// v13: timestamp-independent session summary totals, dirty-session tracking,
// and source-scoped materialization completion markers.
//
// v14: materialized Claude AI/custom session titles. Titles are projected at
// ingest time so list queries stay constant-time and live title changes appear
// without rescanning transcript JSON.
//
// v15: subagents.worktree_path. `SubagentMeta.worktreePath` has been parsed on
// both the TS and Rust sides for a while but was dropped on the floor at write
// time, so "which worktree is this agent working in" was unanswerable from the
// database even though it sat in the meta sidecar on disk.
//
// v17: RFC 011 source catalog, atomic ingest commits, projection readiness,
// record diagnostics, and durable change-log outbox. This TS DDL is a
// transitional migration mirror; Rust remains the target schema authority.
//
// v18: RFC 011 provenance-bearing fact storage, canonical shadow history,
// runtime evidence/state, and contribution-based usage totals.
//
// v19: RFC 011 delegation capability assertions and late-correlated canonical
// subagent relations.
//
// v20: replaceable subagent metadata assertions and late-correlated canonical
// delegation metadata.
//
// v21: native delegation spawn assertions and explicit spawn/metadata
// correlation provenance.
// v22: authoritative Claude team configuration and inbox snapshots with
// normalized membership/message projections and replacement provenance.
// v23: Claude active-session presence assertions, canonical current rows,
// process-incarnation identity, and conflict provenance.
// v24: replaceable task/todo/plan assertions with explicit snapshot coverage,
// canonical current rows, and conflict provenance.
// v25: transcript artifact metadata, independently replaceable file-history
// content, late-join availability, and conflict provenance.
// v26: workflow run snapshots, append journal member events, late joins, and
// workflow/member conflict provenance.
// v27: replaceable Claude session-index snapshots, normalized entry metadata,
// transcript late joins, and project/entry conflict provenance.
export const SCHEMA_VERSION = 27;

export const TOKEN_ACTIVITY_TRIGGER_NAMES = [
  'token_activity_messages_ai',
  'token_activity_messages_ad',
  'token_activity_messages_au',
  'token_activity_subagents_ai',
  'token_activity_subagents_ad',
  'token_activity_subagents_au',
  'token_activity_session_quality_au',
] as const;

export const SESSION_SUMMARY_TRIGGER_NAMES = [
  'session_summary_messages_ai',
  'session_summary_messages_ad',
  'session_summary_messages_au',
  'session_summary_subagents_ai',
  'session_summary_subagents_ad',
  'session_summary_subagents_au',
] as const;

/** Timestamp-independent invalidation for session/project list totals. */
export const SESSION_SUMMARY_TRIGGERS_SQL = `
CREATE TRIGGER IF NOT EXISTS session_summary_messages_ai AFTER INSERT ON messages
WHEN new.session_id IS NOT NULL AND new.project_slug IS NOT NULL BEGIN
  INSERT INTO session_summary_dirty(source_id, project_slug, session_id)
  VALUES (new.source_id, new.project_slug, new.session_id)
  ON CONFLICT(source_id, session_id) DO UPDATE SET project_slug = excluded.project_slug;
END;
CREATE TRIGGER IF NOT EXISTS session_summary_messages_ad AFTER DELETE ON messages
WHEN old.session_id IS NOT NULL AND old.project_slug IS NOT NULL BEGIN
  INSERT INTO session_summary_dirty(source_id, project_slug, session_id)
  VALUES (old.source_id, old.project_slug, old.session_id)
  ON CONFLICT(source_id, session_id) DO UPDATE SET project_slug = excluded.project_slug;
END;
CREATE TRIGGER IF NOT EXISTS session_summary_messages_au
AFTER UPDATE OF input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, source_id, project_slug, session_id ON messages
WHEN old.input_tokens IS NOT new.input_tokens
  OR old.output_tokens IS NOT new.output_tokens
  OR old.cache_creation_tokens IS NOT new.cache_creation_tokens
  OR old.cache_read_tokens IS NOT new.cache_read_tokens
  OR old.source_id IS NOT new.source_id
  OR old.project_slug IS NOT new.project_slug
  OR old.session_id IS NOT new.session_id BEGIN
  INSERT INTO session_summary_dirty(source_id, project_slug, session_id)
  SELECT old.source_id, old.project_slug, old.session_id
   WHERE old.session_id IS NOT NULL AND old.project_slug IS NOT NULL
  ON CONFLICT(source_id, session_id) DO UPDATE SET project_slug = excluded.project_slug;
  INSERT INTO session_summary_dirty(source_id, project_slug, session_id)
  SELECT new.source_id, new.project_slug, new.session_id
   WHERE new.session_id IS NOT NULL AND new.project_slug IS NOT NULL
  ON CONFLICT(source_id, session_id) DO UPDATE SET project_slug = excluded.project_slug;
END;
CREATE TRIGGER IF NOT EXISTS session_summary_subagents_ai AFTER INSERT ON subagent_messages
WHEN new.session_id IS NOT NULL AND new.project_slug IS NOT NULL BEGIN
  INSERT INTO session_summary_dirty(source_id, project_slug, session_id)
  VALUES (new.source_id, new.project_slug, new.session_id)
  ON CONFLICT(source_id, session_id) DO UPDATE SET project_slug = excluded.project_slug;
END;
CREATE TRIGGER IF NOT EXISTS session_summary_subagents_ad AFTER DELETE ON subagent_messages
WHEN old.session_id IS NOT NULL AND old.project_slug IS NOT NULL BEGIN
  INSERT INTO session_summary_dirty(source_id, project_slug, session_id)
  VALUES (old.source_id, old.project_slug, old.session_id)
  ON CONFLICT(source_id, session_id) DO UPDATE SET project_slug = excluded.project_slug;
END;
CREATE TRIGGER IF NOT EXISTS session_summary_subagents_au
AFTER UPDATE OF input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, source_id, project_slug, session_id ON subagent_messages
WHEN old.input_tokens IS NOT new.input_tokens
  OR old.output_tokens IS NOT new.output_tokens
  OR old.cache_creation_tokens IS NOT new.cache_creation_tokens
  OR old.cache_read_tokens IS NOT new.cache_read_tokens
  OR old.source_id IS NOT new.source_id
  OR old.project_slug IS NOT new.project_slug
  OR old.session_id IS NOT new.session_id BEGIN
  INSERT INTO session_summary_dirty(source_id, project_slug, session_id)
  SELECT old.source_id, old.project_slug, old.session_id
   WHERE old.session_id IS NOT NULL AND old.project_slug IS NOT NULL
  ON CONFLICT(source_id, session_id) DO UPDATE SET project_slug = excluded.project_slug;
  INSERT INTO session_summary_dirty(source_id, project_slug, session_id)
  SELECT new.source_id, new.project_slug, new.session_id
   WHERE new.session_id IS NOT NULL AND new.project_slug IS NOT NULL
  ON CONFLICT(source_id, session_id) DO UPDATE SET project_slug = excluded.project_slug;
END;
`;

/** Reused after bulk ingest temporarily suppresses per-row dirty markers. */
export const TOKEN_ACTIVITY_TRIGGERS_SQL = `
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
AFTER UPDATE OF timestamp, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, source_id, project_slug ON messages
WHEN old.timestamp IS NOT new.timestamp
  OR old.input_tokens IS NOT new.input_tokens
  OR old.output_tokens IS NOT new.output_tokens
  OR old.cache_creation_tokens IS NOT new.cache_creation_tokens
  OR old.cache_read_tokens IS NOT new.cache_read_tokens
  OR old.source_id IS NOT new.source_id
  OR old.project_slug IS NOT new.project_slug BEGIN
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
AFTER UPDATE OF timestamp, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, source_id, project_slug ON subagent_messages
WHEN old.timestamp IS NOT new.timestamp
  OR old.input_tokens IS NOT new.input_tokens
  OR old.output_tokens IS NOT new.output_tokens
  OR old.cache_creation_tokens IS NOT new.cache_creation_tokens
  OR old.cache_read_tokens IS NOT new.cache_read_tokens
  OR old.source_id IS NOT new.source_id
  OR old.project_slug IS NOT new.project_slug BEGIN
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  SELECT old.source_id, old.project_slug, substr(old.timestamp, 1, 10)
   WHERE old.timestamp IS NOT NULL AND length(old.timestamp) >= 10
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  SELECT new.source_id, new.project_slug, substr(new.timestamp, 1, 10)
   WHERE new.timestamp IS NOT NULL AND length(new.timestamp) >= 10
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
END;
CREATE TRIGGER IF NOT EXISTS token_activity_session_quality_au AFTER UPDATE OF tokens_estimated ON sessions
WHEN old.tokens_estimated IS NOT new.tokens_estimated BEGIN
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  SELECT source_id, project_slug, substr(timestamp, 1, 10) FROM messages
   WHERE session_id = new.id AND source_id = new.source_id AND timestamp IS NOT NULL AND length(timestamp) >= 10
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
  INSERT INTO token_activity_dirty(source_id, project_slug, activity_day)
  SELECT source_id, project_slug, substr(timestamp, 1, 10) FROM subagent_messages
   WHERE session_id = new.id AND source_id = new.source_id AND timestamp IS NOT NULL AND length(timestamp) >= 10
  ON CONFLICT(source_id, project_slug, activity_day) DO NOTHING;
END;
`;

export const SCHEMA_SQL = `
-- Meta
CREATE TABLE IF NOT EXISTS schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

-- Source file tracking
-- Fingerprints are owned per source. The key is (source_id, path), not path
-- alone: two agents can legitimately hold the same absolute path, and with a
-- path-only key one source's delete removed the other's row (RFC 008 P8).
CREATE TABLE IF NOT EXISTS source_files (
  path TEXT NOT NULL,
  source_id TEXT NOT NULL DEFAULT 'claude-code',
  mtime_ms REAL,
  size INTEGER,
  byte_position INTEGER,
  category TEXT,
  project_slug TEXT,
  session_id TEXT,
  PRIMARY KEY (source_id, path)
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
  ai_title TEXT NOT NULL DEFAULT '',
  custom_title TEXT NOT NULL DEFAULT '',
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
  -- Absolute path of the git worktree the agent ran in, from the meta
  -- sidecar's worktreePath. NULL for the vast majority of agents, which
  -- run directly in the project root.
  worktree_path TEXT,
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
  parent_message_count INTEGER NOT NULL DEFAULT 0,
  session_count INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (source_id, project_slug, activity_day)
);

CREATE TABLE IF NOT EXISTS token_activity_session_daily (
  source_id TEXT NOT NULL,
  project_slug TEXT NOT NULL,
  session_id TEXT NOT NULL,
  activity_day TEXT NOT NULL,
  input_tokens INTEGER NOT NULL DEFAULT 0,
  output_tokens INTEGER NOT NULL DEFAULT 0,
  cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
  cache_read_tokens INTEGER NOT NULL DEFAULT 0,
  total_tokens INTEGER NOT NULL DEFAULT 0,
  exact_tokens INTEGER NOT NULL DEFAULT 0,
  estimated_tokens INTEGER NOT NULL DEFAULT 0,
  message_count INTEGER NOT NULL DEFAULT 0,
  parent_message_count INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (source_id, session_id, activity_day)
);

CREATE TABLE IF NOT EXISTS token_activity_dirty (
  source_id TEXT NOT NULL,
  project_slug TEXT NOT NULL,
  activity_day TEXT NOT NULL,
  PRIMARY KEY (source_id, project_slug, activity_day)
);

CREATE TABLE IF NOT EXISTS session_summary_totals (
  source_id TEXT NOT NULL,
  project_slug TEXT NOT NULL,
  session_id TEXT NOT NULL,
  input_tokens INTEGER NOT NULL DEFAULT 0,
  output_tokens INTEGER NOT NULL DEFAULT 0,
  cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
  cache_read_tokens INTEGER NOT NULL DEFAULT 0,
  parent_message_count INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (source_id, session_id)
);

CREATE TABLE IF NOT EXISTS session_summary_dirty (
  source_id TEXT NOT NULL,
  project_slug TEXT NOT NULL,
  session_id TEXT NOT NULL,
  PRIMARY KEY (source_id, session_id)
);

CREATE TABLE IF NOT EXISTS source_materializations (
  source_id TEXT NOT NULL,
  projection TEXT NOT NULL,
  version INTEGER NOT NULL,
  completed_at INTEGER NOT NULL,
  PRIMARY KEY (source_id, projection)
);

-- RFC 011 source catalog. Binary source keys and cursors are intentionally
-- BLOB-capable: filesystem identities and adapter cursors are not universally
-- UTF-8 strings or integers.
CREATE TABLE IF NOT EXISTS source_instances (
  source_instance_id INTEGER PRIMARY KEY,
  adapter_id TEXT NOT NULL,
  stable_key BLOB NOT NULL,
  display_name TEXT NOT NULL,
  adapter_contract_version INTEGER NOT NULL,
  discovered_at INTEGER NOT NULL,
  last_seen_at INTEGER NOT NULL,
  UNIQUE(adapter_id, stable_key)
);

CREATE TABLE IF NOT EXISTS source_streams (
  source_stream_id INTEGER PRIMARY KEY,
  source_instance_id INTEGER NOT NULL REFERENCES source_instances(source_instance_id) ON DELETE CASCADE,
  stream_key TEXT NOT NULL,
  driver_kind TEXT NOT NULL,
  decoder_key TEXT NOT NULL,
  stream_state TEXT NOT NULL,
  last_reconciled_at INTEGER,
  last_commit_seq INTEGER,
  UNIQUE(source_instance_id, stream_key)
);

CREATE TABLE IF NOT EXISTS source_objects (
  source_object_id INTEGER PRIMARY KEY,
  source_stream_id INTEGER NOT NULL REFERENCES source_streams(source_stream_id) ON DELETE CASCADE,
  object_key BLOB NOT NULL,
  display_path TEXT,
  native_identity BLOB,
  generation INTEGER NOT NULL,
  committed_cursor BLOB NOT NULL,
  observed_revision BLOB,
  adapter_object_context BLOB,
  decoder_state BLOB,
  decoder_state_version INTEGER,
  size_bytes INTEGER,
  mtime_ns INTEGER,
  decoder_contract_version INTEGER NOT NULL,
  last_commit_seq INTEGER,
  state TEXT NOT NULL,
  UNIQUE(source_stream_id, object_key)
);

CREATE TABLE IF NOT EXISTS ingest_commits (
  commit_seq INTEGER PRIMARY KEY AUTOINCREMENT,
  source_instance_id INTEGER NOT NULL REFERENCES source_instances(source_instance_id) ON DELETE RESTRICT,
  reason TEXT NOT NULL,
  started_at INTEGER NOT NULL,
  committed_at INTEGER,
  fact_count INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS change_log (
  commit_seq INTEGER NOT NULL REFERENCES ingest_commits(commit_seq) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL,
  topic TEXT NOT NULL,
  schema_version INTEGER NOT NULL,
  entity_key BLOB NOT NULL,
  operation TEXT NOT NULL,
  payload BLOB NOT NULL,
  PRIMARY KEY (commit_seq, ordinal)
);

CREATE TABLE IF NOT EXISTS projection_versions (
  projection_id TEXT NOT NULL,
  scope_key BLOB NOT NULL,
  desired_version INTEGER NOT NULL,
  completed_version INTEGER,
  readiness TEXT NOT NULL,
  last_commit_seq INTEGER,
  updated_at INTEGER NOT NULL,
  detail TEXT,
  PRIMARY KEY (projection_id, scope_key)
);

CREATE TABLE IF NOT EXISTS source_record_errors (
  source_object_id INTEGER NOT NULL REFERENCES source_objects(source_object_id) ON DELETE CASCADE,
  generation INTEGER NOT NULL,
  cursor_start BLOB NOT NULL,
  cursor_end BLOB NOT NULL,
  payload_hash BLOB NOT NULL,
  media_type TEXT NOT NULL,
  raw_payload BLOB,
  error_class TEXT NOT NULL,
  error_message TEXT NOT NULL,
  adapter_version TEXT NOT NULL,
  contract_version INTEGER NOT NULL,
  first_commit_seq INTEGER NOT NULL,
  last_retry_at INTEGER,
  retry_count INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (source_object_id, generation, cursor_start, cursor_end)
);

-- RFC 011 typed-fact audit store and common projections. Adapters emit the
-- facts; only the common writer maps them into these tables.
CREATE TABLE IF NOT EXISTS fact_records (
  fact_id BLOB PRIMARY KEY,
  fact_kind TEXT NOT NULL,
  entity_key BLOB,
  source_instance_id INTEGER NOT NULL REFERENCES source_instances(source_instance_id) ON DELETE CASCADE,
  source_stream_id INTEGER NOT NULL REFERENCES source_streams(source_stream_id) ON DELETE CASCADE,
  source_object_id INTEGER NOT NULL REFERENCES source_objects(source_object_id) ON DELETE CASCADE,
  source_generation INTEGER NOT NULL,
  cursor_start BLOB NOT NULL,
  cursor_end BLOB NOT NULL,
  payload_hash BLOB NOT NULL,
  local_fact_ordinal INTEGER NOT NULL,
  observed_at INTEGER NOT NULL,
  payload_json BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL REFERENCES ingest_commits(commit_seq) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS canonical_sessions (
  session_key BLOB PRIMARY KEY,
  project_key BLOB NOT NULL,
  native_session_id TEXT NOT NULL,
  native_project_key TEXT NOT NULL,
  cwd TEXT,
  git_branch TEXT,
  first_prompt TEXT,
  ai_title TEXT,
  custom_title TEXT,
  source_time TEXT,
  source_time_quality TEXT,
  fact_id BLOB NOT NULL REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS session_index_snapshot_assertions (
  fact_id BLOB PRIMARY KEY REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  project_key BLOB NOT NULL,
  native_project_key TEXT NOT NULL,
  native_version INTEGER NOT NULL,
  original_path TEXT,
  native_snapshot_json BLOB NOT NULL,
  snapshot_digest BLOB NOT NULL,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS session_index_entry_assertions (
  fact_id BLOB NOT NULL REFERENCES session_index_snapshot_assertions(fact_id) ON DELETE CASCADE,
  session_key BLOB NOT NULL,
  project_key BLOB NOT NULL,
  entry_ordinal INTEGER NOT NULL,
  native_session_id TEXT NOT NULL,
  full_path TEXT NOT NULL,
  file_mtime_ms INTEGER NOT NULL,
  first_prompt TEXT NOT NULL,
  summary TEXT,
  message_count INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  created_at_quality TEXT NOT NULL,
  modified_at TEXT NOT NULL,
  modified_at_quality TEXT NOT NULL,
  git_branch TEXT NOT NULL,
  project_path TEXT NOT NULL,
  is_sidechain INTEGER NOT NULL,
  entry_digest BLOB NOT NULL,
  PRIMARY KEY (fact_id, session_key)
);

CREATE TABLE IF NOT EXISTS canonical_session_indexes (
  project_key BLOB PRIMARY KEY,
  native_project_key TEXT NOT NULL,
  native_version INTEGER NOT NULL,
  original_path TEXT,
  native_snapshot_json BLOB NOT NULL,
  index_status TEXT NOT NULL,
  decisive_fact_id BLOB NOT NULL REFERENCES session_index_snapshot_assertions(fact_id) ON DELETE CASCADE,
  assertion_count INTEGER NOT NULL,
  competing_snapshot_count INTEGER NOT NULL,
  entry_count INTEGER NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS canonical_session_index_entries (
  session_key BLOB PRIMARY KEY,
  project_key BLOB NOT NULL,
  entry_ordinal INTEGER NOT NULL,
  native_session_id TEXT NOT NULL,
  full_path TEXT NOT NULL,
  file_mtime_ms INTEGER NOT NULL,
  first_prompt TEXT NOT NULL,
  summary TEXT,
  message_count INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  created_at_quality TEXT NOT NULL,
  modified_at TEXT NOT NULL,
  modified_at_quality TEXT NOT NULL,
  git_branch TEXT NOT NULL,
  project_path TEXT NOT NULL,
  is_sidechain INTEGER NOT NULL,
  transcript_status TEXT NOT NULL,
  resolution_status TEXT NOT NULL,
  decisive_fact_id BLOB NOT NULL REFERENCES session_index_snapshot_assertions(fact_id) ON DELETE CASCADE,
  assertion_count INTEGER NOT NULL,
  competing_entry_count INTEGER NOT NULL,
  identity_conflict INTEGER NOT NULL,
  join_conflict INTEGER NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS canonical_messages (
  message_key BLOB PRIMARY KEY,
  session_key BLOB NOT NULL,
  native_message_id TEXT,
  native_kind TEXT NOT NULL,
  role TEXT NOT NULL,
  content_json BLOB NOT NULL,
  source_time TEXT,
  source_time_quality TEXT,
  parent_native_message_id TEXT,
  model TEXT,
  search_text TEXT,
  raw_json BLOB NOT NULL,
  fact_id BLOB NOT NULL REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_start BLOB NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS canonical_runs (
  run_key BLOB PRIMARY KEY,
  session_key BLOB NOT NULL,
  native_run_id TEXT NOT NULL,
  parent_run_key BLOB,
  fact_id BLOB NOT NULL REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS run_evidence (
  fact_id BLOB PRIMARY KEY REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  run_key BLOB NOT NULL,
  evidence_kind TEXT NOT NULL,
  evidence_strength TEXT NOT NULL,
  native_state TEXT,
  source_time TEXT,
  source_time_quality TEXT,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS observed_run_states (
  run_key BLOB PRIMARY KEY,
  state TEXT NOT NULL,
  decisive_evidence_id BLOB NOT NULL REFERENCES run_evidence(fact_id) ON DELETE CASCADE,
  last_activity_at TEXT,
  terminal_at TEXT,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS presence_assertions (
  fact_id BLOB PRIMARY KEY REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  presence_key BLOB NOT NULL,
  session_key BLOB NOT NULL,
  run_key BLOB NOT NULL,
  native_session_id TEXT NOT NULL,
  native_pid INTEGER NOT NULL,
  cwd TEXT NOT NULL,
  started_at TEXT NOT NULL,
  started_at_quality TEXT NOT NULL,
  native_kind TEXT,
  entrypoint TEXT,
  name TEXT,
  native_status TEXT,
  updated_at TEXT,
  updated_at_quality TEXT,
  status_updated_at TEXT,
  status_updated_at_quality TEXT,
  native_process_started_at TEXT,
  version TEXT,
  peer_protocol INTEGER,
  name_source TEXT,
  bridge_session_id TEXT,
  messaging_socket_path TEXT,
  presence_digest BLOB NOT NULL,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL,
  CHECK (native_pid > 0)
);

CREATE TABLE IF NOT EXISTS canonical_presences (
  presence_key BLOB PRIMARY KEY,
  session_key BLOB NOT NULL,
  run_key BLOB NOT NULL,
  native_session_id TEXT NOT NULL,
  native_pid INTEGER NOT NULL,
  cwd TEXT NOT NULL,
  started_at TEXT NOT NULL,
  started_at_quality TEXT NOT NULL,
  native_kind TEXT,
  entrypoint TEXT,
  name TEXT,
  native_status TEXT,
  updated_at TEXT,
  updated_at_quality TEXT,
  status_updated_at TEXT,
  status_updated_at_quality TEXT,
  native_process_started_at TEXT,
  version TEXT,
  peer_protocol INTEGER,
  name_source TEXT,
  bridge_session_id TEXT,
  messaging_socket_path TEXT,
  presence_status TEXT NOT NULL,
  decisive_fact_id BLOB NOT NULL REFERENCES presence_assertions(fact_id) ON DELETE CASCADE,
  assertion_count INTEGER NOT NULL,
  competing_assertion_count INTEGER NOT NULL,
  last_commit_seq INTEGER NOT NULL,
  CHECK (native_pid > 0)
);

CREATE TABLE IF NOT EXISTS delegation_assertions (
  fact_id BLOB PRIMARY KEY REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  child_run_key BLOB NOT NULL,
  parent_run_key BLOB,
  session_key BLOB NOT NULL,
  relation_kind TEXT NOT NULL,
  relation_strength TEXT NOT NULL,
  native_child_id TEXT,
  native_task_id TEXT,
  label TEXT,
  prompt TEXT,
  cwd TEXT,
  worktree_path TEXT,
  source_time TEXT,
  source_time_quality TEXT,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS delegation_metadata_assertions (
  fact_id BLOB PRIMARY KEY REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  child_run_key BLOB NOT NULL,
  session_key BLOB NOT NULL,
  native_child_id TEXT NOT NULL,
  agent_type TEXT NOT NULL,
  description TEXT,
  native_name TEXT,
  spawn_depth INTEGER,
  worktree_path TEXT,
  native_task_id TEXT,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS delegation_spawn_assertions (
  fact_id BLOB PRIMARY KEY REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  spawn_key BLOB NOT NULL,
  parent_run_key BLOB NOT NULL,
  parent_message_key BLOB NOT NULL,
  session_key BLOB NOT NULL,
  native_task_id TEXT NOT NULL,
  tool_name TEXT NOT NULL,
  label TEXT,
  prompt TEXT,
  requested_agent_type TEXT,
  source_time TEXT,
  source_time_quality TEXT,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS canonical_delegations (
  child_run_key BLOB PRIMARY KEY,
  parent_run_key BLOB,
  session_key BLOB NOT NULL,
  relation_kind TEXT NOT NULL,
  relation_strength TEXT NOT NULL,
  relation_status TEXT NOT NULL,
  native_child_id TEXT,
  native_task_id TEXT,
  label TEXT,
  prompt TEXT,
  cwd TEXT,
  worktree_path TEXT,
  source_time TEXT,
  source_time_quality TEXT,
  decisive_relation_fact_id BLOB REFERENCES delegation_assertions(fact_id) ON DELETE CASCADE,
  decisive_spawn_fact_id BLOB REFERENCES delegation_spawn_assertions(fact_id) ON DELETE CASCADE,
  decisive_metadata_fact_id BLOB REFERENCES delegation_metadata_assertions(fact_id) ON DELETE CASCADE,
  assertion_count INTEGER NOT NULL,
  competing_relation_count INTEGER NOT NULL,
  child_present INTEGER NOT NULL,
  parent_present INTEGER NOT NULL,
  last_commit_seq INTEGER NOT NULL,
  CHECK (
    (decisive_relation_fact_id IS NOT NULL AND decisive_spawn_fact_id IS NULL AND decisive_metadata_fact_id IS NULL)
    OR
    (decisive_relation_fact_id IS NULL AND decisive_spawn_fact_id IS NOT NULL AND decisive_metadata_fact_id IS NOT NULL)
  )
);

CREATE TABLE IF NOT EXISTS canonical_delegation_metadata (
  child_run_key BLOB PRIMARY KEY,
  session_key BLOB NOT NULL,
  native_child_id TEXT NOT NULL,
  agent_type TEXT NOT NULL,
  description TEXT,
  native_name TEXT,
  spawn_depth INTEGER,
  worktree_path TEXT,
  native_task_id TEXT,
  metadata_status TEXT NOT NULL,
  decisive_fact_id BLOB NOT NULL REFERENCES delegation_metadata_assertions(fact_id) ON DELETE CASCADE,
  assertion_count INTEGER NOT NULL,
  competing_metadata_count INTEGER NOT NULL,
  run_present INTEGER NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS canonical_delegation_spawns (
  spawn_key BLOB PRIMARY KEY,
  parent_run_key BLOB NOT NULL,
  parent_message_key BLOB NOT NULL,
  session_key BLOB NOT NULL,
  native_task_id TEXT NOT NULL,
  tool_name TEXT NOT NULL,
  label TEXT,
  prompt TEXT,
  requested_agent_type TEXT,
  source_time TEXT,
  source_time_quality TEXT,
  spawn_status TEXT NOT NULL,
  decisive_fact_id BLOB NOT NULL REFERENCES delegation_spawn_assertions(fact_id) ON DELETE CASCADE,
  assertion_count INTEGER NOT NULL,
  competing_spawn_count INTEGER NOT NULL,
  parent_present INTEGER NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS team_snapshot_assertions (
  fact_id BLOB PRIMARY KEY REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  team_key BLOB NOT NULL,
  native_team_id TEXT NOT NULL,
  name TEXT NOT NULL,
  description TEXT,
  created_at TEXT NOT NULL,
  created_at_quality TEXT NOT NULL,
  lead_member_key BLOB,
  native_lead_agent_id TEXT NOT NULL,
  lead_session_key BLOB NOT NULL,
  native_lead_session_id TEXT NOT NULL,
  snapshot_digest BLOB NOT NULL,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS team_member_assertions (
  fact_id BLOB NOT NULL REFERENCES team_snapshot_assertions(fact_id) ON DELETE CASCADE,
  member_key BLOB NOT NULL,
  team_key BLOB NOT NULL,
  member_ordinal INTEGER NOT NULL,
  native_agent_id TEXT NOT NULL,
  native_name TEXT NOT NULL,
  agent_type TEXT,
  model TEXT,
  prompt TEXT,
  color TEXT,
  plan_mode_required INTEGER,
  joined_at TEXT NOT NULL,
  joined_at_quality TEXT NOT NULL,
  tmux_pane_id TEXT NOT NULL,
  cwd TEXT NOT NULL,
  subscriptions_json BLOB NOT NULL,
  backend_type TEXT,
  member_digest BLOB NOT NULL,
  PRIMARY KEY (fact_id, member_key)
);

CREATE TABLE IF NOT EXISTS canonical_teams (
  team_key BLOB PRIMARY KEY,
  native_team_id TEXT NOT NULL,
  name TEXT NOT NULL,
  description TEXT,
  created_at TEXT NOT NULL,
  created_at_quality TEXT NOT NULL,
  lead_member_key BLOB,
  native_lead_agent_id TEXT NOT NULL,
  lead_session_key BLOB NOT NULL,
  native_lead_session_id TEXT NOT NULL,
  config_status TEXT NOT NULL,
  decisive_fact_id BLOB NOT NULL REFERENCES team_snapshot_assertions(fact_id) ON DELETE CASCADE,
  assertion_count INTEGER NOT NULL,
  competing_snapshot_count INTEGER NOT NULL,
  member_count INTEGER NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS canonical_team_members (
  member_key BLOB PRIMARY KEY,
  team_key BLOB NOT NULL,
  member_ordinal INTEGER NOT NULL,
  native_agent_id TEXT NOT NULL,
  native_name TEXT NOT NULL,
  agent_type TEXT,
  model TEXT,
  prompt TEXT,
  color TEXT,
  plan_mode_required INTEGER,
  joined_at TEXT NOT NULL,
  joined_at_quality TEXT NOT NULL,
  tmux_pane_id TEXT NOT NULL,
  cwd TEXT NOT NULL,
  subscriptions_json BLOB NOT NULL,
  backend_type TEXT,
  membership_status TEXT NOT NULL,
  decisive_fact_id BLOB NOT NULL REFERENCES team_snapshot_assertions(fact_id) ON DELETE CASCADE,
  assertion_count INTEGER NOT NULL,
  competing_membership_count INTEGER NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS team_inbox_snapshot_assertions (
  fact_id BLOB PRIMARY KEY REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  inbox_key BLOB NOT NULL,
  team_key BLOB NOT NULL,
  recipient_key BLOB NOT NULL,
  native_team_id TEXT NOT NULL,
  native_recipient_name TEXT NOT NULL,
  snapshot_digest BLOB NOT NULL,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS team_inbox_message_assertions (
  fact_id BLOB NOT NULL REFERENCES team_inbox_snapshot_assertions(fact_id) ON DELETE CASCADE,
  message_key BLOB NOT NULL,
  inbox_key BLOB NOT NULL,
  message_ordinal INTEGER NOT NULL,
  sender_key BLOB NOT NULL,
  native_message_id TEXT,
  native_kind TEXT,
  native_version INTEGER,
  native_sender_name TEXT NOT NULL,
  text TEXT NOT NULL,
  summary TEXT,
  color TEXT,
  source_time TEXT NOT NULL,
  source_time_quality TEXT NOT NULL,
  read INTEGER NOT NULL,
  message_digest BLOB NOT NULL,
  PRIMARY KEY (fact_id, message_key)
);

CREATE TABLE IF NOT EXISTS canonical_team_inboxes (
  inbox_key BLOB PRIMARY KEY,
  team_key BLOB NOT NULL,
  recipient_key BLOB NOT NULL,
  native_team_id TEXT NOT NULL,
  native_recipient_name TEXT NOT NULL,
  inbox_status TEXT NOT NULL,
  decisive_fact_id BLOB NOT NULL REFERENCES team_inbox_snapshot_assertions(fact_id) ON DELETE CASCADE,
  assertion_count INTEGER NOT NULL,
  competing_snapshot_count INTEGER NOT NULL,
  message_count INTEGER NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS canonical_team_inbox_messages (
  message_key BLOB PRIMARY KEY,
  inbox_key BLOB NOT NULL,
  message_ordinal INTEGER NOT NULL,
  sender_key BLOB NOT NULL,
  native_message_id TEXT,
  native_kind TEXT,
  native_version INTEGER,
  native_sender_name TEXT NOT NULL,
  text TEXT NOT NULL,
  summary TEXT,
  color TEXT,
  source_time TEXT NOT NULL,
  source_time_quality TEXT NOT NULL,
  read INTEGER NOT NULL,
  message_status TEXT NOT NULL,
  decisive_fact_id BLOB NOT NULL REFERENCES team_inbox_snapshot_assertions(fact_id) ON DELETE CASCADE,
  assertion_count INTEGER NOT NULL,
  competing_message_count INTEGER NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS task_snapshot_assertions (
  fact_id BLOB PRIMARY KEY REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  collection_key BLOB NOT NULL,
  session_key BLOB,
  run_key BLOB,
  team_key BLOB,
  native_collection_id TEXT NOT NULL,
  native_owner_id TEXT,
  collection_kind TEXT NOT NULL,
  native_collection_kind TEXT NOT NULL,
  coverage TEXT NOT NULL,
  metadata_digest BLOB NOT NULL,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS task_item_assertions (
  fact_id BLOB NOT NULL REFERENCES task_snapshot_assertions(fact_id) ON DELETE CASCADE,
  task_key BLOB NOT NULL,
  collection_key BLOB NOT NULL,
  item_ordinal INTEGER NOT NULL,
  native_task_id TEXT,
  subject TEXT NOT NULL,
  description TEXT,
  active_form TEXT,
  native_owner TEXT,
  task_status TEXT NOT NULL,
  native_status TEXT NOT NULL,
  blocks_json BLOB NOT NULL,
  blocked_by_json BLOB NOT NULL,
  item_digest BLOB NOT NULL,
  PRIMARY KEY (fact_id, task_key)
);

CREATE TABLE IF NOT EXISTS canonical_task_collections (
  collection_key BLOB PRIMARY KEY,
  session_key BLOB,
  run_key BLOB,
  team_key BLOB,
  native_collection_id TEXT NOT NULL,
  native_owner_id TEXT,
  collection_kind TEXT NOT NULL,
  native_collection_kind TEXT NOT NULL,
  resolution_status TEXT NOT NULL,
  decisive_fact_id BLOB NOT NULL REFERENCES task_snapshot_assertions(fact_id) ON DELETE CASCADE,
  assertion_count INTEGER NOT NULL,
  competing_metadata_count INTEGER NOT NULL,
  complete_snapshot_count INTEGER NOT NULL,
  item_document_count INTEGER NOT NULL,
  item_count INTEGER NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS canonical_tasks (
  task_key BLOB PRIMARY KEY,
  collection_key BLOB NOT NULL,
  item_ordinal INTEGER NOT NULL,
  native_task_id TEXT,
  subject TEXT NOT NULL,
  description TEXT,
  active_form TEXT,
  native_owner TEXT,
  task_status TEXT NOT NULL,
  native_status TEXT NOT NULL,
  blocks_json BLOB NOT NULL,
  blocked_by_json BLOB NOT NULL,
  resolution_status TEXT NOT NULL,
  decisive_fact_id BLOB NOT NULL REFERENCES task_snapshot_assertions(fact_id) ON DELETE CASCADE,
  assertion_count INTEGER NOT NULL,
  competing_item_count INTEGER NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS plan_assertions (
  fact_id BLOB PRIMARY KEY REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  plan_key BLOB NOT NULL,
  native_plan_id TEXT NOT NULL,
  title TEXT NOT NULL,
  content TEXT NOT NULL,
  size_bytes INTEGER NOT NULL,
  source_time TEXT,
  source_time_quality TEXT,
  plan_digest BLOB NOT NULL,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS canonical_plans (
  plan_key BLOB PRIMARY KEY,
  native_plan_id TEXT NOT NULL,
  title TEXT NOT NULL,
  content TEXT NOT NULL,
  size_bytes INTEGER NOT NULL,
  source_time TEXT,
  source_time_quality TEXT,
  resolution_status TEXT NOT NULL,
  decisive_fact_id BLOB NOT NULL REFERENCES plan_assertions(fact_id) ON DELETE CASCADE,
  assertion_count INTEGER NOT NULL,
  competing_plan_count INTEGER NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS artifact_snapshot_assertions (
  fact_id BLOB PRIMARY KEY REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  session_key BLOB NOT NULL,
  native_message_id TEXT NOT NULL,
  native_snapshot_message_id TEXT NOT NULL,
  observation_kind TEXT NOT NULL,
  is_snapshot_update INTEGER NOT NULL,
  source_time TEXT,
  source_time_quality TEXT,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS artifact_metadata_assertions (
  fact_id BLOB NOT NULL REFERENCES artifact_snapshot_assertions(fact_id) ON DELETE CASCADE,
  artifact_key BLOB NOT NULL,
  session_key BLOB NOT NULL,
  native_artifact_id TEXT,
  tracking_path TEXT NOT NULL,
  real_parent_dir TEXT,
  version INTEGER NOT NULL,
  backup_time TEXT NOT NULL,
  backup_time_quality TEXT NOT NULL,
  capture_status TEXT NOT NULL,
  metadata_digest BLOB NOT NULL,
  PRIMARY KEY (fact_id, artifact_key)
);

CREATE TABLE IF NOT EXISTS artifact_content_assertions (
  fact_id BLOB PRIMARY KEY REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  artifact_key BLOB NOT NULL,
  session_key BLOB NOT NULL,
  native_artifact_id TEXT NOT NULL,
  native_file_hash TEXT NOT NULL,
  version INTEGER NOT NULL,
  content BLOB NOT NULL,
  size_bytes INTEGER NOT NULL,
  content_digest BLOB NOT NULL,
  assertion_digest BLOB NOT NULL,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS canonical_artifacts (
  artifact_key BLOB PRIMARY KEY,
  session_key BLOB NOT NULL,
  native_artifact_id TEXT,
  native_file_hash TEXT,
  version INTEGER NOT NULL,
  tracking_path TEXT,
  real_parent_dir TEXT,
  backup_time TEXT,
  backup_time_quality TEXT,
  capture_status TEXT NOT NULL,
  content BLOB,
  size_bytes INTEGER,
  content_digest BLOB,
  content_status TEXT NOT NULL,
  resolution_status TEXT NOT NULL,
  decisive_metadata_fact_id BLOB REFERENCES artifact_snapshot_assertions(fact_id) ON DELETE CASCADE,
  decisive_content_fact_id BLOB REFERENCES artifact_content_assertions(fact_id) ON DELETE CASCADE,
  metadata_assertion_count INTEGER NOT NULL,
  competing_metadata_count INTEGER NOT NULL,
  content_assertion_count INTEGER NOT NULL,
  competing_content_count INTEGER NOT NULL,
  join_conflict INTEGER NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS workflow_snapshot_assertions (
  fact_id BLOB PRIMARY KEY REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  workflow_key BLOB NOT NULL,
  session_key BLOB NOT NULL,
  project_key BLOB NOT NULL,
  native_workflow_id TEXT NOT NULL,
  native_task_id TEXT NOT NULL,
  name TEXT NOT NULL,
  native_status TEXT NOT NULL,
  workflow_status TEXT NOT NULL,
  default_model TEXT NOT NULL,
  script TEXT NOT NULL,
  script_path TEXT NOT NULL,
  args TEXT,
  summary TEXT NOT NULL,
  error TEXT,
  started_at TEXT NOT NULL,
  started_at_quality TEXT NOT NULL,
  finished_at TEXT NOT NULL,
  finished_at_quality TEXT NOT NULL,
  duration_ms INTEGER NOT NULL,
  agent_count INTEGER NOT NULL,
  total_tokens INTEGER NOT NULL,
  total_tool_calls INTEGER NOT NULL,
  native_snapshot_json BLOB NOT NULL,
  snapshot_digest BLOB NOT NULL,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS workflow_member_event_assertions (
  fact_id BLOB PRIMARY KEY REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  workflow_key BLOB NOT NULL,
  member_key BLOB NOT NULL,
  child_run_key BLOB NOT NULL,
  session_key BLOB NOT NULL,
  project_key BLOB NOT NULL,
  native_workflow_id TEXT NOT NULL,
  native_agent_id TEXT NOT NULL,
  native_event_key TEXT NOT NULL,
  event_kind TEXT NOT NULL,
  result_json BLOB,
  event_digest BLOB NOT NULL,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS canonical_workflows (
  workflow_key BLOB PRIMARY KEY,
  session_key BLOB NOT NULL,
  project_key BLOB NOT NULL,
  native_workflow_id TEXT NOT NULL,
  native_task_id TEXT,
  name TEXT,
  native_status TEXT,
  workflow_status TEXT,
  default_model TEXT,
  script TEXT,
  script_path TEXT,
  args TEXT,
  summary TEXT,
  error TEXT,
  started_at TEXT,
  started_at_quality TEXT,
  finished_at TEXT,
  finished_at_quality TEXT,
  duration_ms INTEGER,
  agent_count INTEGER,
  total_tokens INTEGER,
  total_tool_calls INTEGER,
  native_snapshot_json BLOB,
  snapshot_status TEXT NOT NULL,
  resolution_status TEXT NOT NULL,
  decisive_snapshot_fact_id BLOB REFERENCES workflow_snapshot_assertions(fact_id) ON DELETE CASCADE,
  snapshot_assertion_count INTEGER NOT NULL,
  competing_snapshot_count INTEGER NOT NULL,
  observed_member_count INTEGER NOT NULL,
  started_member_count INTEGER NOT NULL,
  result_member_count INTEGER NOT NULL,
  unresolved_member_count INTEGER NOT NULL,
  conflicting_member_count INTEGER NOT NULL,
  membership_count_status TEXT NOT NULL,
  join_conflict INTEGER NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS canonical_workflow_members (
  member_key BLOB PRIMARY KEY,
  workflow_key BLOB NOT NULL,
  child_run_key BLOB NOT NULL,
  session_key BLOB NOT NULL,
  project_key BLOB NOT NULL,
  native_workflow_id TEXT NOT NULL,
  native_agent_id TEXT NOT NULL,
  native_event_key TEXT NOT NULL,
  member_status TEXT NOT NULL,
  result_json BLOB,
  resolution_status TEXT NOT NULL,
  decisive_started_fact_id BLOB REFERENCES workflow_member_event_assertions(fact_id) ON DELETE CASCADE,
  decisive_result_fact_id BLOB REFERENCES workflow_member_event_assertions(fact_id) ON DELETE CASCADE,
  started_assertion_count INTEGER NOT NULL,
  competing_started_count INTEGER NOT NULL,
  result_assertion_count INTEGER NOT NULL,
  competing_result_count INTEGER NOT NULL,
  event_key_conflict INTEGER NOT NULL,
  identity_conflict INTEGER NOT NULL,
  last_commit_seq INTEGER NOT NULL
);


CREATE TABLE IF NOT EXISTS usage_contributions (
  fact_id BLOB PRIMARY KEY REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  subject_key BLOB NOT NULL,
  session_key BLOB NOT NULL,
  scope TEXT NOT NULL,
  accounting TEXT NOT NULL,
  quality TEXT NOT NULL,
  quality_bucket TEXT NOT NULL,
  input_tokens INTEGER NOT NULL,
  output_tokens INTEGER NOT NULL,
  cache_creation_tokens INTEGER NOT NULL,
  cache_read_tokens INTEGER NOT NULL,
  model TEXT,
  source_time TEXT,
  source_time_quality TEXT,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS usage_totals (
  session_key BLOB PRIMARY KEY,
  exact_input_tokens INTEGER NOT NULL DEFAULT 0,
  exact_output_tokens INTEGER NOT NULL DEFAULT 0,
  exact_cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
  exact_cache_read_tokens INTEGER NOT NULL DEFAULT 0,
  estimated_input_tokens INTEGER NOT NULL DEFAULT 0,
  estimated_output_tokens INTEGER NOT NULL DEFAULT 0,
  estimated_cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
  estimated_cache_read_tokens INTEGER NOT NULL DEFAULT 0,
  last_commit_seq INTEGER NOT NULL
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
CREATE INDEX IF NOT EXISTS idx_sessions_project_source_modified ON sessions(project_slug, source_id, modified_at DESC);
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
CREATE INDEX IF NOT EXISTS idx_token_activity_session_project ON token_activity_session_daily(project_slug, source_id, session_id, activity_day);
CREATE INDEX IF NOT EXISTS idx_session_summary_project ON session_summary_totals(project_slug, source_id, session_id);
CREATE INDEX IF NOT EXISTS idx_subagent_timeline_thread ON subagent_timeline_messages(source_id, session_id, workflow_id, agent_id, timeline_index);
CREATE INDEX IF NOT EXISTS idx_subagent_timeline_type ON subagent_timeline_messages(source_id, session_id, display_type);
CREATE INDEX IF NOT EXISTS idx_subagent_timeline_tool ON subagent_timeline_messages(source_id, session_id, tool_name);
CREATE INDEX IF NOT EXISTS idx_tool_results_session ON tool_results(project_slug, session_id);
CREATE INDEX IF NOT EXISTS idx_todos_session ON todos(session_id);
CREATE INDEX IF NOT EXISTS idx_source_streams_instance_state ON source_streams(source_instance_id, stream_state);
CREATE INDEX IF NOT EXISTS idx_source_objects_stream_state ON source_objects(source_stream_id, state);
CREATE INDEX IF NOT EXISTS idx_source_objects_last_commit ON source_objects(last_commit_seq);
CREATE INDEX IF NOT EXISTS idx_ingest_commits_source_seq ON ingest_commits(source_instance_id, commit_seq);
CREATE INDEX IF NOT EXISTS idx_change_log_topic_cursor ON change_log(topic, commit_seq, ordinal);
CREATE INDEX IF NOT EXISTS idx_projection_versions_readiness ON projection_versions(readiness, projection_id);
CREATE INDEX IF NOT EXISTS idx_source_record_errors_commit ON source_record_errors(first_commit_seq);
CREATE INDEX IF NOT EXISTS idx_fact_records_object_generation ON fact_records(source_object_id, source_generation);
CREATE INDEX IF NOT EXISTS idx_fact_records_entity_kind ON fact_records(entity_key, fact_kind);
CREATE INDEX IF NOT EXISTS idx_canonical_sessions_project ON canonical_sessions(project_key, session_key);
CREATE INDEX IF NOT EXISTS idx_session_index_snapshot_assertions_project ON session_index_snapshot_assertions(project_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_session_index_snapshot_assertions_source ON session_index_snapshot_assertions(source_object_id, project_key);
CREATE INDEX IF NOT EXISTS idx_session_index_entry_assertions_project ON session_index_entry_assertions(project_key, entry_ordinal);
CREATE INDEX IF NOT EXISTS idx_session_index_entry_assertions_session ON session_index_entry_assertions(session_key, project_key);
CREATE INDEX IF NOT EXISTS idx_canonical_session_index_entries_project ON canonical_session_index_entries(project_key, entry_ordinal);
CREATE INDEX IF NOT EXISTS idx_canonical_session_index_entries_transcript ON canonical_session_index_entries(transcript_status, session_key);
CREATE INDEX IF NOT EXISTS idx_canonical_messages_session_order ON canonical_messages(session_key, source_generation, cursor_start);
CREATE INDEX IF NOT EXISTS idx_canonical_runs_session ON canonical_runs(session_key, run_key);
CREATE INDEX IF NOT EXISTS idx_run_evidence_run_order ON run_evidence(run_key, source_generation, cursor_end);
CREATE INDEX IF NOT EXISTS idx_presence_assertions_presence ON presence_assertions(presence_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_presence_assertions_source ON presence_assertions(source_object_id, presence_key);
CREATE INDEX IF NOT EXISTS idx_presence_assertions_session ON presence_assertions(session_key, presence_key);
CREATE INDEX IF NOT EXISTS idx_canonical_presences_session ON canonical_presences(session_key, presence_key);
CREATE INDEX IF NOT EXISTS idx_canonical_presences_run ON canonical_presences(run_key, presence_key);
CREATE INDEX IF NOT EXISTS idx_delegation_assertions_child_order ON delegation_assertions(child_run_key, relation_strength, source_generation, cursor_end);
CREATE INDEX IF NOT EXISTS idx_delegation_assertions_parent ON delegation_assertions(parent_run_key, child_run_key);
CREATE INDEX IF NOT EXISTS idx_canonical_delegations_session ON canonical_delegations(session_key, child_run_key);
CREATE INDEX IF NOT EXISTS idx_delegation_metadata_assertions_child ON delegation_metadata_assertions(child_run_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_delegation_metadata_assertions_source ON delegation_metadata_assertions(source_object_id, child_run_key);
CREATE INDEX IF NOT EXISTS idx_delegation_metadata_assertions_task ON delegation_metadata_assertions(session_key, native_task_id, child_run_key);
CREATE INDEX IF NOT EXISTS idx_canonical_delegation_metadata_session ON canonical_delegation_metadata(session_key, child_run_key);
CREATE INDEX IF NOT EXISTS idx_delegation_spawn_assertions_task ON delegation_spawn_assertions(session_key, native_task_id, parent_run_key);
CREATE INDEX IF NOT EXISTS idx_delegation_spawn_assertions_source ON delegation_spawn_assertions(source_object_id, spawn_key);
CREATE INDEX IF NOT EXISTS idx_canonical_delegation_spawns_session ON canonical_delegation_spawns(session_key, native_task_id, spawn_key);
CREATE INDEX IF NOT EXISTS idx_team_snapshot_assertions_team ON team_snapshot_assertions(team_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_team_snapshot_assertions_source ON team_snapshot_assertions(source_object_id, team_key);
CREATE INDEX IF NOT EXISTS idx_team_member_assertions_team ON team_member_assertions(team_key, member_key);
CREATE INDEX IF NOT EXISTS idx_canonical_team_members_team ON canonical_team_members(team_key, member_ordinal);
CREATE INDEX IF NOT EXISTS idx_team_inbox_snapshot_assertions_inbox ON team_inbox_snapshot_assertions(inbox_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_team_inbox_snapshot_assertions_source ON team_inbox_snapshot_assertions(source_object_id, inbox_key);
CREATE INDEX IF NOT EXISTS idx_team_inbox_message_assertions_inbox ON team_inbox_message_assertions(inbox_key, message_key);
CREATE INDEX IF NOT EXISTS idx_canonical_team_inboxes_team ON canonical_team_inboxes(team_key, inbox_key);
CREATE INDEX IF NOT EXISTS idx_canonical_team_inbox_messages_inbox ON canonical_team_inbox_messages(inbox_key, message_ordinal);
CREATE INDEX IF NOT EXISTS idx_task_snapshot_assertions_collection ON task_snapshot_assertions(collection_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_task_snapshot_assertions_source ON task_snapshot_assertions(source_object_id, collection_key);
CREATE INDEX IF NOT EXISTS idx_task_snapshot_assertions_session ON task_snapshot_assertions(session_key, collection_key);
CREATE INDEX IF NOT EXISTS idx_task_item_assertions_collection ON task_item_assertions(collection_key, task_key);
CREATE INDEX IF NOT EXISTS idx_task_item_assertions_task ON task_item_assertions(task_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_canonical_task_collections_session ON canonical_task_collections(session_key, collection_key);
CREATE INDEX IF NOT EXISTS idx_canonical_tasks_collection ON canonical_tasks(collection_key, item_ordinal);
CREATE INDEX IF NOT EXISTS idx_plan_assertions_plan ON plan_assertions(plan_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_plan_assertions_source ON plan_assertions(source_object_id, plan_key);
CREATE INDEX IF NOT EXISTS idx_artifact_snapshot_assertions_source ON artifact_snapshot_assertions(source_object_id, source_generation);
CREATE INDEX IF NOT EXISTS idx_artifact_snapshot_assertions_session ON artifact_snapshot_assertions(session_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_artifact_metadata_assertions_artifact ON artifact_metadata_assertions(artifact_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_artifact_content_assertions_artifact ON artifact_content_assertions(artifact_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_artifact_content_assertions_source ON artifact_content_assertions(source_object_id, artifact_key);
CREATE INDEX IF NOT EXISTS idx_canonical_artifacts_session ON canonical_artifacts(session_key, backup_time, artifact_key);
CREATE INDEX IF NOT EXISTS idx_workflow_snapshot_assertions_workflow ON workflow_snapshot_assertions(workflow_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_workflow_snapshot_assertions_source ON workflow_snapshot_assertions(source_object_id, workflow_key);
CREATE INDEX IF NOT EXISTS idx_workflow_member_event_assertions_member ON workflow_member_event_assertions(member_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_workflow_member_event_assertions_workflow ON workflow_member_event_assertions(workflow_key, member_key);
CREATE INDEX IF NOT EXISTS idx_workflow_member_event_assertions_source ON workflow_member_event_assertions(source_object_id, source_generation);
CREATE INDEX IF NOT EXISTS idx_canonical_workflows_session ON canonical_workflows(session_key, finished_at, workflow_key);
CREATE INDEX IF NOT EXISTS idx_canonical_workflow_members_workflow ON canonical_workflow_members(workflow_key, native_agent_id);
CREATE INDEX IF NOT EXISTS idx_usage_contributions_session ON usage_contributions(session_key, fact_id);

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
${SESSION_SUMMARY_TRIGGERS_SQL}
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
  'canonical_workflow_members',
  'canonical_workflows',
  'workflow_member_event_assertions',
  'workflow_snapshot_assertions',
  'canonical_session_index_entries',
  'canonical_session_indexes',
  'session_index_entry_assertions',
  'session_index_snapshot_assertions',
  'canonical_artifacts',
  'artifact_content_assertions',
  'artifact_metadata_assertions',
  'artifact_snapshot_assertions',
  'canonical_plans',
  'plan_assertions',
  'canonical_tasks',
  'canonical_task_collections',
  'task_item_assertions',
  'task_snapshot_assertions',
  'canonical_delegation_spawns',
  'canonical_delegation_metadata',
  'canonical_delegations',
  'canonical_team_inbox_messages',
  'canonical_team_inboxes',
  'team_inbox_message_assertions',
  'team_inbox_snapshot_assertions',
  'canonical_team_members',
  'canonical_teams',
  'team_member_assertions',
  'team_snapshot_assertions',
  'delegation_spawn_assertions',
  'delegation_metadata_assertions',
  'delegation_assertions',
  'canonical_presences',
  'presence_assertions',
  'observed_run_states',
  'usage_totals',
  'usage_contributions',
  'run_evidence',
  'canonical_messages',
  'canonical_runs',
  'canonical_sessions',
  'fact_records',
  'source_record_errors',
  'change_log',
  'projection_versions',
  'source_objects',
  'source_streams',
  'ingest_commits',
  'source_instances',
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
  'token_activity_session_daily',
  'token_activity_dirty',
  'session_summary_totals',
  'session_summary_dirty',
  'source_materializations',
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
      ...SESSION_SUMMARY_TRIGGER_NAMES,
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
    // Trigger bodies can receive correctness fixes without changing stored
    // data. CREATE TRIGGER IF NOT EXISTS cannot replace an older body, so
    // refresh this derived-index trigger family explicitly on every attach.
    for (const trigger of [...TOKEN_ACTIVITY_TRIGGER_NAMES, ...SESSION_SUMMARY_TRIGGER_NAMES]) {
      db.exec(`DROP TRIGGER IF EXISTS ${trigger}`);
    }
    db.exec(TOKEN_ACTIVITY_TRIGGERS_SQL);
    db.exec(SESSION_SUMMARY_TRIGGERS_SQL);
  }
}
