//! SQLite schema + migrations — ported from `packages/sdk/src/data/schema.ts`.
//!
//! This module owns:
//! - The full DDL for the Phase 3 dedicated-table schema (core entities,
//!   indexes, the `search_fts` FTS5 virtual table + content-synced triggers).
//! - The `SCHEMA_VERSION` constant and `schema_meta`-based version tracking.
//! - [`initialize_schema`] which creates the schema on a fresh database,
//!   applies explicitly reviewed compatible migrations, or wipes and rebuilds
//!   an unknown/stale layout.
//! - [`set_pragmas`] which applies the same connection-level PRAGMAs the TS
//!   [`SqliteService`](../../../../packages/sdk/src/io/sqlite-service.ts) sets
//!   on open.
//!
//! Schema versions remain wipe-on-stale by default. A version is migrated in
//! place only when the transition is explicitly allow-listed below and proven
//! not to reinterpret existing durable rows.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use std::time::{Duration, Instant};
use thiserror::Error;

mod pragmas;
pub use pragmas::{set_bootstrap_ingest_pragmas, set_pragmas};

const WRITER_CACHE_KIB: i64 = 128_000;
const SQLITE_MMAP_BYTES: i64 = 256 * 1024 * 1024;
// A 32 MiB WAL kept 100-sample 64-record burst p99 below 100 ms without the
// repeated cold-load penalty measured at 16 MiB. See the RFC 011 performance
// optimization record for the controlled checkpoint spike.
// The writer actor owns the checkpoint schedule so checkpoint latency,
// reader blocking, and progress are observable. SQLite's implicit hook is
// disabled rather than competing with that policy inside transaction commit.
const WAL_AUTOCHECKPOINT_PAGES: i64 = 0;
const WAL_JOURNAL_LIMIT_BYTES: i64 = 64 * 1024 * 1024;
const BOOTSTRAP_CACHE_KIB: i64 = 1_000_000;
const BOOTSTRAP_MMAP_BYTES: i64 = 2 * 1024 * 1024 * 1024;
const BOOTSTRAP_JOURNAL_LIMIT_BYTES: i64 = 2 * 1024 * 1024 * 1024;

const BOOTSTRAP_STATE_KEY: &str = "query_bootstrap_state";
/// A bootstrap whose finalization failed validation (foreign-key or integrity
/// check) cannot be repaired in place: re-entering finalization fails
/// identically forever. The marker value makes the next open wipe and rebuild
/// — the database is a pure function of the source files, so a rebuild is the
/// correct response to any validation failure, never a permanent error loop.
const BOOTSTRAP_STATE_VALIDATION_FAILED: &str = "validation_failed";
const BOOTSTRAP_EPOCH_KEY: &str = "query_bootstrap_epoch";
const BOOTSTRAP_COMPLETED_EPOCH_KEY: &str = "query_bootstrap_completed_epoch";
const BOOTSTRAP_SNAPSHOT_COMMIT_KEY: &str = "query_bootstrap_snapshot_commit_seq";

/// Secondary structures that have no ingestion-time consumer. Uniqueness,
/// foreign-key support, generation retraction, usage-series reduction, and
/// run-state reducer indexes deliberately do not appear here.
const BOOTSTRAP_QUERY_INDEXES: &[(&str, &str)] = &[
    (
        "idx_change_log_topic_cursor",
        "CREATE INDEX IF NOT EXISTS idx_change_log_topic_cursor ON change_log(topic, commit_seq, ordinal)",
    ),
    (
        "idx_fact_records_source_instance_compact",
        "CREATE INDEX IF NOT EXISTS idx_fact_records_source_instance_compact ON fact_records(source_instance_id)",
    ),
    (
        "idx_canonical_message_blocks_session_kind",
        "CREATE INDEX IF NOT EXISTS idx_canonical_message_blocks_session_kind ON canonical_message_content_blocks(session_key, content_kind, message_key)",
    ),
    (
        "idx_canonical_message_blocks_session_tool",
        "CREATE INDEX IF NOT EXISTS idx_canonical_message_blocks_session_tool ON canonical_message_content_blocks(session_key, tool_name, message_key) WHERE tool_name IS NOT NULL",
    ),
    (
        "idx_canonical_messages_session_activity",
        "CREATE INDEX IF NOT EXISTS idx_canonical_messages_session_activity ON canonical_messages(session_key, source_time, message_key, source_time_quality, last_commit_seq)",
    ),
    (
        "idx_canonical_messages_run_activity",
        "CREATE INDEX IF NOT EXISTS idx_canonical_messages_run_activity ON canonical_messages(run_key, source_time, message_key)",
    ),
    (
        "idx_usage_v2_response_session_time",
        "CREATE INDEX IF NOT EXISTS idx_usage_v2_response_session_time ON usage_v2_response_contributions(session_key, source_time, usage_key)",
    ),
];

