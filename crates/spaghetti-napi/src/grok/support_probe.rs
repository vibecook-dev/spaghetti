use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::adapter::{AdapterError, NativeArtifactProbe};

const MAX_PROBE_ROOTS: usize = 16;
const MAX_PROJECT_ENTRIES: usize = 512;
const MAX_SESSION_ENTRIES_PER_PROJECT: usize = 2_048;
const MAX_TOTAL_SESSION_ENTRIES: usize = 4_096;
const MAX_SUMMARY_BYTES: usize = 1024 * 1024;
const MAX_SIGNALS_BYTES: usize = 256 * 1024;
const MAX_EVENT_PREFIX_BYTES: usize = 64 * 1024;
const MAX_ID_BYTES: usize = 256;
const MAX_CWD_BYTES: usize = 8 * 1024;
const MAX_EVENT_TYPE_BYTES: usize = 128;

fn probe_error(message: &'static str) -> AdapterError {
    AdapterError::invalid_contract(message)
}

fn platform_id() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "windows",
        other => other,
    }
}

fn sorted_directory_entries(path: &Path, limit: usize) -> Result<Vec<PathBuf>, AdapterError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => {
            return Err(probe_error(
                "native support probe could not inspect a directory",
            ))
        }
    };
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(probe_error(
            "native support probe selected an invalid directory",
        ));
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(path)
        .map_err(|_| probe_error("native support probe could not read a directory"))?
    {
        let entry = entry
            .map_err(|_| probe_error("native support probe could not read a directory entry"))?;
        if entries.len() == limit {
            return Err(probe_error(
                "native support probe directory exceeded its entry bound",
            ));
        }
        entries.push(entry.path());
    }
    entries.sort();
    Ok(entries)
}

fn is_real_directory(path: &Path) -> Result<bool, AdapterError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| probe_error("native support probe could not inspect a directory entry"))?;
    Ok(metadata.file_type().is_dir() && !metadata.file_type().is_symlink())
}

fn has_regular_object(path: &Path) -> Result<bool, AdapterError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() && !metadata.file_type().is_symlink() => {
            Ok(true)
        }
        Ok(_) => Err(probe_error(
            "native support probe selected an invalid sidecar object",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(probe_error(
            "native support probe could not inspect a selected object",
        )),
    }
}

fn latest_complete_session(root: &Path) -> Result<Option<PathBuf>, AdapterError> {
    let projects = sorted_directory_entries(&root.join("sessions"), MAX_PROJECT_ENTRIES)?;
    let mut total_sessions = 0usize;
    let mut latest = None;
    for project in projects {
        if !is_real_directory(&project)? {
            continue;
        }
        for session in sorted_directory_entries(&project, MAX_SESSION_ENTRIES_PER_PROJECT)? {
            if !is_real_directory(&session)? {
                continue;
            }
            total_sessions = total_sessions
                .checked_add(1)
                .ok_or_else(|| probe_error("native support probe session count overflowed"))?;
            if total_sessions > MAX_TOTAL_SESSION_ENTRIES {
                return Err(probe_error(
                    "native support probe exceeded its session entry bound",
                ));
            }
            let summary = has_regular_object(&session.join("summary.json"))?;
            let events = has_regular_object(&session.join("events.jsonl"))?;
            let signals = has_regular_object(&session.join("signals.json"))?;
            if summary && events && signals {
                latest = Some(session);
            }
        }
    }
    Ok(latest)
}

fn bounded_file_bytes(path: &Path, max_bytes: usize) -> Result<Vec<u8>, AdapterError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| probe_error("native support probe could not inspect a selected object"))?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > max_bytes as u64
    {
        return Err(probe_error(
            "native support probe selected an invalid or oversized object",
        ));
    }
    let file = File::open(path)
        .map_err(|_| probe_error("native support probe could not open a selected object"))?;
    let mut bytes = Vec::with_capacity((metadata.len() as usize).min(max_bytes));
    file.take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| probe_error("native support probe could not read a selected object"))?;
    if bytes.len() > max_bytes {
        return Err(probe_error(
            "native support probe object exceeded its byte bound",
        ));
    }
    Ok(bytes)
}

