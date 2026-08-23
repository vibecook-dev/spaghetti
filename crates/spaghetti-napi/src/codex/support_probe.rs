use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::adapter::{
    platform_id, probe_error, sorted_directory_entries, AdapterError, NativeArtifactProbe,
};

const MAX_PROBE_ROOTS: usize = 16;
const MAX_YEAR_ENTRIES: usize = 64;
const MAX_MONTH_ENTRIES: usize = 32;
const MAX_DAY_ENTRIES: usize = 64;
const MAX_ROLLOUTS_PER_DAY: usize = 4_096;
const MAX_HEAD_PREFIX_BYTES: usize = 64 * 1024;
const MAX_VERSION_BYTES: usize = 128;

fn sorted_numeric_directories(
    path: &Path,
    digits: usize,
    limit: usize,
) -> Result<Vec<PathBuf>, AdapterError> {
    let mut directories = Vec::new();
    for entry in sorted_directory_entries(path, limit)? {
        let Some(name) = entry.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.len() != digits || !name.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let metadata = fs::symlink_metadata(&entry)
            .map_err(|_| probe_error("native support probe could not inspect a directory entry"))?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err(probe_error(
                "native support probe selected an invalid date directory",
            ));
        }
        directories.push(entry);
    }
    Ok(directories)
}

fn latest_rollout(root: &Path) -> Result<Option<PathBuf>, AdapterError> {
    let years = sorted_numeric_directories(&root.join("sessions"), 4, MAX_YEAR_ENTRIES)?;
    for year in years.into_iter().rev() {
        let months = sorted_numeric_directories(&year, 2, MAX_MONTH_ENTRIES)?;
        for month in months.into_iter().rev() {
            let days = sorted_numeric_directories(&month, 2, MAX_DAY_ENTRIES)?;
            for day in days.into_iter().rev() {
                let entries = sorted_directory_entries(&day, MAX_ROLLOUTS_PER_DAY)?;
                for path in entries.into_iter().rev() {
                    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                        continue;
                    };
                    if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
                        continue;
                    }
                    let metadata = fs::symlink_metadata(&path).map_err(|_| {
                        probe_error("native support probe could not inspect a selected object")
                    })?;
                    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                        return Err(probe_error(
                            "native support probe selected an invalid rollout object",
                        ));
                    }
                    return Ok(Some(path));
                }
            }
        }
    }
    Ok(None)
}

fn bounded_head_record(path: &Path) -> Result<Vec<u8>, AdapterError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| probe_error("native support probe could not inspect a selected object"))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(probe_error(
            "native support probe selected an invalid rollout object",
        ));
    }
    let file = File::open(path)
        .map_err(|_| probe_error("native support probe could not open a selected object"))?;
    let mut prefix = Vec::with_capacity(MAX_HEAD_PREFIX_BYTES + 1);
    file.take(MAX_HEAD_PREFIX_BYTES as u64 + 1)
        .read_to_end(&mut prefix)
        .map_err(|_| probe_error("native support probe could not read a selected object"))?;

    let mut consumed = 0usize;
    for line in prefix.split_inclusive(|byte| *byte == b'\n') {
        consumed = consumed
            .checked_add(line.len())
            .ok_or_else(|| probe_error("native support probe head accounting overflowed"))?;
        let without_newline = line.strip_suffix(b"\n").unwrap_or(line);
        let record = without_newline
            .strip_suffix(b"\r")
            .unwrap_or(without_newline);
        if record.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if consumed > MAX_HEAD_PREFIX_BYTES
            || (consumed == prefix.len()
                && prefix.len() > MAX_HEAD_PREFIX_BYTES
                && !line.ends_with(b"\n"))
        {
            return Err(probe_error(
                "native support probe rollout head exceeded its byte bound",
            ));
        }
        return Ok(record.to_vec());
    }
    if prefix.len() > MAX_HEAD_PREFIX_BYTES {
        return Err(probe_error(
            "native support probe rollout head exceeded its byte bound",
        ));
    }
    Err(probe_error(
        "native support probe rollout head contained no record",
    ))
}

fn inspect_rollout(path: &Path) -> Result<String, AdapterError> {
    let record = bounded_head_record(path)?;
    let value: Value = serde_json::from_slice(&record)
        .map_err(|_| probe_error("native support probe rollout head JSON is invalid"))?;
    let root = value
        .as_object()
        .ok_or_else(|| probe_error("native support probe rollout head is invalid"))?;
    if root.get("type").and_then(Value::as_str) != Some("session_meta") {
        return Err(probe_error(
            "native support probe rollout head record type is invalid",
        ));
    }
    let version = root
        .get("payload")
        .and_then(Value::as_object)
        .and_then(|payload| payload.get("cli_version"))
        .and_then(Value::as_str)
        .ok_or_else(|| probe_error("native support probe session version is invalid"))?;
    if version.is_empty()
        || version.len() > MAX_VERSION_BYTES
        || version.trim() != version
        || version.chars().any(char::is_control)
    {
        return Err(probe_error(
            "native support probe session version is invalid",
        ));
    }
    Ok(version.to_owned())
}