const CANONICAL_FTS_TRIGGERS: &[&str] = &[
    "canonical_messages_search_ai",
    "canonical_messages_search_ad",
    "canonical_messages_search_au",
];

/// The current schema version. Bumping this forces a wipe-and-rebuild unless
/// the preceding version has an explicitly reviewed compatible migration.
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
/// v32: message-to-run identity plus a writer-maintained canonical-message
/// FTS5 projection. This intentionally forces a rebuild so pre-existing
/// canonical messages cannot appear searchable without index entries.
/// v33: writer-maintained common content-block metadata for canonical timeline
/// filters and facets. Canonical content remains authoritative on the message;
/// this narrow index avoids decoding JSON arrays during aggregate queries.
/// v34: durable common-driver retry state on source objects so a stable
/// malformed snapshot is bounded across process restarts.
/// v35: durable change-log retention accounting and the pruned commit floor
/// required to reject stale subscription cursors deterministically.
/// v36: raw counter values and an indexed series key for truthful cumulative
/// and snapshot usage reduction.
/// v37: durable adapter version, source-schema versions, and capability
/// declarations on each source instance for offline SDK inspection.
/// v38: stamped adapter dependency reads attached to durable fact records.
/// v39: versioned zstd storage for fact audit blobs and canonical native
/// message payloads. Message audit JSON no longer duplicates the native body;
/// stale rebuildable caches are compacted after the wipe.
/// v40: stream retention is durable, non-Full streams keep provenance-only
/// fact records, and the unused wide fact entity/kind index is removed.
/// v41: canonical FTS uses a filtered external-content view. This makes the
/// FTS5 content-integrity contract match the intentionally searchable subset
/// instead of treating NULL/empty canonical messages as missing documents.
/// v42: canonical normalized message content uses the same bounded versioned
/// zstd storage as lossless native payloads. Timeline/detail queries decode
/// only their bounded page, reducing cold-ingest and checkpoint I/O.
/// v43: run evidence is compacted per run/source-generation/category while
/// retaining exact winner provenance, total evidence counts, and maximum
/// activity time. This removes per-message projection/index amplification;
/// fact_records remains the complete durable evidence ledger.
/// v44: an unambiguous message fact owns its paired native-activity evidence
/// from the same source record/run/timestamp. The compact run-evidence row
/// retains the adapter's evidence dimensions and count while the rebuild drops
/// the second provenance row for the identical source observation.
/// v45: explicit RFC 012A source-record/fact/revision identities are retained
/// beside the RFC 011 storage key. Legacy facts keep an all-NULL semantic
/// triple; a partial triple or duplicate semantic revision is rejected.
/// v46: RFC 012C response-level usage-v2 qualification definitions and latest
/// response revisions land as a non-public shadow projection beside legacy
/// additive usage.
/// v47: topology-neutral runtime actor and affiliation revisions used to
/// regroup usage without copying response contributions.
/// v48: normalized, persistable RFC 012A source/fact-family coverage sets,
/// points, absences, and errors owned by common projection transitions.
/// v49: source-scoped query-pack selection with explicit rollback target,
/// epoch, and durable commit ownership for RFC 012C usage-v2 migration.
/// v50: source-neutral RFC 012B Library coverage-plan registration and the
/// initial Pending/Building readiness lineage on the common commit clock.
/// v51: immutable RFC 012B initial catalog snapshots, private typed payload
/// frames, and the atomically linked initial Ready state.
/// v52: ordinary RFC 012B Ready refresh administration that retains the exact
/// current complete snapshot while advancing the durable state commit.
/// v53: atomic RFC 012B ordinary-refresh successor snapshots with exact
/// predecessor commitments and cumulative member-identity history.
/// v54: append-only RFC 012B logical query-retirement evidence for retained
/// catalog snapshots.
/// v55: append-only independently-safe integrity-failure evidence for an
/// active RFC 012B Library refresh while retaining its exact prior snapshot.
/// v56: bounded current-generation RFC 012C unknown-native evidence owned by
/// topology-neutral source-record identities.
/// v57: append-only RFC 012B terminal source-unavailability evidence plus
/// durable retrying/degraded/recovery Library readiness that retains its last
/// safe snapshot.
/// v58: RFC 012B discarded initial-build integrity evidence and retryable
/// no-snapshot Error readiness.
/// v59: append-only RFC 012B Partial coverage milestones with exact
/// predecessor ownership and restart validation.
/// v60: immutable RFC 012B source-generation epoch invalidation lineage.
/// v61: append-only RFC 012B required-source failure evidence for an initial
/// no-snapshot catalog build, kept separate from retained-refresh authority.
/// v62: immutable RFC 012B coverage-plan replacement lineage, retaining an
/// independently safe prior-plan snapshot while the successor builds.
/// v64: RFC 012B catalog rows replace the publication/epoch/lineage tables.
/// Discovery upserts projects/sessions with their RFC 012A external
/// references, association evidence, and conflicts; readiness and
/// `catalog_state` are derived from committed rows instead of a durable
/// state machine.
pub const SCHEMA_VERSION: u32 = 64;