fn bounded_first_event(path: &Path) -> Result<Vec<u8>, AdapterError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| probe_error("native support probe could not inspect a selected object"))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(probe_error(
            "native support probe selected an invalid sidecar object",
        ));
    }
    let file = File::open(path)
        .map_err(|_| probe_error("native support probe could not open a selected object"))?;
    let mut prefix = Vec::with_capacity(MAX_EVENT_PREFIX_BYTES + 1);
    file.take(MAX_EVENT_PREFIX_BYTES as u64 + 1)
        .read_to_end(&mut prefix)
        .map_err(|_| probe_error("native support probe could not read a selected object"))?;
    let mut consumed = 0usize;
    for line in prefix.split_inclusive(|byte| *byte == b'\n') {
        consumed = consumed
            .checked_add(line.len())
            .ok_or_else(|| probe_error("native support probe event accounting overflowed"))?;
        let without_newline = line.strip_suffix(b"\n").unwrap_or(line);
        let record = without_newline
            .strip_suffix(b"\r")
            .unwrap_or(without_newline);
        if record.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if consumed > MAX_EVENT_PREFIX_BYTES
            || (consumed == prefix.len()
                && prefix.len() > MAX_EVENT_PREFIX_BYTES
                && !line.ends_with(b"\n"))
        {
            return Err(probe_error(
                "native support probe event head exceeded its byte bound",
            ));
        }
        return Ok(record.to_vec());
    }
    if prefix.len() > MAX_EVENT_PREFIX_BYTES {
        return Err(probe_error(
            "native support probe event head exceeded its byte bound",
        ));
    }
    Err(probe_error(
        "native support probe event sidecar contained no record",
    ))
}

fn valid_canonical_text(value: Option<&str>, max_bytes: usize) -> bool {
    value.is_some_and(|value| {
        !value.is_empty()
            && value.len() <= max_bytes
            && value.trim() == value
            && !value.chars().any(char::is_control)
    })
}

fn inspect_complete_session(session: &Path) -> Result<(), AdapterError> {
    let summary = bounded_file_bytes(&session.join("summary.json"), MAX_SUMMARY_BYTES)?;
    let summary: Value = serde_json::from_slice(&summary)
        .map_err(|_| probe_error("native support probe summary JSON is invalid"))?;
    let info = summary
        .as_object()
        .and_then(|summary| summary.get("info"))
        .and_then(Value::as_object)
        .ok_or_else(|| probe_error("native support probe summary shape is invalid"))?;
    if !valid_canonical_text(info.get("id").and_then(Value::as_str), MAX_ID_BYTES)
        || !valid_canonical_text(info.get("cwd").and_then(Value::as_str), MAX_CWD_BYTES)
    {
        return Err(probe_error("native support probe summary shape is invalid"));
    }

    let event = bounded_first_event(&session.join("events.jsonl"))?;
    let event: Value = serde_json::from_slice(&event)
        .map_err(|_| probe_error("native support probe event JSON is invalid"))?;
    let event_type = event
        .as_object()
        .and_then(|event| event.get("type"))
        .and_then(Value::as_str);
    if !valid_canonical_text(event_type, MAX_EVENT_TYPE_BYTES) {
        return Err(probe_error("native support probe event type is invalid"));
    }

    let signals = bounded_file_bytes(&session.join("signals.json"), MAX_SIGNALS_BYTES)?;
    let signals: Value = serde_json::from_slice(&signals)
        .map_err(|_| probe_error("native support probe signals JSON is invalid"))?;
    let has_token_shape = signals.as_object().is_some_and(|signals| {
        signals
            .get("contextTokensUsed")
            .or_else(|| signals.get("context_tokens_used"))
            .is_some_and(|value| value.as_u64().is_some())
    });
    if !has_token_shape {
        return Err(probe_error("native support probe signals shape is invalid"));
    }
    Ok(())
}

