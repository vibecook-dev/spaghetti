//! CodexReader — walk `sessions/**/rollout-*.jsonl` → [`IngestEvent`] stream.
//!
//! Mirrors `packages/sdk/src/sources/codex/reader.ts` + token attribution
//! from `ingest-service.ts` (ccusage-style last_token_usage onto previous
//! assistant). Tiktoken estimate for missing token_count is TS-only for now.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crossbeam_channel::{SendError, Sender};
use once_cell::sync::Lazy;
use regex::Regex;

use crate::claude::fingerprint::DiscoveryError;
use crate::claude::types::{SessionIndexEntry, SessionsIndex};
use crate::codex::message_extractor::{self, MessageProjection};
use crate::core::event::IngestEvent;
use crate::core::jsonl::read_jsonl_streaming;

static ROLLOUT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^rollout-.*\.jsonl$").expect("rollout regex"));
static UUID_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").expect("uuid")
});

const FIRST_PROMPT_MAX: usize = 200;
const PEEK_LINE_LIMIT: u32 = 100;

/// True when a Codex user-turn body is injected scaffolding, not the human prompt.
/// Mirrors `packages/sdk/src/sources/codex/first-prompt.ts`.
fn is_injected_user_text(text: &str) -> bool {
    let t = text.trim_start();
    if t.is_empty() {
        return true;
    }
    if t.starts_with("<environment_context>")
        || t.starts_with("<recommended_plugins>")
        || t.starts_with("<permissions instructions>")
        || t.starts_with("<collaboration_mode>")
        || t.starts_with("<skills_instructions>")
        || t.starts_with("<apps_instructions>")
        || t.starts_with("<plugins_instructions>")
        || t.starts_with("<multi_agent_mode>")
        || t.starts_with("<INSTRUCTIONS>")
        || t.starts_with("# AGENTS.md instructions")
        || t.starts_with(
            "The following is the Codex agent history whose request action you are assessing.",
        )
        || t.starts_with(
            "The following is the Codex agent history added since your last approval assessment.",
        )
    {
        return true;
    }
    if t.starts_with('<') && (t.contains("</cwd>") || t.contains("<shell>")) && t.contains("<cwd>")
    {
        return true;
    }
    false
}

/// Prefer `event_msg/user_message`, else first non-injected user response_item.
fn consider_first_prompt(current: &str, line: &str) -> Option<String> {
    if !current.is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    if ty == "event_msg" {
        let payload = v.get("payload")?;
        if payload.get("type").and_then(|t| t.as_str()) != Some("user_message") {
            return None;
        }
        let msg = payload.get("message").and_then(|m| m.as_str())?;
        if msg.trim().is_empty() || is_injected_user_text(msg) {
            return None;
        }
        return Some(crate::core::text::truncate_utf16(msg, FIRST_PROMPT_MAX).to_owned());
    }
    if ty == "response_item" {
        if let Ok(Some(proj)) = message_extractor::project_jsonl_line(line) {
            if proj.msg_type == "user" {
                if let Some(t) = proj.fts_text.as_deref() {
                    if !is_injected_user_text(t) {
                        return Some(
                            crate::core::text::truncate_utf16(t, FIRST_PROMPT_MAX).to_owned(),
                        );
                    }
                }
            }
        }
    }
    None
}

