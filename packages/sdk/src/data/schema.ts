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
// v28: independently replaceable project-memory Markdown documents, native
// index classification, and deterministic conflict provenance.
// v29: persisted tool-result text assertions, deterministic transcript block
// correlation, and explicit join/conflict state.
// v30: redacted interpretation-settings assertions, document health, and
// native global/local effective-setting reduction.
// v31: separately versioned common-driver checkpoints for restart-safe source
// resume without conflating driver state with adapter decoder state.
// v32: message-to-run identity plus a writer-maintained canonical-message
// FTS5 projection. Existing databases rebuild so every canonical row is
// guaranteed to have both its relation and search index entry.
// v33: writer-maintained source-neutral content-block metadata for indexed
// canonical timeline filters and facets.
// v34: durable common-driver retry state for bounded malformed snapshots.
// v35: durable change-log retention accounting and stale-cursor reset floor.
// v36: truthful cumulative/snapshot usage series and raw counter values.
// v37: durable adapter/source-schema/capability manifest snapshots.
// v38: stamped adapter dependency reads attached to durable fact records.
// v39: versioned compact fact/native-message blobs. Rust is the production
// writer; this DDL remains its migration mirror for transitional tooling.
// v40: durable stream retention, provenance-only fact rows for non-Full
// streams, and removal of the unused wide fact entity/kind index.
// v41: canonical FTS uses a filtered external-content view so FTS5 integrity
// checks cover exactly the non-empty searchable canonical-message subset.
// v42: canonical normalized message content uses bounded versioned zstd
// storage and is decoded only for bounded detail/timeline pages.
// v43: compact run-evidence reductions retain the exact decisive fact, total
// count, and maximum activity without indexing one projection row per message.
// v44: unambiguous message facts own paired native-activity evidence from the
// same source observation; compact run evidence keeps its dimensions/count.
// v45: nullable RFC 012A source-record/fact/revision identity beside the RFC
// 011 store key, with complete-triple and unique-revision enforcement.
// v46: non-public RFC 012C response-level usage-v2 shadow state and interned
// qualified-value evidence beside the retained legacy usage projection.
// v47: topology-neutral runtime actor and affiliation revisions used to
// regroup usage without copying response contributions.
// v48: normalized, persistable RFC 012A source/fact-family coverage sets,
// points, absences, and errors owned by common projection transitions.
// v49: source-scoped query-pack selection with explicit rollback target,
// epoch, and durable commit ownership for RFC 012C usage-v2 migration.
// v50: source-neutral RFC 012B Library coverage-plan registration and the
// initial Pending/Building readiness lineage on the common commit clock.
// v51: immutable RFC 012B initial catalog snapshots, private typed payload
// frames, and the atomically linked initial Ready state.
// v52: ordinary RFC 012B Ready refresh administration that retains the exact
// current complete snapshot while advancing the durable state commit.
// v53: atomic RFC 012B ordinary-refresh successor snapshots with exact
// predecessor commitments and cumulative member-identity history.
export const SCHEMA_VERSION = 53;

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
  adapter_version TEXT NOT NULL,
  adapter_contract_version INTEGER NOT NULL,
  source_schema_versions_json TEXT NOT NULL,
  capabilities_json TEXT NOT NULL,
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
  raw_retention TEXT NOT NULL DEFAULT 'none',
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
  driver_checkpoint BLOB,
  driver_checkpoint_version INTEGER,
  decoder_state BLOB,
  decoder_state_version INTEGER,
  retry_state BLOB,
  size_bytes INTEGER,
  mtime_ns INTEGER,
  decoder_contract_version INTEGER NOT NULL,
  last_commit_seq INTEGER,
  state TEXT NOT NULL,
  UNIQUE(source_stream_id, object_key)
);

