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
/// v12: ingestion-owned session/day rollups; query paths are read-only.
/// v13: timestamp-independent summary totals and materialization checkpoints.
/// v14: materialized Claude AI/custom session titles.
/// v15: subagents.worktree_path — the meta sidecar's `worktreePath`, which
/// both ingest paths parsed and then discarded at write time.
/// v17: RFC 011 source catalog, atomic ingest commits, projection readiness,
/// record diagnostics, and durable change-log outbox.
/// v18: RFC 011 provenance-bearing fact storage, canonical shadow history,
/// runtime evidence/state, and contribution-based usage totals.
/// v19: RFC 011 delegation capability assertions and late-correlated canonical
/// subagent relations.
/// v20: replaceable subagent metadata assertions and late-correlated canonical
/// delegation metadata.
/// v21: native delegation spawn assertions and explicit spawn/metadata
/// correlation provenance.
/// v22: authoritative Claude team configuration and inbox snapshots with
/// normalized membership/message projections and replacement provenance.
/// v23: Claude active-session presence assertions, canonical current rows,
/// process-incarnation identity, and conflict provenance.
/// v24: replaceable task/todo/plan assertions with explicit snapshot
/// coverage, canonical current rows, and conflict provenance.
/// v25: transcript artifact metadata, independently replaceable file-history
/// content, late-join availability, and conflict provenance.
/// v26: workflow run snapshots, append journal member events, late joins, and
/// workflow/member conflict provenance.
/// v27: replaceable Claude session-index snapshots, normalized entry metadata,
/// transcript late joins, and project/entry conflict provenance.
/// v28: independently replaceable project-memory Markdown documents, native
/// index classification, and deterministic conflict provenance.
/// v29: persisted tool-result text assertions, deterministic transcript block
/// correlation, and explicit join/conflict state.
/// v30: redacted interpretation-settings assertions, document health, and
/// native global/local effective-setting reduction.
/// v31: separately versioned common-driver checkpoints for restart-safe source
/// resume without conflating driver state with adapter decoder state.
pub const SCHEMA_VERSION: u32 = 31;

/// Full DDL for the current schema — lifted verbatim from the TS `SCHEMA_SQL`
/// template literal. Whitespace differs; structure does not.
const SCHEMA_SQL: &str = r#"
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
  -- sidecar's `worktreePath`. NULL for the vast majority of agents, which
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
  driver_checkpoint BLOB,
  driver_checkpoint_version INTEGER,
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
CREATE INDEX IF NOT EXISTS idx_canonical_messages_session_order ON canonical_messages(session_key, source_generation, cursor_start);
CREATE INDEX IF NOT EXISTS idx_canonical_messages_session_activity ON canonical_messages(session_key, source_time, message_key, source_time_quality, last_commit_seq);
CREATE INDEX IF NOT EXISTS idx_canonical_runs_session ON canonical_runs(session_key, run_key);
CREATE INDEX IF NOT EXISTS idx_usage_contributions_session_time ON usage_contributions(session_key, source_time, fact_id);
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
"#;

/// Tables from previous schema versions that should be dropped during
/// migration. Kept verbatim with the TS `LEGACY_TABLES` list.
const LEGACY_TABLES: &[&str] = &["segments", "search_index", "schema_version"];

