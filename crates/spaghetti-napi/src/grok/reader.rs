//! GrokReader — walk `sessions/**/<uuid>/chat_history.jsonl` → [`IngestEvent`] stream.
//!
//! Mirrors `packages/sdk/src/sources/grok/reader.ts`. Session metadata lives in
//! sibling `summary.json` (not inside the JSONL), so no line-peek is needed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crossbeam_channel::{SendError, Sender};
use once_cell::sync::Lazy;
use regex::Regex;

use crate::claude::fingerprint::DiscoveryError;
use crate::claude::types::{SessionIndexEntry, SessionsIndex};
use crate::core::event::IngestEvent;
use crate::core::jsonl::read_jsonl_streaming;
use crate::grok::message_extractor;
use crate::grok::sidecars;

static UUID_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").expect("uuid")
});

const CHAT_HISTORY_FILE: &str = "chat_history.jsonl";
const SUMMARY_FILE: &str = "summary.json";
const FIRST_PROMPT_MAX: usize = 200;

#[derive(Debug, thiserror::Error)]
pub enum GrokReadError {
    #[error("event channel closed")]
    ChannelClosed(#[source] Box<SendError<IngestEvent>>),
}

impl From<SendError<IngestEvent>> for GrokReadError {
    fn from(e: SendError<IngestEvent>) -> Self {
        Self::ChannelClosed(Box::new(e))
    }
}

#[derive(Debug, Clone)]
struct SessionMeta {
    cwd: String,
    session_id: String,
    created: Option<String>,
    updated: Option<String>,
    title: String,
    summary: String,
    git_branch: String,
}

/// Encode cwd → project slug (Claude-compatible `/` → `-`).
/// Windows cwds separate with `\\`, so both separators are folded
/// (parity with the TS readers).
pub fn encode_slug(cwd: &str) -> String {
    cwd.replace(['/', '\\'], "-")
}

/// Percent-decode a path segment (matches JS `decodeURIComponent` for `%XX`).
fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let h1 = from_hex(bytes[i + 1])?;
            let h2 = from_hex(bytes[i + 2])?;
            out.push((h1 << 4) | h2);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

struct SessionFile {
    path: PathBuf,
    meta: SessionMeta,
    mtime_ms: f64,
    size: u64,
}

/// Discover + stream Grok sessions into the ingest event channel.
pub struct GrokReader;

impl GrokReader {
    /// Full cold-style read of every chat_history under `sessions_dir`.
    pub fn read_all(
        sessions_dir: &Path,
        events: &Sender<IngestEvent>,
    ) -> Result<GrokReadStats, GrokReadError> {
        let (files, discovery_errors) = discover(sessions_dir);
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
            let Some(meta) = read_session_meta(&path) else {
                continue;
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

        let mut stats = GrokReadStats {
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

    /// Warm-start: true when every known chat_history is unchanged vs `stored`
    /// fingerprints (path → mtime/size) and there are no new/deleted files.
    pub fn warm_unchanged(
        sessions_dir: &Path,
        stored: &std::collections::HashMap<String, crate::claude::fingerprint::SourceFingerprint>,
    ) -> bool {
        let (files, discovery_errors) = discover(sessions_dir);
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
pub struct GrokReadStats {
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
        if entry.file_name().to_string_lossy() == CHAT_HISTORY_FILE {
            out.push(entry.path().to_path_buf());
        }
    }
    out.sort();
    (out, errors)
}

/// Read session metadata from sibling `summary.json`, with directory fallbacks.
fn read_session_meta(chat_history_file: &Path) -> Option<SessionMeta> {
    let session_dir = chat_history_file.parent()?;
    let uuid_dir = session_dir.file_name()?.to_string_lossy();
    let encoded_cwd_dir = session_dir
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut cwd: Option<String> = None;
    let mut session_id: Option<String> = None;
    let mut created: Option<String> = None;
    let mut updated: Option<String> = None;
    let mut title = String::new();
    let mut summary = String::new();
    let mut git_branch = String::new();

    let summary_path = session_dir.join(SUMMARY_FILE);
    if let Ok(raw) = std::fs::read_to_string(&summary_path) {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(info) = parsed.get("info") {
                if let Some(c) = info.get("cwd").and_then(|v| v.as_str()) {
                    cwd = Some(c.to_owned());
                }
                if let Some(id) = info.get("id").and_then(|v| v.as_str()) {
                    session_id = Some(id.to_owned());
                }
            }
            if cwd.is_none() {
                if let Some(g) = parsed.get("git_root_dir").and_then(|v| v.as_str()) {
                    cwd = Some(g.trim_end_matches('/').to_owned());
                }
            }
            if let Some(c) = parsed.get("created_at").and_then(|v| v.as_str()) {
                created = Some(c.to_owned());
            }
            if let Some(u) = parsed.get("updated_at").and_then(|v| v.as_str()) {
                updated = Some(u.to_owned());
            } else if let Some(u) = parsed.get("last_active_at").and_then(|v| v.as_str()) {
                updated = Some(u.to_owned());
            }
            if let Some(t) = parsed.get("generated_title").and_then(|v| v.as_str()) {
                title = t.to_owned();
            }
            if let Some(s) = parsed.get("session_summary").and_then(|v| v.as_str()) {
                summary = s.to_owned();
                if title.is_empty() {
                    title = s.to_owned();
                }
            }
            if let Some(b) = parsed.get("head_branch").and_then(|v| v.as_str()) {
                git_branch = b.to_owned();
            }
        }
    }

    if cwd.is_none() {
        cwd = percent_decode(&encoded_cwd_dir);
    }
    let cwd = cwd?;

    if session_id.is_none() {
        session_id = UUID_RE
            .find(uuid_dir.as_ref())
            .map(|m| m.as_str().to_owned())
            .or_else(|| Some(uuid_dir.into_owned()));
    }
    let session_id = session_id?;

    // 200 UTF-16 code units, matching TS `title.slice(0, FIRST_PROMPT_MAX)`.
    title = crate::core::text::truncate_utf16(&title, FIRST_PROMPT_MAX).to_owned();

    Some(SessionMeta {
        cwd,
        session_id,
        created,
        updated,
        title,
        summary,
        git_branch,
    })
}

fn session_entry(s: &SessionFile) -> SessionIndexEntry {
    let modified = s.meta.updated.clone().unwrap_or_else(|| {
        if s.mtime_ms > 0.0 {
            ms_to_iso(s.mtime_ms)
        } else {
            String::new()
        }
    });
    let created = s.meta.created.clone().unwrap_or_else(|| modified.clone());
    let first_prompt = if s.meta.title.is_empty() {
        "No prompt".into()
    } else {
        s.meta.title.clone()
    };
    SessionIndexEntry {
        session_id: s.meta.session_id.clone(),
        full_path: s.path.to_string_lossy().into_owned(),
        file_mtime: s.mtime_ms,
        first_prompt,
        summary: s.meta.summary.clone(),
        message_count: 0,
        created,
        modified,
        git_branch: s.meta.git_branch.clone(),
        project_path: s.meta.cwd.clone(),
        is_sidechain: false,
    }
}

fn stream_session(
    slug: &str,
    sess: &SessionFile,
    events: &Sender<IngestEvent>,
) -> Result<(u32, u64), GrokReadError> {
    let session_id = &sess.meta.session_id;
    let mut message_count: u32 = 0;
    let mut last_byte: u64 = 0;
    // Absolute non-empty line index (including skipped tool lines) — matches TS.
    let mut line_index: u32 = 0;
    let mut send_err: Option<SendError<IngestEvent>> = None;

    // events.jsonl + signals.json (same join rules as TS sidecars).
    let side = sidecars::load_sidecars(&sess.path, sess.meta.created.as_deref());
    let ts_map = &side.ts_map;

    let stream = read_jsonl_streaming(&sess.path, 0, |line, _idx, byte_offset| {
        if send_err.is_some() {
            return;
        }
        last_byte = byte_offset;
        let idx = line_index;
        line_index = line_index.saturating_add(1);

        match message_extractor::project_jsonl_line(line) {
            Ok(Some(proj)) => {
                let fts = if proj.fts_text.is_empty() {
                    None
                } else {
                    Some(proj.fts_text.clone())
                };
                let timestamp = ts_map.get(&idx).cloned();
                let ev = IngestEvent::Message {
                    slug: slug.to_owned(),
                    session_id: session_id.clone(),
                    index: idx,
                    byte_offset,
                    raw_json: line.to_owned(),
                    msg_type: proj.msg_type.clone(),
                    uuid: proj.uuid.clone(),
                    timestamp,
                    input_tokens: side.tokens.get(&idx).copied().unwrap_or(0),
                    output_tokens: 0,
                    cache_creation_tokens: 0,
                    cache_read_tokens: 0,
                    fts_text: fts,
                };
                if let Err(e) = events.send(ev) {
                    send_err = Some(e);
                    return;
                }
                message_count = message_count.saturating_add(1);
            }
            Ok(None) | Err(_) => {
                // Unknown record / bad JSON — skip (TS behaves identically).
            }
        }
    });

    if let Some(e) = send_err {
        return Err(e.into());
    }
    if let Ok(r) = stream {
        last_byte = r.final_byte_position.max(last_byte);
    }

    // Per-message tokens were attached inline from `side.tokens`; all that is
    // left is the estimate flag. TS raises it whenever signals report usage,
    // even if the allocation ends up empty, so the flag does not depend on the
    // session happening to contain an assistant row.
    if side
        .signals
        .as_ref()
        .is_some_and(|sig| sig.context_tokens_used > 0)
    {
        events.send(IngestEvent::SessionTokensEstimated {
            session_id: session_id.clone(),
            estimated: true,
        })?;
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
                .map(|d| d.as_secs() as f64 * 1000.0 + d.subsec_nanos() as f64 / 1_000_000.0)
                .unwrap_or(0.0);
            (mtime_ms, m.len())
        }
        Err(_) => (0.0, 0),
    }
}

/// Epoch-ms → ISO 8601, matching JS `new Date(ms).toISOString()` (3-digit ms
/// fraction + trailing `Z`). Shared with the Claude/Codex readers so all three
/// engines format session timestamps identically — `time`'s `Rfc3339` trims
/// trailing fractional zeros, which JS never does.
fn ms_to_iso(mtime_ms: f64) -> String {
    crate::core::timefmt::epoch_ms_to_iso8601(mtime_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_slug_replaces_slashes() {
        assert_eq!(encode_slug("/tmp/proj-a"), "-tmp-proj-a");
        assert_eq!(encode_slug(r"C:\tmp\proj-a"), "C:-tmp-proj-a");
    }

    #[test]
    fn percent_decode_url_encoded_cwd() {
        let enc = "%2FUsers%2Fme%2Fproj";
        assert_eq!(percent_decode(enc).as_deref(), Some("/Users/me/proj"));
    }
}
