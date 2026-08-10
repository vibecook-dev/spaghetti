//! `IngestEvent` — the channel-friendly equivalent of the TS
//! `ProjectParseSink` callback interface, used to pipe parsed ingest data
//! from one or more parser threads into the single SQLite writer thread.
//!
//! # Design
//!
//! The TS parser calls callbacks on a `ProjectParseSink`:
//!
//! ```text
//! sink.onProject(slug, originalPath, sessionsIndex);
//! sink.onMessage(slug, sessionId, message, index, byteOffset);
//! // ... etc
//! ```
//!
//! In Rust we flatten that callback surface into a `#[derive(Debug)]` enum
//! whose variants are pushed through a `crossbeam_channel`. The writer
//! consumes them in order and emits exactly one row (or one batch of rows,
//! in the case of `Message`) per variant.
//!
//! # Notes on the `Message` shape
//!
//! The RFC mandates `raw_json: String` for `Message`: the worker thread has
//! *already* validated the line via sonic-rs, and instead of re-serialising
//! the parsed `SessionMessage` we persist the original JSONL line bytes
//! verbatim into `messages.data`. This saves a full round-trip through
//! serde for every row.
//!
//! The other fields (`msg_type`, `uuid`, `timestamp`, token counts,
//! `fts_text`) are pre-extracted by the parser so the writer does no
//! reflection. `fts_text` is `Option<String>` because the parser may choose
//! to skip extraction on non-user/assistant/summary variants and pass
//! `None`; the writer treats `None` as an empty string, matching the TS
//! path where `extractTextContent` always returns a (possibly empty)
//! string.
//!
//! # Why no `Clone`
//!
//! These events are moved through a channel and consumed once. Deriving
//! `Clone` would permit accidental duplication of the large `raw_json`
//! payload. Add it only if a specific use-case requires it.
//!
//! Populated in RFC 003 commit 1.5.

use crate::claude::types::{
    FileHistorySession, PersistedToolResult, PlanFile, SessionIndexEntry, SubagentTranscript,
    TaskEntry, TodoFile, WorkflowRun,
};

/// One unit of work pushed from a parser thread to the writer thread.
///
/// Variants mirror the TS `ProjectParseSink` callbacks one-for-one, plus the
/// control events that carry failures and transaction boundaries. Each
/// data variant carries the minimum needed to write exactly one row (or, for
/// `Message`, one `messages` + one derived `search_fts` row).
///
/// The three failure variants are deliberately distinct rather than one
/// overloaded error, because the writer has to react differently to each:
/// [`RecordSkip`](IngestEvent::RecordSkip) keeps the project,
/// [`ProjectFatal`](IngestEvent::ProjectFatal) rolls it back and poisons the
/// slug, and [`SourceError`](IngestEvent::SourceError) belongs to no project
/// at all. Collapsing them is what let one bad line silently discard a whole
/// project before RFC 008 Phase 2.
#[derive(Debug)]
pub enum IngestEvent {
    /// Start of a project directory. Maps to `onProject`.
    ///
    /// `sessions_index_json` is the raw JSON bytes of the project's
    /// `sessions.json` — the parser already has this as a string and the
    /// writer stores it verbatim into `projects.sessions_index`, so we
    /// skip a re-serialize round trip.
    Project {
        slug: String,
        original_path: String,
        sessions_index_json: String,
    },

    /// Project-level `MEMORY.md`. Maps to `onProjectMemory`.
    ProjectMemory { slug: String, content: String },

    /// One entry from a project's `sessions.json`. Maps to `onSession`.
    Session {
        slug: String,
        entry: SessionIndexEntry,
    },

    /// One JSONL line from a session file. Maps to `onMessage`.
    ///
    /// `raw_json` is the original UTF-8 line bytes as-parsed by sonic-rs
    /// in the worker; it is written verbatim into `messages.data`. The
    /// other fields are pre-extracted projections of that same JSON used
    /// to populate typed columns without requiring the writer to re-parse.
    Message {
        slug: String,
        session_id: String,
        index: u32,
        byte_offset: u64,
        raw_json: String,
        msg_type: String,
        uuid: Option<String>,
        timestamp: Option<String>,
        input_tokens: u64,
        output_tokens: u64,
        cache_creation_tokens: u64,
        cache_read_tokens: u64,
        /// `None` means "parser didn't extract"; writer treats as empty.
        fts_text: Option<String>,
    },

    /// Subagent transcript (`agent-{id}.jsonl`). Maps to `onSubagent`.
    Subagent {
        slug: String,
        session_id: String,
        transcript: SubagentTranscript,
    },

    /// Workflow run record (`workflows/{runId}.json`). Maps to `onWorkflow`.
    Workflow {
        slug: String,
        session_id: String,
        workflow: WorkflowRun,
    },

    /// Persisted tool result (.txt file). Maps to `onToolResult`.
    ToolResult {
        slug: String,
        session_id: String,
        tool_result: PersistedToolResult,
    },

