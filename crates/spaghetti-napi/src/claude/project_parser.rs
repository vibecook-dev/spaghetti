//! Per-project streaming parser — ported from
//! `packages/sdk/src/parser/project-parser.ts`.
//!
//! Single-threaded: given one `<root_dir>/projects/<slug>/` directory,
//! walks every artifact (sessions index, MEMORY.md, JSONL session files,
//! subagent transcripts, tool-result .txt files, todos, tasks, file-history
//! snapshots) and pushes one [`IngestEvent`] per discovered artifact into a
//! [`crossbeam_channel::Sender`].
//!
//! Parse errors inside an individual session or file are swallowed and
//! re-emitted as [`IngestEvent::RecordSkip`] — this matches the TS
//! parser's behaviour of wrapping each sub-parse in its own `try/catch`.
//! The only error the caller sees is a channel-send failure, which is
//! fatal (the writer has gone away).
//!
//! Populated in RFC 003 commit 1.4.

use std::path::{Path, PathBuf};
use std::time::Instant;

use crossbeam_channel::{SendError, Sender};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;

use crate::claude::message_extractor;
use crate::claude::session_metadata;
use crate::claude::types::{
    FileHistorySession, FileHistorySnapshotFile, PersistedToolResult, PlanFile, SessionIndexEntry,
    SessionMessage, SessionsIndex, SubagentMeta, SubagentTranscript, SubagentType, TaskEntry,
    TodoFile, TodoItem, WorkflowRun,
};
use crate::core::event::IngestEvent;
use crate::core::jsonl::read_jsonl_streaming;

// ─── Regex patterns — copied verbatim from project-parser.ts ────────────────

/// Matches canonical session file names `<uuid>.jsonl` — identical to the
/// `UUID_JSONL` regex in `discoverSessionEntries`.
static UUID_JSONL: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\.jsonl$")
        .expect("UUID_JSONL regex compiles")
});

/// Matches `agent-<id>.jsonl` where `<id>` starts with `a`. Port of
/// `extractAgentId`'s `^agent-(a.+)\.jsonl$` pattern.
static SUBAGENT_FILE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^agent-(a.+)\.jsonl$").expect("SUBAGENT_FILE regex compiles"));

/// Matches file-history snapshot file names `<hash>@v<version>`. Port of
/// `parseFileHistory`'s `^([0-9a-f]+)@v(\d+)$` pattern.
static FILE_HISTORY_SNAPSHOT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^([0-9a-f]+)@v(\d+)$").expect("FILE_HISTORY_SNAPSHOT regex compiles")
});

/// Matches todo file names `<session-id>-agent-<agent-id>.json`. Port of
/// `parseTodos`'s `^(.+?)-agent-(.+)\.json$` pattern.
static TODO_FILE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(.+?)-agent-(.+)\.json$").expect("TODO_FILE regex compiles"));

// ─── Errors ─────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// The writer channel was dropped — no point in continuing work.
    ///
    /// The inner error is boxed to keep `ParseError` small (the
    /// `SendError` payload is a full `IngestEvent`, which can be ~250
    /// bytes); clippy's `result_large_err` flags anything over ~128.
    #[error("event channel closed")]
    ChannelClosed(#[source] Box<SendError<IngestEvent>>),

    /// The project cannot be trusted as a whole — its directory could not be
    /// enumerated, or a file failed in a way that makes the remaining view
    /// partial rather than merely incomplete.
    ///
    /// Distinct from a skipped record: this rolls the project back, where a
    /// bad line leaves every other record in the project committed.
    #[error("{path}: {message}")]
    Fatal { path: String, message: String },
}

impl From<SendError<IngestEvent>> for ParseError {
    fn from(e: SendError<IngestEvent>) -> Self {
        Self::ChannelClosed(Box::new(e))
    }
}

// ─── Public API ─────────────────────────────────────────────────────────────

/// Per-project streaming parser. Stateless; one `ProjectParser` can parse
/// any number of projects sequentially (or be cloned cheaply across rayon
/// workers — there's no interior state).
#[derive(Debug, Default, Clone, Copy)]
pub struct ProjectParser;

impl ProjectParser {
    pub fn new() -> Self {
        Self
    }

    /// Parse a single project and push its [`IngestEvent`]s into `events`.
    ///
    /// Record-level failures (a bad JSON line, an unreadable session) are
    /// emitted as [`IngestEvent::RecordSkip`] and do not stop the project.
    /// Project-level failures return [`ParseError::Fatal`], which this
    /// function converts into the terminal `ProjectFatal` + `ProjectAbort`
    /// pair before returning.
    ///
    /// **Exactly one terminal event per started project.** The guard below is
    /// the only place `ProjectComplete` or `ProjectAbort` is emitted, so no
    /// early return can leave the writer holding an open transaction for a
    /// project nobody finished (RFC 008 Phase 2).
    pub fn parse_project(
        &self,
        root_dir: &Path,
        slug: &str,
        events: &Sender<IngestEvent>,
    ) -> Result<(), ParseError> {
        let start = Instant::now();
        let outcome = self.parse_project_body(root_dir, slug, events);

        match &outcome {
            Ok(()) => {
                let duration_ms = u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX);
                events.send(IngestEvent::ProjectComplete {
                    slug: slug.to_owned(),
                    duration_ms,
                })?;
            }
            // The writer is gone; there is nobody left to tell. Sending here
            // would just fail again and mask the original error.
            Err(ParseError::ChannelClosed(_)) => {}
            Err(ParseError::Fatal { path, message }) => {
                let _ = events.send(IngestEvent::ProjectFatal {
                    slug: slug.to_owned(),
                    path: path.clone(),
                    message: message.clone(),
                });
                let _ = events.send(IngestEvent::ProjectAbort {
                    slug: slug.to_owned(),
                });
            }
        }

        outcome
    }

    fn parse_project_body(
        &self,
        root_dir: &Path,
        slug: &str,
        events: &Sender<IngestEvent>,
    ) -> Result<(), ParseError> {
        let project_dir = root_dir.join("projects").join(slug);

        // 1. Read + parse sessions-index.json (or synthesise an empty one).
        let sessions_index = read_sessions_index(&project_dir);
        let original_path = sessions_index
            .original_path
            .clone()
            .unwrap_or_else(|| slug_to_path(slug));

        // 2. Merge index entries with any on-disk JSONL files the index doesn't
        //    already know about — matches TS `mergeWithDiscoveredEntries`.
        let merged_index = SessionsIndex {
            version: sessions_index.version,
            entries: merge_with_discovered_entries(
                sessions_index.entries,
                &project_dir,
                sessions_index.original_path.as_deref(),
            )?,
            original_path: sessions_index.original_path,
        };

        // 3. Emit the Project event storing the serialized MERGED index
        //    (parity with TS `onProject`, which stores
        //    `JSON.stringify(parseSessionsIndex(...))`). Storing the raw file
        //    text here would drop discovered-but-unindexed sessions from
        //    `projects.sessions_index`.
        let sessions_index_json =
            serde_json::to_string(&merged_index).unwrap_or_else(|_| "{}".to_owned());
        events.send(IngestEvent::Project {
            slug: slug.to_owned(),
            original_path,
            sessions_index_json,
        })?;

        // 4. MEMORY.md (optional)
        if let Some(memory) = read_project_memory(&project_dir) {
            events.send(IngestEvent::ProjectMemory {
                slug: slug.to_owned(),
                content: memory,
            })?;
        }

        // 5. Walk sessions. The only error returned from `parse_one_session`
        //    is `ChannelClosed` — propagate it immediately so we don't spin
        //    over a dead channel.
        for entry in &merged_index.entries {
            parse_one_session(root_dir, &project_dir, slug, entry, events)?;
        }

        // The terminal event is emitted by `parse_project`'s guard, not here —
        // see its doc comment.
        Ok(())
    }
}