#[derive(Debug, thiserror::Error)]
pub enum CodexReadError {
    #[error("event channel closed")]
    ChannelClosed(#[source] Box<SendError<IngestEvent>>),
}

impl From<SendError<IngestEvent>> for CodexReadError {
    fn from(e: SendError<IngestEvent>) -> Self {
        Self::ChannelClosed(Box::new(e))
    }
}

#[derive(Debug, Clone)]
struct PeekMeta {
    cwd: String,
    session_id: String,
    timestamp: Option<String>,
    first_prompt: String,
}

/// Encode cwd → project slug (Claude-compatible `/` → `-`).
/// Windows cwds separate with `\\`, so both separators are folded
/// (parity with the TS readers).
pub fn encode_slug(cwd: &str) -> String {
    cwd.replace(['/', '\\'], "-")
}

struct SessionFile {
    path: PathBuf,
    meta: PeekMeta,
    mtime_ms: f64,
    size: u64,
}

/// Discover + stream Codex rollouts into the ingest event channel.
pub struct CodexReader;

impl CodexReader {
    /// Full cold-style read of every rollout under `sessions_dir`.
    ///
    /// Emits Project / Session / Message* / SessionComplete / ProjectComplete
    /// plus Fingerprint events for each file (caller should ClearSourceFiles
    /// first for a clean codex-scoped fingerprint set).
    pub fn read_all(
        sessions_dir: &Path,
        events: &Sender<IngestEvent>,
    ) -> Result<CodexReadStats, CodexReadError> {
        let (files, discovery_errors) = discover_root_sessions(sessions_dir);
        for err in discovery_errors {
            // No project identity yet — this failed while working out which
            // sessions exist. Poisons nothing, but withholds the marker.
            let _ = events.send(IngestEvent::SourceError {
                path: err.path,
                message: err.message,
            });
        }
        let mut by_project: BTreeMap<String, (String, Vec<SessionFile>)> = BTreeMap::new();

        for path in files {
            let meta = match peek(&path) {
                Ok(Some(m)) => m,
                // Readable but not attributable — nothing to report.
                Ok(None) => continue,
                Err(message) => {
                    let _ = events.send(IngestEvent::SourceError {
                        path: path.to_string_lossy().into_owned(),
                        message,
                    });
                    continue;
                }
            };
            let slug = encode_slug(&meta.cwd);
            let (mtime_ms, size) = file_stats(&path);
            by_project
                .entry(slug)
                .or_insert_with(|| (meta.cwd.clone(), Vec::new()))
                .1
                .push(SessionFile {
                    path,
                    meta,
                    mtime_ms,
                    size,
                });
        }

        let mut stats = CodexReadStats {
            projects: by_project.len() as u32,
            ..Default::default()
        };

        for (slug, (original_path, sessions)) in by_project {
            let entries: Vec<SessionIndexEntry> = sessions.iter().map(session_entry).collect();
            let sessions_index = SessionsIndex {
                version: 1,
                original_path: Some(original_path.clone()),
                entries: entries.clone(),
            };
            let sessions_index_json =
                serde_json::to_string(&sessions_index).unwrap_or_else(|_| "{}".into());

            events.send(IngestEvent::Project {
                slug: slug.clone(),
                original_path,
                sessions_index_json,
            })?;

            for (i, sess) in sessions.iter().enumerate() {
                events.send(IngestEvent::Session {
                    slug: slug.clone(),
                    entry: entries[i].clone(),
                })?;
                let (msg_count, last_byte) = stream_session(&slug, sess, events)?;
                stats.sessions += 1;
                stats.messages += msg_count;

                events.send(IngestEvent::SessionComplete {
                    slug: slug.clone(),
                    session_id: sess.meta.session_id.clone(),
                    message_count: msg_count,
                    last_byte_position: last_byte,
                })?;

                events.send(IngestEvent::Fingerprint {
                    path: sess.path.to_string_lossy().into_owned(),
                    mtime_ms: sess.mtime_ms,
                    size: sess.size,
                    byte_position: Some(last_byte),
                    category: "session".into(),
                    project_slug: Some(slug.clone()),
                    session_id: Some(sess.meta.session_id.clone()),
                })?;
            }

            events.send(IngestEvent::ProjectComplete {
                slug,
                duration_ms: 0,
            })?;
        }

        Ok(stats)
    }

