//! Single-thread SQLite writer — ported from the write paths of
//! `packages/sdk/src/data/ingest-service.ts`.
//!
//! # Role in the pipeline
//!
//! The writer owns exactly one `rusqlite::Connection` for the duration of
//! an ingest. It consumes [`IngestEvent`]s from a `crossbeam_channel`
//! receiver, maintains one open transaction per project, and writes into
//! the schema-1.3 tables via a set of prepared statements created once at
//! startup.
//!
//! # Transaction boundaries
//!
//! - The writer begins a transaction on the first event of each project,
//!   which is almost always [`IngestEvent::Project`] but may be any other
//!   variant if the parser emitted them in a permissive order.
//! - It commits on [`IngestEvent::ProjectComplete`].
//! - A [`IngestEvent::WorkerError`] rolls back the current transaction
//!   and skips forward to the next project boundary.
//! - A fatal SQL error is returned up; the caller decides whether to
//!   continue.
//!
//! # FTS5 sync
//!
//! The content-synced triggers defined in `schema.rs` keep the
//! `search_fts` virtual table in lock-step with `messages` via INSERT/
//! UPDATE/DELETE hooks. The writer does **not** write to `search_fts`
//! directly — the triggers handle it.
//!
//! # Bulk ingest
//!
//! Matches the TS `beginBulkIngest` pattern: the three FTS auto-sync
//! triggers are dropped up front, messages are inserted against an
//! index-free FTS content table, and the index is rebuilt in one pass
//! via the `'rebuild'` command in [`finish`] before the triggers are
//! recreated. Combined with `synchronous = OFF` and an enlarged page
//! cache this is the main lever for cold-ingest throughput.
//!
//! Populated in RFC 003 commit 1.5; trigger-drop added in RFC 004 Item 2.

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::Receiver;
use rusqlite::{params, Connection};
use thiserror::Error;

use super::event::IngestEvent;
use super::schema::{self, SchemaError};

// ═══════════════════════════════════════════════════════════════════════════
// Errors
// ═══════════════════════════════════════════════════════════════════════════

/// Errors produced by the SQLite writer.
#[derive(Debug, Error)]
pub enum WriterError {
    /// An underlying SQLite error occurred.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// Schema initialization or PRAGMA setup failed.
    #[error("schema error: {0}")]
    Schema(#[from] SchemaError),

    /// JSON (re-)serialization failed. Only fires for variants that need
    /// to serialise structured payloads (subagent messages, file history,
    /// todos); the `messages.data` column uses `raw_json` as-is.
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

// ═══════════════════════════════════════════════════════════════════════════
// Bulk mode
// ═══════════════════════════════════════════════════════════════════════════

/// Durability profile for [`Writer::open_for_bulk_ingest_with_mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BulkMode {
    /// MEMORY journal + synchronous=OFF — max throughput (CLI default).
    #[default]
    Fast,
    /// Stay on WAL + synchronous=NORMAL — safer for long-lived desktop.
    Safe,
}

// ═══════════════════════════════════════════════════════════════════════════
// Stats
// ═══════════════════════════════════════════════════════════════════════════

/// Counters returned from [`Writer::run`]. Incremented on successful
/// writes only — rolled-back rows are not counted.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WriterStats {
    pub projects_processed: u32,
    pub sessions_processed: u32,
    pub messages_written: u32,
    pub subagents_written: u32,
}

/// Per-table row counters used by both [`Writer::handle_event`] (cold-start
/// loop, where the caller accumulates them into [`WriterStats`]) and
/// [`write_batch_with_tx`] (live-ingest path, where they are surfaced to
/// the caller as [`WriteBatchStats`]).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DispatchCounts {
    pub sessions_processed: u32,
    pub messages_written: u32,
    pub subagents_written: u32,
    pub tool_results_written: u32,
    pub file_histories_written: u32,
    pub todos_written: u32,
    pub tasks_written: u32,
    pub plans_written: u32,
}

impl DispatchCounts {
    fn add(&mut self, other: DispatchCounts) {
        self.sessions_processed = self
            .sessions_processed
            .saturating_add(other.sessions_processed);
        self.messages_written = self.messages_written.saturating_add(other.messages_written);
        self.subagents_written = self
            .subagents_written
            .saturating_add(other.subagents_written);
        self.tool_results_written = self
            .tool_results_written
            .saturating_add(other.tool_results_written);
        self.file_histories_written = self
            .file_histories_written
            .saturating_add(other.file_histories_written);
        self.todos_written = self.todos_written.saturating_add(other.todos_written);
        self.tasks_written = self.tasks_written.saturating_add(other.tasks_written);
        self.plans_written = self.plans_written.saturating_add(other.plans_written);
    }
}

/// Counters returned from [`write_batch_with_tx`]. Mirrors the per-table
/// `DispatchCounts` plus a wall-clock duration for the whole batch.
///
/// Introduced in RFC 005 Phase 4 C4.1 so the upcoming `live_ingest_batch`
/// NAPI entry (C4.2) can share the same transaction-wrapped write path
/// as cold-start ingest. `duration_ms` mirrors the TS live-path
/// `WriteResult.durationMs` and is measured around the whole call
/// (including BEGIN/COMMIT).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WriteBatchStats {
    pub messages_written: u32,
    pub subagents_written: u32,
    pub tool_results_written: u32,
    pub file_histories_written: u32,
    pub todos_written: u32,
    pub tasks_written: u32,
    pub plans_written: u32,
    pub duration_ms: u32,
}

// ═══════════════════════════════════════════════════════════════════════════
// SQL templates
// ═══════════════════════════════════════════════════════════════════════════
//
// Lifted verbatim from `packages/sdk/src/data/ingest-service.ts`. Kept as
// module-level `const`s so they're easy to diff against the TS source.

const SQL_INSERT_PROJECT: &str = r#"
INSERT INTO projects (slug, original_path, sessions_index, updated_at, source_id)
VALUES (?, ?, ?, ?, ?)
ON CONFLICT(source_id, slug) DO UPDATE SET
  original_path = excluded.original_path,
  sessions_index = excluded.sessions_index,
  updated_at = excluded.updated_at
"#;

const SQL_INSERT_MEMORY: &str = r#"
INSERT INTO project_memories (project_slug, content, updated_at)
VALUES (?, ?, ?)
ON CONFLICT(project_slug) DO UPDATE SET
  content = excluded.content,
  updated_at = excluded.updated_at
"#;

const SQL_INSERT_SESSION: &str = r#"
INSERT INTO sessions (
  id, project_slug, full_path, first_prompt, summary, git_branch,
  project_path, is_sidechain, created_at, modified_at, file_mtime,
  plan_slug, has_task, updated_at, source_id
)
VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(id) DO UPDATE SET
  project_slug = excluded.project_slug,
  full_path = excluded.full_path,
  first_prompt = excluded.first_prompt,
  summary = excluded.summary,
  git_branch = excluded.git_branch,
  project_path = excluded.project_path,
  is_sidechain = excluded.is_sidechain,
  created_at = excluded.created_at,
  modified_at = excluded.modified_at,
  file_mtime = excluded.file_mtime,
  plan_slug = excluded.plan_slug,
  has_task = excluded.has_task,
  updated_at = excluded.updated_at,
  source_id = excluded.source_id
"#;

const SQL_INSERT_MESSAGE: &str = r#"
INSERT INTO messages (
  project_slug, session_id, msg_index, msg_type, uuid, timestamp, data,
  input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
  text_content, byte_offset, source_id
)
VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(session_id, msg_index) DO UPDATE SET
  project_slug = excluded.project_slug,
  msg_type = excluded.msg_type,
  uuid = excluded.uuid,
  timestamp = excluded.timestamp,
  data = excluded.data,
  input_tokens = excluded.input_tokens,
  output_tokens = excluded.output_tokens,
  cache_creation_tokens = excluded.cache_creation_tokens,
  cache_read_tokens = excluded.cache_read_tokens,
  text_content = excluded.text_content,
  byte_offset = excluded.byte_offset,
  source_id = excluded.source_id
"#;