// ─── Deterministic fault injection ──────────────────────────────────────────

/// A test-only seam for forcing an I/O failure at a named path.
///
/// RFC 008 Phase 2 requires the transaction/error matrix to pass on Linux,
/// macOS, and Windows, and explicitly rules out a Unix-only `chmod 0` as the
/// sole acceptance test. Some failures — a project directory that exists but
/// cannot be enumerated — have no portable way to provoke from the filesystem
/// alone, so the RFC allows a deterministic fault seam instead. This is it.
///
/// Compiled out of release builds entirely: in `#[cfg(not(test))]` the check
/// is a `None` that the optimiser deletes.
pub(crate) mod fault {
    #[cfg(test)]
    use std::collections::HashMap;
    use std::path::Path;
    #[cfg(test)]
    use std::path::PathBuf;
    #[cfg(test)]
    use std::sync::Mutex;

    #[cfg(test)]
    static ARMED: Mutex<Option<HashMap<PathBuf, String>>> = Mutex::new(None);

    /// Make reads of `path` fail with `message` until [`disarm`] is called.
    ///
    /// Keyed by absolute path, and every test that uses this owns a unique
    /// temp directory, so concurrently running tests cannot see each other's
    /// faults.
    #[cfg(test)]
    pub(crate) fn arm(path: &Path, message: &str) {
        let mut guard = ARMED.lock().expect("fault registry poisoned");
        guard
            .get_or_insert_with(HashMap::new)
            .insert(path.to_path_buf(), message.to_owned());
    }

    #[cfg(test)]
    pub(crate) fn disarm(path: &Path) {
        if let Some(map) = ARMED.lock().expect("fault registry poisoned").as_mut() {
            map.remove(path);
        }
    }

    /// The injected failure for `path`, if one is armed.
    #[cfg(test)]
    pub(crate) fn injected(path: &Path) -> Option<String> {
        ARMED
            .lock()
            .expect("fault registry poisoned")
            .as_ref()
            .and_then(|m| m.get(path).cloned())
    }

    #[cfg(not(test))]
    #[inline(always)]
    pub(crate) fn injected(_path: &Path) -> Option<String> {
        None
    }
}

// ─── Session parsing ────────────────────────────────────────────────────────

fn parse_one_session(
    root_dir: &Path,
    project_dir: &Path,
    slug: &str,
    entry: &SessionIndexEntry,
    events: &Sender<IngestEvent>,
) -> Result<(), ParseError> {
    let session_id = entry.session_id.clone();

    events.send(IngestEvent::Session {
        slug: slug.to_owned(),
        entry: entry.clone(),
    })?;

    // Canonical path, with fallback to entry.full_path if the canonical
    // file doesn't exist (handles stale indices pointing at relocated
    // JSONL files). Port of the TS `filePath` ternary.
    let canonical_path = project_dir.join(format!("{session_id}.jsonl"));
    let file_path: PathBuf = if canonical_path.exists() {
        canonical_path.clone()
    } else if !entry.full_path.is_empty() && Path::new(&entry.full_path).exists() {
        PathBuf::from(&entry.full_path)
    } else {
        canonical_path.clone()
    };

    let mut message_count: u32 = 0;
    let mut last_byte_position: u64 = 0;

    // Collect send errors from inside the closure. read_jsonl_streaming
    // calls the closure in a loop — we can't propagate channel failures
    // through a `?` inside the closure, so we stash them.
    let mut send_error: Option<SendError<IngestEvent>> = None;

    let stream_result = read_jsonl_streaming(&file_path, 0, |line, index, byte_offset| {
        if send_error.is_some() {
            // A previous send failed; skip further work. The outer loop
            // will still run but do nothing.
            return;
        }
        match build_message_event(slug, &session_id, line, index, byte_offset) {
            Ok(ev) => {
                if let Err(e) = events.send(ev) {
                    send_error = Some(e);
                    return;
                }
                message_count = message_count.saturating_add(1);
                last_byte_position = byte_offset;
            }
            Err(parse_err) => {
                // Skip the record, keep the project. The path is the session
                // file, which is what the fingerprint suppression keys on, so
                // the file is re-read next run rather than being recorded as
                // successfully ingested.
                if let Err(e) = events.send(IngestEvent::RecordSkip {
                    slug: slug.to_owned(),
                    path: file_path.to_string_lossy().into_owned(),
                    message: format!("session {session_id} line {index}: {parse_err}"),
                }) {
                    send_error = Some(e);
                }
            }
        }
    });

    if let Some(e) = send_error {
        return Err(ParseError::from(e));
    }

    match stream_result {
        Ok(r) => {
            // read_jsonl_streaming reports the final byte position past the
            // last byte read, even if no complete lines were yielded.
            last_byte_position = r.final_byte_position.max(last_byte_position);
        }
        Err(e) => {
            // A mid-read failure costs this session, not the project: the
            // messages already streamed above are real and other sessions in
            // the project are unaffected. Withholding the fingerprint is what
            // makes the truncated read retry.
            events.send(IngestEvent::RecordSkip {
                slug: slug.to_owned(),
                path: file_path.to_string_lossy().into_owned(),
                message: format!("session {session_id} read error: {e}"),
            })?;
        }
    }

    // Subagents (incl. nested workflow transcripts, tagged by workflow_id)
    for transcript in read_subagents(project_dir, &session_id) {
        events.send(IngestEvent::Subagent {
            slug: slug.to_owned(),
            session_id: session_id.clone(),
            transcript,
        })?;
    }

    // Workflow run records (agent-orchestration analytics)
    for workflow in read_workflows(project_dir, &session_id) {
        events.send(IngestEvent::Workflow {
            slug: slug.to_owned(),
            session_id: session_id.clone(),
            workflow,
        })?;
    }

    // Tool results
    for tool_result in read_tool_results(project_dir, &session_id) {
        events.send(IngestEvent::ToolResult {
            slug: slug.to_owned(),
            session_id: session_id.clone(),
            tool_result,
        })?;
    }

    events.send(IngestEvent::SessionComplete {
        slug: slug.to_owned(),
        session_id: session_id.clone(),
        message_count,
        last_byte_position,
    })?;

    // File history (always parsed, not gated by skipMessages — matching TS)
    if let Some(history) = read_file_history(root_dir, &session_id) {
        events.send(IngestEvent::FileHistory {
            session_id: session_id.clone(),
            history,
        })?;
    }

    // Todos
    for todo in read_todos(root_dir, &session_id) {
        events.send(IngestEvent::Todo {
            session_id: session_id.clone(),
            todo,
        })?;
    }

    // Task
    if let Some(task) = read_task(root_dir, &session_id) {
        events.send(IngestEvent::Task { session_id, task })?;
    }

    Ok(())
}

/// Parse one JSONL line into an `IngestEvent::Message` via the Claude
/// [`message_extractor`] (RFC 006 / Phase B seam).
fn build_message_event(
    slug: &str,
    session_id: &str,
    line: &str,
    index: u32,
    byte_offset: u64,
) -> Result<IngestEvent, serde_json::Error> {
    let p = message_extractor::project_jsonl_line(line)?;
    Ok(IngestEvent::Message {
        slug: slug.to_owned(),
        session_id: session_id.to_owned(),
        index,
        byte_offset,
        raw_json: line.to_owned(),
        msg_type: p.msg_type,
        uuid: p.uuid,
        timestamp: p.timestamp,
        input_tokens: p.input_tokens,
        output_tokens: p.output_tokens,
        cache_creation_tokens: p.cache_creation_tokens,
        cache_read_tokens: p.cache_read_tokens,
        fts_text: p.fts_text,
    })
}

// ─── sessions-index.json ────────────────────────────────────────────────────