    /// Warm-start: true when every known rollout is unchanged vs `stored`
    /// fingerprints (path → mtime) and there are no new/deleted rollouts.
    pub fn warm_unchanged(
        sessions_dir: &Path,
        stored: &std::collections::HashMap<String, crate::claude::fingerprint::SourceFingerprint>,
    ) -> bool {
        let (files, discovery_errors) = discover_root_sessions(sessions_dir);
        if !discovery_errors.is_empty() {
            // A directory we could not read may hold changes we cannot see.
            return false;
        }
        if files.is_empty() && stored.is_empty() {
            return true;
        }
        let mut seen = std::collections::HashSet::new();
        for path in &files {
            let key = path.to_string_lossy().into_owned();
            seen.insert(key.clone());
            let (mtime_ms, size) = file_stats(path);
            match stored.get(&key) {
                None => return false,
                Some(fp) if (fp.mtime_ms - mtime_ms).abs() > 0.5 || fp.size != size => {
                    return false
                }
                Some(_) => {}
            }
        }
        // Any stored fingerprint under sessions_dir missing on disk?
        let prefix = sessions_dir.to_string_lossy();
        for path in stored.keys() {
            if path.starts_with(prefix.as_ref()) && !seen.contains(path) {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct CodexReadStats {
    pub projects: u32,
    pub sessions: u32,
    pub messages: u32,
}

fn discover(sessions_dir: &Path) -> (Vec<PathBuf>, Vec<DiscoveryError>) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    // Absence is not a failure: a machine that never ran this agent has no
    // sessions directory.
    if !sessions_dir.is_dir() {
        return (out, errors);
    }
    let walker = walkdir::WalkDir::new(sessions_dir).follow_links(false);
    for entry in walker {
        // A descent that failed is not "no files here". Swallowing it made an
        // unreadable directory look like an empty one, so its sessions were
        // silently dropped from the index (RFC 008 Phase 2C).
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                let at = e.path().unwrap_or(sessions_dir).to_path_buf();
                errors.push(DiscoveryError {
                    path: at.to_string_lossy().into_owned(),
                    message: e.to_string(),
                });
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if ROLLOUT_RE.is_match(&name) {
            out.push(entry.path().to_path_buf());
        }
    }
    out.sort();
    (out, errors)
}

/// Return only human/root rollouts. `session_meta` is the first line in Codex
/// files, so this avoids scanning large guardian rollouts merely to reject
/// them and keeps warm fingerprints scoped to files that can affect the UI.
fn discover_root_sessions(sessions_dir: &Path) -> (Vec<PathBuf>, Vec<DiscoveryError>) {
    let (files, errors) = discover(sessions_dir);
    (
        files
            .into_iter()
            .filter(|path| !is_internal_rollout(path))
            .collect(),
        errors,
    )
}

fn is_internal_rollout(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut line = String::new();
    if BufReader::new(file).read_line(&mut line).is_err() {
        return false;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
        return false;
    };
    if value.get("type").and_then(|v| v.as_str()) != Some("session_meta") {
        return false;
    }
    let Some(payload) = value.get("payload") else {
        return false;
    };

    if payload.get("thread_source").and_then(|v| v.as_str()) == Some("subagent") {
        return true;
    }
    if payload
        .get("source")
        .and_then(|v| v.as_object())
        .is_some_and(|source| source.contains_key("subagent"))
    {
        return true;
    }

    let id = payload.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let logical_id = payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let has_parent = payload
        .get("parent_thread_id")
        .and_then(|v| v.as_str())
        .is_some_and(|parent| !parent.is_empty());
    has_parent && !id.is_empty() && !logical_id.is_empty() && logical_id != id
}

/// Read just enough of a rollout to learn which project it belongs to.
///
/// `Ok(None)` means the file was readable but carries no `cwd` — a truncated
/// or not-yet-started rollout, which is normal and not worth reporting.
/// `Err` means the file could not be read at all, which is a pre-identity
/// failure: there is no slug to attribute it to, and silently skipping it
/// dropped the session from the index with nothing said (RFC 008 Phase 2).
fn peek(path: &Path) -> Result<Option<PeekMeta>, String> {
    let mut cwd: Option<String> = None;
    let mut session_id: Option<String> = None;
    let mut timestamp: Option<String> = None;
    let mut first_prompt = String::new();

    let read = read_jsonl_streaming(path, 0, |line, index, _| {
        // Keep scanning until we have cwd + a real (non-injected) prompt, or hit the cap.
        if index >= PEEK_LINE_LIMIT || (cwd.is_some() && !first_prompt.is_empty()) {
            return;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if ty == "session_meta" {
                if let Some(p) = v.get("payload") {
                    if cwd.is_none() {
                        if let Some(c) = p.get("cwd").and_then(|x| x.as_str()) {
                            cwd = Some(c.to_owned());
                        }
                    }
                    if session_id.is_none() {
                        if let Some(id) = p.get("id").and_then(|x| x.as_str()) {
                            session_id = Some(id.to_owned());
                        }
                    }
                }
                if timestamp.is_none() {
                    if let Some(ts) = v.get("timestamp").and_then(|x| x.as_str()) {
                        timestamp = Some(ts.to_owned());
                    }
                }
            } else if let Some(p) = consider_first_prompt(&first_prompt, line) {
                first_prompt = p;
            }
        }
    });

    if let Err(e) = read {
        return Err(e.to_string());
    }

    let Some(cwd) = cwd else {
        return Ok(None);
    };
    let session_id = session_id
        .or_else(|| {
            let name = path.file_name()?.to_str()?;
            UUID_RE.find(name).map(|m| m.as_str().to_owned())
        })
        .unwrap_or_else(|| path.file_name().unwrap().to_string_lossy().into_owned());

    Ok(Some(PeekMeta {
        cwd,
        session_id,
        timestamp,
        first_prompt,
    }))
}

fn session_entry(s: &SessionFile) -> SessionIndexEntry {
    let modified = if s.mtime_ms > 0.0 {
        ms_to_iso(s.mtime_ms)
    } else {
        s.meta.timestamp.clone().unwrap_or_default()
    };
    SessionIndexEntry {
        session_id: s.meta.session_id.clone(),
        full_path: s.path.to_string_lossy().into_owned(),
        file_mtime: s.mtime_ms,
        first_prompt: if s.meta.first_prompt.is_empty() {
            "No prompt".into()
        } else {
            s.meta.first_prompt.clone()
        },
        summary: String::new(),
        message_count: 0,
        created: s.meta.timestamp.clone().unwrap_or_else(|| modified.clone()),
        modified,
        git_branch: String::new(),
        project_path: s.meta.cwd.clone(),
        is_sidechain: false,
    }
}

fn stream_session(
    slug: &str,
    sess: &SessionFile,
    events: &Sender<IngestEvent>,
) -> Result<(u32, u64), CodexReadError> {
    let session_id = &sess.meta.session_id;
    let mut message_count: u32 = 0;
    let mut last_byte: u64 = 0;
    // Absolute line index in file (including skipped lines) — matches TS CodexReader
    let mut line_index: u32 = 0;
    // Last assistant message for token attribution: (index, byte_offset, raw, proj).
    let mut last_assistant: Option<(u32, u64, String, MessageProjection)> = None;
    let mut send_err: Option<SendError<IngestEvent>> = None;

    let stream = read_jsonl_streaming(&sess.path, 0, |line, _idx, byte_offset| {
        if send_err.is_some() {
            return;
        }
        last_byte = byte_offset;
        let idx = line_index;
        line_index = line_index.saturating_add(1);

        match message_extractor::project_jsonl_line(line) {
            Ok(Some(proj)) => {
                let ev = IngestEvent::Message {
                    slug: slug.to_owned(),
                    session_id: session_id.clone(),
                    index: idx,
                    byte_offset,
                    raw_json: line.to_owned(),
                    msg_type: proj.msg_type.clone(),
                    uuid: proj.uuid.clone(),
                    timestamp: proj.timestamp.clone(),
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_tokens: 0,
                    cache_read_tokens: 0,
                    fts_text: proj.fts_text.clone(),
                };
                if proj.msg_type == "assistant" {
                    last_assistant = Some((idx, byte_offset, line.to_owned(), proj));
                }
                if let Err(e) = events.send(ev) {
                    send_err = Some(e);
                    return;
                }
                message_count = message_count.saturating_add(1);
            }
            Ok(None) => {
                // token_count attribution onto the preceding assistant.
                if let Some(tc) = message_extractor::parse_token_count(line) {
                    // Re-emit the assistant carrying its ORIGINAL byte offset
                    // (not 0 — a hardcoded 0 clobbered the stored offset on the
                    // UPSERT, breaking incremental resume for that row).
                    let mut clear_pointer = false;
                    if let Some((a_idx, a_byte, raw, proj)) = last_assistant.as_ref() {
                        let ev = IngestEvent::Message {
                            slug: slug.to_owned(),
                            session_id: session_id.clone(),
                            index: *a_idx,
                            byte_offset: *a_byte,
                            raw_json: raw.clone(),
                            msg_type: proj.msg_type.clone(),
                            uuid: proj.uuid.clone(),
                            timestamp: proj.timestamp.clone(),
                            input_tokens: tc.input,
                            output_tokens: tc.output,
                            cache_creation_tokens: tc.cache_creation,
                            cache_read_tokens: tc.cache_read,
                            fts_text: proj.fts_text.clone(),
                        };
                        if let Err(e) = events.send(ev) {
                            send_err = Some(e);
                            return;
                        }
                        // Total-only count (no per-turn last_token_usage): clear
                        // the pointer so a subsequent total-only count isn't
                        // re-applied to this same assistant (mirrors the TS
                        // onSkippedRecord guard in codex ingest-hooks).
                        if !tc.from_last {
                            clear_pointer = true;
                        }
                    }
                    if clear_pointer {
                        last_assistant = None;
                    }
                }
            }
            Err(_) => {
                // bad JSON line — skip (TS swallows)
            }
        }
    });

    if let Some(e) = send_err {
        return Err(e.into());
    }
    if let Ok(r) = stream {
        last_byte = r.final_byte_position.max(last_byte);
    }
    Ok((message_count, last_byte))
}

fn file_stats(path: &Path) -> (f64, u64) {
    match std::fs::metadata(path) {
        Ok(m) => {
            let mtime_ms = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64() * 1000.0)
                .unwrap_or(0.0);
            (mtime_ms, m.len())
        }
        Err(_) => (0.0, 0),
    }
}

/// Epoch-ms → ISO 8601, matching JS `new Date(ms).toISOString()` (3-digit ms
/// fraction + trailing `Z`). Shared with the Claude/Grok readers so all three
/// engines format session timestamps identically — `time`'s `Rfc3339` trims
/// trailing fractional zeros, which JS never does.
fn ms_to_iso(mtime_ms: f64) -> String {
    crate::core::timefmt::epoch_ms_to_iso8601(mtime_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn encode_slug_replaces_slashes() {
        assert_eq!(encode_slug("/Users/me/proj"), "-Users-me-proj");
        assert_eq!(encode_slug(r"C:\Users\me\proj"), "C:-Users-me-proj");
    }

    #[test]
    fn first_prompt_skips_injected_environment_context() {
        // Real Codex rollouts open with environment_context / AGENTS.md user
        // turns before the human prompt; peek must not treat those as the title.
        let tmp = TempDir::new().unwrap();
        let day = tmp.path().join("sessions/2026/01/01");
        std::fs::create_dir_all(&day).unwrap();
        let file = day.join("rollout-2026-01-01T00-00-00-019bbbbbbbbbbbbbbbbbbbbbbbb.jsonl");
        let mut f = std::fs::File::create(&file).unwrap();
        writeln!(
            f,
            r#"{{"timestamp":"2026-01-01T00:00:00.000Z","type":"session_meta","payload":{{"id":"sess-fp","cwd":"/tmp/demo"}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"<environment_context>\n  <cwd>/tmp/demo</cwd>\n  <shell>zsh</shell>\n</environment_context>"}}]}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r##"{{"type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"# AGENTS.md instructions for /tmp/demo\n\nDo stuff"}}]}}}}"##
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"do a full code audit of this project"}}]}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"event_msg","payload":{{"type":"user_message","message":"do a full code audit of this project","images":[],"local_images":[]}}}}"#
        )
        .unwrap();

        let meta = peek(&file).expect("readable").expect("attributable");
        assert_eq!(meta.first_prompt, "do a full code audit of this project");
    }

    #[test]
    fn first_prompt_prefers_event_msg_user_message() {
        let tmp = TempDir::new().unwrap();
        let day = tmp.path().join("sessions/2026/01/01");
        std::fs::create_dir_all(&day).unwrap();
        let file = day.join("rollout-2026-01-01T00-00-00-019dddddddddddddddddddddddd.jsonl");
        let mut f = std::fs::File::create(&file).unwrap();
        writeln!(
            f,
            r#"{{"type":"session_meta","payload":{{"id":"sess-ev","cwd":"/tmp/x"}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"<environment_context><cwd>/tmp/x</cwd></environment_context>"}}]}}}}"#
        )
        .unwrap();
        // No non-injected response_item user — only event_msg carries the human text.
        writeln!(
            f,
            r#"{{"type":"event_msg","payload":{{"type":"user_message","message":"help me find iframe code"}}}}"#
        )
        .unwrap();

        let meta = peek(&file).expect("readable").expect("attributable");
        assert_eq!(meta.first_prompt, "help me find iframe code");
    }

    #[test]
    fn read_all_skips_internal_subagent_rollouts() {
        let tmp = TempDir::new().unwrap();
        let day = tmp.path().join("sessions/2026/01/01");
        std::fs::create_dir_all(&day).unwrap();

        let root = day.join("rollout-2026-01-01T00-00-00-019aaaaaaaaaaaaaaaaaaaaaaaa.jsonl");
        std::fs::write(
            root,
            r#"{"timestamp":"2026-01-01T00:00:00.000Z","type":"session_meta","payload":{"id":"root-session","session_id":"root-session","thread_source":"user","source":"cli","cwd":"/tmp/demo"}}
{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"real prompt"}]}}
"#,
        )
        .unwrap();

        let child = day.join("rollout-2026-01-01T00-00-00-019bbbbbbbbbbbbbbbbbbbbbbbb.jsonl");
        std::fs::write(
            child,
            r#"{"timestamp":"2026-01-01T00:00:00.000Z","type":"session_meta","payload":{"id":"guardian-session","session_id":"root-session","parent_thread_id":"root-session","thread_source":"subagent","source":{"subagent":{"other":"guardian"}},"cwd":"/tmp/demo"}}
{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"The following is the Codex agent history whose request action you are assessing."}]}}
"#,
        )
        .unwrap();

        let (tx, rx) = unbounded();
        let stats = CodexReader::read_all(tmp.path().join("sessions").as_path(), &tx).unwrap();
        drop(tx);
        let events: Vec<_> = rx.iter().collect();

        assert_eq!(stats.sessions, 1);
        assert!(events.iter().all(|event| match event {
            IngestEvent::Session { entry, .. } => entry.session_id != "guardian-session",
            IngestEvent::Message { session_id, .. } => session_id != "guardian-session",
            _ => true,
        }));
    }

    #[test]
    fn read_all_emits_project_session_messages() {
        let tmp = TempDir::new().unwrap();
        let day = tmp.path().join("sessions/2026/01/01");
        std::fs::create_dir_all(&day).unwrap();
        let file = day.join("rollout-2026-01-01T00-00-00-019aaaaaaaaaaaaaaaaaaaaaaaa.jsonl");
        let mut f = std::fs::File::create(&file).unwrap();
        writeln!(
            f,
            r#"{{"timestamp":"2026-01-01T00:00:00.000Z","type":"session_meta","payload":{{"id":"sess-1","cwd":"/tmp/demo"}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"timestamp":"2026-01-01T00:00:01.000Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"hi"}}]}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"timestamp":"2026-01-01T00:00:02.000Z","type":"response_item","payload":{{"type":"message","role":"assistant","id":"a1","content":[{{"type":"output_text","text":"hello"}}]}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"event_msg","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":3,"output_tokens":7,"cached_input_tokens":0,"reasoning_output_tokens":0,"total_tokens":10}}}}}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"response_item","payload":{{"type":"function_call","name":"shell"}}}}"#
        )
        .unwrap();

        let (tx, rx) = unbounded();
        let stats = CodexReader::read_all(tmp.path().join("sessions").as_path(), &tx).unwrap();
        drop(tx);
        let events: Vec<_> = rx.iter().collect();

        assert_eq!(stats.projects, 1);
        assert_eq!(stats.sessions, 1);
        assert_eq!(stats.messages, 3); // user + assistant + tool call (token re-upsert is not new)

        let msgs: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                IngestEvent::Message {
                    msg_type,
                    input_tokens,
                    output_tokens,
                    fts_text,
                    ..
                } => Some((
                    msg_type.as_str(),
                    *input_tokens,
                    *output_tokens,
                    fts_text.clone(),
                )),
                _ => None,
            })
            .collect();
        // user, assistant (0 tokens), assistant re-upsert with tokens, tool call
        assert!(msgs.iter().any(|(t, _, _, _)| *t == "user"));
        assert!(msgs
            .iter()
            .any(|(t, i, o, _)| *t == "assistant" && *i == 3 && *o == 7));
        assert!(msgs
            .iter()
            .any(|(_, _, _, f)| f.as_deref() == Some("hello")));
        assert!(msgs.iter().any(|(t, _, _, _)| *t == "tool_use"));
    }

    #[test]
    fn token_reemit_carries_byte_offset_and_guards_repeat_total() {
        let tmp = TempDir::new().unwrap();
        let day = tmp.path().join("sessions/2026/01/01");
        std::fs::create_dir_all(&day).unwrap();
        let file = day.join("rollout-2026-01-01T00-00-00-019cccccccccccccccccccccccc.jsonl");
        let mut f = std::fs::File::create(&file).unwrap();
        writeln!(
            f,
            r#"{{"timestamp":"2026-01-01T00:00:00.000Z","type":"session_meta","payload":{{"id":"sess-2","cwd":"/tmp/demo2"}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"timestamp":"2026-01-01T00:00:01.000Z","type":"response_item","payload":{{"type":"message","role":"assistant","id":"a1","content":[{{"type":"output_text","text":"hi"}}]}}}}"#
        )
        .unwrap();
        // total-only token_count → attribute to a1 AND clear the pointer.
        writeln!(
            f,
            r#"{{"type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":100,"output_tokens":50,"cached_input_tokens":0,"reasoning_output_tokens":0}}}}}}}}"#
        )
        .unwrap();
        // second total-only token_count → pointer cleared → must NOT re-apply.
        writeln!(
            f,
            r#"{{"type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":999,"output_tokens":999,"cached_input_tokens":0,"reasoning_output_tokens":0}}}}}}}}"#
        )
        .unwrap();

        let (tx, rx) = unbounded();
        CodexReader::read_all(tmp.path().join("sessions").as_path(), &tx).unwrap();
        drop(tx);
        let events: Vec<_> = rx.iter().collect();

        let assistant_msgs: Vec<(u64, u64, u64)> = events
            .iter()
            .filter_map(|e| match e {
                IngestEvent::Message {
                    msg_type,
                    byte_offset,
                    input_tokens,
                    output_tokens,
                    ..
                } if msg_type == "assistant" => Some((*byte_offset, *input_tokens, *output_tokens)),
                _ => None,
            })
            .collect();

        // Original emit (0 tokens) + exactly one token re-emit (first total).
        // The second total-only count must NOT produce a third.
        assert_eq!(
            assistant_msgs.len(),
            2,
            "second total-only count must not re-apply to the same assistant"
        );
        let original = assistant_msgs
            .iter()
            .find(|(_, i, _)| *i == 0)
            .expect("original assistant emit");
        let reemit = assistant_msgs
            .iter()
            .find(|(_, i, _)| *i == 100)
            .expect("token re-emit with first total");
        // 5a: the re-emit carries the ORIGINAL byte offset (not a hardcoded 0).
        assert_ne!(
            original.0, 0,
            "assistant is not the first line -> offset > 0"
        );
        assert_eq!(
            reemit.0, original.0,
            "re-emit must carry the original byte offset"
        );
        assert_eq!(reemit.2, 50);
        // 5b: no emit ever carries the second total (999).
        assert!(!assistant_msgs.iter().any(|(_, i, _)| *i == 999));
    }
}