CREATE TABLE IF NOT EXISTS ingest_commits (
  commit_seq INTEGER PRIMARY KEY AUTOINCREMENT,
  source_instance_id INTEGER REFERENCES source_instances(source_instance_id) ON DELETE RESTRICT,
  reason TEXT NOT NULL,
  started_at INTEGER NOT NULL,
  committed_at INTEGER,
  fact_count INTEGER NOT NULL DEFAULT 0,
  CHECK (
    source_instance_id IS NOT NULL
    OR (
      reason IN (
        'catalog.library.plan.registered',
        'catalog.library.build.scheduled',
        'catalog.library.initial_snapshot.published',
        'catalog.library.refresh.started',
        'catalog.library.refresh_snapshot.published'
      )
      AND committed_at IS NOT NULL
      AND fact_count = 0
    )
  )
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

CREATE TABLE IF NOT EXISTS change_log_retention_state (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  pruned_through_commit_seq INTEGER NOT NULL DEFAULT 0,
  retained_change_count INTEGER NOT NULL DEFAULT 0,
  retained_payload_bytes INTEGER NOT NULL DEFAULT 0,
  last_pruned_at INTEGER
);

INSERT OR IGNORE INTO change_log_retention_state (
  singleton, pruned_through_commit_seq, retained_change_count,
  retained_payload_bytes, last_pruned_at
) VALUES (1, 0, 0, 0, NULL);

CREATE TABLE IF NOT EXISTS catalog_coverage_plans (
  coverage_plan_id BLOB PRIMARY KEY CHECK (length(coverage_plan_id) = 32),
  coverage_plan_contract_version INTEGER NOT NULL CHECK (coverage_plan_contract_version > 0),
  scope_kind TEXT NOT NULL CHECK (scope_kind = 'library'),
  plan_json BLOB NOT NULL CHECK (length(plan_json) BETWEEN 1 AND 4194304),
  content_digest BLOB NOT NULL CHECK (length(content_digest) = 32),
  created_commit_seq INTEGER NOT NULL REFERENCES ingest_commits(commit_seq) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS catalog_snapshots (
  snapshot_commit_seq INTEGER PRIMARY KEY REFERENCES ingest_commits(commit_seq) ON DELETE RESTRICT,
  build_commit_seq INTEGER NOT NULL REFERENCES ingest_commits(commit_seq) ON DELETE RESTRICT,
  durable_publication_contract_version INTEGER NOT NULL CHECK (durable_publication_contract_version > 0),
  pack_contract_version INTEGER NOT NULL CHECK (pack_contract_version > 0),
  coverage_plan_id BLOB NOT NULL REFERENCES catalog_coverage_plans(coverage_plan_id) ON DELETE RESTRICT CHECK (length(coverage_plan_id) = 32),
  readiness_epoch INTEGER NOT NULL CHECK (readiness_epoch > 0),
  attempt INTEGER NOT NULL CHECK (attempt > 0),
  contract_selection_json BLOB NOT NULL CHECK (length(contract_selection_json) BETWEEN 1 AND 4194304),
  member_identity_contract_id TEXT,
  publication_digest BLOB NOT NULL CHECK (length(publication_digest) = 32),
  reducer_revision BLOB NOT NULL CHECK (length(reducer_revision) = 32),
  entries_digest BLOB NOT NULL CHECK (length(entries_digest) = 32),
  content_digest BLOB NOT NULL CHECK (length(content_digest) = 32),
  entry_count INTEGER NOT NULL CHECK (entry_count BETWEEN 1 AND 2100000),
  encoded_bytes INTEGER NOT NULL CHECK (encoded_bytes BETWEEN 1 AND 536870912),
  source_count INTEGER NOT NULL CHECK (source_count BETWEEN 0 AND 4096),
  member_count INTEGER NOT NULL CHECK (member_count BETWEEN 0 AND 1000000),
  project_row_count INTEGER NOT NULL CHECK (project_row_count BETWEEN 0 AND 1000000),
  session_row_count INTEGER NOT NULL CHECK (session_row_count BETWEEN 0 AND 1000000),
  tombstone_count INTEGER NOT NULL CHECK (tombstone_count BETWEEN 0 AND 1000000),
  replaces_snapshot_commit_seq INTEGER REFERENCES catalog_snapshots(snapshot_commit_seq) ON DELETE RESTRICT,
  replaces_publication_digest BLOB,
  replaces_content_digest BLOB,
  published_at INTEGER NOT NULL,
  CHECK (member_identity_contract_id IS NULL OR length(CAST(member_identity_contract_id AS BLOB)) BETWEEN 1 AND 256),
  CHECK (
    (source_count = 0 AND member_identity_contract_id IS NULL)
    OR (source_count > 0 AND member_identity_contract_id IS NOT NULL)
  ),
  CHECK (
    (
      replaces_snapshot_commit_seq IS NULL
      AND replaces_publication_digest IS NULL
      AND replaces_content_digest IS NULL
      AND durable_publication_contract_version = 1
    )
    OR
    (
      replaces_snapshot_commit_seq IS NOT NULL
      AND replaces_snapshot_commit_seq < snapshot_commit_seq
      AND typeof(replaces_publication_digest) = 'blob'
      AND length(replaces_publication_digest) = 32
      AND typeof(replaces_content_digest) = 'blob'
      AND length(replaces_content_digest) = 32
      AND durable_publication_contract_version = 2
    )
  ),
  CHECK (snapshot_commit_seq > build_commit_seq),
  UNIQUE (replaces_snapshot_commit_seq),
  UNIQUE (pack_contract_version, coverage_plan_id, readiness_epoch, snapshot_commit_seq)
);

CREATE TABLE IF NOT EXISTS catalog_snapshot_entries (
  snapshot_commit_seq INTEGER NOT NULL REFERENCES catalog_snapshots(snapshot_commit_seq) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  entry_kind TEXT NOT NULL CHECK (entry_kind IN ('source', 'member_binding', 'member_history', 'reducer_state', 'project_row', 'session_row', 'tombstone')),
  entry_key BLOB NOT NULL CHECK (length(entry_key) = 32),
  payload BLOB NOT NULL CHECK (length(payload) BETWEEN 1 AND 536870912),
  payload_digest BLOB NOT NULL CHECK (length(payload_digest) = 32),
  PRIMARY KEY (snapshot_commit_seq, entry_kind, entry_key),
  UNIQUE (snapshot_commit_seq, ordinal)
);

CREATE TABLE IF NOT EXISTS catalog_build_state (
  scope_kind TEXT PRIMARY KEY CHECK (scope_kind = 'library'),
  coverage_plan_id BLOB NOT NULL REFERENCES catalog_coverage_plans(coverage_plan_id) ON DELETE RESTRICT,
  desired_contract_version INTEGER NOT NULL CHECK (desired_contract_version > 0),
  epoch INTEGER NOT NULL CHECK (epoch > 0),
  attempt INTEGER NOT NULL CHECK (attempt > 0),
  state TEXT NOT NULL CHECK (state IN ('pending', 'building', 'ready')),
  completed_contract_version INTEGER CHECK (completed_contract_version > 0),
  complete_through_commit INTEGER REFERENCES catalog_snapshots(snapshot_commit_seq) ON DELETE RESTRICT,
  last_complete_snapshot_commit INTEGER REFERENCES catalog_snapshots(snapshot_commit_seq) ON DELETE RESTRICT,
  refreshing_from_snapshot_commit INTEGER REFERENCES catalog_snapshots(snapshot_commit_seq) ON DELETE RESTRICT,
  last_commit_seq INTEGER NOT NULL REFERENCES ingest_commits(commit_seq) ON DELETE RESTRICT,
  updated_at INTEGER NOT NULL,
  CHECK (
    (
      state IN ('pending', 'building')
      AND completed_contract_version IS NULL
      AND complete_through_commit IS NULL
      AND last_complete_snapshot_commit IS NULL
      AND refreshing_from_snapshot_commit IS NULL
    )
    OR
    (
      state = 'ready'
      AND completed_contract_version IS NOT NULL
      AND complete_through_commit IS NOT NULL
      AND last_complete_snapshot_commit IS NOT NULL
      AND completed_contract_version = desired_contract_version
      AND complete_through_commit = last_complete_snapshot_commit
      AND (
        (
          refreshing_from_snapshot_commit IS NULL
          AND last_commit_seq = complete_through_commit
        )
        OR
        (
          refreshing_from_snapshot_commit = last_complete_snapshot_commit
          AND last_commit_seq > complete_through_commit
        )
      )
    )
  )
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

CREATE TABLE IF NOT EXISTS query_pack_selections (
  source_instance_id INTEGER NOT NULL REFERENCES source_instances(source_instance_id) ON DELETE CASCADE,
  query_pack_id TEXT NOT NULL,
  scope_key BLOB NOT NULL CHECK (length(scope_key) BETWEEN 1 AND 4096),
  selected_query_id TEXT NOT NULL,
  selected_contract_version INTEGER NOT NULL CHECK (selected_contract_version > 0),
  rollback_query_id TEXT NOT NULL,
  rollback_contract_version INTEGER NOT NULL CHECK (rollback_contract_version > 0),
  selection_epoch INTEGER NOT NULL CHECK (selection_epoch > 0),
  last_commit_seq INTEGER NOT NULL REFERENCES ingest_commits(commit_seq) ON DELETE RESTRICT,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (source_instance_id, query_pack_id, scope_key)
);

CREATE TABLE IF NOT EXISTS source_coverage_sets (
  coverage_set_id INTEGER PRIMARY KEY,
  source_instance_id INTEGER NOT NULL REFERENCES source_instances(source_instance_id) ON DELETE CASCADE,
  owner_id TEXT NOT NULL,
  owner_scope_key BLOB NOT NULL,
  coverage_set_contract_version INTEGER NOT NULL CHECK (coverage_set_contract_version > 0),
  coverage_contract_version INTEGER NOT NULL CHECK (coverage_contract_version > 0),
  domain_kind TEXT NOT NULL CHECK (domain_kind IN ('decode', 'fact_family', 'projection_pack')),
  domain_name TEXT NOT NULL,
  domain_version INTEGER NOT NULL,
  adapter_id TEXT NOT NULL,
  canonical_source_instance_key BLOB NOT NULL CHECK (length(canonical_source_instance_key) = 32),
  root_entity_key BLOB NOT NULL DEFAULT X'' CHECK (length(root_entity_key) IN (0, 32)),
  support_release_id TEXT NOT NULL,
  declaration_digest BLOB NOT NULL CHECK (length(declaration_digest) = 32),
  membership_revision BLOB NOT NULL CHECK (length(membership_revision) = 32),
  completeness TEXT NOT NULL CHECK (completeness IN ('complete', 'partial', 'unavailable')),
  content_digest BLOB NOT NULL CHECK (length(content_digest) = 32),
  last_commit_seq INTEGER NOT NULL REFERENCES ingest_commits(commit_seq) ON DELETE RESTRICT,
  updated_at INTEGER NOT NULL,
  CHECK (
    (domain_kind = 'decode' AND domain_name = '' AND domain_version = 0)
    OR
    (domain_kind IN ('fact_family', 'projection_pack') AND length(domain_name) > 0 AND domain_version > 0)
  ),
  UNIQUE (owner_id, owner_scope_key, domain_kind, domain_name, domain_version, root_entity_key)
);

CREATE TABLE IF NOT EXISTS source_coverage_points (
  coverage_set_id INTEGER NOT NULL REFERENCES source_coverage_sets(coverage_set_id) ON DELETE CASCADE,
  stream_key BLOB NOT NULL CHECK (length(stream_key) = 32),
  object_key BLOB NOT NULL CHECK (length(object_key) = 32),
  generation INTEGER NOT NULL CHECK (generation >= 0),
  position_kind TEXT CHECK (position_kind IN ('append_cursor', 'document_revision', 'snapshot_revision', 'database_watermark', 'key_range_token')),
  position_ref BLOB CHECK (position_ref IS NULL OR length(position_ref) = 32),
  monotonic_order INTEGER CHECK (monotonic_order IS NULL OR monotonic_order >= 0),
  status TEXT NOT NULL CHECK (status IN ('complete_through', 'exact_snapshot', 'partial', 'unavailable')),
  unavailable_reason TEXT,
  source_record_id BLOB CHECK (source_record_id IS NULL OR length(source_record_id) = 32),
  semantic_revision_ref BLOB CHECK (semantic_revision_ref IS NULL OR length(semantic_revision_ref) = 32),
  observed_at INTEGER,
  PRIMARY KEY (coverage_set_id, stream_key, object_key, generation),
  CHECK (
    (position_kind IS NULL AND position_ref IS NULL AND monotonic_order IS NULL)
    OR
    (position_kind IS NOT NULL AND position_ref IS NOT NULL)
  ),
  CHECK (
    (status = 'unavailable' AND unavailable_reason IS NOT NULL AND length(unavailable_reason) > 0)
    OR
    (status != 'unavailable' AND unavailable_reason IS NULL)
  )
);

CREATE TABLE IF NOT EXISTS source_coverage_absences (
  coverage_set_id INTEGER NOT NULL REFERENCES source_coverage_sets(coverage_set_id) ON DELETE CASCADE,
  stream_key BLOB NOT NULL CHECK (length(stream_key) = 32),
  object_key BLOB NOT NULL CHECK (length(object_key) = 32),
  generation INTEGER NOT NULL CHECK (generation >= 0),
  absence_kind TEXT NOT NULL CHECK (absence_kind IN ('absent', 'deleted')),
  PRIMARY KEY (coverage_set_id, stream_key, object_key, generation)
);

CREATE TABLE IF NOT EXISTS source_coverage_errors (
  coverage_set_id INTEGER NOT NULL REFERENCES source_coverage_sets(coverage_set_id) ON DELETE CASCADE,
  error_ordinal INTEGER NOT NULL CHECK (error_ordinal >= 0),
  stream_key BLOB CHECK (stream_key IS NULL OR length(stream_key) = 32),
  object_key BLOB CHECK (object_key IS NULL OR length(object_key) = 32),
  error_code TEXT NOT NULL,
  PRIMARY KEY (coverage_set_id, error_ordinal)
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

-- RFC 011 typed-fact provenance store and common projections. payload_json is
-- empty with codec 'omitted' unless the stream declares Full retention or a
-- DiagnosticExcerpt stream preserves one bounded redacted unknown-record
-- shape; only the common writer maps facts into these tables.
CREATE TABLE IF NOT EXISTS fact_records (
  fact_id BLOB PRIMARY KEY,
  fact_kind TEXT NOT NULL,
  entity_key BLOB,
  semantic_source_record_id BLOB,
  semantic_fact_id BLOB,
  semantic_fact_revision_id BLOB,
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
  payload_codec TEXT NOT NULL DEFAULT 'identity',
  last_commit_seq INTEGER NOT NULL REFERENCES ingest_commits(commit_seq) ON DELETE RESTRICT,
  CHECK (
    (semantic_source_record_id IS NULL AND semantic_fact_id IS NULL AND semantic_fact_revision_id IS NULL)
    OR
    (
      semantic_source_record_id IS NOT NULL
      AND semantic_fact_id IS NOT NULL
      AND semantic_fact_revision_id IS NOT NULL
      AND length(semantic_source_record_id) = 32
      AND length(semantic_fact_id) = 32
      AND length(semantic_fact_revision_id) = 32
    )
  )
);

CREATE TABLE IF NOT EXISTS fact_dependency_reads (
  fact_id BLOB NOT NULL REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  source_instance_id INTEGER NOT NULL REFERENCES source_instances(source_instance_id) ON DELETE CASCADE,
  root_name TEXT NOT NULL,
  object_key BLOB NOT NULL,
  revision BLOB NOT NULL,
  PRIMARY KEY (fact_id, source_instance_id, root_name, object_key)
);

CREATE INDEX IF NOT EXISTS idx_fact_dependency_reads_object
ON fact_dependency_reads(source_instance_id, root_name, object_key, fact_id);

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

CREATE TABLE IF NOT EXISTS project_memory_document_assertions (
  fact_id BLOB PRIMARY KEY REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  document_key BLOB NOT NULL,
  project_key BLOB NOT NULL,
  native_project_key TEXT NOT NULL,
  native_document_path TEXT NOT NULL,
  title TEXT NOT NULL,
  content TEXT NOT NULL,
  size_bytes INTEGER NOT NULL,
  is_index INTEGER NOT NULL,
  document_digest BLOB NOT NULL,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS canonical_project_memory_documents (
  document_key BLOB PRIMARY KEY,
  project_key BLOB NOT NULL,
  native_project_key TEXT NOT NULL,
  native_document_path TEXT NOT NULL,
  title TEXT NOT NULL,
  content TEXT NOT NULL,
  size_bytes INTEGER NOT NULL,
  is_index INTEGER NOT NULL,
  resolution_status TEXT NOT NULL,
  decisive_fact_id BLOB NOT NULL REFERENCES project_memory_document_assertions(fact_id) ON DELETE CASCADE,
  assertion_count INTEGER NOT NULL,
  competing_document_count INTEGER NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS persisted_tool_result_assertions (
  fact_id BLOB PRIMARY KEY REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  result_key BLOB NOT NULL,
  session_key BLOB NOT NULL,
  project_key BLOB NOT NULL,
  native_project_key TEXT NOT NULL,
  native_session_id TEXT NOT NULL,
  native_tool_use_id TEXT NOT NULL,
  native_document_path TEXT NOT NULL,
  content TEXT NOT NULL,
  size_bytes INTEGER NOT NULL,
  result_digest BLOB NOT NULL,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS canonical_persisted_tool_results (
  result_key BLOB PRIMARY KEY,
  session_key BLOB NOT NULL,
  project_key BLOB NOT NULL,
  native_project_key TEXT NOT NULL,
  native_session_id TEXT NOT NULL,
  native_tool_use_id TEXT NOT NULL,
  native_document_path TEXT NOT NULL,
  content TEXT NOT NULL,
  size_bytes INTEGER NOT NULL,
  resolution_status TEXT NOT NULL,
  correlation_status TEXT NOT NULL,
  tool_call_message_key BLOB,
  tool_result_message_key BLOB,
  decisive_fact_id BLOB NOT NULL REFERENCES persisted_tool_result_assertions(fact_id) ON DELETE CASCADE,
  assertion_count INTEGER NOT NULL,
  competing_result_count INTEGER NOT NULL,
  tool_call_match_count INTEGER NOT NULL,
  tool_result_match_count INTEGER NOT NULL,
  join_conflict INTEGER NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS interpretation_settings_assertions (
  fact_id BLOB PRIMARY KEY REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  document_key BLOB NOT NULL,
  scope_key BLOB NOT NULL,
  layer TEXT NOT NULL,
  native_document_path TEXT NOT NULL,
  document_status TEXT NOT NULL,
  settings_json BLOB,
  error_code TEXT,
  size_bytes INTEGER NOT NULL,
  settings_digest BLOB NOT NULL,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS canonical_interpretation_settings_documents (
  document_key BLOB PRIMARY KEY,
  scope_key BLOB NOT NULL,
  layer TEXT NOT NULL,
  native_document_path TEXT NOT NULL,
  document_status TEXT NOT NULL,
  settings_json BLOB,
  error_code TEXT,
  size_bytes INTEGER NOT NULL,
  resolution_status TEXT NOT NULL,
  decisive_fact_id BLOB NOT NULL REFERENCES interpretation_settings_assertions(fact_id) ON DELETE CASCADE,
  assertion_count INTEGER NOT NULL,
  competing_settings_count INTEGER NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS canonical_effective_interpretation_settings (
  scope_key BLOB PRIMARY KEY,
  effective_settings_json BLOB NOT NULL,
  global_document_status TEXT NOT NULL,
  local_document_status TEXT NOT NULL,
  resolution_status TEXT NOT NULL,
  global_decisive_fact_id BLOB REFERENCES interpretation_settings_assertions(fact_id) ON DELETE SET NULL,
  local_decisive_fact_id BLOB REFERENCES interpretation_settings_assertions(fact_id) ON DELETE SET NULL,
  document_count INTEGER NOT NULL,
  assertion_count INTEGER NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS canonical_messages (
  message_key BLOB PRIMARY KEY,
  session_key BLOB NOT NULL,
  run_key BLOB NOT NULL,
  native_message_id TEXT,
  native_kind TEXT NOT NULL,
  role TEXT NOT NULL,
  content_json BLOB NOT NULL,
  content_json_codec TEXT NOT NULL DEFAULT 'identity',
  source_time TEXT,
  source_time_quality TEXT,
  parent_native_message_id TEXT,
  model TEXT,
  search_text TEXT,
  raw_json BLOB NOT NULL,
  raw_json_codec TEXT NOT NULL DEFAULT 'identity',
  fact_id BLOB NOT NULL REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_start BLOB NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS message_tool_references (
  message_key BLOB NOT NULL REFERENCES canonical_messages(message_key) ON DELETE CASCADE,
  session_key BLOB NOT NULL,
  native_tool_use_id TEXT NOT NULL,
  reference_kind TEXT NOT NULL,
  block_ordinal INTEGER NOT NULL,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  PRIMARY KEY (message_key, reference_kind, block_ordinal)
);

CREATE TABLE IF NOT EXISTS canonical_message_content_blocks (
  message_key BLOB NOT NULL REFERENCES canonical_messages(message_key) ON DELETE CASCADE,
  session_key BLOB NOT NULL,
  run_key BLOB NOT NULL,
  block_ordinal INTEGER NOT NULL,
  content_kind TEXT NOT NULL,
  tool_name TEXT,
  native_tool_call_id TEXT,
  PRIMARY KEY (message_key, block_ordinal)
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
  last_commit_seq INTEGER NOT NULL,
  evidence_count INTEGER NOT NULL DEFAULT 1,
  last_activity_at TEXT
);

CREATE TABLE IF NOT EXISTS observed_run_states (
  run_key BLOB PRIMARY KEY,
  state TEXT NOT NULL,
  decisive_evidence_id BLOB NOT NULL REFERENCES run_evidence(fact_id) ON DELETE CASCADE ON UPDATE CASCADE,
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
  series_key BLOB NOT NULL,
  scope TEXT NOT NULL,
  accounting TEXT NOT NULL,
  quality TEXT NOT NULL,
  quality_bucket TEXT NOT NULL,
  input_tokens INTEGER NOT NULL,
  output_tokens INTEGER NOT NULL,
  cache_creation_tokens INTEGER NOT NULL,
  cache_read_tokens INTEGER NOT NULL,
  reported_input_tokens INTEGER NOT NULL,
  reported_output_tokens INTEGER NOT NULL,
  reported_cache_creation_tokens INTEGER NOT NULL,
  reported_cache_read_tokens INTEGER NOT NULL,
  model TEXT,
  source_time TEXT,
  source_time_quality TEXT,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

-- RFC 012C shadow projection. Repeated qualification metadata is interned so
-- response rows retain full value quality/provenance without multiplying the
-- same agent-field strings across the corpus.
CREATE TABLE IF NOT EXISTS usage_v2_qualification_specs (
  qualification_key BLOB PRIMARY KEY CHECK (length(qualification_key) = 32),
  quality TEXT NOT NULL,
  completeness TEXT NOT NULL,
  unknown_reason TEXT,
  authority TEXT NOT NULL,
  native_field TEXT NOT NULL,
  normalization_contract_version INTEGER NOT NULL CHECK (normalization_contract_version > 0),
  CHECK (length(native_field) BETWEEN 1 AND 256),
  CHECK (
    (quality = 'unknown' AND unknown_reason IS NOT NULL AND completeness <> 'complete')
    OR
    (quality <> 'unknown' AND unknown_reason IS NULL)
  )
);

-- RFC 012C topology-neutral actor identity and orthogonal affiliations. These
-- are deliberately independent of legacy catalog-local run/team/workflow
-- keys so every runtime adapter and database-free observer can share them.
CREATE TABLE IF NOT EXISTS runtime_actor_runs_v2 (
  actor_run_key BLOB PRIMARY KEY CHECK (length(actor_run_key) = 32),
  semantic_fact_id BLOB NOT NULL UNIQUE CHECK (length(semantic_fact_id) = 32),
  fact_revision_id BLOB NOT NULL UNIQUE CHECK (length(fact_revision_id) = 32),
  source_record_id BLOB NOT NULL CHECK (length(source_record_id) = 32),
  session_key BLOB NOT NULL CHECK (length(session_key) = 32),
  role TEXT NOT NULL CHECK (role IN ('root', 'child')),
  parent_actor_run_key BLOB CHECK (parent_actor_run_key IS NULL OR length(parent_actor_run_key) = 32),
  native_session_id TEXT,
  native_actor_id TEXT,
  native_actor_type TEXT,
  fact_id BLOB NOT NULL UNIQUE REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL,
  CHECK (role = 'child' OR parent_actor_run_key IS NULL),
  CHECK (role = 'root' OR parent_actor_run_key IS NOT NULL),
  CHECK (parent_actor_run_key IS NULL OR parent_actor_run_key <> actor_run_key),
  CHECK (native_session_id IS NULL OR length(native_session_id) BETWEEN 1 AND 8192),
  CHECK (native_actor_id IS NULL OR length(native_actor_id) BETWEEN 1 AND 8192),
  CHECK (native_actor_type IS NULL OR length(native_actor_type) BETWEEN 1 AND 8192)
);

CREATE TABLE IF NOT EXISTS runtime_actor_affiliations_v2 (
  affiliation_key BLOB PRIMARY KEY CHECK (length(affiliation_key) = 32),
  semantic_fact_id BLOB NOT NULL UNIQUE CHECK (length(semantic_fact_id) = 32),
  fact_revision_id BLOB NOT NULL UNIQUE CHECK (length(fact_revision_id) = 32),
  source_record_id BLOB NOT NULL CHECK (length(source_record_id) = 32),
  actor_run_key BLOB NOT NULL CHECK (length(actor_run_key) = 32),
  session_key BLOB NOT NULL CHECK (length(session_key) = 32),
  dimension TEXT NOT NULL CHECK (dimension IN ('team', 'workflow')),
  target_key BLOB NOT NULL CHECK (length(target_key) = 32),
  member_key BLOB CHECK (member_key IS NULL OR length(member_key) = 32),
  native_target_id TEXT,
  native_member_id TEXT,
  state TEXT NOT NULL CHECK (state IN ('present', 'removed', 'unknown')),
  effective_at TEXT,
  effective_at_quality TEXT,
  fact_id BLOB NOT NULL UNIQUE REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL,
  CHECK (native_target_id IS NULL OR length(native_target_id) BETWEEN 1 AND 8192),
  CHECK (native_member_id IS NULL OR length(native_member_id) BETWEEN 1 AND 8192),
  CHECK (effective_at IS NULL OR length(effective_at) BETWEEN 1 AND 8192)
);

CREATE TABLE IF NOT EXISTS usage_v2_response_contributions (
  usage_key BLOB PRIMARY KEY CHECK (length(usage_key) = 32),
  fact_revision_id BLOB NOT NULL UNIQUE CHECK (length(fact_revision_id) = 32),
  source_record_id BLOB NOT NULL CHECK (length(source_record_id) = 32),
  fact_id BLOB NOT NULL UNIQUE REFERENCES fact_records(fact_id) ON DELETE CASCADE,
  session_key BLOB NOT NULL CHECK (length(session_key) = 32),
  actor_run_key BLOB NOT NULL CHECK (length(actor_run_key) = 32),
  response_key BLOB NOT NULL CHECK (length(response_key) BETWEEN 1 AND 8192),
  response_identity TEXT NOT NULL CHECK (response_identity IN ('native_message_id', 'source_record_fallback')),
  native_message_id TEXT,
  request_id TEXT,
  input_tokens INTEGER CHECK (input_tokens IS NULL OR input_tokens >= 0),
  input_qualification_key BLOB NOT NULL REFERENCES usage_v2_qualification_specs(qualification_key),
  input_effective_at INTEGER,
  output_tokens INTEGER CHECK (output_tokens IS NULL OR output_tokens >= 0),
  output_qualification_key BLOB NOT NULL REFERENCES usage_v2_qualification_specs(qualification_key),
  output_effective_at INTEGER,
  cache_creation_input_tokens INTEGER CHECK (cache_creation_input_tokens IS NULL OR cache_creation_input_tokens >= 0),
  cache_creation_qualification_key BLOB NOT NULL REFERENCES usage_v2_qualification_specs(qualification_key),
  cache_creation_effective_at INTEGER,
  cache_read_input_tokens INTEGER CHECK (cache_read_input_tokens IS NULL OR cache_read_input_tokens >= 0),
  cache_read_qualification_key BLOB NOT NULL REFERENCES usage_v2_qualification_specs(qualification_key),
  cache_read_effective_at INTEGER,
  model TEXT,
  model_qualification_key BLOB REFERENCES usage_v2_qualification_specs(qualification_key),
  model_effective_at INTEGER,
  effort TEXT,
  effort_qualification_key BLOB REFERENCES usage_v2_qualification_specs(qualification_key),
  effort_effective_at INTEGER,
  source_time TEXT,
  source_time_quality TEXT,
  source_object_id INTEGER NOT NULL,
  source_generation INTEGER NOT NULL,
  cursor_end BLOB NOT NULL,
  last_commit_seq INTEGER NOT NULL,
  CHECK (request_id IS NULL OR length(request_id) > 0),
  CHECK (
    (response_identity = 'native_message_id' AND native_message_id IS NOT NULL AND length(native_message_id) > 0)
    OR
    (response_identity = 'source_record_fallback' AND native_message_id IS NULL)
  ),
  CHECK (
    (model IS NULL AND model_qualification_key IS NULL AND model_effective_at IS NULL)
    OR
    (model_qualification_key IS NOT NULL AND (model IS NULL OR length(model) > 0))
  ),
  CHECK (
    (effort IS NULL AND effort_qualification_key IS NULL AND effort_effective_at IS NULL)
    OR
    (effort_qualification_key IS NOT NULL AND (effort IS NULL OR length(effort) > 0))
  )
);

CREATE INDEX IF NOT EXISTS idx_usage_contributions_series_cursor
ON usage_contributions (
  series_key, source_generation, cursor_end, fact_id
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
CREATE INDEX IF NOT EXISTS idx_ingest_commits_retention ON ingest_commits(committed_at, commit_seq);
CREATE INDEX IF NOT EXISTS idx_change_log_topic_cursor ON change_log(topic, commit_seq, ordinal);
CREATE INDEX IF NOT EXISTS idx_projection_versions_readiness ON projection_versions(readiness, projection_id);
CREATE INDEX IF NOT EXISTS idx_source_coverage_sets_instance_owner ON source_coverage_sets(source_instance_id, owner_id);
CREATE INDEX IF NOT EXISTS idx_source_coverage_points_object ON source_coverage_points(stream_key, object_key, coverage_set_id);
CREATE INDEX IF NOT EXISTS idx_source_record_errors_commit ON source_record_errors(first_commit_seq);
CREATE INDEX IF NOT EXISTS idx_fact_records_object_generation ON fact_records(source_object_id, source_generation);
CREATE UNIQUE INDEX IF NOT EXISTS idx_fact_records_semantic_revision ON fact_records(semantic_fact_revision_id) WHERE semantic_fact_revision_id IS NOT NULL;
DROP INDEX IF EXISTS idx_fact_records_source_instance;
CREATE INDEX IF NOT EXISTS idx_fact_records_source_instance_compact ON fact_records(source_instance_id);
CREATE INDEX IF NOT EXISTS idx_canonical_sessions_project ON canonical_sessions(project_key, session_key);
CREATE INDEX IF NOT EXISTS idx_canonical_sessions_source_generation ON canonical_sessions(source_object_id, source_generation);
CREATE INDEX IF NOT EXISTS idx_session_index_snapshot_assertions_project ON session_index_snapshot_assertions(project_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_session_index_snapshot_assertions_source ON session_index_snapshot_assertions(source_object_id, project_key);
CREATE INDEX IF NOT EXISTS idx_session_index_entry_assertions_project ON session_index_entry_assertions(project_key, entry_ordinal);
CREATE INDEX IF NOT EXISTS idx_session_index_entry_assertions_session ON session_index_entry_assertions(session_key, project_key);
CREATE INDEX IF NOT EXISTS idx_canonical_session_index_entries_project ON canonical_session_index_entries(project_key, entry_ordinal);
CREATE INDEX IF NOT EXISTS idx_canonical_session_index_entries_transcript ON canonical_session_index_entries(transcript_status, session_key);
CREATE INDEX IF NOT EXISTS idx_project_memory_document_assertions_document ON project_memory_document_assertions(document_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_project_memory_document_assertions_source ON project_memory_document_assertions(source_object_id, document_key);
CREATE INDEX IF NOT EXISTS idx_project_memory_document_assertions_project ON project_memory_document_assertions(project_key, native_document_path);
CREATE INDEX IF NOT EXISTS idx_canonical_project_memory_documents_project ON canonical_project_memory_documents(project_key, is_index DESC, native_document_path);
CREATE INDEX IF NOT EXISTS idx_persisted_tool_result_assertions_result ON persisted_tool_result_assertions(result_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_persisted_tool_result_assertions_source ON persisted_tool_result_assertions(source_object_id, result_key);
CREATE INDEX IF NOT EXISTS idx_persisted_tool_result_assertions_native ON persisted_tool_result_assertions(session_key, native_tool_use_id, result_key);
CREATE INDEX IF NOT EXISTS idx_canonical_persisted_tool_results_session ON canonical_persisted_tool_results(session_key, native_tool_use_id, result_key);
CREATE INDEX IF NOT EXISTS idx_interpretation_settings_assertions_document ON interpretation_settings_assertions(document_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_interpretation_settings_assertions_source ON interpretation_settings_assertions(source_object_id, document_key);
CREATE INDEX IF NOT EXISTS idx_interpretation_settings_assertions_scope ON interpretation_settings_assertions(scope_key, layer, document_key);
CREATE INDEX IF NOT EXISTS idx_canonical_interpretation_settings_documents_scope ON canonical_interpretation_settings_documents(scope_key, layer, document_key);
CREATE INDEX IF NOT EXISTS idx_message_tool_references_native ON message_tool_references(session_key, native_tool_use_id, reference_kind, message_key);
CREATE INDEX IF NOT EXISTS idx_message_tool_references_source ON message_tool_references(source_object_id, source_generation, session_key, native_tool_use_id);
CREATE INDEX IF NOT EXISTS idx_canonical_message_blocks_session_kind ON canonical_message_content_blocks(session_key, content_kind, message_key);
CREATE INDEX IF NOT EXISTS idx_canonical_message_blocks_session_tool ON canonical_message_content_blocks(session_key, tool_name, message_key) WHERE tool_name IS NOT NULL;
DROP INDEX IF EXISTS idx_canonical_message_blocks_run;
DROP INDEX IF EXISTS idx_canonical_messages_session_order;
CREATE INDEX IF NOT EXISTS idx_canonical_messages_session_activity ON canonical_messages(session_key, source_time, message_key, source_time_quality, last_commit_seq);
CREATE INDEX IF NOT EXISTS idx_canonical_messages_run_activity ON canonical_messages(run_key, source_time, message_key);
CREATE INDEX IF NOT EXISTS idx_canonical_messages_source_generation ON canonical_messages(source_object_id, source_generation);
CREATE INDEX IF NOT EXISTS idx_canonical_runs_session ON canonical_runs(session_key, run_key);
CREATE INDEX IF NOT EXISTS idx_canonical_runs_commit ON canonical_runs(last_commit_seq DESC, run_key DESC);
CREATE INDEX IF NOT EXISTS idx_canonical_runs_source_generation ON canonical_runs(source_object_id, source_generation);
CREATE INDEX IF NOT EXISTS idx_usage_contributions_session_time ON usage_contributions(session_key, source_time, fact_id);
CREATE INDEX IF NOT EXISTS idx_usage_contributions_source_generation ON usage_contributions(source_object_id, source_generation);
CREATE INDEX IF NOT EXISTS idx_usage_v2_response_session ON usage_v2_response_contributions(session_key, usage_key);
CREATE INDEX IF NOT EXISTS idx_usage_v2_response_actor ON usage_v2_response_contributions(actor_run_key, usage_key);
CREATE INDEX IF NOT EXISTS idx_usage_v2_response_source_generation ON usage_v2_response_contributions(source_object_id, source_generation);
CREATE INDEX IF NOT EXISTS idx_runtime_actor_runs_v2_session ON runtime_actor_runs_v2(session_key, actor_run_key);
CREATE INDEX IF NOT EXISTS idx_runtime_actor_runs_v2_source_generation ON runtime_actor_runs_v2(source_object_id, source_generation);
CREATE INDEX IF NOT EXISTS idx_runtime_actor_affiliations_v2_actor ON runtime_actor_affiliations_v2(actor_run_key, dimension, affiliation_key);
CREATE INDEX IF NOT EXISTS idx_runtime_actor_affiliations_v2_target ON runtime_actor_affiliations_v2(dimension, target_key, state, actor_run_key);
CREATE INDEX IF NOT EXISTS idx_runtime_actor_affiliations_v2_source_generation ON runtime_actor_affiliations_v2(source_object_id, source_generation);
DROP INDEX IF EXISTS idx_run_evidence_run_order;
CREATE UNIQUE INDEX IF NOT EXISTS idx_run_evidence_compact ON run_evidence(
  run_key, source_object_id, source_generation, evidence_kind, evidence_strength
);
CREATE INDEX IF NOT EXISTS idx_run_evidence_decisive ON run_evidence(
  run_key,
  (CASE evidence_kind
    WHEN 'terminal_succeeded' THEN 60
    WHEN 'terminal_failed' THEN 60
    WHEN 'terminal_cancelled' THEN 60
    WHEN 'input_requested' THEN 50
    WHEN 'waiting_observed' THEN 45
    WHEN 'run_started' THEN 40
    WHEN 'activity_observed' THEN 35
    WHEN 'run_declared' THEN 20
    ELSE 0
  END) DESC,
  (CASE evidence_strength
    WHEN 'native_explicit' THEN 40
    WHEN 'native_activity' THEN 30
    WHEN 'presence' THEN 20
    WHEN 'layout' THEN 10
    ELSE 0
  END) DESC,
  source_generation DESC, cursor_end DESC, last_commit_seq DESC, fact_id DESC
);
CREATE INDEX IF NOT EXISTS idx_run_evidence_activity_time ON run_evidence(run_key, last_activity_at DESC)
  WHERE last_activity_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_run_evidence_source_generation ON run_evidence(source_object_id, source_generation);
CREATE INDEX IF NOT EXISTS idx_presence_assertions_presence ON presence_assertions(presence_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_presence_assertions_source ON presence_assertions(source_object_id, presence_key);
CREATE INDEX IF NOT EXISTS idx_presence_assertions_session ON presence_assertions(session_key, presence_key);
CREATE INDEX IF NOT EXISTS idx_canonical_presences_session ON canonical_presences(session_key, presence_key);
CREATE INDEX IF NOT EXISTS idx_canonical_presences_run ON canonical_presences(run_key, presence_key);
CREATE INDEX IF NOT EXISTS idx_canonical_presences_commit ON canonical_presences(last_commit_seq DESC, presence_key DESC);
CREATE INDEX IF NOT EXISTS idx_delegation_assertions_child_order ON delegation_assertions(child_run_key, relation_strength, source_generation, cursor_end);
CREATE INDEX IF NOT EXISTS idx_delegation_assertions_parent ON delegation_assertions(parent_run_key, child_run_key);
CREATE INDEX IF NOT EXISTS idx_delegation_assertions_source_generation ON delegation_assertions(source_object_id, source_generation);
CREATE INDEX IF NOT EXISTS idx_canonical_delegations_session ON canonical_delegations(session_key, child_run_key);
CREATE INDEX IF NOT EXISTS idx_canonical_delegations_session_activity ON canonical_delegations(session_key, (CASE WHEN source_time IS NULL THEN 1 ELSE 0 END), COALESCE(source_time, '') DESC, child_run_key DESC);
CREATE INDEX IF NOT EXISTS idx_delegation_metadata_assertions_child ON delegation_metadata_assertions(child_run_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_delegation_metadata_assertions_source ON delegation_metadata_assertions(source_object_id, child_run_key);
CREATE INDEX IF NOT EXISTS idx_delegation_metadata_assertions_task ON delegation_metadata_assertions(session_key, native_task_id, child_run_key);
CREATE INDEX IF NOT EXISTS idx_canonical_delegation_metadata_session ON canonical_delegation_metadata(session_key, child_run_key);
CREATE INDEX IF NOT EXISTS idx_delegation_spawn_assertions_task ON delegation_spawn_assertions(session_key, native_task_id, parent_run_key);
CREATE INDEX IF NOT EXISTS idx_delegation_spawn_assertions_source ON delegation_spawn_assertions(source_object_id, spawn_key);
CREATE INDEX IF NOT EXISTS idx_delegation_spawn_assertions_source_generation ON delegation_spawn_assertions(source_object_id, source_generation);
CREATE INDEX IF NOT EXISTS idx_canonical_delegation_spawns_session ON canonical_delegation_spawns(session_key, native_task_id, spawn_key);
CREATE INDEX IF NOT EXISTS idx_team_snapshot_assertions_team ON team_snapshot_assertions(team_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_team_snapshot_assertions_source ON team_snapshot_assertions(source_object_id, team_key);
CREATE INDEX IF NOT EXISTS idx_team_member_assertions_team ON team_member_assertions(team_key, member_key);
CREATE INDEX IF NOT EXISTS idx_canonical_team_members_team ON canonical_team_members(team_key, member_ordinal);
CREATE INDEX IF NOT EXISTS idx_canonical_teams_native ON canonical_teams(native_team_id, team_key);
CREATE INDEX IF NOT EXISTS idx_team_inbox_snapshot_assertions_inbox ON team_inbox_snapshot_assertions(inbox_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_team_inbox_snapshot_assertions_source ON team_inbox_snapshot_assertions(source_object_id, inbox_key);
CREATE INDEX IF NOT EXISTS idx_team_inbox_message_assertions_inbox ON team_inbox_message_assertions(inbox_key, message_key);
CREATE INDEX IF NOT EXISTS idx_canonical_team_inboxes_team ON canonical_team_inboxes(team_key, inbox_key);
CREATE INDEX IF NOT EXISTS idx_canonical_team_inboxes_recipient ON canonical_team_inboxes(team_key, native_recipient_name, inbox_key);
CREATE INDEX IF NOT EXISTS idx_canonical_team_inbox_messages_inbox ON canonical_team_inbox_messages(inbox_key, message_ordinal);
CREATE INDEX IF NOT EXISTS idx_task_snapshot_assertions_collection ON task_snapshot_assertions(collection_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_task_snapshot_assertions_source ON task_snapshot_assertions(source_object_id, collection_key);
CREATE INDEX IF NOT EXISTS idx_task_snapshot_assertions_session ON task_snapshot_assertions(session_key, collection_key);
CREATE INDEX IF NOT EXISTS idx_task_item_assertions_collection ON task_item_assertions(collection_key, task_key);
CREATE INDEX IF NOT EXISTS idx_task_item_assertions_task ON task_item_assertions(task_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_canonical_task_collections_session ON canonical_task_collections(session_key, collection_key);
CREATE INDEX IF NOT EXISTS idx_canonical_task_collections_session_native ON canonical_task_collections(session_key, native_collection_id, collection_key);
CREATE INDEX IF NOT EXISTS idx_canonical_task_collections_run ON canonical_task_collections(run_key, native_collection_id, collection_key);
CREATE INDEX IF NOT EXISTS idx_canonical_task_collections_team ON canonical_task_collections(team_key, native_collection_id, collection_key);
CREATE INDEX IF NOT EXISTS idx_canonical_task_collections_native ON canonical_task_collections(native_collection_id, collection_key);
CREATE INDEX IF NOT EXISTS idx_canonical_tasks_collection ON canonical_tasks(collection_key, item_ordinal);
CREATE INDEX IF NOT EXISTS idx_plan_assertions_plan ON plan_assertions(plan_key, fact_id);
CREATE INDEX IF NOT EXISTS idx_plan_assertions_source ON plan_assertions(source_object_id, plan_key);
CREATE INDEX IF NOT EXISTS idx_canonical_plans_native ON canonical_plans(native_plan_id, plan_key);
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
CREATE INDEX IF NOT EXISTS idx_canonical_workflows_session_activity ON canonical_workflows(session_key, project_key, (CASE WHEN COALESCE(finished_at, started_at) IS NULL THEN 1 ELSE 0 END), COALESCE(finished_at, started_at, '') DESC, workflow_key DESC);
CREATE INDEX IF NOT EXISTS idx_canonical_workflow_members_workflow ON canonical_workflow_members(workflow_key, native_agent_id);
CREATE INDEX IF NOT EXISTS idx_canonical_workflow_members_workflow_order ON canonical_workflow_members(workflow_key, native_agent_id, member_key);
DROP INDEX IF EXISTS idx_usage_contributions_session;

-- Persistent FTS5 (content-synced with messages)
CREATE VIRTUAL TABLE IF NOT EXISTS search_fts USING fts5(text_content, content='messages', content_rowid='id');
CREATE VIRTUAL TABLE IF NOT EXISTS subagent_search_fts USING fts5(search_text, content='subagent_timeline_messages', content_rowid='id');
CREATE VIEW IF NOT EXISTS canonical_searchable_messages AS
SELECT rowid, search_text
FROM canonical_messages
WHERE search_text IS NOT NULL AND trim(search_text) <> '';
CREATE VIRTUAL TABLE IF NOT EXISTS canonical_message_search_fts USING fts5(search_text, content='canonical_searchable_messages', content_rowid='rowid');

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
CREATE TRIGGER IF NOT EXISTS canonical_messages_search_ai AFTER INSERT ON canonical_messages
WHEN new.search_text IS NOT NULL AND trim(new.search_text) <> '' BEGIN
  INSERT INTO canonical_message_search_fts(rowid, search_text) VALUES (new.rowid, new.search_text);
END;
CREATE TRIGGER IF NOT EXISTS canonical_messages_search_ad AFTER DELETE ON canonical_messages
WHEN old.search_text IS NOT NULL AND trim(old.search_text) <> '' BEGIN
  INSERT INTO canonical_message_search_fts(canonical_message_search_fts, rowid, search_text)
  VALUES ('delete', old.rowid, old.search_text);
END;
CREATE TRIGGER IF NOT EXISTS canonical_messages_search_au AFTER UPDATE OF search_text ON canonical_messages BEGIN
  INSERT INTO canonical_message_search_fts(canonical_message_search_fts, rowid, search_text)
  SELECT 'delete', old.rowid, old.search_text
  WHERE old.search_text IS NOT NULL AND trim(old.search_text) <> '';
  INSERT INTO canonical_message_search_fts(rowid, search_text)
  SELECT new.rowid, new.search_text
  WHERE new.search_text IS NOT NULL AND trim(new.search_text) <> '';
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

const CURRENT_VIEWS = ['canonical_searchable_messages'];

/**
 * All tables in the current schema (used for drop-and-recreate).
 */
const CURRENT_TABLES = [
  'search_fts',
  'subagent_search_fts',
  'canonical_message_search_fts',
  'canonical_workflow_members',
  'canonical_workflows',
  'workflow_member_event_assertions',
  'workflow_snapshot_assertions',
  'canonical_session_index_entries',
  'canonical_session_indexes',
  'session_index_entry_assertions',
  'session_index_snapshot_assertions',
  'canonical_project_memory_documents',
  'project_memory_document_assertions',
  'canonical_effective_interpretation_settings',
  'canonical_interpretation_settings_documents',
  'interpretation_settings_assertions',
  'canonical_persisted_tool_results',
  'persisted_tool_result_assertions',
  'canonical_message_content_blocks',
  'message_tool_references',
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
  'runtime_actor_affiliations_v2',
  'runtime_actor_runs_v2',
  'usage_v2_response_contributions',
  'usage_v2_qualification_specs',
  'usage_totals',
  'usage_contributions',
  'run_evidence',
  'canonical_messages',
  'canonical_runs',
  'canonical_sessions',
  'fact_dependency_reads',
  'fact_records',
  'source_record_errors',
  'change_log',
  'change_log_retention_state',
  'source_coverage_errors',
  'source_coverage_absences',
  'source_coverage_points',
  'source_coverage_sets',
  'query_pack_selections',
  'projection_versions',
  'catalog_snapshot_entries',
  'catalog_build_state',
  'catalog_snapshots',
  'catalog_coverage_plans',
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
 * - For every stale version, drops ALL old + current tables and recreates
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

    for (const view of CURRENT_VIEWS) {
      try {
        db.exec(`DROP VIEW IF EXISTS ${view}`);
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
      'canonical_messages_search_ai',
      'canonical_messages_search_ad',
      'canonical_messages_search_au',
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
    // Version matches — ensure current objects exist and retire explicitly
    // superseded indexes. The schema DDL is idempotent on a healthy database.
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