/// Read + parse `sessions-index.json` from `<project_dir>/sessions-index.json`.
///
/// Returns the parsed [`SessionsIndex`]. On a missing or malformed file we
/// fall back to a synthetic empty index — matching TS `parseSessionsIndex`,
/// which reconstructs the index from disk rather than persisting the raw
/// (possibly malformed) file text.
fn read_sessions_index(project_dir: &Path) -> SessionsIndex {
    let path = project_dir.join("sessions-index.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return empty_sessions_index();
    };
    serde_json::from_str::<SessionsIndex>(&raw).unwrap_or_else(|_| empty_sessions_index())
}

fn empty_sessions_index() -> SessionsIndex {
    SessionsIndex {
        version: 1,
        original_path: None,
        entries: Vec::new(),
    }
}

/// Port of TS `mergeWithDiscoveredEntries` — appends any on-disk JSONL
/// files whose session IDs aren't already present in the index.
fn merge_with_discovered_entries(
    index_entries: Vec<SessionIndexEntry>,
    project_dir: &Path,
    original_path: Option<&str>,
) -> Result<Vec<SessionIndexEntry>, ParseError> {
    let mut indexed: std::collections::HashSet<String> =
        index_entries.iter().map(|e| e.session_id.clone()).collect();
    let mut merged = index_entries;

    // Sorted, because the source is a directory listing and neither engine
    // gets a guaranteed order from one. NTFS returns entries sorted while ext4
    // and APFS do not, so an unsorted merge agreed on Windows and disagreed on
    // Linux and macOS — a cross-engine divergence that depended on the
    // developer's filesystem (RFC 008 Phase 5).
    let mut discovered = discover_session_entries(project_dir, original_path)?;
    discovered.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    for entry in discovered {
        if indexed.insert(entry.session_id.clone()) {
            merged.push(entry);
        }
    }
    Ok(merged)
}

/// Port of TS `discoverSessionEntries` — scans the project dir for
/// canonical `<uuid>.jsonl` files and builds a stub entry for each.
///
/// We skip the TS "peek at first user prompt" streaming read here: the
/// real parser reads the whole file immediately afterwards anyway, so
/// re-reading just to extract a 200-char prompt is wasteful. The entry's
/// `first_prompt` stays empty — the writer fills it in from the first
/// user message it ingests.
fn discover_session_entries(
    project_dir: &Path,
    original_path: Option<&str>,
) -> Result<Vec<SessionIndexEntry>, ParseError> {
    // Enumerating the project directory is what tells us which sessions exist.
    // If it fails we cannot know, and a merged index built from the
    // sessions-index alone would look complete while silently omitting every
    // session on disk — so this is fatal to the project rather than a skipped
    // record (RFC 008 Phase 2).
    //
    // A missing directory is not a failure: the slug came from a directory
    // listing, so this means it was deleted mid-run, which the next warm start
    // handles as the deletion it is.
    if let Some(err) = fault::injected(project_dir) {
        return Err(ParseError::Fatal {
            path: project_dir.to_string_lossy().into_owned(),
            message: err,
        });
    }
    let read_dir = match std::fs::read_dir(project_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(ParseError::Fatal {
                path: project_dir.to_string_lossy().into_owned(),
                message: e.to_string(),
            })
        }
    };

    let slug_fallback = project_dir
        .file_name()
        .and_then(|s| s.to_str())
        .map(slug_to_path);
    let project_path = original_path
        .map(str::to_owned)
        .or(slug_fallback)
        .unwrap_or_default();

    let mut out = Vec::new();
    for entry in read_dir.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !UUID_JSONL.is_match(name) {
            continue;
        }
        let path = entry.path();
        let session_id = name.trim_end_matches(".jsonl").to_owned();

        let file_mtime = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            // Node computes `stats.mtimeMs` as `sec * 1000 + nsec / 1e6`. Computing
            // `as_secs_f64() * 1000.0` instead rounds differently in the last
            // millisecond, and both engines then truncate when rendering three
            // decimal digits — which turned a floating-point hair's breadth into
            // a visible 1 ms disagreement on session timestamps (RFC 008 Phase 5).
            .map(|d| d.as_secs() as f64 * 1000.0 + d.subsec_nanos() as f64 / 1_000_000.0)
            .unwrap_or(0.0);

        // Port of the TS discoverSessionEntries: set `created` and
        // `modified` from file mtime as ISO-8601 (matches what
        // `new Date(mtimeMs).toISOString()` produces), and peek at the
        // file's first user message for `first_prompt`. Without these,
        // projects that have no sessions-index.json end up with blank
        // timestamps (UI sort-by-modified breaks) and all sessions
        // labeled "No prompt".
        let modified_iso = epoch_ms_to_iso8601(file_mtime);
        let first_prompt = peek_first_user_prompt(&path).unwrap_or_else(|| "No prompt".to_owned());

        out.push(SessionIndexEntry {
            session_id,
            full_path: path.to_string_lossy().into_owned(),
            file_mtime,
            first_prompt,
            summary: String::new(),
            message_count: 0,
            created: modified_iso.clone(),
            modified: modified_iso,
            git_branch: String::new(),
            project_path: project_path.clone(),
            is_sidechain: false,
        });
    }
    Ok(out)
}

// ─── MEMORY.md ──────────────────────────────────────────────────────────────

fn read_project_memory(project_dir: &Path) -> Option<String> {
    let path = project_dir.join("memory").join("MEMORY.md");
    std::fs::read_to_string(path).ok()
}

// ─── Subagents ──────────────────────────────────────────────────────────────

fn read_subagents(project_dir: &Path, session_id: &str) -> Vec<SubagentTranscript> {
    let dir = project_dir.join(session_id).join("subagents");
    let mut out = Vec::new();

    // Top-level subagent transcripts (not associated with a workflow).
    if let Ok(read_dir) = std::fs::read_dir(&dir) {
        for entry in read_dir.flatten() {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if !name.ends_with(".jsonl") {
                continue;
            }
            out.push(read_one_subagent(&entry.path(), name, ""));
        }
    }

    // Nested workflow subagent transcripts:
    //   subagents/workflows/{wf_id}/agent-*.jsonl  (journal.jsonl is skipped
    //   by the `agent-` prefix). Prior to this the parser only walked the
    //   flat subagents/ dir, so workflow-orchestrated transcripts were
    //   invisible. Grouped to their run via `workflow_id`.
    if let Ok(wf_dirs) = std::fs::read_dir(dir.join("workflows")) {
        for wf_entry in wf_dirs.flatten() {
            if !wf_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let wf_name = wf_entry.file_name();
            let Some(workflow_id) = wf_name.to_str() else {
                continue;
            };
            if let Ok(agent_dir) = std::fs::read_dir(wf_entry.path()) {
                for entry in agent_dir.flatten() {
                    let file_name = entry.file_name();
                    let Some(name) = file_name.to_str() else {
                        continue;
                    };
                    if !name.starts_with("agent-") || !name.ends_with(".jsonl") {
                        continue;
                    }
                    out.push(read_one_subagent(&entry.path(), name, workflow_id));
                }
            }
        }
    }

    out
}

