use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::adapter::{AdapterError, NativeArtifactProbe};

const MAX_PROBE_ROOTS: usize = 16;
const MAX_SESSION_ENTRIES: usize = 256;
const MAX_PROJECT_ENTRIES: usize = 256;
const MAX_TRANSCRIPT_ENTRIES: usize = 1_024;
const MAX_SETTINGS_BYTES: usize = 1024 * 1024;
const MAX_ACTIVE_SESSION_BYTES: usize = 64 * 1024;
const MAX_TRANSCRIPT_PREFIX_BYTES: usize = 64 * 1024;

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

fn inspect_settings(root: &Path) -> Result<bool, AdapterError> {
    let path = root.join("settings.json");
    if !path.exists() {
        return Ok(false);
    }
    let bytes = bounded_file_bytes(&path, MAX_SETTINGS_BYTES)?;
    Ok(serde_json::from_slice::<Value>(&bytes)
        .ok()
        .is_some_and(|value| value.is_object()))
}

fn inspect_active_sessions(
    root: &Path,
    versions: &mut BTreeSet<String>,
) -> Result<bool, AdapterError> {
    let mut found_version = false;
    for path in sorted_directory_entries(&root.join("sessions"), MAX_SESSION_ENTRIES)? {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.ends_with(".json")
            || !name
                .trim_end_matches(".json")
                .bytes()
                .all(|byte| byte.is_ascii_digit())
        {
            continue;
        }
        let bytes = bounded_file_bytes(&path, MAX_ACTIVE_SESSION_BYTES)?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| probe_error("native support probe active-session JSON is invalid"))?;
        let version = value
            .as_object()
            .and_then(|object| object.get("version"))
            .and_then(Value::as_str)
            .ok_or_else(|| probe_error("native support probe active-session version is invalid"))?;
        if version.is_empty() || version.len() > 256 {
            return Err(probe_error(
                "native support probe active-session version is invalid",
            ));
        }
        versions.insert(version.to_string());
        found_version = true;
    }
    Ok(found_version)
}

fn inspect_transcripts(root: &Path) -> Result<bool, AdapterError> {
    let mut inspected = 0usize;
    for project in sorted_directory_entries(&root.join("projects"), MAX_PROJECT_ENTRIES)? {
        let metadata = fs::symlink_metadata(&project)
            .map_err(|_| probe_error("native support probe could not inspect a project"))?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        for path in sorted_directory_entries(&project, MAX_TRANSCRIPT_ENTRIES)? {
            if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
                continue;
            }
            inspected = inspected
                .checked_add(1)
                .ok_or_else(|| probe_error("native support probe transcript count overflowed"))?;
            if inspected > MAX_TRANSCRIPT_ENTRIES {
                return Err(probe_error(
                    "native support probe exceeded its transcript entry bound",
                ));
            }
            let metadata = fs::symlink_metadata(&path)
                .map_err(|_| probe_error("native support probe could not inspect a transcript"))?;
            if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                continue;
            }
            let file = File::open(&path)
                .map_err(|_| probe_error("native support probe could not open a transcript"))?;
            let mut reader = BufReader::new(file.take(MAX_TRANSCRIPT_PREFIX_BYTES as u64 + 1));
            let mut line = String::new();
            while reader
                .read_line(&mut line)
                .map_err(|_| probe_error("native support probe could not read a transcript"))?
                != 0
            {
                if line.len() > MAX_TRANSCRIPT_PREFIX_BYTES {
                    return Err(probe_error(
                        "native support probe transcript prefix exceeded its byte bound",
                    ));
                }
                if line.trim().is_empty() {
                    line.clear();
                    continue;
                }
                let value: Value = serde_json::from_str(&line)
                    .map_err(|_| probe_error("native support probe transcript JSON is invalid"))?;
                return Ok(value
                    .as_object()
                    .and_then(|object| object.get("type"))
                    .and_then(Value::as_str)
                    .is_some_and(|kind| !kind.is_empty()));
            }
        }
    }
    Ok(false)
}

/// Derive one bounded, path-free probe from the configured Claude roots.
/// Missing evidence produces an unverified probe; malformed, conflicting, or
/// over-limit evidence fails closed before adapter discovery or source access.
pub(crate) fn probe_claude_native_artifact(
    roots: &[PathBuf],
) -> Result<NativeArtifactProbe, AdapterError> {
    if roots.is_empty() || roots.len() > MAX_PROBE_ROOTS {
        return Err(probe_error(
            "native support probe requires a bounded nonempty root set",
        ));
    }
    let mut versions = BTreeSet::new();
    let mut settings_shape = false;
    let mut active_session_version = false;
    let mut transcript_type = false;
    for root in roots {
        settings_shape |= inspect_settings(root)?;
        active_session_version |= inspect_active_sessions(root, &mut versions)?;
        transcript_type |= inspect_transcripts(root)?;
    }
    let contradictory_markers = versions.len() > 1;
    let version = (versions.len() == 1).then(|| versions.into_iter().next().unwrap());
    let mut markers = Vec::new();
    if active_session_version {
        markers.push("active-session.version".to_string());
    }
    if settings_shape {
        markers.push("settings.schema-shape".to_string());
    }
    if transcript_type {
        markers.push("transcript.type".to_string());
    }
    Ok(NativeArtifactProbe {
        family: "claude-code".to_string(),
        platform: platform_id().to_string(),
        version,
        markers,
        contradictory_markers,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn exact_root(version: &str) -> TempDir {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("settings.json"), b"{}").unwrap();
        fs::create_dir(temp.path().join("sessions")).unwrap();
        fs::write(
            temp.path().join("sessions/123.json"),
            format!(r#"{{"version":"{version}"}}"#),
        )
        .unwrap();
        fs::create_dir_all(temp.path().join("projects/project")).unwrap();
        fs::write(
            temp.path().join("projects/project/session.jsonl"),
            b"{\"type\":\"user\"}\n",
        )
        .unwrap();
        temp
    }

    #[test]
    fn exact_probe_requires_all_three_native_markers() {
        let root = exact_root("2.1.223");
        let probe = probe_claude_native_artifact(&[root.path().to_path_buf()]).unwrap();
        assert_eq!(probe.platform, platform_id());
        assert_eq!(probe.version.as_deref(), Some("2.1.223"));
        assert_eq!(
            probe.markers,
            vec![
                "active-session.version",
                "settings.schema-shape",
                "transcript.type",
            ]
        );
        assert!(!probe.contradictory_markers);
    }

    #[test]
    fn conflicting_active_versions_fail_closed_without_exposing_paths() {
        let root = exact_root("2.1.223");
        fs::write(
            root.path().join("sessions/456.json"),
            r#"{"version":"2.1.238"}"#,
        )
        .unwrap();
        let probe = probe_claude_native_artifact(&[root.path().to_path_buf()]).unwrap();
        assert!(probe.version.is_none());
        assert!(probe.contradictory_markers);
        assert!(format!("{probe:?}").contains("contradictory_markers: true"));
        assert!(!format!("{probe:?}").contains(root.path().to_str().unwrap()));
    }
}