    /// File-history snapshots for a session. Maps to `onFileHistory`.
    FileHistory {
        session_id: String,
        history: FileHistorySession,
    },

    /// One todo file tied to a session/agent. Maps to `onTodo`.
    Todo { session_id: String, todo: TodoFile },

    /// One task entry for a session. Maps to `onTask`.
    Task { session_id: String, task: TaskEntry },

    /// One plan file for a project. Maps to `onPlan`.
    Plan { slug: String, plan: PlanFile },

    /// End-of-session marker. Maps to `onSessionComplete`.
    SessionComplete {
        slug: String,
        session_id: String,
        message_count: u32,
        last_byte_position: u64,
    },

    /// Stamp `sessions.tokens_estimated` (Grok session-aggregate / Codex estimate).
    SessionTokensEstimated { session_id: String, estimated: bool },

    /// Overwrite one message's token columns, leaving the rest of the row
    /// alone. Mirrors the TS `updateMessageTokens`.
    ///
    /// The alternative — re-sending the whole `Message` — means holding every
    /// message's raw JSON for the length of a session just in case an estimate
    /// is needed at the end, which on a large session doubles peak memory for
    /// a case that arises roughly once in eighteen sessions.
    MessageTokens {
        session_id: String,
        index: u32,
        input_tokens: u64,
        output_tokens: u64,
    },

    /// End-of-project marker — signals the writer to commit the current
    /// transaction. Maps to `onProjectComplete`.
    ProjectComplete { slug: String, duration_ms: u32 },

    /// End-of-project marker for a project the writer must **not** commit.
    /// Rolls back without committing and clears the project's poison state.
    ///
    /// Paired with [`IngestEvent::ProjectComplete`]: every project a reader
    /// starts emits exactly one of the two, so the writer can never be left
    /// holding an open transaction for a project nobody finished.
    ProjectAbort { slug: String },

    /// One record inside a project could not be read — a malformed JSONL
    /// line, say. Records the error and nothing else: the project keeps its
    /// transaction and every other record in it still commits.
    ///
    /// Replaces half of the old `WorkerError`, which rolled the whole
    /// project back on one bad line even though both emission sites meant
    /// "skip this record" (RFC 008 Phase 2).
    RecordSkip {
        slug: String,
        path: String,
        message: String,
    },

    /// A project cannot be trusted as a whole. Rolls its transaction back and
    /// poisons the slug, so any data event still in flight for it is ignored
    /// rather than written into the next project's transaction.
    ProjectFatal {
        slug: String,
        path: String,
        message: String,
    },

    /// A failure that happened before any project identity existed —
    /// discovery could not enumerate a directory, or a file failed to open
    /// before its owning project was known.
    ///
    /// Carries no slug on purpose. The alternative is inventing one, which
    /// RFC 008 P8 forbids: a fake slug becomes a real row. It poisons no
    /// project, but it does invalidate the source's success marker, so the
    /// unchanged fast path cannot skip the retry.
    SourceError { path: String, message: String },

    /// Truncate the `source_files` table. Emitted by the orchestrator
    /// before a batch of `Fingerprint` events so warm-fallback re-ingests
    /// start from a clean slate — stale paths (files that were deleted
    /// between runs) don't linger as ghost fingerprints.
    ///
    /// Claude cold path emits this *after* writing entity rows (fingerprints
    /// only). Do **not** delete sessions/messages here.
    ClearSourceFiles,

    /// Delete all durable rows for this writer's `source_id` (messages,
    /// sessions, projects, source_files). Used by Codex/Grok non-fast-path
    /// re-ingest *before* a full read so sessions removed on disk do not
    /// leave permanent orphans. FTS follows via message DELETE triggers.
    ClearSourceData,

    /// Warm-start fingerprint record — one row to UPSERT into
    /// `source_files`. Emitted by the orchestrator after all per-project
    /// events have been flushed, so the writer persists fingerprints
    /// inside its own transaction(s) without needing a second connection.
    ///
    /// Used for both cold ingest (where stored = empty, so every
    /// discovered file yields a Fingerprint event) and warm-fallback
    /// (where the full re-ingest repopulates source_files from scratch).
    Fingerprint {
        path: String,
        mtime_ms: f64,
        size: u64,
        byte_position: Option<u64>,
        category: String,
        project_slug: Option<String>,
        session_id: Option<String>,
    },
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::types::{
        FileHistorySession, PersistedToolResult, PlanFile, SessionIndexEntry, SubagentTranscript,
        SubagentType, TaskEntry, TodoFile,
    };

    fn sample_session_entry() -> SessionIndexEntry {
        SessionIndexEntry {
            session_id: "s1".into(),
            full_path: "/tmp/s1.jsonl".into(),
            file_mtime: 1.0,
            first_prompt: "hi".into(),
            summary: Some("summary".into()),
            message_count: 3,
            created: "2026-04-17T00:00:00Z".into(),
            modified: "2026-04-17T00:00:01Z".into(),
            git_branch: "main".into(),
            project_path: "/tmp/proj".into(),
            is_sidechain: false,
        }
    }