/// Derive a bounded, path-free Codex artifact probe from the latest rollout
/// beneath each configured data root. Historical rollouts legitimately carry
/// older CLI versions, so only the latest declared rollout in each root is a
/// current-version witness; disagreement between configured roots is closed as
/// contradictory evidence.
pub(crate) fn probe_codex_native_artifact(
    roots: &[PathBuf],
) -> Result<NativeArtifactProbe, AdapterError> {
    if roots.is_empty() || roots.len() > MAX_PROBE_ROOTS {
        return Err(probe_error(
            "native support probe requires a bounded nonempty root set",
        ));
    }
    let mut versions = std::collections::BTreeSet::new();
    for root in roots {
        if let Some(path) = latest_rollout(root)? {
            versions.insert(inspect_rollout(&path)?);
        }
    }
    let contradictory_markers = versions.len() > 1;
    let version = (versions.len() == 1).then(|| versions.into_iter().next().unwrap());
    let markers = version
        .as_ref()
        .map(|_| {
            vec![
                "rollout.record-type".to_owned(),
                "session-meta.cli-version".to_owned(),
            ]
        })
        .unwrap_or_default();
    Ok(NativeArtifactProbe {
        family: "codex".to_owned(),
        platform: platform_id().to_owned(),
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
    use crate::adapter::{CompatibilityClass, SupportCatalog};

    fn write_rollout(root: &Path, day: &str, name: &str, version: &str) {
        let directory = root.join("sessions/2026/08").join(day);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join(name),
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"cli_version\":\"{version}\"}}}}\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn latest_rollout_supplies_declared_markers_without_historical_conflict() {
        let root = TempDir::new().unwrap();
        write_rollout(root.path(), "20", "rollout-001.jsonl", "0.97.0");
        write_rollout(root.path(), "21", "rollout-002.jsonl", "0.98.0");
        let probe = probe_codex_native_artifact(&[root.path().to_path_buf()]).unwrap();
        assert_eq!(probe.version.as_deref(), Some("0.98.0"));
        assert_eq!(
            probe.markers,
            vec!["rollout.record-type", "session-meta.cli-version"]
        );
        assert!(!probe.contradictory_markers);

        let catalog =
            SupportCatalog::new([crate::codex::verified_support_release().unwrap()]).unwrap();
        let decision = catalog.classify(&probe).unwrap();
        assert_eq!(
            decision.compatibility_class(),
            CompatibilityClass::RecognizedUnverified
        );
        assert!(!decision.permissions().catalog);
        assert!(!decision.permissions().durable);
    }

    #[test]
    fn configured_roots_with_different_current_versions_are_contradictory() {
        let first = TempDir::new().unwrap();
        let second = TempDir::new().unwrap();
        write_rollout(first.path(), "21", "rollout-001.jsonl", "0.98.0");
        write_rollout(second.path(), "21", "rollout-001.jsonl", "0.99.0");
        let probe =
            probe_codex_native_artifact(&[first.path().to_path_buf(), second.path().to_path_buf()])
                .unwrap();
        assert!(probe.version.is_none());
        assert!(probe.markers.is_empty());
        assert!(probe.contradictory_markers);
    }

    #[test]
    fn malformed_and_oversized_heads_fail_without_path_disclosure() {
        let malformed = TempDir::new().unwrap();
        let directory = malformed.path().join("sessions/2026/08/21");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("rollout-private.jsonl"), b"{\n").unwrap();
        let error = probe_codex_native_artifact(&[malformed.path().to_path_buf()]).unwrap_err();
        let message = error.to_string();
        assert!(!message.contains("private"));
        assert!(!message.contains(malformed.path().to_string_lossy().as_ref()));

        let oversized = TempDir::new().unwrap();
        let directory = oversized.path().join("sessions/2026/08/21");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("rollout-secret.jsonl"),
            vec![b'x'; MAX_HEAD_PREFIX_BYTES + 1],
        )
        .unwrap();
        let error = probe_codex_native_artifact(&[oversized.path().to_path_buf()]).unwrap_err();
        let message = error.to_string();
        assert!(!message.contains("secret"));
        assert!(!message.contains(oversized.path().to_string_lossy().as_ref()));
    }

    #[cfg(unix)]
    #[test]
    fn rollout_symlinks_are_rejected() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().unwrap();
        let directory = root.path().join("sessions/2026/08/21");
        fs::create_dir_all(&directory).unwrap();
        let target = root.path().join("outside.jsonl");
        fs::write(
            &target,
            b"{\"type\":\"session_meta\",\"payload\":{\"cli_version\":\"0.98.0\"}}\n",
        )
        .unwrap();
        symlink(target, directory.join("rollout-link.jsonl")).unwrap();
        assert!(probe_codex_native_artifact(&[root.path().to_path_buf()]).is_err());
    }
}