const SQL_INSERT_SUBAGENT: &str = r#"
INSERT INTO subagents (
  project_slug, session_id, agent_id, agent_type, file_name,
  messages, message_count, workflow_id, updated_at
)
VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(project_slug, session_id, workflow_id, agent_id) DO UPDATE SET
  agent_type = excluded.agent_type,
  file_name = excluded.file_name,
  messages = excluded.messages,
  message_count = excluded.message_count,
  updated_at = excluded.updated_at
"#;

const SQL_INSERT_WORKFLOW: &str = r#"
INSERT INTO workflows (
  project_slug, session_id, workflow_id, name, status,
  agent_count, total_tokens, total_tool_calls, duration_ms,
  subagent_count, data, journal, updated_at
)
VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(project_slug, session_id, workflow_id) DO UPDATE SET
  name = excluded.name,
  status = excluded.status,
  agent_count = excluded.agent_count,
  total_tokens = excluded.total_tokens,
  total_tool_calls = excluded.total_tool_calls,
  duration_ms = excluded.duration_ms,
  subagent_count = excluded.subagent_count,
  data = excluded.data,
  journal = excluded.journal,
  updated_at = excluded.updated_at
"#;

const SQL_INSERT_TOOL_RESULT: &str = r#"
INSERT INTO tool_results (project_slug, session_id, tool_use_id, content, updated_at)
VALUES (?, ?, ?, ?, ?)
ON CONFLICT(project_slug, session_id, tool_use_id) DO UPDATE SET
  content = excluded.content,
  updated_at = excluded.updated_at
"#;

const SQL_INSERT_FILE_HISTORY: &str = r#"
INSERT INTO file_history (session_id, data, updated_at)
VALUES (?, ?, ?)
ON CONFLICT(session_id) DO UPDATE SET
  data = excluded.data,
  updated_at = excluded.updated_at
"#;

const SQL_INSERT_TODO: &str = r#"
INSERT INTO todos (session_id, agent_id, items, updated_at)
VALUES (?, ?, ?, ?)
ON CONFLICT(session_id, agent_id) DO UPDATE SET
  items = excluded.items,
  updated_at = excluded.updated_at
"#;

const SQL_INSERT_TASK: &str = r#"
INSERT INTO tasks (session_id, has_highwatermark, highwatermark, lock_exists, updated_at)
VALUES (?, ?, ?, ?, ?)
ON CONFLICT(session_id) DO UPDATE SET
  has_highwatermark = excluded.has_highwatermark,
  highwatermark = excluded.highwatermark,
  lock_exists = excluded.lock_exists,
  updated_at = excluded.updated_at
"#;

const SQL_INSERT_PLAN: &str = r#"
INSERT INTO plans (slug, title, content, size, updated_at)
VALUES (?, ?, ?, ?, ?)
ON CONFLICT(slug) DO UPDATE SET
  title = excluded.title,
  content = excluded.content,
  size = excluded.size,
  updated_at = excluded.updated_at
"#;

const SQL_UPDATE_SESSION_HAS_TASK: &str = "UPDATE sessions SET has_task = 1 WHERE id = ?";