fn read_one_subagent(path: &Path, name: &str, workflow_id: &str) -> SubagentTranscript {
    let mut messages: Vec<SessionMessage> = Vec::new();
    let _ = read_jsonl_streaming(path, 0, |line, _idx, _off| {
        if let Ok(msg) = serde_json::from_str::<SessionMessage>(line) {
            messages.push(msg);
        }
    });
    // Sibling `agent-{id}.meta.json` carries the real agent type + description.
    let meta = std::fs::read_to_string(path.with_extension("meta.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<SubagentMeta>(&raw).ok());
    SubagentTranscript {
        agent_id: extract_agent_id(name),
        agent_type: infer_agent_type(name),
        file_name: name.to_owned(),
        messages,
        meta,
        workflow_id: workflow_id.to_owned(),
    }
}

/// Parse workflow run records under `projects/{slug}/{sid}/workflows/`.
fn read_workflows(project_dir: &Path, session_id: &str) -> Vec<WorkflowRun> {
    let dir = project_dir.join(session_id).join("workflows");
    let Ok(read_dir) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for entry in read_dir.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !name.starts_with("wf_") || !name.ends_with(".json") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(data) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };

        let workflow_id = data
            .get("runId")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| name.trim_end_matches(".json").to_owned());
        let num = |k: &str| data.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let name_field = data
            .get("workflowName")
            .and_then(|v| v.as_str())
            .unwrap_or(&workflow_id)
            .to_owned();
        let status = data
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        let subagent_count = count_workflow_subagents(project_dir, session_id, &workflow_id) as f64;
        let journal = read_workflow_journal(project_dir, session_id, &workflow_id);

        out.push(WorkflowRun {
            name: name_field,
            status,
            agent_count: num("agentCount"),
            total_tokens: num("totalTokens"),
            total_tool_calls: num("totalToolCalls"),
            duration_ms: num("durationMs"),
            subagent_count,
            data,
            journal,
            workflow_id,
        });
    }

    out.sort_by(|a, b| a.workflow_id.cmp(&b.workflow_id));
    out
}

fn count_workflow_subagents(project_dir: &Path, session_id: &str, workflow_id: &str) -> usize {
    let dir = project_dir
        .join(session_id)
        .join("subagents")
        .join("workflows")
        .join(workflow_id);
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .map(|n| n.starts_with("agent-") && n.ends_with(".jsonl"))
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0)
}

fn read_workflow_journal(project_dir: &Path, session_id: &str, workflow_id: &str) -> Vec<Value> {
    let path = project_dir
        .join(session_id)
        .join("subagents")
        .join("workflows")
        .join(workflow_id)
        .join("journal.jsonl");
    let mut out = Vec::new();
    let _ = read_jsonl_streaming(&path, 0, |line, _idx, _off| {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            out.push(v);
        }
    });
    out
}

/// TS: `const match = fileName.match(/^agent-(a.+)\.jsonl$/);`
fn extract_agent_id(file_name: &str) -> String {
    if let Some(caps) = SUBAGENT_FILE.captures(file_name) {
        caps.get(1)
            .map(|m| m.as_str().to_owned())
            .unwrap_or_else(|| file_name.trim_end_matches(".jsonl").to_owned())
    } else {
        file_name.trim_end_matches(".jsonl").to_owned()
    }
}

/// TS:
/// ```ts
/// if (fileName.includes('prompt_suggestion')) return 'prompt_suggestion';
/// if (fileName.includes('compact')) return 'compact';
/// return 'task';
/// ```
fn infer_agent_type(file_name: &str) -> SubagentType {
    if file_name.contains("prompt_suggestion") {
        SubagentType::PromptSuggestion
    } else if file_name.contains("compact") {
        SubagentType::Compact
    } else {
        SubagentType::Task
    }
}

// ─── Tool results ───────────────────────────────────────────────────────────

fn read_tool_results(project_dir: &Path, session_id: &str) -> Vec<PersistedToolResult> {
    let dir = project_dir.join(session_id).join("tool-results");
    let Ok(read_dir) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for entry in read_dir.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !name.ends_with(".txt") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let tool_use_id = name.trim_end_matches(".txt").to_owned();
        out.push(PersistedToolResult {
            tool_use_id,
            content,
        });
    }
    out
}

// ─── File history ───────────────────────────────────────────────────────────

fn read_file_history(root_dir: &Path, session_id: &str) -> Option<FileHistorySession> {
    let dir = root_dir.join("file-history").join(session_id);
    let read_dir = std::fs::read_dir(&dir).ok()?;

    let mut snapshots = Vec::new();
    for entry in read_dir.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        let Some(caps) = FILE_HISTORY_SNAPSHOT.captures(name) else {
            continue;
        };
        let hash = caps.get(1)?.as_str().to_owned();
        let Ok(version) = caps.get(2)?.as_str().parse::<u64>() else {
            continue;
        };
        let path = entry.path();
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

        snapshots.push(FileHistorySnapshotFile {
            hash,
            version,
            file_name: name.to_owned(),
            content,
            size,
        });
    }

    if snapshots.is_empty() {
        None
    } else {
        Some(FileHistorySession {
            session_id: session_id.to_owned(),
            snapshots,
        })
    }
}

// ─── Todos ──────────────────────────────────────────────────────────────────

fn read_todos(root_dir: &Path, session_id: &str) -> Vec<TodoFile> {
    let dir = root_dir.join("todos");
    let Ok(read_dir) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let prefix = format!("{session_id}-agent-");
    let mut out = Vec::new();
    for entry in read_dir.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        // TS glob equivalent: `${sessionId}-agent-*.json`
        if !name.starts_with(&prefix) || !name.ends_with(".json") {
            continue;
        }
        let Some(caps) = TODO_FILE.captures(name) else {
            continue;
        };
        let match_session = caps
            .get(1)
            .map(|m| m.as_str().to_owned())
            .unwrap_or_default();
        let match_agent = caps
            .get(2)
            .map(|m| m.as_str().to_owned())
            .unwrap_or_default();

        let items = match std::fs::read_to_string(entry.path()) {
            Ok(raw) => serde_json::from_str::<Vec<TodoItem>>(&raw).unwrap_or_default(),
            Err(_) => Vec::new(),
        };

        out.push(TodoFile {
            session_id: match_session,
            agent_id: match_agent,
            items,
        });
    }
    out
}

// ─── Tasks ──────────────────────────────────────────────────────────────────

fn read_task(root_dir: &Path, session_id: &str) -> Option<TaskEntry> {
    let task_dir = root_dir.join("tasks").join(session_id);
    let lock_path = task_dir.join(".lock");
    if !lock_path.exists() {
        return None;
    }

    let (has_highwatermark, highwatermark) =
        match std::fs::read_to_string(task_dir.join(".highwatermark")) {
            // Lenient parse to match TS `parseInt(hwContent.trim(), 10)`:
            // "12abc" → 12, non-numeric → None. `.parse::<i64>()` is strict and
            // would reject any trailing noise, dropping a valid highwatermark.
            Ok(raw) => (true, parse_leading_int(&raw)),
            Err(_) => (false, None),
        };

    Some(TaskEntry {
        task_id: session_id.to_owned(),
        has_highwatermark,
        highwatermark,
        lock_exists: true,
        items: None,
    })
}

/// Parse the leading base-10 integer prefix of `s`, mirroring JS
/// `parseInt(s, 10)`: skip leading whitespace, consume an optional sign, then
/// consecutive ASCII digits, and ignore any trailing characters
/// (`"12abc"` → `12`, `"  -5x"` → `-5`). Returns `None` when no digits follow.
fn parse_leading_int(s: &str) -> Option<i64> {
    let t = s.trim_start();
    let bytes = t.as_bytes();
    let mut i = 0;
    let mut negative = false;
    if let Some(&c) = bytes.first() {
        if c == b'+' || c == b'-' {
            negative = c == b'-';
            i = 1;
        }
    }
    let start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == start {
        return None;
    }
    t[start..i]
        .parse::<i64>()
        .ok()
        .map(|n| if negative { -n } else { n })
}

// ─── Slug → path (filesystem-probing) ───────────────────────────────────────