/// Core DDL for the current schema, lifted from the TS `SCHEMA_SQL` template
/// literal. The query-only indexes in [`BOOTSTRAP_QUERY_INDEXES`] and the
/// canonical FTS triggers are installed separately so an interrupted deferred
/// bootstrap can reopen without eagerly rebuilding them.
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
  CHECK (source_instance_id IS NOT NULL)
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

-- RFC 012B catalog: what is DISCOVERABLE, independent of what is decoded.
--
-- A catalog row is bounded native evidence that a project or session exists.
-- It never asserts decoded history: `catalog_state` (discovered ->
-- transcript_backed -> hydrated -> searchable) is derived at read time by
-- joining these rows against the RFC 011 canonical tables inside one snapshot,
-- so each fact keeps exactly one authority and the two cannot drift.
--
-- `project_key`/`session_key` are the same `EntityKey::native` bytes the
-- adapter decoders emit, so a discovered row and its later transcript-backed
-- history are one entity. `external_ref` is the RFC 012A `ExternalEntityRef`
-- digest that downstream consumers persist.
CREATE TABLE IF NOT EXISTS catalog_sources (
  source_instance_id INTEGER PRIMARY KEY REFERENCES source_instances(source_instance_id) ON DELETE CASCADE,
  adapter_id TEXT NOT NULL,
  degraded INTEGER NOT NULL DEFAULT 0 CHECK (degraded IN (0, 1)),
  degraded_reason TEXT CHECK (degraded_reason IS NULL OR length(degraded_reason) BETWEEN 1 AND 512),
  project_count INTEGER NOT NULL DEFAULT 0 CHECK (project_count >= 0),
  session_count INTEGER NOT NULL DEFAULT 0 CHECK (session_count >= 0),
  scanned_at_commit_seq INTEGER NOT NULL,
  scanned_at INTEGER NOT NULL,
  -- Digest of everything the last pass asserted. A pass that matches it has
  -- nothing to write, so an unchanged rescan costs no commit.
  content_digest BLOB NOT NULL CHECK (length(content_digest) = 32),
  CHECK ((degraded = 0) = (degraded_reason IS NULL))
);