    #[test]
    fn project_variant_debug() {
        let ev = IngestEvent::Project {
            slug: "proj".into(),
            original_path: "/tmp/proj".into(),
            sessions_index_json: "{}".into(),
        };
        let s = format!("{ev:?}");
        assert!(s.contains("Project"));
        assert!(s.contains("proj"));
    }

    #[test]
    fn project_memory_variant_debug() {
        let ev = IngestEvent::ProjectMemory {
            slug: "proj".into(),
            content: "# memory".into(),
        };
        let s = format!("{ev:?}");
        assert!(s.contains("ProjectMemory"));
    }

    #[test]
    fn session_variant_debug() {
        let ev = IngestEvent::Session {
            slug: "proj".into(),
            entry: sample_session_entry(),
        };
        let s = format!("{ev:?}");
        assert!(s.contains("Session"));
        assert!(s.contains("s1"));
    }

    #[test]
    fn message_variant_debug() {
        let ev = IngestEvent::Message {
            slug: "proj".into(),
            session_id: "s1".into(),
            index: 0,
            byte_offset: 0,
            raw_json: "{\"type\":\"user\"}".into(),
            msg_type: "user".into(),
            uuid: Some("u1".into()),
            timestamp: Some("2026-04-17T00:00:00Z".into()),
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            fts_text: Some("hello".into()),
        };
        let s = format!("{ev:?}");
        assert!(s.contains("Message"));
        assert!(s.contains("raw_json"));
    }

    #[test]
    fn subagent_variant_debug() {
        let ev = IngestEvent::Subagent {
            slug: "proj".into(),
            session_id: "s1".into(),
            transcript: SubagentTranscript {
                agent_id: "a1".into(),
                agent_type: SubagentType::Task,
                file_name: "agent-a1.jsonl".into(),
                messages: vec![],
                meta: None,
                workflow_id: String::new(),
            },
        };
        let s = format!("{ev:?}");
        assert!(s.contains("Subagent"));
    }

    #[test]
    fn tool_result_and_file_history_variants_debug() {
        let tr = IngestEvent::ToolResult {
            slug: "proj".into(),
            session_id: "s1".into(),
            tool_result: PersistedToolResult {
                tool_use_id: "t1".into(),
                content: "out".into(),
            },
        };
        assert!(format!("{tr:?}").contains("ToolResult"));

        let fh = IngestEvent::FileHistory {
            session_id: "s1".into(),
            history: FileHistorySession {
                session_id: "s1".into(),
                snapshots: vec![],
            },
        };
        assert!(format!("{fh:?}").contains("FileHistory"));
    }

    #[test]
    fn todo_task_plan_variants_debug() {
        let todo = IngestEvent::Todo {
            session_id: "s1".into(),
            todo: TodoFile {
                session_id: "s1".into(),
                agent_id: "a1".into(),
                items: vec![],
            },
        };
        assert!(format!("{todo:?}").contains("Todo"));

        let task = IngestEvent::Task {
            session_id: "s1".into(),
            task: TaskEntry {
                task_id: "s1".into(),
                has_highwatermark: false,
                highwatermark: None,
                lock_exists: false,
                items: None,
            },
        };
        assert!(format!("{task:?}").contains("Task"));

        let plan = IngestEvent::Plan {
            slug: "proj".into(),
            plan: PlanFile {
                slug: "p1".into(),
                title: "Plan".into(),
                content: "body".into(),
                size: 4,
            },
        };
        assert!(format!("{plan:?}").contains("Plan"));
    }

    #[test]
    fn completion_and_error_variants_debug() {
        let sc = IngestEvent::SessionComplete {
            slug: "proj".into(),
            session_id: "s1".into(),
            message_count: 3,
            last_byte_position: 100,
        };
        assert!(format!("{sc:?}").contains("SessionComplete"));

        let pc = IngestEvent::ProjectComplete {
            slug: "proj".into(),
            duration_ms: 5,
        };
        assert!(format!("{pc:?}").contains("ProjectComplete"));

        let skip = IngestEvent::RecordSkip {
            slug: "proj".into(),
            path: "/tmp/proj/s.jsonl".into(),
            message: "boom".into(),
        };
        assert!(format!("{skip:?}").contains("RecordSkip"));

        let fatal = IngestEvent::ProjectFatal {
            slug: "proj".into(),
            path: "/tmp/proj".into(),
            message: "boom".into(),
        };
        assert!(format!("{fatal:?}").contains("ProjectFatal"));

        let src = IngestEvent::SourceError {
            path: "/tmp".into(),
            message: "boom".into(),
        };
        assert!(format!("{src:?}").contains("SourceError"));
    }
}