/// The fixed leading portion of a decoded slug, plus the separator that
/// joins its segments and the still-encoded remainder.
///
/// Claude Code derives a slug from the project's absolute cwd, so the
/// leading characters tell us which platform wrote it:
///
/// | cwd                      | slug                     | shape         |
/// |--------------------------|--------------------------|---------------|
/// | `/Users/me/app`          | `-Users-me-app`          | `/`, `/`      |
/// | `D:\Projects\app`        | `D--Projects-app`        | `D:\`, `\`    |
/// | `D:\Projects\app` (old)  | `D:-Projects-app`        | `D:\`, `\`    |
///
/// A POSIX cwd is always absolute, so it always yields a leading `-`.
/// That makes "no leading dash, but starts with `<letter>--` or
/// `<letter>:-`" an unambiguous Windows drive marker. The two Windows
/// spellings both occur in the wild: current Claude Code folds the
/// colon into `-` (giving the doubled dash), older builds and the
/// Codex/Grok readers preserve it.
///
/// Detection is platform-independent on purpose — a synced `~/.claude`
/// must decode to the same path text on any host, and the CLI compares
/// these against `process.cwd()` verbatim.
struct SlugShape<'a> {
    /// Precedes the first segment: `/` on POSIX, `D:\` on Windows.
    prefix: String,
    /// Joins segments: `/` on POSIX, `\` on Windows.
    sep: char,
    /// The dash-encoded remainder, still to be split.
    rest: &'a str,
}

fn slug_shape(slug: &str) -> SlugShape<'_> {
    let bytes = slug.as_bytes();
    // `X--…` or `X:-…` — a Windows drive letter. Length 3 is the bare
    // drive root (`D--` → `D:\`), so `>=` not `>`.
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && (bytes[1] == b':' || bytes[1] == b'-')
        && bytes[2] == b'-'
    {
        let drive = (bytes[0] as char).to_ascii_uppercase();
        return SlugShape {
            prefix: format!("{drive}:\\"),
            sep: '\\',
            rest: &slug[3..],
        };
    }
    if let Some(rest) = slug.strip_prefix('-') {
        return SlugShape {
            prefix: "/".to_string(),
            sep: '/',
            rest,
        };
    }
    SlugShape {
        prefix: String::new(),
        sep: '/',
        rest: slug,
    }
}

/// Reconstruct the absolute path a project slug was derived from.
///
/// Claude Code encodes project paths into slugs by replacing every path
/// separator with `-`. Reversing that naively (every `-` → separator)
/// corrupts legitimate hyphens in directory names — e.g. a slug like
/// `-Users-me-Projects-vibe-ctl` can decode to either
/// `/Users/me/Projects/vibe/ctl` or `/Users/me/Projects/vibe-ctl`
/// depending on what actually exists on disk.
///
/// We resolve the ambiguity the same way the TS parser does: probe the
/// filesystem from left to right. At each position, try the longest
/// remaining suffix first (`vibe-ctl`, then `vibe-ctl-specs`, etc.). If
/// stat() succeeds, consume that range as a single segment and move on.
/// Fall back to the next-longest, and finally to a single segment.
///
/// For slugs whose underlying project dir no longer exists (stale
/// state, deleted project, or a Windows slug being decoded on a POSIX
/// host), the probe fails for every candidate and we degrade gracefully
/// to the naive one-segment-per-dash mapping — the display path may be
/// wrong but ingest still works.
fn slug_to_path(slug: &str) -> String {
    let SlugShape { prefix, sep, rest } = slug_shape(slug);
    if rest.is_empty() {
        return prefix;
    }

    let parts: Vec<&str> = rest.split('-').collect();
    let mut resolved = String::new();

    let mut i = 0;
    while i < parts.len() {
        let mut matched = false;
        for end in (i + 1..=parts.len()).rev() {
            let candidate_segment = parts[i..end].join("-");
            let probe_path = if resolved.is_empty() {
                format!("{prefix}{candidate_segment}")
            } else {
                format!("{prefix}{resolved}{sep}{candidate_segment}")
            };
            if std::fs::metadata(&probe_path).is_ok() {
                if !resolved.is_empty() {
                    resolved.push(sep);
                }
                resolved.push_str(&candidate_segment);
                i = end;
                matched = true;
                break;
            }
        }
        if !matched {
            if !resolved.is_empty() {
                resolved.push(sep);
            }
            resolved.push_str(parts[i]);
            i += 1;
        }
    }

    format!("{prefix}{resolved}")
}

/// Format an epoch-millisecond timestamp as an ISO 8601 string matching
/// what JS `new Date(ms).toISOString()` produces (e.g. `2026-04-17T14:36:40.342Z`).
/// Used to populate `created` / `modified` on discovered sessions so the
/// SDK's sort-by-modified-at queries work when sessions-index.json is
/// absent.
fn epoch_ms_to_iso8601(ms: f64) -> String {
    use time::format_description::well_known::{iso8601, Iso8601};

    // Clamp to the representable range; negative or absurd values just
    // fall back to the epoch, matching how JS rounds NaN → "Invalid Date".
    let nanos = (ms * 1_000_000.0) as i128;
    let dt = time::OffsetDateTime::from_unix_timestamp_nanos(nanos)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);

    // JS's toISOString() renders milliseconds (3 digits) in UTC with a
    // trailing 'Z'. `Iso8601::DEFAULT` would emit nanoseconds, so use a
    // 3-digit subsecond config to match JS byte-for-byte.
    const CFG: iso8601::EncodedConfig = iso8601::Config::DEFAULT
        .set_time_precision(iso8601::TimePrecision::Second {
            decimal_digits: std::num::NonZeroU8::new(3),
        })
        .encode();
    dt.format(&Iso8601::<CFG>)
        .unwrap_or_else(|_| "1970-01-01T00:00:00.000Z".to_string())
}

/// Read the first genuine human prompt from a JSONL session file.
///
/// Claude uses user-role envelopes for local commands, metadata, compact
/// summaries, and tool results. Those are skipped until a human text record is
/// found. Text is truncated to 200 UTF-16 code units for TS parity.
fn peek_first_user_prompt(path: &Path) -> Option<String> {
    use std::cell::RefCell;

    let found: RefCell<Option<String>> = RefCell::new(None);
    let _ = crate::core::jsonl::read_jsonl_streaming(path, 0, |line, _, _| {
        if found.borrow().is_some() {
            return;
        }
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            return;
        };
        if let Some(prompt) = session_metadata::extract_human_prompt(&val) {
            *found.borrow_mut() = Some(prompt);
        }
    });
    found.into_inner()
}

// ═══════════════════════════════════════════════════════════════════════════
// Plans — <root_dir>/plans/*.md (global, not per-project)
// ═══════════════════════════════════════════════════════════════════════════

/// First markdown heading — the TS side's `/^#\s+(.+)$/m` in `buildPlanIndex`.
static PLAN_TITLE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^#\s+(.+)$").expect("PLAN_TITLE regex compiles"));