const SQL_INSERT_SOURCE_FILE: &str = r#"
INSERT INTO source_files (path, mtime_ms, size, byte_position, category, project_slug, session_id, source_id)
VALUES (?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(path) DO UPDATE SET
  mtime_ms = excluded.mtime_ms,
  size = excluded.size,
  byte_position = excluded.byte_position,
  category = excluded.category,
  project_slug = excluded.project_slug,
  session_id = excluded.session_id
"#;

/// Scoped clear — multi-source indexes must not wipe another agent's
/// fingerprints when one source re-ingests (Phase B).
const SQL_CLEAR_SOURCE_FILES: &str = "DELETE FROM source_files WHERE source_id = ?";

/// Synthetic slug for the transaction that batches the tail-of-stream
/// `Fingerprint` writes. `run` commits (rather than rolls back) the open
/// transaction on channel close when it carries this slug — every other open
/// transaction on close is a partial project that must be rolled back.
const FINGERPRINT_TX_SLUG: &str = "<fingerprints>";

/// Scoped entity wipe for Codex/Grok full re-read (orphans + extract bumps).
const SQL_CLEAR_SOURCE_MESSAGES: &str = "DELETE FROM messages WHERE source_id = ?";
const SQL_CLEAR_SOURCE_SESSIONS: &str = "DELETE FROM sessions WHERE source_id = ?";
const SQL_CLEAR_SOURCE_PROJECTS: &str = "DELETE FROM projects WHERE source_id = ?";

// ═══════════════════════════════════════════════════════════════════════════
// Writer
// ═══════════════════════════════════════════════════════════════════════════

/// Single-thread SQLite writer.
///
/// Owns a `rusqlite::Connection`, a set of prepared statement *texts*, and
/// running counters. Prepared statements themselves are created inline on
/// each call rather than cached on the struct because rusqlite's
/// `Statement<'_>` borrows the connection, which would make this struct
/// self-referential. rusqlite maintains an internal prepared-statement
/// cache (`conn.prepare_cached`) that gives us the same amortised cost.
pub struct Writer {
    conn: Connection,
    /// DB path — only used for diagnostic output.
    #[allow(dead_code)]
    db_path: PathBuf,
    /// Agent product id stamped on every core row (projects / sessions /
    /// messages / source_files). Defaults to [`super::DEFAULT_SOURCE_ID`].
    source_id: String,
    /// Whether [`open_for_bulk_ingest`] has been called. Used by
    /// [`finish`] to know whether to restore PRAGMAs.
    bulk_mode: bool,
    /// Tracks whether there's an in-flight transaction we need to commit
    /// or roll back.
    in_transaction: bool,
    /// Slug of the current project's transaction, if any.
    current_slug: Option<String>,
    stats: WriterStats,
}

impl Writer {
    /// Open (or create) the SQLite database at `db_path`, apply the
    /// connection-level PRAGMAs, and run migrations.
    ///
    /// Rows are stamped with [`super::DEFAULT_SOURCE_ID`] (`claude-code`).
    /// Prefer [`with_source_id`] when the producer is not Claude Code.
    pub fn new(db_path: &Path) -> Result<Self, WriterError> {
        Self::with_source_id(db_path, super::DEFAULT_SOURCE_ID)
    }

    /// Like [`new`], but bind every core row to `source_id`.
    pub fn with_source_id(
        db_path: &Path,
        source_id: impl Into<String>,
    ) -> Result<Self, WriterError> {
        let conn = Connection::open(db_path)?;
        schema::set_pragmas(&conn)?;
        schema::initialize_schema(&conn)?;
        Ok(Self {
            conn,
            db_path: db_path.to_path_buf(),
            source_id: source_id.into(),
            bulk_mode: false,
            in_transaction: false,
            current_slug: None,
            stats: WriterStats::default(),
        })
    }

    /// Test-only constructor that wraps an existing `Connection`. Runs
    /// `set_pragmas` + `initialize_schema` so the caller gets a ready-to-
    /// use writer against an in-memory DB.
    #[cfg(test)]
    pub(crate) fn from_connection(conn: Connection) -> Result<Self, WriterError> {
        Self::from_connection_with_source(conn, super::DEFAULT_SOURCE_ID)
    }

    #[cfg(test)]
    pub(crate) fn from_connection_with_source(
        conn: Connection,
        source_id: impl Into<String>,
    ) -> Result<Self, WriterError> {
        schema::set_pragmas(&conn)?;
        schema::initialize_schema(&conn)?;
        Ok(Self {
            conn,
            db_path: PathBuf::from(":memory:"),
            source_id: source_id.into(),
            bulk_mode: false,
            in_transaction: false,
            current_slug: None,
            stats: WriterStats::default(),
        })
    }

    /// The `source_id` this writer stamps on core rows.
    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    /// Enter bulk-ingest mode (fast defaults). See
    /// [`open_for_bulk_ingest_with_mode`].
    pub fn open_for_bulk_ingest(&mut self) -> Result<(), WriterError> {
        self.open_for_bulk_ingest_with_mode(BulkMode::Fast)
    }

    /// Enter bulk-ingest mode with an explicit durability profile.
    ///
    /// # [`BulkMode::Fast`] (CLI one-shots)
    /// - `synchronous = OFF` — trade a crash-window for throughput.
    /// - `journal_mode = MEMORY` — rollback journal in RAM. A crash
    ///   mid-ingest can leave a half-written DB; the cache is rebuildable
    ///   from agent roots (`ensureSqliteCacheHealthy` / `quick_check` + wipe).
    /// - `temp_store = MEMORY`, large `cache_size` / `mmap_size`.
    ///
    /// # [`BulkMode::Safe`] (desktop / `live: true`)
    /// - Stay on `journal_mode = WAL` + `synchronous = NORMAL` so a kill
    ///   mid-ingest is much less likely to leave SQLITE_CORRUPT.
    /// - Still drops FTS triggers for the bulk window (rebuilt in
    ///   [`finish`]); still enlarges the page cache.
    ///
    /// Both modes drop the three FTS auto-sync triggers so the hot-path
    /// INSERT into `messages` does not update `search_fts` per row.
    /// [`finish`] rebuilds FTS, recreates triggers, and restores durable
    /// PRAGMAs before closing.
    pub fn open_for_bulk_ingest_with_mode(&mut self, mode: BulkMode) -> Result<(), WriterError> {
        if self.bulk_mode {
            return Ok(());
        }
        self.bulk_mode = true;
        match mode {
            BulkMode::Fast => {
                self.conn.pragma_update(None, "synchronous", "OFF")?;
                self.conn.pragma_update(None, "journal_mode", "MEMORY")?;
                self.conn.pragma_update(None, "temp_store", "MEMORY")?;
                self.conn.pragma_update(None, "cache_size", -256_000i64)?;
                self.conn
                    .pragma_update(None, "mmap_size", 30_000_000_000i64)?;
            }
            BulkMode::Safe => {
                self.conn.pragma_update(None, "synchronous", "NORMAL")?;
                self.conn.pragma_update(None, "journal_mode", "WAL")?;
                self.conn.pragma_update(None, "temp_store", "MEMORY")?;
                self.conn.pragma_update(None, "cache_size", -256_000i64)?;
            }
        }
        schema::drop_fts_triggers(&self.conn)?;
        Ok(())
    }

    /// Drain events from `events` until the channel is empty and
    /// disconnected. Returns the final counters.
    ///
    /// Per-project transaction handling:
    /// - The first data-bearing event after a boundary (or at startup)
    ///   starts a transaction.
    /// - [`IngestEvent::ProjectComplete`] commits it.
    /// - [`IngestEvent::WorkerError`] rolls it back.
    /// - Channel-close with an open transaction rolls it back as well
    ///   (matching the TS `close()` behaviour which rolls back to avoid
    ///   persisting partial data).
    pub fn run(&mut self, events: Receiver<IngestEvent>) -> Result<WriterStats, WriterError> {
        while let Ok(ev) = events.recv() {
            self.handle_event(ev)?;
        }

        // Channel closed. The fingerprint tail batch has no explicit
        // ProjectComplete, so commit it here; any *other* open transaction is
        // a partial project (the parser died before ProjectComplete) whose
        // rows must not persist, so roll it back.
        if self.in_transaction {
            if self.current_slug.as_deref() == Some(FINGERPRINT_TX_SLUG) {
                self.commit_transaction()?;
            } else {
                self.rollback_transaction();
            }
        }

        Ok(self.stats)
    }

    /// Restore normal PRAGMAs and close. Takes `self` by value so the
    /// connection is dropped on return.
    ///
    /// When the writer was in bulk mode, rebuilds `search_fts` from
    /// `messages` and recreates the auto-sync triggers so the FTS index
    /// is back in lock-step with the content table before the connection
    /// is dropped.
    pub fn finish(mut self) -> Result<(), WriterError> {
        if self.in_transaction {
            self.rollback_transaction();
        }
        if self.bulk_mode {
            // FTS rebuild is large but single-pass; done inside an
            // implicit transaction so the index flips atomically.
            schema::rebuild_fts_and_recreate_triggers(&self.conn)?;
            // Restore safe defaults; mirrors TS `endBulkIngest`.
            self.conn.pragma_update(None, "synchronous", "NORMAL")?;
            self.conn.pragma_update(None, "journal_mode", "WAL")?;
            self.conn.pragma_update(None, "cache_size", -2_000i64)?;
            self.bulk_mode = false;
        }
        // `self` drops here, closing the connection.
        Ok(())
    }

    // ───────────────────────────────────────────────────────────────────────
    // Event dispatch
    // ───────────────────────────────────────────────────────────────────────

    fn handle_event(&mut self, ev: IngestEvent) -> Result<(), WriterError> {
        // Data-bearing variants: open the appropriate transaction, then
        // delegate the SQL work to the shared `dispatch_event` helper
        // (which is also used by `write_batch_with_tx` on the live-ingest
        // path). Control-flow variants (SessionComplete / ProjectComplete
        // / WorkerError) are handled inline because they own the per-
        // project transaction state machine.
        match ev {
            IngestEvent::Project { ref slug, .. }
            | IngestEvent::ProjectMemory { ref slug, .. }
            | IngestEvent::Session { ref slug, .. }
            | IngestEvent::Message { ref slug, .. }
            | IngestEvent::Subagent { ref slug, .. }
            | IngestEvent::Workflow { ref slug, .. }
            | IngestEvent::ToolResult { ref slug, .. }
            | IngestEvent::Plan { ref slug, .. } => {
                let slug = slug.clone();
                self.ensure_transaction(&slug)?;
                let counts = dispatch_event(&self.conn, &ev, &self.source_id)?;
                self.stats.sessions_processed = self
                    .stats
                    .sessions_processed
                    .saturating_add(counts.sessions_processed);
                self.stats.messages_written = self
                    .stats
                    .messages_written
                    .saturating_add(counts.messages_written);
                self.stats.subagents_written = self
                    .stats
                    .subagents_written
                    .saturating_add(counts.subagents_written);
                // tool_results / plans aren't tracked on `WriterStats`;
                // their counts are visible via the live-path
                // `WriteBatchStats` only.
            }

            IngestEvent::FileHistory { .. }
            | IngestEvent::Todo { .. }
            | IngestEvent::Task { .. } => {
                // No slug on these events. Use the current transaction if
                // one is open; otherwise open one under a synthetic slug
                // so writes aren't auto-committed per-row.
                if !self.in_transaction {
                    self.begin_transaction("<orphan>")?;
                }
                let _counts = dispatch_event(&self.conn, &ev, &self.source_id)?;
                // file_history / todo / task don't contribute to
                // `WriterStats`; their counts only surface via
                // `WriteBatchStats` on the live path.
            }

            IngestEvent::SessionComplete { .. } => {
                // No-op at the writer level (matches TS). Reserved for
                // future byte_position updates on source_files.
            }

            IngestEvent::SessionTokensEstimated {
                ref session_id,
                estimated,
            } => {
                if !self.in_transaction {
                    self.begin_transaction("<orphan>")?;
                }
                self.conn.execute(
                    "UPDATE sessions SET tokens_estimated = ? WHERE id = ?",
                    params![if estimated { 1 } else { 0 }, session_id],
                )?;
            }

            IngestEvent::ProjectComplete { slug, .. } => {
                // Commit the current transaction if it belongs to this
                // project. If it's an orphan transaction, commit anyway —
                // the orchestrator is telling us we've reached a natural
                // boundary.
                if self.in_transaction {
                    self.commit_transaction()?;
                    self.stats.projects_processed = self.stats.projects_processed.saturating_add(1);
                } else {
                    // No pending writes for this project; still bump
                    // projects_processed because the project was seen.
                    self.stats.projects_processed = self.stats.projects_processed.saturating_add(1);
                }
                // Always clear the slug regardless of the branch above.
                let _ = slug;
                self.current_slug = None;
            }

            IngestEvent::WorkerError { slug: _, error: _ } => {
                // Roll back any in-flight work for this project and keep
                // going. The error is surfaced by the orchestrator via
                // `IngestStats.errors`; the writer's only job is to not
                // persist partial project data.
                if self.in_transaction {
                    self.rollback_transaction();
                }
                self.current_slug = None;
            }

            IngestEvent::ClearSourceFiles => {
                if self.in_transaction {
                    self.commit_transaction()?;
                    self.stats.projects_processed = self.stats.projects_processed.saturating_add(1);
                    self.current_slug = None;
                }
                // The DELETE runs as its own autocommit statement: the writer
                // has just committed any open project tx above, and the
                // fingerprint upserts that follow open their own dedicated
                // batch transaction (see the `Fingerprint` arm, committed by
                // `run` at channel close). Scoped to this writer's source so
                // multi-source indexes keep other agents' fingerprints
                // (Phase B). Fingerprints only — Claude emits this *after*
                // entity writes.
                self.conn
                    .execute(SQL_CLEAR_SOURCE_FILES, params![self.source_id])?;
            }

            IngestEvent::ClearSourceData => {
                if self.in_transaction {
                    self.commit_transaction()?;
                    self.stats.projects_processed = self.stats.projects_processed.saturating_add(1);
                    self.current_slug = None;
                }
                // Full source-scoped wipe before Codex/Grok full re-read so
                // deleted-on-disk sessions do not leave permanent orphans.
                // Order: messages (FTS triggers) → sessions → projects → files.
                self.conn
                    .execute(SQL_CLEAR_SOURCE_MESSAGES, params![self.source_id])?;
                self.conn
                    .execute(SQL_CLEAR_SOURCE_SESSIONS, params![self.source_id])?;
                self.conn
                    .execute(SQL_CLEAR_SOURCE_PROJECTS, params![self.source_id])?;
                self.conn
                    .execute(SQL_CLEAR_SOURCE_FILES, params![self.source_id])?;
            }

            IngestEvent::Fingerprint { .. } => {
                // Fingerprints are orchestrator-emitted at the tail of the
                // stream, after all per-project events and the
                // ClearSourceFiles marker. Batch every fingerprint upsert into
                // one dedicated transaction (committed by `run` at channel
                // close) instead of one autocommit per row.
                if self.in_transaction && self.current_slug.as_deref() != Some(FINGERPRINT_TX_SLUG)
                {
                    // Defensive: a still-open project tx (ClearSourceFiles
                    // normally already closed it). Commit before opening the
                    // fingerprint batch.
                    self.commit_transaction()?;
                    self.stats.projects_processed = self.stats.projects_processed.saturating_add(1);
                    self.current_slug = None;
                }
                if !self.in_transaction {
                    self.begin_transaction(FINGERPRINT_TX_SLUG)?;
                }
                let _counts = dispatch_event(&self.conn, &ev, &self.source_id)?;
            }
        }

        Ok(())
    }

    // ───────────────────────────────────────────────────────────────────────
    // Transaction helpers
    // ───────────────────────────────────────────────────────────────────────

    /// Begin a transaction for `slug` if we don't already have one. If a
    /// transaction is open for a *different* slug the old one is committed
    /// first — this tolerates parsers that forget to emit
    /// `ProjectComplete` before starting a new project.
    fn ensure_transaction(&mut self, slug: &str) -> Result<(), WriterError> {
        if let Some(current) = &self.current_slug {
            if current != slug {
                // Different project — commit the old one before starting
                // the new one.
                if self.in_transaction {
                    self.commit_transaction()?;
                    self.stats.projects_processed = self.stats.projects_processed.saturating_add(1);
                }
            }
        }
        if !self.in_transaction {
            self.begin_transaction(slug)?;
        }
        Ok(())
    }

    fn begin_transaction(&mut self, slug: &str) -> Result<(), WriterError> {
        self.conn.execute_batch("BEGIN TRANSACTION")?;
        self.in_transaction = true;
        self.current_slug = Some(slug.to_string());
        Ok(())
    }

    fn commit_transaction(&mut self) -> Result<(), WriterError> {
        self.conn.execute_batch("COMMIT")?;
        self.in_transaction = false;
        self.current_slug = None;
        Ok(())
    }

    /// Roll back the current transaction, swallowing any error that
    /// occurs during rollback itself (matches the TS empty-catch). Used
    /// on `WorkerError` and on channel-close with an open transaction.
    fn rollback_transaction(&mut self) {
        let _ = self.conn.execute_batch("ROLLBACK");
        self.in_transaction = false;
        self.current_slug = None;
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Shared batch-write API
// ═══════════════════════════════════════════════════════════════════════════
//
// These two functions are the piece of the writer that's shared between
// the cold-start loop (`Writer::handle_event`) and the live-ingest
// entrypoint (RFC 005 Phase 4: `live_ingest_batch`). The cold-start loop
// owns its own per-project transaction state machine and delegates the
// per-event SQL to `dispatch_event`; the live path wraps one BEGIN
// IMMEDIATE / COMMIT around a whole batch via `write_batch_with_tx`.
//
// Both paths run the exact same INSERT/UPDATE statements with the same
// parameter binding, so SQLite output is bit-identical between the two
// entries (modulo transaction grouping).

/// Dispatch one [`IngestEvent`] to its corresponding INSERT/UPDATE
/// statement. Returns per-table row counters for the caller to accumulate.
///
/// This function does **not** manage transactions — callers must open
/// one first (either via [`Writer::ensure_transaction`] / the `<orphan>`
/// fallback in `handle_event`, or via [`write_batch_with_tx`] on the
/// live path).
///
/// Orchestration variants (`ProjectComplete`, `SessionComplete`,
/// `WorkerError`, `ClearSourceFiles`) are rejected here — they're
/// control-flow events, not row writes, and belong in the caller's
/// state machine.
pub fn dispatch_event(
    conn: &Connection,
    ev: &IngestEvent,
    source_id: &str,
) -> Result<DispatchCounts, WriterError> {
    let mut counts = DispatchCounts::default();

    match ev {
        IngestEvent::Project {
            slug,
            original_path,
            sessions_index_json,
        } => {
            let now = now_ms();
            conn.execute(
                SQL_INSERT_PROJECT,
                params![slug, original_path, sessions_index_json, now, source_id],
            )?;
        }

        IngestEvent::ProjectMemory { slug, content } => {
            let now = now_ms();
            conn.execute(SQL_INSERT_MEMORY, params![slug, content, now])?;
        }

        IngestEvent::Session { slug, entry } => {
            let now = now_ms();
            conn.execute(
                SQL_INSERT_SESSION,
                params![
                    entry.session_id,
                    slug,
                    entry.full_path,
                    entry.first_prompt,
                    entry.summary,
                    entry.git_branch,
                    entry.project_path,
                    entry.is_sidechain as i64,
                    entry.created,
                    entry.modified,
                    entry.file_mtime,
                    Option::<String>::None, // plan_slug set later if found
                    0_i64,                  // has_task set later if found
                    now,
                    source_id,
                ],
            )?;
            counts.sessions_processed = 1;
        }

        IngestEvent::Message {
            slug,
            session_id,
            index,
            byte_offset,
            raw_json,
            msg_type,
            uuid,
            timestamp,
            input_tokens,
            output_tokens,
            cache_creation_tokens,
            cache_read_tokens,
            fts_text,
        } => {
            let text = fts_text.clone().unwrap_or_default();
            conn.execute(
                SQL_INSERT_MESSAGE,
                params![
                    slug,
                    session_id,
                    *index as i64,
                    msg_type,
                    uuid,
                    timestamp,
                    raw_json,
                    *input_tokens as i64,
                    *output_tokens as i64,
                    *cache_creation_tokens as i64,
                    *cache_read_tokens as i64,
                    text,
                    *byte_offset as i64,
                    source_id,
                ],
            )?;
            counts.messages_written = 1;
        }

        IngestEvent::Subagent {
            slug,
            session_id,
            transcript,
        } => {
            let now = now_ms();
            let messages_json = serde_json::to_string(&transcript.messages)?;
            // Prefer the sidecar's real agent type (general-purpose, Explore, …)
            // over the filename-inferred enum kind. `to_string` on the enum
            // produces `"task"` (with quotes) — strip them to store the bare
            // string, matching the TS convention.
            let agent_type = match transcript.meta.as_ref() {
                Some(meta) => meta.agent_type.clone(),
                None => serde_json::to_string(&transcript.agent_type)?
                    .trim_matches('"')
                    .to_string(),
            };
            let message_count = transcript.messages.len() as i64;
            conn.execute(
                SQL_INSERT_SUBAGENT,
                params![
                    slug,
                    session_id,
                    transcript.agent_id,
                    agent_type,
                    transcript.file_name,
                    messages_json,
                    message_count,
                    transcript.workflow_id,
                    now,
                ],
            )?;
            counts.subagents_written = 1;
        }

        IngestEvent::Workflow {
            slug,
            session_id,
            workflow,
        } => {
            let now = now_ms();
            let data_json = serde_json::to_string(&workflow.data)?;
            let journal_json = serde_json::to_string(&workflow.journal)?;
            conn.execute(
                SQL_INSERT_WORKFLOW,
                params![
                    slug,
                    session_id,
                    workflow.workflow_id,
                    workflow.name,
                    workflow.status,
                    workflow.agent_count,
                    workflow.total_tokens,
                    workflow.total_tool_calls,
                    workflow.duration_ms,
                    workflow.subagent_count,
                    data_json,
                    journal_json,
                    now,
                ],
            )?;
        }

        IngestEvent::ToolResult {
            slug,
            session_id,
            tool_result,
        } => {
            let now = now_ms();
            conn.execute(
                SQL_INSERT_TOOL_RESULT,
                params![
                    slug,
                    session_id,
                    tool_result.tool_use_id,
                    tool_result.content,
                    now
                ],
            )?;
            counts.tool_results_written = 1;
        }

        IngestEvent::FileHistory {
            session_id,
            history,
        } => {
            let now = now_ms();
            let data = serde_json::to_string(&history)?;
            conn.execute(SQL_INSERT_FILE_HISTORY, params![session_id, data, now])?;
            counts.file_histories_written = 1;
        }

        IngestEvent::Todo { session_id, todo } => {
            let now = now_ms();
            let items = serde_json::to_string(&todo.items)?;
            conn.execute(
                SQL_INSERT_TODO,
                params![session_id, todo.agent_id, items, now],
            )?;
            counts.todos_written = 1;
        }

        IngestEvent::Task { session_id, task } => {
            let now = now_ms();
            conn.execute(
                SQL_INSERT_TASK,
                params![
                    session_id,
                    task.has_highwatermark as i64,
                    task.highwatermark,
                    task.lock_exists as i64,
                    now
                ],
            )?;
            // Mirror TS: also flip the session's has_task flag.
            conn.execute(SQL_UPDATE_SESSION_HAS_TASK, params![session_id])?;
            counts.tasks_written = 1;
        }

        IngestEvent::Plan { slug, plan } => {
            let _ = slug;
            let now = now_ms();
            conn.execute(
                SQL_INSERT_PLAN,
                params![plan.slug, plan.title, plan.content, plan.size as i64, now],
            )?;
            counts.plans_written = 1;
        }

        IngestEvent::Fingerprint {
            path,
            mtime_ms,
            size,
            byte_position,
            category,
            project_slug,
            session_id,
        } => {
            conn.execute(
                SQL_INSERT_SOURCE_FILE,
                params![
                    path,
                    mtime_ms,
                    *size as i64,
                    byte_position.map(|b| b as i64),
                    category,
                    project_slug,
                    session_id,
                    source_id,
                ],
            )?;
        }

        IngestEvent::SessionTokensEstimated {
            session_id,
            estimated,
        } => {
            conn.execute(
                "UPDATE sessions SET tokens_estimated = ? WHERE id = ?",
                params![if *estimated { 1 } else { 0 }, session_id],
            )?;
        }

        // Orchestration-only variants: callers (the cold-start loop)
        // handle these directly in their transaction state machine and
        // must not route them here. If this ever fires it's a logic bug
        // in the caller — surface it loudly rather than silently no-op.
        IngestEvent::SessionComplete { .. }
        | IngestEvent::ProjectComplete { .. }
        | IngestEvent::WorkerError { .. }
        | IngestEvent::ClearSourceFiles
        | IngestEvent::ClearSourceData => {
            // Intentionally no-op for compatibility with callers that
            // mix orchestration and data events in a single stream
            // (`write_batch_with_tx` accepts any event list; the live
            // path only ever feeds it data-bearing variants). Counts
            // stay zero.
        }
    }

    Ok(counts)
}

/// Write a batch of [`IngestEvent`]s inside a single transaction.
///
/// This is the shared entry point used by RFC 005 Phase 4's live-ingest
/// NAPI call (`live_ingest_batch`, landed in C4.2). It opens a
/// `BEGIN IMMEDIATE`, dispatches every event via [`dispatch_event`],
/// and commits on success — or rolls back and returns the error on any
/// single-event failure. Callers on the live path treat a rolled-back
/// batch as a fallible unit and are free to retry / downgrade.
///
/// `BEGIN IMMEDIATE` (rather than plain `BEGIN`) matches the TS
/// live-path in `IngestService.writeBatch` and avoids the SQLite
/// "upgrade from read lock to write lock" deadlock trap under concurrent
/// readers — which live ingest will absolutely have.
///
/// Empty batches are **not** special-cased here: opening a tx on an
/// empty list is ~microseconds and the NAPI layer (C4.2) short-circuits
/// upstream anyway. Keeping the function total makes it simpler to test
/// and removes one corner case for the caller to reason about.
pub fn write_batch_with_tx(
    conn: &Connection,
    events: &[IngestEvent],
    source_id: &str,
) -> Result<WriteBatchStats, WriterError> {
    let started = Instant::now();

    conn.execute_batch("BEGIN IMMEDIATE")?;

    let mut totals = DispatchCounts::default();
    let dispatch_result: Result<(), WriterError> = (|| {
        for ev in events {
            let c = dispatch_event(conn, ev, source_id)?;
            totals.add(c);
        }
        Ok(())
    })();

    match dispatch_result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
        }
        Err(e) => {
            // Best-effort rollback. If this itself fails we still want to
            // surface the original error, not the rollback failure.
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }

    let duration_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);

    Ok(WriteBatchStats {
        messages_written: totals.messages_written,
        subagents_written: totals.subagents_written,
        tool_results_written: totals.tool_results_written,
        file_histories_written: totals.file_histories_written,
        todos_written: totals.todos_written,
        tasks_written: totals.tasks_written,
        plans_written: totals.plans_written,
        duration_ms,
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Current Unix time in milliseconds — TS uses `Date.now()` for all the
/// `updated_at` columns. Returns 0 on the astronomically unlikely event
/// that the system clock is before the epoch.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::types::{SessionIndexEntry, SubagentTranscript, SubagentType};
    use crossbeam_channel::unbounded;
    use rusqlite::Connection;

    fn fresh_writer() -> Writer {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        Writer::from_connection(conn).expect("new writer")
    }

    /// Phase B: custom source_id is bound on projects/sessions/messages.
    #[test]
    fn write_batch_stamps_source_id_column() {
        let conn = Connection::open_in_memory().unwrap();
        schema::set_pragmas(&conn).unwrap();
        schema::initialize_schema(&conn).unwrap();
        let events = vec![
            IngestEvent::Project {
                slug: "p".into(),
                original_path: "/x".into(),
                sessions_index_json: "{}".into(),
            },
            IngestEvent::Session {
                slug: "p".into(),
                entry: sample_session("s1"),
            },
            message_event("p", "s1", 0),
        ];
        write_batch_with_tx(&conn, &events, "codex").expect("batch");
        let sid: String = conn
            .query_row("SELECT source_id FROM projects WHERE slug = 'p'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(sid, "codex");
        let mid: String = conn
            .query_row("SELECT source_id FROM messages LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mid, "codex");
        let sess: String = conn
            .query_row("SELECT source_id FROM sessions WHERE id = 's1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(sess, "codex");
    }

    fn sample_session(id: &str) -> SessionIndexEntry {
        SessionIndexEntry {
            session_id: id.into(),
            full_path: format!("/tmp/{id}.jsonl"),
            file_mtime: 100.0,
            first_prompt: "first".into(),
            summary: "sum".into(),
            message_count: 3,
            created: "2026-04-17T00:00:00Z".into(),
            modified: "2026-04-17T00:00:01Z".into(),
            git_branch: "main".into(),
            project_path: "/tmp/proj".into(),
            is_sidechain: false,
        }
    }

    fn message_event(slug: &str, session_id: &str, index: u32) -> IngestEvent {
        IngestEvent::Message {
            slug: slug.into(),
            session_id: session_id.into(),
            index,
            byte_offset: u64::from(index) * 100,
            raw_json: format!(r#"{{"type":"user","idx":{index}}}"#),
            msg_type: "user".into(),
            uuid: Some(format!("u-{index}")),
            timestamp: Some("2026-04-17T00:00:00Z".into()),
            input_tokens: 1,
            output_tokens: 2,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            fts_text: Some(format!("text {index}")),
        }
    }

    fn count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .expect("count")
    }

    /// One project, one session, three messages → all written, FTS synced.
    #[test]
    fn single_project_writes_rows_and_syncs_fts() {
        let mut w = fresh_writer();
        let (tx, rx) = unbounded::<IngestEvent>();

        tx.send(IngestEvent::Project {
            slug: "p1".into(),
            original_path: "/tmp/p1".into(),
            sessions_index_json: "{}".into(),
        })
        .unwrap();
        tx.send(IngestEvent::Session {
            slug: "p1".into(),
            entry: sample_session("s1"),
        })
        .unwrap();
        tx.send(message_event("p1", "s1", 0)).unwrap();
        tx.send(message_event("p1", "s1", 1)).unwrap();
        tx.send(message_event("p1", "s1", 2)).unwrap();
        tx.send(IngestEvent::ProjectComplete {
            slug: "p1".into(),
            duration_ms: 0,
        })
        .unwrap();
        drop(tx);

        let stats = w.run(rx).expect("run");
        assert_eq!(stats.projects_processed, 1);
        assert_eq!(stats.sessions_processed, 1);
        assert_eq!(stats.messages_written, 3);

        assert_eq!(count(&w.conn, "projects"), 1);
        assert_eq!(count(&w.conn, "sessions"), 1);
        assert_eq!(count(&w.conn, "messages"), 3);
        // FTS triggers fire on INSERT — should see 3 rows via content-sync.
        assert_eq!(count(&w.conn, "search_fts"), 3);
    }

    /// Partial writes for project 2 must not be visible until
    /// `ProjectComplete` — verified by draining the first project only
    /// and checking row counts mid-stream via a fresh reader.
    #[test]
    fn transaction_boundary_is_per_project() {
        let mut w = fresh_writer();

        // Project 1: insert + commit.
        {
            let (tx, rx) = unbounded::<IngestEvent>();
            tx.send(IngestEvent::Project {
                slug: "p1".into(),
                original_path: "/tmp/p1".into(),
                sessions_index_json: "{}".into(),
            })
            .unwrap();
            tx.send(message_event("p1", "s1", 0)).unwrap();
            tx.send(IngestEvent::ProjectComplete {
                slug: "p1".into(),
                duration_ms: 0,
            })
            .unwrap();
            drop(tx);
            w.run(rx).expect("run p1");
        }
        assert_eq!(count(&w.conn, "projects"), 1);
        assert_eq!(count(&w.conn, "messages"), 1);

        // Project 2: send rows but NO ProjectComplete, then close the
        // channel. The writer rolls back on channel-close — project 2
        // must not appear.
        {
            let (tx, rx) = unbounded::<IngestEvent>();
            tx.send(IngestEvent::Project {
                slug: "p2".into(),
                original_path: "/tmp/p2".into(),
                sessions_index_json: "{}".into(),
            })
            .unwrap();
            tx.send(message_event("p2", "s2", 0)).unwrap();
            tx.send(message_event("p2", "s2", 1)).unwrap();
            drop(tx);
            w.run(rx).expect("run p2 partial");
        }
        assert_eq!(
            count(&w.conn, "projects"),
            1,
            "project 2 must be rolled back"
        );
        assert_eq!(
            count(&w.conn, "messages"),
            1,
            "project 2 messages must be rolled back"
        );
    }

    /// Message UPSERT — second write with same `(session_id, msg_index)`
    /// replaces the first.
    #[test]
    fn message_upsert_replaces_existing_row() {
        let mut w = fresh_writer();
        let (tx, rx) = unbounded::<IngestEvent>();

        tx.send(IngestEvent::Project {
            slug: "p1".into(),
            original_path: "/tmp/p1".into(),
            sessions_index_json: "{}".into(),
        })
        .unwrap();
        // First version
        tx.send(IngestEvent::Message {
            slug: "p1".into(),
            session_id: "s1".into(),
            index: 0,
            byte_offset: 0,
            raw_json: "{\"v\":1}".into(),
            msg_type: "user".into(),
            uuid: Some("u1".into()),
            timestamp: Some("t1".into()),
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            fts_text: Some("first".into()),
        })
        .unwrap();
        // Second version, same session_id + msg_index — should win.
        tx.send(IngestEvent::Message {
            slug: "p1".into(),
            session_id: "s1".into(),
            index: 0,
            byte_offset: 10,
            raw_json: "{\"v\":2}".into(),
            msg_type: "assistant".into(),
            uuid: Some("u2".into()),
            timestamp: Some("t2".into()),
            input_tokens: 5,
            output_tokens: 6,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            fts_text: Some("second".into()),
        })
        .unwrap();
        tx.send(IngestEvent::ProjectComplete {
            slug: "p1".into(),
            duration_ms: 0,
        })
        .unwrap();
        drop(tx);

        let stats = w.run(rx).expect("run");
        assert_eq!(
            count(&w.conn, "messages"),
            1,
            "UNIQUE(session_id, msg_index) enforced"
        );
        // Counter increments per successful INSERT, including the upsert.
        assert_eq!(stats.messages_written, 2);

        let (data, msg_type, text): (String, String, String) = w
            .conn
            .query_row(
                "SELECT data, msg_type, text_content FROM messages WHERE session_id='s1' AND msg_index=0",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("query");
        assert_eq!(data, "{\"v\":2}");
        assert_eq!(msg_type, "assistant");
        assert_eq!(text, "second");
    }

    /// WorkerError mid-project rolls back, next project still writes.
    #[test]
    fn worker_error_rolls_back_current_project() {
        let mut w = fresh_writer();
        let (tx, rx) = unbounded::<IngestEvent>();

        // Project 1 — gets a WorkerError and should be rolled back.
        tx.send(IngestEvent::Project {
            slug: "p1".into(),
            original_path: "/tmp/p1".into(),
            sessions_index_json: "{}".into(),
        })
        .unwrap();
        tx.send(message_event("p1", "s1", 0)).unwrap();
        tx.send(IngestEvent::WorkerError {
            slug: "p1".into(),
            error: "boom".into(),
        })
        .unwrap();

        // Project 2 — clean, should persist.
        tx.send(IngestEvent::Project {
            slug: "p2".into(),
            original_path: "/tmp/p2".into(),
            sessions_index_json: "{}".into(),
        })
        .unwrap();
        tx.send(message_event("p2", "s2", 0)).unwrap();
        tx.send(IngestEvent::ProjectComplete {
            slug: "p2".into(),
            duration_ms: 0,
        })
        .unwrap();
        drop(tx);

        w.run(rx).expect("run");
        let slugs: Vec<String> = w
            .conn
            .prepare("SELECT slug FROM projects ORDER BY slug")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(slugs, vec!["p2".to_string()]);
        assert_eq!(count(&w.conn, "messages"), 1);
    }

    /// Subagent + ToolResult + Plan all write correctly; stats counters
    /// increment as expected.
    #[test]
    fn stats_counters_and_multiple_entity_types() {
        let mut w = fresh_writer();
        let (tx, rx) = unbounded::<IngestEvent>();

        tx.send(IngestEvent::Project {
            slug: "p1".into(),
            original_path: "/tmp/p1".into(),
            sessions_index_json: "{}".into(),
        })
        .unwrap();
        tx.send(IngestEvent::Session {
            slug: "p1".into(),
            entry: sample_session("s1"),
        })
        .unwrap();
        tx.send(IngestEvent::Session {
            slug: "p1".into(),
            entry: sample_session("s2"),
        })
        .unwrap();
        tx.send(message_event("p1", "s1", 0)).unwrap();
        tx.send(message_event("p1", "s1", 1)).unwrap();
        tx.send(IngestEvent::Subagent {
            slug: "p1".into(),
            session_id: "s1".into(),
            transcript: SubagentTranscript {
                agent_id: "a1".into(),
                agent_type: SubagentType::Task,
                file_name: "agent-a1.jsonl".into(),
                messages: vec![],
                meta: None,
                workflow_id: String::new(),
            },
        })
        .unwrap();
        tx.send(IngestEvent::ProjectComplete {
            slug: "p1".into(),
            duration_ms: 0,
        })
        .unwrap();
        drop(tx);

        let stats = w.run(rx).expect("run");
        assert_eq!(stats.projects_processed, 1);
        assert_eq!(stats.sessions_processed, 2);
        assert_eq!(stats.messages_written, 2);
        assert_eq!(stats.subagents_written, 1);

        assert_eq!(count(&w.conn, "subagents"), 1);
        // Verify stored agent_type is the bare string (not JSON-quoted).
        let agent_type: String = w
            .conn
            .query_row(
                "SELECT agent_type FROM subagents WHERE agent_id='a1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(agent_type, "task");
    }

    /// Fingerprints arrive as a tail batch (after ClearSourceFiles, with no
    /// ProjectComplete). They must be wrapped in one transaction that commits
    /// on channel close — not one autocommit per row, and not rolled back.
    #[test]
    fn fingerprints_are_batched_and_committed_on_close() {
        fn fingerprint_event(path: &str) -> IngestEvent {
            IngestEvent::Fingerprint {
                path: path.into(),
                mtime_ms: 123.0,
                size: 10,
                byte_position: Some(10),
                category: "session".into(),
                project_slug: Some("p".into()),
                session_id: Some("s".into()),
            }
        }

        let mut w = fresh_writer();
        let (tx, rx) = unbounded::<IngestEvent>();
        // A committed project first, then the fingerprint tail.
        tx.send(IngestEvent::Project {
            slug: "p1".into(),
            original_path: "/tmp/p1".into(),
            sessions_index_json: "{}".into(),
        })
        .unwrap();
        tx.send(IngestEvent::ProjectComplete {
            slug: "p1".into(),
            duration_ms: 0,
        })
        .unwrap();
        tx.send(IngestEvent::ClearSourceFiles).unwrap();
        tx.send(fingerprint_event("/abs/a.jsonl")).unwrap();
        tx.send(fingerprint_event("/abs/b.jsonl")).unwrap();
        drop(tx);

        w.run(rx).expect("run");
        assert_eq!(
            count(&w.conn, "source_files"),
            2,
            "fingerprint batch must commit on channel close"
        );
    }

    /// Full bulk-ingest roundtrip on a file-backed DB: `open_for_bulk_ingest`
    /// drops the FTS triggers, messages are inserted without per-row FTS
    /// sync, and `finish` rebuilds the index + recreates the triggers.
    /// After finish, `search_fts` row count must match `messages`, and the
    /// triggers must be back so a follow-up warm INSERT syncs incrementally.
    #[test]
    fn bulk_ingest_rebuilds_fts_at_finish() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("bulk-fts.sqlite");

        // Bulk ingest scope: write three messages with triggers dropped.
        {
            let mut w = Writer::new(&db_path).expect("open db");
            w.open_for_bulk_ingest().expect("bulk on");

            // Triggers should be gone mid-bulk.
            let trigger_count: i64 = w
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name LIKE 'messages_%'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                trigger_count, 0,
                "FTS triggers must be dropped in bulk mode"
            );

            let (tx, rx) = unbounded::<IngestEvent>();
            tx.send(IngestEvent::Project {
                slug: "p1".into(),
                original_path: "/tmp/p1".into(),
                sessions_index_json: "{}".into(),
            })
            .unwrap();
            tx.send(message_event("p1", "s1", 0)).unwrap();
            tx.send(message_event("p1", "s1", 1)).unwrap();
            tx.send(message_event("p1", "s1", 2)).unwrap();
            tx.send(IngestEvent::ProjectComplete {
                slug: "p1".into(),
                duration_ms: 0,
            })
            .unwrap();
            drop(tx);

            w.run(rx).expect("run");
            w.finish().expect("finish");
        }

        // Reopen read-only and verify rebuild + triggers restored.
        let conn = Connection::open(&db_path).expect("reopen");
        let msgs: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        let fts: i64 = conn
            .query_row("SELECT COUNT(*) FROM search_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(msgs, 3);
        assert_eq!(fts, msgs, "search_fts must match messages after rebuild");

        let triggers: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name LIKE 'messages_%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            triggers, 3,
            "auto-sync triggers must be recreated by finish"
        );
    }

    /// `open_for_bulk_ingest` sets synchronous=OFF; `finish` restores
    /// synchronous=NORMAL.
    #[test]
    fn bulk_pragma_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("bulk.sqlite");
        let mut w = Writer::new(&db).expect("new writer");

        // After `new`, synchronous = NORMAL (1).
        let sync: i64 = w
            .conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sync, 1);

        w.open_for_bulk_ingest().expect("bulk on");
        let sync: i64 = w
            .conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sync, 0, "synchronous should be OFF in bulk mode");

        // Re-open a second time is a no-op.
        w.open_for_bulk_ingest().expect("bulk on again");

        // Reopen the DB in a second connection to verify `finish` restored
        // the persistent PRAGMAs. (synchronous is per-connection in SQLite,
        // so we check the state on the writer's own connection before
        // it's consumed.)
        w.finish().expect("finish");

        // synchronous is a connection-level pragma, so it's moot after
        // finish drops the connection. The check above covers the write-
        // path assertion; finish()'s behaviour is exercised by not
        // panicking.
    }

    /// Channel close mid-project rolls back; no partial writes persist.
    #[test]
    fn channel_close_mid_project_rolls_back() {
        let mut w = fresh_writer();
        let (tx, rx) = unbounded::<IngestEvent>();

        tx.send(IngestEvent::Project {
            slug: "p1".into(),
            original_path: "/tmp/p1".into(),
            sessions_index_json: "{}".into(),
        })
        .unwrap();
        tx.send(message_event("p1", "s1", 0)).unwrap();
        // No ProjectComplete — just close.
        drop(tx);

        w.run(rx).expect("run");
        assert_eq!(count(&w.conn, "projects"), 0);
        assert_eq!(count(&w.conn, "messages"), 0);
    }

    // ─────────────────────────────────────────────────────────────────
    // write_batch_with_tx — RFC 005 Phase 4 C4.1
    // ─────────────────────────────────────────────────────────────────

    use crate::claude::types::{
        FileHistorySession, PersistedToolResult, PlanFile, TaskEntry, TodoFile,
    };

    fn fresh_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        schema::set_pragmas(&conn).expect("pragmas");
        schema::initialize_schema(&conn).expect("schema");
        conn
    }

    /// Happy path: one of each data-bearing variant goes through
    /// `write_batch_with_tx`, rows land in the right tables, and the
    /// returned `WriteBatchStats` matches the per-table counts.
    #[test]
    fn write_batch_with_tx_covers_every_data_variant() {
        let conn = fresh_conn();

        let events = vec![
            IngestEvent::Project {
                slug: "p1".into(),
                original_path: "/tmp/p1".into(),
                sessions_index_json: "{}".into(),
            },
            IngestEvent::ProjectMemory {
                slug: "p1".into(),
                content: "# memory".into(),
            },
            IngestEvent::Session {
                slug: "p1".into(),
                entry: sample_session("s1"),
            },
            message_event("p1", "s1", 0),
            message_event("p1", "s1", 1),
            IngestEvent::Subagent {
                slug: "p1".into(),
                session_id: "s1".into(),
                transcript: SubagentTranscript {
                    agent_id: "a1".into(),
                    agent_type: SubagentType::Task,
                    file_name: "agent-a1.jsonl".into(),
                    messages: vec![],
                    meta: None,
                    workflow_id: String::new(),
                },
            },
            IngestEvent::ToolResult {
                slug: "p1".into(),
                session_id: "s1".into(),
                tool_result: PersistedToolResult {
                    tool_use_id: "t1".into(),
                    content: "result body".into(),
                },
            },
            IngestEvent::FileHistory {
                session_id: "s1".into(),
                history: FileHistorySession {
                    session_id: "s1".into(),
                    snapshots: vec![],
                },
            },
            IngestEvent::Todo {
                session_id: "s1".into(),
                todo: TodoFile {
                    session_id: "s1".into(),
                    agent_id: "a1".into(),
                    items: vec![],
                },
            },
            IngestEvent::Task {
                session_id: "s1".into(),
                task: TaskEntry {
                    task_id: "s1".into(),
                    has_highwatermark: true,
                    highwatermark: Some(42),
                    lock_exists: false,
                    items: None,
                },
            },
            IngestEvent::Plan {
                slug: "p1".into(),
                plan: PlanFile {
                    slug: "plan-1".into(),
                    title: "Plan 1".into(),
                    content: "body".into(),
                    size: 4,
                },
            },
        ];

        let stats =
            write_batch_with_tx(&conn, &events, crate::core::DEFAULT_SOURCE_ID).expect("batch");

        assert_eq!(stats.messages_written, 2);
        assert_eq!(stats.subagents_written, 1);
        assert_eq!(stats.tool_results_written, 1);
        assert_eq!(stats.file_histories_written, 1);
        assert_eq!(stats.todos_written, 1);
        assert_eq!(stats.tasks_written, 1);
        assert_eq!(stats.plans_written, 1);

        // Every target table should see the expected rows.
        assert_eq!(count(&conn, "projects"), 1);
        assert_eq!(count(&conn, "project_memories"), 1);
        assert_eq!(count(&conn, "sessions"), 1);
        assert_eq!(count(&conn, "messages"), 2);
        assert_eq!(count(&conn, "subagents"), 1);
        assert_eq!(count(&conn, "tool_results"), 1);
        assert_eq!(count(&conn, "file_history"), 1);
        assert_eq!(count(&conn, "todos"), 1);
        assert_eq!(count(&conn, "tasks"), 1);
        assert_eq!(count(&conn, "plans"), 1);
        // FTS content-sync triggers should fire for the two message INSERTs.
        assert_eq!(count(&conn, "search_fts"), 2);
    }

    /// Empty batch: function opens BEGIN IMMEDIATE, commits immediately,
    /// and returns a zero-count stats struct. Not meant to be the fast
    /// path (the NAPI layer short-circuits empty input upstream) but
    /// the function must stay total.
    #[test]
    fn write_batch_with_tx_empty_input_is_ok() {
        let conn = fresh_conn();
        let stats =
            write_batch_with_tx(&conn, &[], crate::core::DEFAULT_SOURCE_ID).expect("empty batch");
        assert_eq!(stats.messages_written, 0);
        assert_eq!(stats.subagents_written, 0);
        assert_eq!(stats.tool_results_written, 0);
        assert_eq!(stats.file_histories_written, 0);
        assert_eq!(stats.todos_written, 0);
        assert_eq!(stats.tasks_written, 0);
        assert_eq!(stats.plans_written, 0);
    }

    /// Mid-batch SQL failure must roll back the whole batch — no rows
    /// persist, and the error propagates. Here we trip the failure by
    /// passing a non-NUL-byte slug that can't appear in the schema (the
    /// schema is lenient, so we simulate a row-level failure by writing
    /// into a dropped table).
    #[test]
    fn write_batch_with_tx_rolls_back_on_error() {
        let conn = fresh_conn();

        // Drop the `messages` table so the next INSERT fails.
        conn.execute_batch("DROP TABLE messages").unwrap();

        let events = vec![
            IngestEvent::Project {
                slug: "p1".into(),
                original_path: "/tmp/p1".into(),
                sessions_index_json: "{}".into(),
            },
            // This one fails — `messages` no longer exists.
            message_event("p1", "s1", 0),
        ];

        let err = write_batch_with_tx(&conn, &events, crate::core::DEFAULT_SOURCE_ID)
            .expect_err("batch must fail");
        matches!(err, WriterError::Sqlite(_));

        // The Project row must NOT persist — the whole batch rolled back.
        assert_eq!(count(&conn, "projects"), 0);
    }

    /// Orchestration-only events (ProjectComplete, SessionComplete,
    /// WorkerError, ClearSourceFiles) are no-ops inside the batch — they
    /// don't write rows, don't move counters, and don't error.
    #[test]
    fn write_batch_with_tx_ignores_orchestration_events() {
        let conn = fresh_conn();
        let events = vec![
            IngestEvent::SessionComplete {
                slug: "p1".into(),
                session_id: "s1".into(),
                message_count: 0,
                last_byte_position: 0,
            },
            IngestEvent::ProjectComplete {
                slug: "p1".into(),
                duration_ms: 0,
            },
            IngestEvent::WorkerError {
                slug: "p1".into(),
                error: "ignored".into(),
            },
            IngestEvent::ClearSourceFiles,
        ];
        let stats = write_batch_with_tx(&conn, &events, crate::core::DEFAULT_SOURCE_ID)
            .expect("orchestration-only batch");
        assert_eq!(stats.messages_written, 0);
        assert_eq!(stats.subagents_written, 0);
        assert_eq!(stats.tool_results_written, 0);
        assert_eq!(stats.file_histories_written, 0);
        assert_eq!(stats.todos_written, 0);
        assert_eq!(stats.tasks_written, 0);
        assert_eq!(stats.plans_written, 0);
    }

    /// Verifies the cold-start loop (`Writer::handle_event` → shared
    /// `dispatch_event`) and `write_batch_with_tx` produce the same
    /// row content for the same inputs — a sanity check that the
    /// refactor didn't drift the two paths.
    #[test]
    fn handle_event_and_write_batch_with_tx_agree() {
        // Arrange: identical event streams, two separate in-memory DBs.
        let events_for_cold = || {
            vec![
                IngestEvent::Project {
                    slug: "p1".into(),
                    original_path: "/tmp/p1".into(),
                    sessions_index_json: "{}".into(),
                },
                IngestEvent::Session {
                    slug: "p1".into(),
                    entry: sample_session("s1"),
                },
                message_event("p1", "s1", 0),
                IngestEvent::ProjectComplete {
                    slug: "p1".into(),
                    duration_ms: 0,
                },
            ]
        };

        let events_for_live = vec![
            IngestEvent::Project {
                slug: "p1".into(),
                original_path: "/tmp/p1".into(),
                sessions_index_json: "{}".into(),
            },
            IngestEvent::Session {
                slug: "p1".into(),
                entry: sample_session("s1"),
            },
            message_event("p1", "s1", 0),
        ];

        // Cold path
        let mut cold = fresh_writer();
        let (tx, rx) = unbounded::<IngestEvent>();
        for ev in events_for_cold() {
            tx.send(ev).unwrap();
        }
        drop(tx);
        cold.run(rx).expect("cold run");

        // Live path
        let live_conn = fresh_conn();
        write_batch_with_tx(&live_conn, &events_for_live, crate::core::DEFAULT_SOURCE_ID)
            .expect("live batch");

        // Both DBs should have the same row counts in the core tables.
        for table in &["projects", "sessions", "messages"] {
            assert_eq!(
                count(&cold.conn, table),
                count(&live_conn, table),
                "row count differs on {table}"
            );
        }

        // And the message's text_content / byte_offset should match.
        let cold_msg: (String, i64) = cold
            .conn
            .query_row(
                "SELECT text_content, byte_offset FROM messages WHERE msg_index = 0",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let live_msg: (String, i64) = live_conn
            .query_row(
                "SELECT text_content, byte_offset FROM messages WHERE msg_index = 0",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(cold_msg, live_msg);
    }
}