/// All tables in the current schema, used for drop-and-recreate. Kept verbatim
/// with the TS `CURRENT_TABLES` list (same order).
const CURRENT_TABLES: &[&str] = &[
    "search_fts",
    "subagent_search_fts",
    "canonical_workflow_members",
    "canonical_workflows",
    "workflow_member_event_assertions",
    "workflow_snapshot_assertions",
    "canonical_session_index_entries",
    "canonical_session_indexes",
    "session_index_entry_assertions",
    "session_index_snapshot_assertions",
    "canonical_project_memory_documents",
    "project_memory_document_assertions",
    "canonical_effective_interpretation_settings",
    "canonical_interpretation_settings_documents",
    "interpretation_settings_assertions",
    "canonical_persisted_tool_results",
    "persisted_tool_result_assertions",
    "message_tool_references",
    "canonical_artifacts",
    "artifact_content_assertions",
    "artifact_metadata_assertions",
    "artifact_snapshot_assertions",
    "canonical_plans",
    "plan_assertions",
    "canonical_tasks",
    "canonical_task_collections",
    "task_item_assertions",
    "task_snapshot_assertions",
    "canonical_delegation_spawns",
    "canonical_delegation_metadata",
    "canonical_delegations",
    "canonical_team_inbox_messages",
    "canonical_team_inboxes",
    "team_inbox_message_assertions",
    "team_inbox_snapshot_assertions",
    "canonical_team_members",
    "canonical_teams",
    "team_member_assertions",
    "team_snapshot_assertions",
    "delegation_spawn_assertions",
    "delegation_metadata_assertions",
    "delegation_assertions",
    "canonical_presences",
    "presence_assertions",
    "observed_run_states",
    "usage_totals",
    "usage_contributions",
    "run_evidence",
    "canonical_messages",
    "canonical_runs",
    "canonical_sessions",
    "fact_records",
    "source_record_errors",
    "change_log",
    "projection_versions",
    "source_objects",
    "source_streams",
    "ingest_commits",
    "source_instances",
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
    "token_activity_session_daily",
    "token_activity_dirty",
    "session_summary_totals",
    "session_summary_dirty",
    "source_materializations",
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

const SESSION_SUMMARY_TRIGGERS: &[&str] = &[
    "session_summary_messages_ai",
    "session_summary_messages_ad",
    "session_summary_messages_au",
    "session_summary_subagents_ai",
    "session_summary_subagents_ad",
    "session_summary_subagents_au",
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
"#;

const SESSION_SUMMARY_TRIGGERS_SQL: &str = r#"
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
    for trigger in SESSION_SUMMARY_TRIGGERS {
        conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {trigger}"))?;
    }
    Ok(())
}

fn refresh_token_activity_triggers(conn: &Connection) -> Result<(), SchemaError> {
    for trigger in TOKEN_ACTIVITY_TRIGGERS {
        conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {trigger}"))?;
    }
    conn.execute_batch(TOKEN_ACTIVITY_TRIGGERS_SQL)?;
    for trigger in SESSION_SUMMARY_TRIGGERS {
        conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {trigger}"))?;
    }
    conn.execute_batch(SESSION_SUMMARY_TRIGGERS_SQL)?;
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
    conn.execute_batch(SESSION_SUMMARY_TRIGGERS_SQL)?;
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

    // Kept as a reusable block so bulk ingestion can drop/recreate this
    // invalidation family without duplicating it inside SCHEMA_SQL.
    conn.execute_batch(SESSION_SUMMARY_TRIGGERS_SQL)?;

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
        assert!(object_exists(&conn, "table", "session_summary_totals"));
        assert!(object_exists(&conn, "table", "source_materializations"));
        assert!(object_exists(&conn, "table", "source_instances"));
        assert!(object_exists(&conn, "table", "source_streams"));
        assert!(object_exists(&conn, "table", "source_objects"));
        for column in ["driver_checkpoint", "driver_checkpoint_version"] {
            let present: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('source_objects') WHERE name = ?1",
                    [column],
                    |row| row.get(0),
                )
                .expect("inspect source object schema");
            assert_eq!(present, 1, "missing source_objects.{column}");
        }
        assert!(object_exists(&conn, "table", "ingest_commits"));
        assert!(object_exists(&conn, "table", "projection_versions"));
        assert!(object_exists(&conn, "table", "source_record_errors"));
        assert!(object_exists(&conn, "table", "change_log"));
        assert!(object_exists(&conn, "table", "fact_records"));
        assert!(object_exists(&conn, "table", "canonical_sessions"));
        assert!(object_exists(
            &conn,
            "table",
            "session_index_snapshot_assertions"
        ));
        assert!(object_exists(
            &conn,
            "table",
            "session_index_entry_assertions"
        ));
        assert!(object_exists(&conn, "table", "canonical_session_indexes"));
        assert!(object_exists(
            &conn,
            "table",
            "canonical_session_index_entries"
        ));
        assert!(object_exists(
            &conn,
            "table",
            "project_memory_document_assertions"
        ));
        assert!(object_exists(
            &conn,
            "table",
            "canonical_project_memory_documents"
        ));
        assert!(object_exists(
            &conn,
            "index",
            "idx_canonical_project_memory_documents_project"
        ));
        assert!(object_exists(
            &conn,
            "table",
            "persisted_tool_result_assertions"
        ));
        assert!(object_exists(
            &conn,
            "table",
            "canonical_persisted_tool_results"
        ));
        assert!(object_exists(
            &conn,
            "table",
            "interpretation_settings_assertions"
        ));
        assert!(object_exists(
            &conn,
            "table",
            "canonical_interpretation_settings_documents"
        ));
        assert!(object_exists(
            &conn,
            "table",
            "canonical_effective_interpretation_settings"
        ));
        assert!(object_exists(
            &conn,
            "index",
            "idx_canonical_interpretation_settings_documents_scope"
        ));
        assert!(object_exists(&conn, "table", "message_tool_references"));
        assert!(object_exists(
            &conn,
            "index",
            "idx_message_tool_references_native"
        ));
        assert!(object_exists(&conn, "table", "canonical_messages"));
        assert!(object_exists(&conn, "table", "canonical_runs"));
        assert!(object_exists(&conn, "table", "run_evidence"));
        assert!(object_exists(&conn, "table", "observed_run_states"));
        assert!(object_exists(&conn, "table", "presence_assertions"));
        assert!(object_exists(&conn, "table", "canonical_presences"));
        assert!(object_exists(&conn, "table", "delegation_assertions"));
        assert!(object_exists(&conn, "table", "canonical_delegations"));
        assert!(object_exists(
            &conn,
            "table",
            "delegation_metadata_assertions"
        ));
        assert!(object_exists(
            &conn,
            "table",
            "canonical_delegation_metadata"
        ));
        assert!(object_exists(&conn, "table", "delegation_spawn_assertions"));
        assert!(object_exists(&conn, "table", "canonical_delegation_spawns"));
        assert!(object_exists(&conn, "table", "team_snapshot_assertions"));
        assert!(object_exists(&conn, "table", "team_member_assertions"));
        assert!(object_exists(&conn, "table", "canonical_teams"));
        assert!(object_exists(&conn, "table", "canonical_team_members"));
        assert!(object_exists(
            &conn,
            "table",
            "team_inbox_snapshot_assertions"
        ));
        assert!(object_exists(
            &conn,
            "table",
            "team_inbox_message_assertions"
        ));
        assert!(object_exists(&conn, "table", "canonical_team_inboxes"));
        assert!(object_exists(
            &conn,
            "table",
            "canonical_team_inbox_messages"
        ));
        assert!(object_exists(&conn, "table", "task_snapshot_assertions"));
        assert!(object_exists(&conn, "table", "task_item_assertions"));
        assert!(object_exists(&conn, "table", "canonical_task_collections"));
        assert!(object_exists(&conn, "table", "canonical_tasks"));
        assert!(object_exists(&conn, "table", "plan_assertions"));
        assert!(object_exists(&conn, "table", "canonical_plans"));
        assert!(object_exists(
            &conn,
            "table",
            "artifact_snapshot_assertions"
        ));
        assert!(object_exists(
            &conn,
            "table",
            "artifact_metadata_assertions"
        ));
        assert!(object_exists(&conn, "table", "artifact_content_assertions"));
        assert!(object_exists(&conn, "table", "canonical_artifacts"));
        assert!(object_exists(
            &conn,
            "table",
            "workflow_snapshot_assertions"
        ));
        assert!(object_exists(
            &conn,
            "table",
            "workflow_member_event_assertions"
        ));
        assert!(object_exists(&conn, "table", "canonical_workflows"));
        assert!(object_exists(&conn, "table", "canonical_workflow_members"));
        assert!(object_exists(&conn, "table", "usage_contributions"));
        assert!(object_exists(&conn, "table", "usage_totals"));
        assert!(object_exists(&conn, "table", "search_fts")); // FTS5 virtual table
        assert!(object_exists(&conn, "index", "idx_messages_session"));
        assert!(object_exists(&conn, "index", "idx_change_log_topic_cursor"));
        assert!(object_exists(
            &conn,
            "index",
            "idx_presence_assertions_source"
        ));
        assert!(object_exists(
            &conn,
            "index",
            "idx_canonical_presences_session"
        ));
        assert!(object_exists(
            &conn,
            "index",
            "idx_canonical_tasks_collection"
        ));
        assert!(object_exists(
            &conn,
            "index",
            "idx_workflow_member_event_assertions_workflow"
        ));
        assert!(object_exists(
            &conn,
            "index",
            "idx_canonical_workflow_members_workflow"
        ));
        assert!(object_exists(
            &conn,
            "index",
            "idx_session_index_entry_assertions_session"
        ));
        assert!(object_exists(&conn, "trigger", "messages_ai"));
        assert!(object_exists(&conn, "trigger", "messages_ad"));
        assert!(object_exists(&conn, "trigger", "messages_au"));
        assert!(object_exists(
            &conn,
            "trigger",
            "session_summary_messages_ai"
        ));

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
            DROP TRIGGER session_summary_messages_ai;
            CREATE TRIGGER session_summary_messages_ai AFTER INSERT ON messages BEGIN SELECT 1; END;
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
        let summary_trigger_sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='trigger' AND name='session_summary_messages_ai'",
                [],
                |row| row.get(0),
            )
            .expect("read summary trigger body");
        assert!(summary_trigger_sql.contains("session_summary_dirty"));

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
    fn stale_schema_drops_rfc011_foreign_key_graph_in_dependency_order() {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.pragma_update(None, "foreign_keys", "ON")
            .expect("enable foreign keys");
        initialize_schema(&conn).expect("first init");
        conn.execute_batch(
            r#"
            INSERT INTO source_instances (
              source_instance_id, adapter_id, stable_key, display_name,
              adapter_contract_version, discovered_at, last_seen_at
            ) VALUES (1, 'fixture', X'01', 'Fixture', 1, 1, 1);
            INSERT INTO source_streams (
              source_stream_id, source_instance_id, stream_key, driver_kind,
              decoder_key, stream_state
            ) VALUES (1, 1, 'history', 'append_file', 'fixture', 'available');
            INSERT INTO source_objects (
              source_object_id, source_stream_id, object_key, generation,
              committed_cursor, decoder_contract_version, state
            ) VALUES (1, 1, X'02', 1, X'03', 1, 'active');
            INSERT INTO ingest_commits (
              commit_seq, source_instance_id, reason, started_at, committed_at
            ) VALUES (1, 1, 'fixture', 1, 2);
            INSERT INTO change_log (
              commit_seq, ordinal, topic, schema_version, entity_key,
              operation, payload
            ) VALUES (1, 0, 'history.session.changed', 1, X'04', 'upsert', X'05');
            INSERT INTO source_record_errors (
              source_object_id, generation, cursor_start, cursor_end,
              payload_hash, media_type, error_class, error_message,
              adapter_version, contract_version, first_commit_seq
            ) VALUES (
              1, 1, X'01', X'02', X'03', 'application/json', 'fixture',
              'fixture', '1.0.0', 1, 1
            );
            "#,
        )
        .expect("seed RFC 011 graph");
        conn.execute(
            "UPDATE schema_meta SET value = ?1 WHERE key = 'version'",
            [SCHEMA_VERSION.saturating_sub(1).to_string()],
        )
        .expect("mark schema stale");

        initialize_schema(&conn).expect("wipe and rebuild RFC 011 graph");

        for table in [
            "source_instances",
            "source_streams",
            "source_objects",
            "ingest_commits",
            "change_log",
            "source_record_errors",
        ] {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("count rebuilt table");
            assert_eq!(count, 0, "{table} should be rebuilt empty");
        }
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