/// Parse every plan file under `<root_dir>/plans/` — the port of
/// `ProjectParserImpl.buildPlanIndex` (project-parser.ts): slug is the
/// file stem, title the first `# ` heading (else the slug), size the
/// on-disk byte length. Unreadable / non-`.md` entries are skipped like
/// the TS per-file `try/catch`. Sorted by slug for deterministic emission.
pub(crate) fn parse_plans(root_dir: &Path) -> Vec<PlanFile> {
    let plans_dir = root_dir.join("plans");
    let mut plans: Vec<PlanFile> = Vec::new();

    let entries = match std::fs::read_dir(&plans_dir) {
        Ok(entries) => entries,
        Err(_) => return plans, // plans dir doesn't exist
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(slug) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let size = entry
            .metadata()
            .map(|m| m.len())
            .unwrap_or(content.len() as u64);
        // Trimming the carriage return is not cosmetic. JS treats CR as a
        // line terminator for `$` in multiline mode, so its `(.+)` stops
        // before it; Rust's regex crate recognises only LF, so the same
        // pattern swallows it. On a CRLF plan file the two engines therefore
        // disagreed by exactly one invisible character (RFC 008 Phase 5).
        let title = PLAN_TITLE
            .captures(&content)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().trim_end_matches('\r').to_owned())
            .unwrap_or_else(|| slug.clone());

        plans.push(PlanFile {
            slug,
            title,
            content,
            size,
        });
    }

    plans.sort_by(|a, b| a.slug.cmp(&b.slug));
    plans
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::{bounded, Receiver};
    use std::fs;
    use tempfile::{tempdir, TempDir};

    /// Fully drain the receiver into a vec. The project parser always
    /// finishes (it's single-threaded) before we inspect, so `try_iter`
    /// is sufficient.
    fn drain(rx: &Receiver<IngestEvent>) -> Vec<IngestEvent> {
        rx.try_iter().collect()
    }

    fn run_parser(root_dir: &Path, slug: &str) -> Vec<IngestEvent> {
        let (tx, rx) = bounded::<IngestEvent>(1024);
        let parser = ProjectParser::new();
        parser
            .parse_project(root_dir, slug, &tx)
            .expect("parse_project should succeed");
        drop(tx);
        drain(&rx)
    }

    /// Build `<root_dir>/projects/<slug>/` and return the project dir.
    fn mk_project(root_dir: &Path, slug: &str) -> PathBuf {
        let p = root_dir.join("projects").join(slug);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn mk_tempdir() -> TempDir {
        tempdir().expect("tempdir")
    }

    // ── 1. Empty project directory ────────────────────────────────────────

    #[test]
    fn empty_project_emits_only_project_and_complete() {
        let dir = mk_tempdir();
        mk_project(dir.path(), "proj-a");
        let events = run_parser(dir.path(), "proj-a");
        assert_eq!(events.len(), 2, "got: {events:#?}");
        assert!(matches!(events[0], IngestEvent::Project { .. }));
        assert!(matches!(events[1], IngestEvent::ProjectComplete { .. }));
    }

    // ── 2. Single session, 3 messages ─────────────────────────────────────

    fn user_line(uuid: &str) -> String {
        format!(
            r#"{{"type":"user","uuid":"{uuid}","timestamp":"2026-04-17T00:00:00Z","sessionId":"s1","cwd":"/tmp","version":"1","gitBranch":"main","isSidechain":false,"userType":"external","message":{{"role":"user","content":"hi {uuid}"}}}}"#
        )
    }

    #[test]
    fn single_session_three_messages_sequence() {
        let dir = mk_tempdir();
        let session_id = "11111111-1111-1111-1111-111111111111";
        let project_dir = mk_project(dir.path(), "proj-b");

        // sessions-index.json listing one session
        let idx = format!(
            r#"{{"version":1,"originalPath":"/orig/path","entries":[{{"sessionId":"{session_id}","fullPath":"","fileMtime":0,"firstPrompt":"hi","summary":"","messageCount":0,"created":"","modified":"","gitBranch":"","projectPath":"/orig/path","isSidechain":false}}]}}"#
        );
        fs::write(project_dir.join("sessions-index.json"), idx).unwrap();

        // Three user lines
        let body = format!(
            "{}\n{}\n{}\n",
            user_line("u1"),
            user_line("u2"),
            user_line("u3")
        );
        fs::write(project_dir.join(format!("{session_id}.jsonl")), body).unwrap();

        let events = run_parser(dir.path(), "proj-b");
        // Expected sequence: Project, Session, Message×3, SessionComplete,
        // ProjectComplete — seven total.
        assert_eq!(events.len(), 7, "got: {events:#?}");
        assert!(matches!(events[0], IngestEvent::Project { .. }));
        assert!(matches!(events[1], IngestEvent::Session { .. }));
        for (i, ev) in events.iter().enumerate().skip(2).take(3) {
            match ev {
                IngestEvent::Message {
                    session_id: sid,
                    msg_type,
                    index,
                    ..
                } => {
                    assert_eq!(sid, session_id);
                    assert_eq!(msg_type, "user");
                    assert_eq!(*index, (i - 2) as u32);
                }
                other => panic!("expected Message, got {other:?}"),
            }
        }
        assert!(matches!(events[5], IngestEvent::SessionComplete { .. }));
        assert!(matches!(events[6], IngestEvent::ProjectComplete { .. }));
    }

    // ── 3. MEMORY.md present ──────────────────────────────────────────────

    #[test]
    fn memory_md_emits_project_memory_event() {
        let dir = mk_tempdir();
        let project_dir = mk_project(dir.path(), "proj-mem");
        fs::create_dir_all(project_dir.join("memory")).unwrap();
        fs::write(
            project_dir.join("memory").join("MEMORY.md"),
            "# memory body",
        )
        .unwrap();

        let events = run_parser(dir.path(), "proj-mem");
        assert!(events.iter().any(|ev| matches!(
            ev,
            IngestEvent::ProjectMemory { content, .. } if content == "# memory body"
        )));
    }

    // ── 4. Subagent transcript ────────────────────────────────────────────

    #[test]
    fn subagent_transcript_emits_subagent_event() {
        let dir = mk_tempdir();
        let session_id = "22222222-2222-2222-2222-222222222222";
        let project_dir = mk_project(dir.path(), "proj-sub");

        // Minimal sessions-index so the parser visits this session
        let idx = format!(
            r#"{{"version":1,"entries":[{{"sessionId":"{session_id}","fullPath":"","fileMtime":0,"firstPrompt":"","summary":"","messageCount":0,"created":"","modified":"","gitBranch":"","projectPath":"","isSidechain":false}}]}}"#
        );
        fs::write(project_dir.join("sessions-index.json"), idx).unwrap();

        // Empty session file + a subagent transcript
        fs::write(project_dir.join(format!("{session_id}.jsonl")), "").unwrap();
        let subagents_dir = project_dir.join(session_id).join("subagents");
        fs::create_dir_all(&subagents_dir).unwrap();
        let transcript = user_line("sub1");
        fs::write(
            subagents_dir.join("agent-abc123.jsonl"),
            format!("{transcript}\n"),
        )
        .unwrap();

        let events = run_parser(dir.path(), "proj-sub");
        let hit = events.iter().find_map(|ev| match ev {
            IngestEvent::Subagent { transcript, .. } => Some(transcript),
            _ => None,
        });
        let transcript = hit.expect("expected Subagent event");
        // TS regex `^agent-(a.+)\.jsonl$` requires the id to start with `a`
        // and captures it verbatim — so the capture here is "abc123".
        assert_eq!(transcript.agent_id, "abc123");
        assert_eq!(transcript.agent_type, SubagentType::Task);
        assert_eq!(transcript.messages.len(), 1);
    }

    // ── 5. Todo file ──────────────────────────────────────────────────────

    #[test]
    fn todo_file_emits_todo_event() {
        let dir = mk_tempdir();
        let session_id = "33333333-3333-3333-3333-333333333333";
        let project_dir = mk_project(dir.path(), "proj-todo");

        let idx = format!(
            r#"{{"version":1,"entries":[{{"sessionId":"{session_id}","fullPath":"","fileMtime":0,"firstPrompt":"","summary":"","messageCount":0,"created":"","modified":"","gitBranch":"","projectPath":"","isSidechain":false}}]}}"#
        );
        fs::write(project_dir.join("sessions-index.json"), idx).unwrap();
        fs::write(project_dir.join(format!("{session_id}.jsonl")), "").unwrap();

        // todos live under <root_dir>/todos/<session>-agent-<agent>.json
        let todos_dir = dir.path().join("todos");
        fs::create_dir_all(&todos_dir).unwrap();
        let todo_file = todos_dir.join(format!("{session_id}-agent-xyz.json"));
        fs::write(&todo_file, r#"[{"content":"buy milk","status":"pending"}]"#).unwrap();

        let events = run_parser(dir.path(), "proj-todo");
        let todo = events.iter().find_map(|ev| match ev {
            IngestEvent::Todo { todo, .. } => Some(todo),
            _ => None,
        });
        let todo = todo.expect("expected Todo event");
        assert_eq!(todo.session_id, session_id);
        assert_eq!(todo.agent_id, "xyz");
        assert_eq!(todo.items.len(), 1);
        assert_eq!(todo.items[0].content, "buy milk");
    }

    // ── 6. Malformed JSONL line ───────────────────────────────────────────

    #[test]
    fn malformed_jsonl_line_emits_worker_error_and_skips() {
        let dir = mk_tempdir();
        let session_id = "44444444-4444-4444-4444-444444444444";
        let project_dir = mk_project(dir.path(), "proj-bad");

        let idx = format!(
            r#"{{"version":1,"entries":[{{"sessionId":"{session_id}","fullPath":"","fileMtime":0,"firstPrompt":"","summary":"","messageCount":0,"created":"","modified":"","gitBranch":"","projectPath":"","isSidechain":false}}]}}"#
        );
        fs::write(project_dir.join("sessions-index.json"), idx).unwrap();

        // One good line + one garbage line + one good line.
        let body = format!("{}\nnot-valid-json\n{}\n", user_line("a"), user_line("b"));
        fs::write(project_dir.join(format!("{session_id}.jsonl")), body).unwrap();

        let events = run_parser(dir.path(), "proj-bad");
        let msgs: Vec<_> = events
            .iter()
            .filter(|ev| matches!(ev, IngestEvent::Message { .. }))
            .collect();
        assert_eq!(msgs.len(), 2, "bad line should be skipped");
        assert!(
            events
                .iter()
                .any(|ev| matches!(ev, IngestEvent::RecordSkip { .. })),
            "bad line should emit RecordSkip"
        );
        assert!(
            events
                .iter()
                .any(|ev| matches!(ev, IngestEvent::ProjectComplete { .. })),
            "a skipped record must still let its project complete"
        );
    }

    // ── 7. Assistant message with usage → token extraction ────────────────

    #[test]
    fn assistant_usage_block_extracts_tokens() {
        let dir = mk_tempdir();
        let session_id = "55555555-5555-5555-5555-555555555555";
        let project_dir = mk_project(dir.path(), "proj-tokens");

        let idx = format!(
            r#"{{"version":1,"entries":[{{"sessionId":"{session_id}","fullPath":"","fileMtime":0,"firstPrompt":"","summary":"","messageCount":0,"created":"","modified":"","gitBranch":"","projectPath":"","isSidechain":false}}]}}"#
        );
        fs::write(project_dir.join("sessions-index.json"), idx).unwrap();

        let assistant = r#"{"type":"assistant","uuid":"a1","timestamp":"2026-04-17T00:00:00Z","sessionId":"s1","cwd":"/tmp","version":"1","gitBranch":"main","isSidechain":false,"userType":"external","requestId":"r1","message":{"model":"claude","id":"m1","type":"message","role":"assistant","content":[{"type":"text","text":"hey"}],"usage":{"input_tokens":11,"output_tokens":22,"cache_creation_input_tokens":33,"cache_read_input_tokens":44}}}"#;
        fs::write(
            project_dir.join(format!("{session_id}.jsonl")),
            format!("{assistant}\n"),
        )
        .unwrap();

        let events = run_parser(dir.path(), "proj-tokens");
        let msg = events
            .iter()
            .find_map(|ev| match ev {
                IngestEvent::Message {
                    msg_type,
                    input_tokens,
                    output_tokens,
                    cache_creation_tokens,
                    cache_read_tokens,
                    ..
                } if msg_type == "assistant" => Some((
                    *input_tokens,
                    *output_tokens,
                    *cache_creation_tokens,
                    *cache_read_tokens,
                )),
                _ => None,
            })
            .expect("assistant message event");
        assert_eq!(msg, (11, 22, 33, 44));
    }

    // ── 8. fts_text populated on a plain-text user message ────────────────

    #[test]
    fn user_message_populates_fts_text() {
        let dir = mk_tempdir();
        let session_id = "66666666-6666-6666-6666-666666666666";
        let project_dir = mk_project(dir.path(), "proj-fts");

        let idx = format!(
            r#"{{"version":1,"entries":[{{"sessionId":"{session_id}","fullPath":"","fileMtime":0,"firstPrompt":"","summary":"","messageCount":0,"created":"","modified":"","gitBranch":"","projectPath":"","isSidechain":false}}]}}"#
        );
        fs::write(project_dir.join("sessions-index.json"), idx).unwrap();
        fs::write(
            project_dir.join(format!("{session_id}.jsonl")),
            format!("{}\n", user_line("u1")),
        )
        .unwrap();

        let events = run_parser(dir.path(), "proj-fts");
        let fts = events.iter().find_map(|ev| match ev {
            IngestEvent::Message { fts_text, .. } => fts_text.clone(),
            _ => None,
        });
        assert_eq!(fts.as_deref(), Some("hi u1"));
    }

    // ── 9. Merged sessions-index serialization (item 6) ───────────────────

    #[test]
    fn no_sessions_index_stores_merged_discovered_entries() {
        // A project with a session JSONL but NO sessions-index.json must store
        // a MERGED index containing the discovered entry — not "{}" (which is
        // what storing the raw missing-file fallback would produce).
        let dir = mk_tempdir();
        let session_id = "99999999-9999-9999-9999-999999999999";
        let project_dir = mk_project(dir.path(), "proj-merge");
        fs::write(
            project_dir.join(format!("{session_id}.jsonl")),
            format!("{}\n", user_line("m1")),
        )
        .unwrap();

        let events = run_parser(dir.path(), "proj-merge");
        let json = events
            .iter()
            .find_map(|ev| match ev {
                IngestEvent::Project {
                    sessions_index_json,
                    ..
                } => Some(sessions_index_json.clone()),
                _ => None,
            })
            .expect("Project event");

        assert_ne!(json, "{}", "merged index must not be the empty fallback");
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let entries = parsed
            .get("entries")
            .and_then(|v| v.as_array())
            .expect("entries array");
        assert_eq!(entries.len(), 1, "discovered session must be in the index");
        assert_eq!(
            entries[0].get("sessionId").and_then(|v| v.as_str()),
            Some(session_id)
        );
        // The discovered entry carries the peeked first prompt, not blank.
        assert_eq!(
            entries[0].get("firstPrompt").and_then(|v| v.as_str()),
            Some("hi m1")
        );
    }

    // ── 10. first-prompt peek parity (item 8) ─────────────────────────────

    #[test]
    fn peek_first_prompt_skips_non_prompt_user_records() {
        // Claude uses user-role rows for tool results and metadata. Neither
        // should prevent a later genuine human prompt from naming the session.
        let dir = mk_tempdir();
        let path = dir.path().join("s.jsonl");
        let body = concat!(
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"<local-command-caveat>ignore</local-command-caveat>"}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"done"}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"later text"}}"#,
            "\n",
        );
        fs::write(&path, body).unwrap();
        assert_eq!(peek_first_user_prompt(&path).as_deref(), Some("later text"));
    }

    #[test]
    fn peek_first_prompt_truncates_at_200_utf16_units() {
        let dir = mk_tempdir();
        let path = dir.path().join("s.jsonl");
        let long = "z".repeat(500);
        let body = format!(r#"{{"type":"user","message":{{"role":"user","content":"{long}"}}}}"#);
        fs::write(&path, format!("{body}\n")).unwrap();
        let out = peek_first_user_prompt(&path).expect("first prompt");
        assert_eq!(out.chars().count(), 200);
    }

    // ── 11. highwatermark leniency (item 9) ───────────────────────────────

    #[test]
    fn parse_leading_int_matches_parseint_semantics() {
        assert_eq!(parse_leading_int("12abc"), Some(12));
        assert_eq!(parse_leading_int("  42  "), Some(42));
        assert_eq!(parse_leading_int("-5x"), Some(-5));
        assert_eq!(parse_leading_int("+7"), Some(7));
        assert_eq!(parse_leading_int("abc"), None);
        assert_eq!(parse_leading_int(""), None);
        assert_eq!(parse_leading_int("  -"), None);
    }

    #[test]
    fn read_task_highwatermark_is_lenient() {
        let dir = mk_tempdir();
        let session_id = "77777777-7777-7777-7777-777777777777";
        let task_dir = dir.path().join("tasks").join(session_id);
        fs::create_dir_all(&task_dir).unwrap();
        fs::write(task_dir.join(".lock"), "").unwrap();
        fs::write(task_dir.join(".highwatermark"), "12abc\n").unwrap();
        let task = read_task(dir.path(), session_id).expect("task");
        assert!(task.has_highwatermark);
        assert_eq!(task.highwatermark, Some(12));
    }

    #[test]
    fn read_task_highwatermark_non_numeric_is_none() {
        let dir = mk_tempdir();
        let session_id = "88888888-8888-8888-8888-888888888888";
        let task_dir = dir.path().join("tasks").join(session_id);
        fs::create_dir_all(&task_dir).unwrap();
        fs::write(task_dir.join(".lock"), "").unwrap();
        fs::write(task_dir.join(".highwatermark"), "not-a-number").unwrap();
        let task = read_task(dir.path(), session_id).expect("task");
        assert!(task.has_highwatermark);
        assert_eq!(task.highwatermark, None);
    }

    // ── 12. api-error assistant kept in subagent transcript (item 1) ──────

    #[test]
    fn subagent_keeps_assistant_line_without_usage_or_request_id() {
        // An assistant transcript line without `requestId` / `message.usage`
        // (API-error line) previously failed the typed parse and was dropped
        // from the subagent's `messages`, undercounting message_count. It must
        // now be retained.
        let dir = mk_tempdir();
        let session_id = "abababab-1111-2222-3333-444444444444";
        let project_dir = mk_project(dir.path(), "proj-suberr");
        let idx = format!(
            r#"{{"version":1,"entries":[{{"sessionId":"{session_id}","fullPath":"","fileMtime":0,"firstPrompt":"","summary":"","messageCount":0,"created":"","modified":"","gitBranch":"","projectPath":"","isSidechain":false}}]}}"#
        );
        fs::write(project_dir.join("sessions-index.json"), idx).unwrap();
        fs::write(project_dir.join(format!("{session_id}.jsonl")), "").unwrap();

        let subagents_dir = project_dir.join(session_id).join("subagents");
        fs::create_dir_all(&subagents_dir).unwrap();
        let assistant = r#"{"type":"assistant","uuid":"a1","timestamp":"2026-04-17T00:00:00Z","sessionId":"s","cwd":"/","version":"1","gitBranch":"main","isSidechain":true,"userType":"external","isApiErrorMessage":true,"message":{"model":"m","id":"m1","type":"message","role":"assistant","content":[{"type":"text","text":"overloaded"}]}}"#;
        fs::write(
            subagents_dir.join("agent-abc.jsonl"),
            format!("{assistant}\n"),
        )
        .unwrap();

        let events = run_parser(dir.path(), "proj-suberr");
        let transcript = events
            .iter()
            .find_map(|ev| match ev {
                IngestEvent::Subagent { transcript, .. } => Some(transcript),
                _ => None,
            })
            .expect("subagent event");
        assert_eq!(
            transcript.messages.len(),
            1,
            "api-error assistant line must not be dropped from the transcript"
        );
    }

    // ── Plans ─────────────────────────────────────────────────────────────

    #[test]
    fn parse_plans_reads_title_content_size_and_skips_junk() {
        let dir = mk_tempdir();
        let plans_dir = dir.path().join("plans");
        fs::create_dir_all(&plans_dir).unwrap();

        fs::write(
            plans_dir.join("zesty-plan.md"),
            "# Zesty Title\n\nBody text.\n",
        )
        .unwrap();
        fs::write(plans_dir.join("headless-plan.md"), "no heading here\n").unwrap();
        fs::write(plans_dir.join("notes.txt"), "not a plan").unwrap();
        fs::write(plans_dir.join(".DS_Store"), "junk").unwrap();

        let plans = parse_plans(dir.path());

        assert_eq!(plans.len(), 2);
        // Sorted by slug for deterministic emission.
        assert_eq!(plans[0].slug, "headless-plan");
        assert_eq!(plans[0].title, "headless-plan"); // falls back to slug
        assert_eq!(plans[1].slug, "zesty-plan");
        assert_eq!(plans[1].title, "Zesty Title");
        assert_eq!(plans[1].content, "# Zesty Title\n\nBody text.\n");
        assert_eq!(plans[1].size, plans[1].content.len() as u64);
    }

    #[test]
    fn parse_plans_missing_dir_is_empty() {
        let dir = mk_tempdir();
        assert!(parse_plans(dir.path()).is_empty());
    }

    // ─── slug → path ────────────────────────────────────────────────────

    /// These assert the *naive* decode (no probe hits), which is what
    /// runs whenever the encoded directory no longer exists on this host
    /// — including every Windows slug decoded on a POSIX CI runner. The
    /// probing behaviour is covered separately below.
    #[test]
    fn slug_to_path_decodes_posix_slugs() {
        assert_eq!(
            slug_to_path("-Users-me-Projects-app"),
            "/Users/me/Projects/app"
        );
        assert_eq!(slug_to_path("-"), "/");
        assert_eq!(slug_to_path(""), "");
    }

    #[test]
    fn slug_to_path_decodes_windows_drive_slugs() {
        // Current Claude Code folds the colon into a dash, so `D:\` → `D--`.
        assert_eq!(
            slug_to_path("D--Projects-p100-spaghetti"),
            "D:\\Projects\\p100\\spaghetti"
        );
        assert_eq!(slug_to_path("C--Users-me"), "C:\\Users\\me");
        // Older builds (and the Codex/Grok readers) keep the colon.
        assert_eq!(slug_to_path("D:-I3T-WordplayAR"), "D:\\I3T\\WordplayAR");
        // Bare drive root.
        assert_eq!(slug_to_path("D--"), "D:\\");
        assert_eq!(slug_to_path("D:-"), "D:\\");
        // Drive letters normalize to uppercase so the two spellings of the
        // same project agree.
        assert_eq!(slug_to_path("d--Projects-app"), "D:\\Projects\\app");
    }

    /// A leading dash always wins: a POSIX directory that merely *starts*
    /// with something drive-shaped must not be read as a Windows path.
    #[test]
    fn slug_to_path_prefers_posix_when_leading_dash_present() {
        assert_eq!(slug_to_path("-d--Projects-app"), "/d//Projects/app");
        assert_eq!(slug_to_path("-home-d--foo"), "/home/d//foo");
    }

    /// The filesystem probe is what disambiguates hyphens that belong to a
    /// directory name from hyphens that encode a separator. Build a real
    /// tree and confirm the longest-match-first probe consumes `p100-app`
    /// as one segment rather than splitting it.
    #[test]
    fn slug_to_path_probes_filesystem_to_keep_hyphenated_dirs() {
        let dir = mk_tempdir();
        let nested = dir.path().join("p100-app").join("sub");
        fs::create_dir_all(&nested).unwrap();

        // Re-encode the real temp dir the way Claude Code would, then
        // decode it back and expect the original path.
        let root = dir.path().to_str().unwrap();
        let slug = format!("{root}/p100-app/sub").replace(['/', '\\'], "-");
        // On Windows the temp root carries a drive letter, so re-encoding
        // yields the `C:-…` spelling; both shapes are handled.
        assert_eq!(slug_to_path(&slug), nested.to_str().unwrap());
    }
}