/// Derive bounded shape markers from one complete, co-located Grok sidecar
/// set. The reviewed native files do not carry an artifact version, so this
/// probe intentionally leaves `version` absent until a separate distributable
/// pin is evidenced; shape recognition alone cannot grant typed access.
pub(crate) fn probe_grok_native_artifact(
    roots: &[PathBuf],
) -> Result<NativeArtifactProbe, AdapterError> {
    if roots.is_empty() || roots.len() > MAX_PROBE_ROOTS {
        return Err(probe_error(
            "native support probe requires a bounded nonempty root set",
        ));
    }
    let mut found_complete_shape = false;
    for root in roots {
        if let Some(session) = latest_complete_session(root)? {
            inspect_complete_session(&session)?;
            found_complete_shape = true;
        }
    }
    let markers = if found_complete_shape {
        vec![
            "event.type".to_owned(),
            "signal.shape".to_owned(),
            "summary.shape".to_owned(),
        ]
    } else {
        Vec::new()
    };
    Ok(NativeArtifactProbe {
        family: "grok".to_owned(),
        platform: platform_id().to_owned(),
        version: None,
        markers,
        contradictory_markers: false,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::adapter::SupportCatalog;

    fn session(root: &Path, project: &str, id: &str) -> PathBuf {
        let path = root.join("sessions").join(project).join(id);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_complete_session(path: &Path) {
        fs::write(
            path.join("summary.json"),
            br#"{"info":{"id":"session-1","cwd":"/sanitized/project"}}"#,
        )
        .unwrap();
        fs::write(
            path.join("events.jsonl"),
            br#"{"ts":"2026-08-22T00:00:00Z","type":"turn_started"}
"#,
        )
        .unwrap();
        fs::write(path.join("signals.json"), br#"{"contextTokensUsed":0}"#).unwrap();
    }

    #[test]
    fn co_located_sidecars_supply_declared_markers_without_version_authority() {
        let root = TempDir::new().unwrap();
        write_complete_session(&session(root.path(), "%2Fsanitized", "session-1"));
        let probe = probe_grok_native_artifact(&[root.path().to_path_buf()]).unwrap();
        assert_eq!(
            probe.markers,
            vec!["event.type", "signal.shape", "summary.shape"]
        );
        assert!(probe.version.is_none());
        assert!(!probe.contradictory_markers);

        let catalog =
            SupportCatalog::new([crate::grok::verified_support_release().unwrap()]).unwrap();
        let decision = catalog.classify(&probe).unwrap();
        assert!(!decision.permissions().catalog);
        assert!(!decision.permissions().durable);
    }

    #[test]
    fn markers_cannot_be_mosaicked_across_sessions() {
        let root = TempDir::new().unwrap();
        let summary = session(root.path(), "project", "summary-only");
        fs::write(
            summary.join("summary.json"),
            br#"{"info":{"id":"one","cwd":"/sanitized/one"}}"#,
        )
        .unwrap();
        let events = session(root.path(), "project", "events-only");
        fs::write(
            events.join("events.jsonl"),
            br#"{"type":"turn_started"}
"#,
        )
        .unwrap();
        let signals = session(root.path(), "project", "signals-only");
        fs::write(signals.join("signals.json"), br#"{"contextTokensUsed":1}"#).unwrap();
        let probe = probe_grok_native_artifact(&[root.path().to_path_buf()]).unwrap();
        assert!(probe.markers.is_empty());
    }

    #[test]
    fn invalid_and_oversized_sidecars_fail_without_path_disclosure() {
        let malformed = TempDir::new().unwrap();
        let path = session(malformed.path(), "private-project", "private-session");
        write_complete_session(&path);
        fs::write(path.join("signals.json"), br#"{"secret":"/Users/alice"}"#).unwrap();
        let error = probe_grok_native_artifact(&[malformed.path().to_path_buf()]).unwrap_err();
        let message = error.to_string();
        assert!(!message.contains("alice"));
        assert!(!message.contains("private"));
        assert!(!message.contains(malformed.path().to_string_lossy().as_ref()));

        let oversized = TempDir::new().unwrap();
        let path = session(oversized.path(), "hidden-project", "hidden-session");
        write_complete_session(&path);
        fs::write(path.join("summary.json"), vec![b'x'; MAX_SUMMARY_BYTES + 1]).unwrap();
        let error = probe_grok_native_artifact(&[oversized.path().to_path_buf()]).unwrap_err();
        let message = error.to_string();
        assert!(!message.contains("hidden"));
        assert!(!message.contains(oversized.path().to_string_lossy().as_ref()));
    }

    #[cfg(unix)]
    #[test]
    fn sidecar_symlinks_are_rejected() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().unwrap();
        let path = session(root.path(), "project", "session");
        write_complete_session(&path);
        let target = root.path().join("outside.json");
        fs::write(&target, br#"{"contextTokensUsed":1}"#).unwrap();
        fs::remove_file(path.join("signals.json")).unwrap();
        symlink(target, path.join("signals.json")).unwrap();
        assert!(probe_grok_native_artifact(&[root.path().to_path_buf()]).is_err());
    }
}