CREATE TABLE IF NOT EXISTS catalog_projects (
  project_key BLOB PRIMARY KEY CHECK (length(project_key) BETWEEN 1 AND 4096),
  source_instance_id INTEGER NOT NULL REFERENCES source_instances(source_instance_id) ON DELETE CASCADE,
  adapter_id TEXT NOT NULL,
  external_ref BLOB NOT NULL CHECK (length(external_ref) = 32),
  native_project_key TEXT NOT NULL,
  display_name TEXT,
  display_path TEXT,
  first_seen_commit_seq INTEGER NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS catalog_projects_source
  ON catalog_projects(source_instance_id);

CREATE TABLE IF NOT EXISTS catalog_sessions (
  session_key BLOB PRIMARY KEY CHECK (length(session_key) BETWEEN 1 AND 4096),
  project_key BLOB NOT NULL,
  source_instance_id INTEGER NOT NULL REFERENCES source_instances(source_instance_id) ON DELETE CASCADE,
  adapter_id TEXT NOT NULL,
  external_ref BLOB NOT NULL CHECK (length(external_ref) = 32),
  native_session_key TEXT NOT NULL,
  native_session_id TEXT,
  title TEXT,
  association_basis TEXT NOT NULL CHECK (
    association_basis IN ('native_project_index', 'session_directory', 'rollout_header', 'transcript_cwd')
  ),
  association_quality TEXT NOT NULL CHECK (
    association_quality IN ('exact', 'native_claimed', 'derived')
  ),
  association_provenance TEXT NOT NULL,
  native_created_at TEXT,
  native_updated_at TEXT,
  native_message_count INTEGER CHECK (native_message_count IS NULL OR native_message_count >= 0),
  transcript_present INTEGER NOT NULL DEFAULT 0 CHECK (transcript_present IN (0, 1)),
  transcript_locator TEXT,
  source_size_bytes INTEGER CHECK (source_size_bytes IS NULL OR source_size_bytes >= 0),
  source_modified_ms INTEGER,
  sort_time TEXT NOT NULL DEFAULT '',
  first_seen_commit_seq INTEGER NOT NULL,
  last_commit_seq INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS catalog_sessions_project
  ON catalog_sessions(project_key, sort_time DESC, session_key DESC);

CREATE INDEX IF NOT EXISTS catalog_sessions_source
  ON catalog_sessions(source_instance_id);

CREATE INDEX IF NOT EXISTS catalog_sessions_activity
  ON catalog_sessions(sort_time DESC, session_key DESC);

-- Competing project associations that lost precedence. RFC 012B forbids
-- merging them away, so they stay queryable next to the selected one.
CREATE TABLE IF NOT EXISTS catalog_association_conflicts (
  session_key BLOB NOT NULL REFERENCES catalog_sessions(session_key) ON DELETE CASCADE,
  competing_native_project_key TEXT NOT NULL,
  basis TEXT NOT NULL,
  provenance TEXT NOT NULL,
  last_commit_seq INTEGER NOT NULL,
  PRIMARY KEY (session_key, competing_native_project_key, basis)
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

-- RFC 012C bounded unknown-native evidence. This is a current-generation
-- semantic projection, not a per-record diagnostic log. The common writer
-- enforces the hard per-object occurrence ceiling and derives samples and the
-- complete aggregate digest with the topology-neutral reducer.
CREATE TABLE IF NOT EXISTS unknown_native_evidence (
  source_record_id BLOB PRIMARY KEY CHECK (length(source_record_id) = 32),
  source_instance_id INTEGER NOT NULL REFERENCES source_instances(source_instance_id) ON DELETE CASCADE,
  source_stream_id INTEGER NOT NULL REFERENCES source_streams(source_stream_id) ON DELETE CASCADE,
  source_object_id INTEGER NOT NULL REFERENCES source_objects(source_object_id) ON DELETE CASCADE,
  source_generation INTEGER NOT NULL CHECK (source_generation >= 0),
  family_hint TEXT CHECK (family_hint IS NULL OR (length(family_hint) BETWEEN 1 AND 128)),
  observed_bytes INTEGER NOT NULL CHECK (observed_bytes >= 0 AND observed_bytes <= 4194304),
  payload_digest BLOB NOT NULL CHECK (length(payload_digest) = 32),
  sanitized_excerpt BLOB NOT NULL CHECK (length(sanitized_excerpt) BETWEEN 1 AND 1024),
  last_commit_seq INTEGER NOT NULL REFERENCES ingest_commits(commit_seq) ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_unknown_native_evidence_object_generation
ON unknown_native_evidence(source_object_id, source_generation, source_record_id);

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
CREATE INDEX IF NOT EXISTS idx_projection_versions_readiness ON projection_versions(readiness, projection_id);
CREATE INDEX IF NOT EXISTS idx_source_coverage_sets_instance_owner ON source_coverage_sets(source_instance_id, owner_id);
CREATE INDEX IF NOT EXISTS idx_source_coverage_points_object ON source_coverage_points(stream_key, object_key, coverage_set_id);
CREATE INDEX IF NOT EXISTS idx_source_record_errors_commit ON source_record_errors(first_commit_seq);
CREATE INDEX IF NOT EXISTS idx_fact_records_object_generation ON fact_records(source_object_id, source_generation);
CREATE UNIQUE INDEX IF NOT EXISTS idx_fact_records_semantic_revision ON fact_records(semantic_fact_revision_id) WHERE semantic_fact_revision_id IS NOT NULL;
DROP INDEX IF EXISTS idx_fact_records_source_instance;
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
DROP INDEX IF EXISTS idx_canonical_message_blocks_run;
DROP INDEX IF EXISTS idx_canonical_messages_session_order;
CREATE INDEX IF NOT EXISTS idx_canonical_messages_source_generation ON canonical_messages(source_object_id, source_generation);
CREATE INDEX IF NOT EXISTS idx_canonical_runs_session ON canonical_runs(session_key, run_key);
CREATE INDEX IF NOT EXISTS idx_canonical_runs_commit ON canonical_runs(last_commit_seq DESC, run_key DESC);
CREATE INDEX IF NOT EXISTS idx_canonical_runs_source_generation ON canonical_runs(source_object_id, source_generation);
CREATE INDEX IF NOT EXISTS idx_usage_v2_response_session ON usage_v2_response_contributions(session_key, usage_key);
CREATE INDEX IF NOT EXISTS idx_usage_v2_response_actor ON usage_v2_response_contributions(actor_run_key, usage_key);
CREATE INDEX IF NOT EXISTS idx_usage_v2_response_source_generation ON usage_v2_response_contributions(source_object_id, source_generation);
CREATE INDEX IF NOT EXISTS idx_runtime_actor_runs_v2_session ON runtime_actor_runs_v2(session_key, actor_run_key);
-- The usage scope bridge joins catalog sessions to RFC 012C identities on the
-- native session id, so that column needs its own lookup path.
CREATE INDEX IF NOT EXISTS idx_runtime_actor_runs_v2_native_session ON runtime_actor_runs_v2(native_session_id, session_key);
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

const CURRENT_VIEWS: &[&str] = &["canonical_searchable_messages"];

/// All tables in the current schema, used for drop-and-recreate. Kept verbatim
/// with the TS `CURRENT_TABLES` list (same order).
const CURRENT_TABLES: &[&str] = &[
    "search_fts",
    "subagent_search_fts",
    "canonical_message_search_fts",
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
    "canonical_message_content_blocks",
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
    "runtime_actor_affiliations_v2",
    "runtime_actor_runs_v2",
    "usage_v2_response_contributions",
    "usage_v2_qualification_specs",
    "run_evidence",
    "canonical_messages",
    "canonical_runs",
    "canonical_sessions",
    "fact_dependency_reads",
    "unknown_native_evidence",
    "fact_records",
    "source_record_errors",
    "change_log",
    "change_log_retention_state",
    "source_coverage_errors",
    "source_coverage_absences",
    "source_coverage_points",
    "source_coverage_sets",
    "projection_versions",
    "catalog_association_conflicts",
    "catalog_sessions",
    "catalog_projects",
    "catalog_sources",
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
const CURRENT_TRIGGERS: &[&str] = &[
    "messages_ai",
    "messages_ad",
    "messages_au",
    "subagent_timeline_ai",
    "subagent_timeline_ad",
    "subagent_timeline_au",
    "canonical_messages_search_ai",
    "canonical_messages_search_ad",
    "canonical_messages_search_au",
];

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

/// FTS auto-sync trigger DDL, extracted so bulk-ingest can drop and recreate
/// them around a high-volume INSERT run. The canonical subset is deliberately
/// also available as [`CANONICAL_FTS_TRIGGERS_SQL`] for deferred finalization.
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
"#;

const CANONICAL_FTS_TRIGGERS_SQL: &str = r#"
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
"#;

fn create_bootstrap_query_structures(conn: &Connection) -> Result<(), SchemaError> {
    for (_, sql) in BOOTSTRAP_QUERY_INDEXES {
        conn.execute_batch(sql)?;
    }
    conn.execute_batch(CANONICAL_FTS_TRIGGERS_SQL)?;
    Ok(())
}

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

    #[error("query bootstrap validation failed: {0}")]
    BootstrapValidation(String),
}

/// Return the durable query-bootstrap stage, if finalization is incomplete.
/// Readers use this as a defense-in-depth admission check; the engine also
/// keeps its query pool absent for the complete marked interval.
pub fn query_bootstrap_state(conn: &Connection) -> Result<Option<String>, SchemaError> {
    let meta_exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_meta'",
        [],
        |row| row.get(0),
    )?;
    if meta_exists == 0 {
        return Ok(None);
    }
    conn.query_row(
        "SELECT value FROM schema_meta WHERE key = ?1",
        [BOOTSTRAP_STATE_KEY],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Atomically mark a fresh observation database as unavailable and remove
/// only the reviewed query-side structures. Returning `false` means the
/// database already contains committed observation history and must remain
/// fully indexed.
pub fn begin_query_bootstrap(conn: &mut Connection) -> Result<bool, SchemaError> {
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if transaction
        .query_row(
            "SELECT value FROM schema_meta WHERE key = ?1",
            [BOOTSTRAP_STATE_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .is_some()
    {
        transaction.rollback()?;
        return Ok(true);
    }

    let committed: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM ingest_commits WHERE committed_at IS NOT NULL",
        [],
        |row| row.get(0),
    )?;
    if committed != 0 {
        transaction.rollback()?;
        return Ok(false);
    }

    let prior_epoch = transaction
        .query_row(
            "SELECT value FROM schema_meta WHERE key = ?1",
            [BOOTSTRAP_EPOCH_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_default();
    let epoch = prior_epoch.saturating_add(1).to_string();
    transaction.execute(
        "INSERT INTO schema_meta(key, value) VALUES (?1, 'building') \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [BOOTSTRAP_STATE_KEY],
    )?;
    transaction.execute(
        "INSERT INTO schema_meta(key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [BOOTSTRAP_EPOCH_KEY, epoch.as_str()],
    )?;
    for trigger in CANONICAL_FTS_TRIGGERS {
        transaction.execute_batch(&format!("DROP TRIGGER IF EXISTS {trigger}"))?;
    }
    for (index, _) in BOOTSTRAP_QUERY_INDEXES {
        transaction.execute_batch(&format!("DROP INDEX IF EXISTS {index}"))?;
    }
    transaction.commit()?;
    Ok(true)
}

/// Recreate and verify all deferred structures before atomically clearing the
/// durable unavailability marker. The returned watermark is the snapshot
/// boundary that a newly admitted subscriber must start from.
pub fn finalize_query_bootstrap(conn: &mut Connection) -> Result<Option<u64>, SchemaError> {
    finalize_query_bootstrap_inner(conn, true, true, &mut |_, _| {})
}

/// Isolation-only: rebuild deferred structures without the foreign-key audit.
/// Production readiness still uses [`finalize_query_bootstrap`].
pub fn finalize_query_bootstrap_skip_fk_check(
    conn: &mut Connection,
) -> Result<Option<u64>, SchemaError> {
    finalize_query_bootstrap_inner(conn, false, true, &mut |_, _| {})
}

/// Finalize a cold bootstrap while exposing non-overlapping phase durations.
/// The observer is intentionally synchronous: finalization owns the sole
/// writer, and telemetry must describe the exact operation that delayed
/// readiness rather than a sampled approximation.
pub(crate) fn finalize_query_bootstrap_profiled(
    conn: &mut Connection,
    check_foreign_keys: bool,
    check_database_integrity: bool,
    mut observe: impl FnMut(&'static str, Duration),
) -> Result<Option<u64>, SchemaError> {
    finalize_query_bootstrap_inner(
        conn,
        check_foreign_keys,
        check_database_integrity,
        &mut observe,
    )
}

fn finalize_query_bootstrap_inner<F>(
    conn: &mut Connection,
    check_foreign_keys: bool,
    check_database_integrity: bool,
    observe: &mut F,
) -> Result<Option<u64>, SchemaError>
where
    F: FnMut(&'static str, Duration),
{
    if query_bootstrap_state(conn)?.is_none() {
        return Ok(None);
    }
    let started = Instant::now();
    conn.execute(
        "UPDATE schema_meta SET value = 'finalizing' WHERE key = ?1",
        [BOOTSTRAP_STATE_KEY],
    )?;

    // Index/FTS rebuilds run on the sole writer with no readers. Memory temp
    // sorts avoid an extra spill file; the writer restores interactive pragmas
    // after this function returns.
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    observe("state_and_temp_store", started.elapsed());
    for (index, sql) in BOOTSTRAP_QUERY_INDEXES {
        let started = Instant::now();
        conn.execute_batch(sql)?;
        observe(index, started.elapsed());
    }
    let started = Instant::now();
    conn.execute_batch(
        "INSERT INTO canonical_message_search_fts(canonical_message_search_fts) VALUES('rebuild')",
    )?;
    observe("fts_rebuild", started.elapsed());
    let started = Instant::now();
    conn.execute_batch(CANONICAL_FTS_TRIGGERS_SQL)?;
    observe("fts_triggers", started.elapsed());
    let started = Instant::now();
    conn.execute_batch("PRAGMA optimize=0x10002")?;
    observe("optimize", started.elapsed());
    if let Err(error) =
        validate_query_bootstrap(conn, check_foreign_keys, check_database_integrity, observe)
    {
        if matches!(error, SchemaError::BootstrapValidation(_)) {
            // Best-effort poison stamp: without it, every later open re-enters
            // this same finalization and fails identically — the only exit is
            // deleting the database by hand. With it, the next open rebuilds.
            let _ = conn.execute(
                "UPDATE schema_meta SET value = ?2 WHERE key = ?1",
                [BOOTSTRAP_STATE_KEY, BOOTSTRAP_STATE_VALIDATION_FAILED],
            );
        }
        return Err(error);
    }

    let started = Instant::now();
    let watermark: i64 = conn.query_row(
        "SELECT COALESCE(MAX(commit_seq), 0) FROM ingest_commits WHERE committed_at IS NOT NULL",
        [],
        |row| row.get(0),
    )?;
    let epoch: String = conn.query_row(
        "SELECT value FROM schema_meta WHERE key = ?1",
        [BOOTSTRAP_EPOCH_KEY],
        |row| row.get(0),
    )?;
    let watermark_text = watermark.max(0).to_string();
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute(
        "INSERT INTO schema_meta(key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [BOOTSTRAP_COMPLETED_EPOCH_KEY, epoch.as_str()],
    )?;
    transaction.execute(
        "INSERT INTO schema_meta(key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [BOOTSTRAP_SNAPSHOT_COMMIT_KEY, watermark_text.as_str()],
    )?;
    transaction.execute(
        "DELETE FROM schema_meta WHERE key = ?1",
        [BOOTSTRAP_STATE_KEY],
    )?;
    transaction.commit()?;
    observe("publish_readiness", started.elapsed());
    Ok(Some(u64::try_from(watermark).unwrap_or_default()))
}

/// Complete a bootstrap left behind by an abrupt process exit. Schema
/// initialization may already have recreated missing indexes; finalization is
/// intentionally idempotent and always rebuilds canonical FTS from content.
pub fn recover_query_bootstrap(conn: &mut Connection) -> Result<bool, SchemaError> {
    if query_bootstrap_state(conn)?.is_none() {
        return Ok(false);
    }
    finalize_query_bootstrap(conn).map(|_| true)
}

fn validate_query_bootstrap<F>(
    conn: &mut Connection,
    check_foreign_keys: bool,
    check_database_integrity: bool,
    observe: &mut F,
) -> Result<(), SchemaError>
where
    F: FnMut(&'static str, Duration),
{
    let started = Instant::now();
    for (index, _) in BOOTSTRAP_QUERY_INDEXES {
        let exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
            [*index],
            |row| row.get(0),
        )?;
        if exists != 1 {
            return Err(SchemaError::BootstrapValidation(format!(
                "deferred index {index} is missing"
            )));
        }
    }
    for trigger in CANONICAL_FTS_TRIGGERS {
        let exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
            [*trigger],
            |row| row.get(0),
        )?;
        if exists != 1 {
            return Err(SchemaError::BootstrapValidation(format!(
                "canonical FTS trigger {trigger} is missing"
            )));
        }
    }
    observe("validate_structures", started.elapsed());

    if check_database_integrity {
        let started = Instant::now();
        let quick_check: String = conn.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
        if quick_check != "ok" {
            return Err(SchemaError::BootstrapValidation(format!(
                "quick_check returned {quick_check}"
            )));
        }
        observe("quick_check", started.elapsed());
    }
    if check_foreign_keys {
        let started = Instant::now();
        let foreign_key_violation = {
            let mut statement = conn.prepare("PRAGMA foreign_key_check")?;
            let mut rows = statement.query([])?;
            rows.next()?.is_some()
        };
        if foreign_key_violation {
            return Err(SchemaError::BootstrapValidation(
                "foreign_key_check found at least one violation".to_string(),
            ));
        }
        observe("foreign_key_check", started.elapsed());
    }

    let started = Instant::now();
    let transaction = conn.transaction()?;
    let integrity = transaction.execute_batch(
        "INSERT INTO canonical_message_search_fts(canonical_message_search_fts, rank) \
         VALUES('integrity-check', 1)",
    );
    transaction.rollback()?;
    integrity.map_err(SchemaError::from)?;
    observe("fts_integrity_check", started.elapsed());
    Ok(())
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
    conn.execute_batch("INSERT INTO subagent_search_fts(subagent_search_fts) VALUES('rebuild')")?;
    conn.execute_batch(
        "INSERT INTO canonical_message_search_fts(canonical_message_search_fts) VALUES('rebuild')",
    )?;
    conn.execute_batch(FTS_TRIGGERS_SQL)?;
    conn.execute_batch(TOKEN_ACTIVITY_TRIGGERS_SQL)?;
    conn.execute_batch(SESSION_SUMMARY_TRIGGERS_SQL)?;
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
/// - If the stored version is missing or `!= SCHEMA_VERSION`, drops
///   all legacy + current tables (and their triggers) and rebuilds from
///   [`SCHEMA_SQL`].
/// - If the version matches and no query bootstrap is active, reruns
///   [`SCHEMA_SQL`] (`IF NOT EXISTS` creates plus explicit `DROP INDEX IF
///   EXISTS` retirement statements are idempotent).
/// - If an incomplete query bootstrap is durable, preserves its intentionally
///   absent indexes and FTS triggers. Explicit finalization recreates and
///   validates those structures after catalog readers have been admitted.
/// - Writes the current [`SCHEMA_VERSION`] into `schema_meta` after a wipe.
///
/// This mirrors `initializeSchema` in `packages/sdk/src/data/schema.ts`.
pub fn initialize_schema(conn: &Connection) -> Result<(), SchemaError> {
    // Ensure schema_meta exists so we can read the version.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
    )?;

    let current = current_schema_version(conn)?;
    let bootstrap_state = query_bootstrap_state(conn)?;
    // A validation-failed bootstrap is wiped exactly like a stale version:
    // the data is derived, and no in-place repair exists for it.
    let poisoned = bootstrap_state.as_deref() == Some(BOOTSTRAP_STATE_VALIDATION_FAILED);
    let rebuilt = current != Some(SCHEMA_VERSION) || poisoned;
    let bootstrap_active = !rebuilt && bootstrap_state.is_some();

    if rebuilt {
        // Drop legacy tables from previous schema versions. Errors here are
        // deliberately ignored (match TS try/catch with empty catch) so a
        // partially-broken legacy state still migrates.
        for table in LEGACY_TABLES {
            let _ = conn.execute_batch(&format!("DROP TABLE IF EXISTS {table}"));
        }

        for view in CURRENT_VIEWS {
            let _ = conn.execute_batch(&format!("DROP VIEW IF EXISTS {view}"));
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

        // Create all tables and ingestion-time structures, then install the
        // query-only structures expected on a ready database.
        conn.execute_batch(SCHEMA_SQL)?;
        create_bootstrap_query_structures(conn)?;

        // Record the new version.
        conn.execute(
            "INSERT INTO schema_meta (key, value) VALUES ('version', ?1) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [SCHEMA_VERSION.to_string()],
        )?;
    } else {
        // Version matches — make sure all current objects exist and retire
        // explicitly superseded indexes. The DDL is idempotent on a healthy
        // database and deliberately excludes deferred query structures.
        conn.execute_batch(SCHEMA_SQL)?;
        if !bootstrap_active {
            create_bootstrap_query_structures(conn)?;
        }
        // Refresh derived-index trigger bodies: IF NOT EXISTS cannot replace
        // a correctness-fixed definition from the same schema version.
        refresh_token_activity_triggers(conn)?;
    }

    // Kept as a reusable block so bulk ingestion can drop/recreate this
    // invalidation family without duplicating it inside SCHEMA_SQL.
    conn.execute_batch(SESSION_SUMMARY_TRIGGERS_SQL)?;

    // Dropping a stale rebuildable cache leaves its old pages allocated. Repack
    // now, while this writer is still the only connection, so a cold ingest
    // does not begin with gigabytes of dead space.
    if rebuilt {
        conn.execute_batch("VACUUM")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests;
