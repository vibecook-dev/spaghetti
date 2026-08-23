//! Codex catalog discovery.
//!
//! Codex spells nothing structural about projects on the filesystem: rollout
//! files live under date directories and the working directory is declared by
//! the first `session_meta` record. Discovery therefore reads a bounded head
//! of each rollout — the same evidence the Phase 0 census used — and never the
//! transcript body.
//!
//! ```text
//! <sessions>/**/rollout-*.jsonl  -> one session; its head declares id and cwd
//! ```
//!
//! The project key is `encode_project_key(cwd)`, byte-identical to what the
//! Codex decoder derives from the same record.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Map, Value};

use super::adapter::{normalize_session_meta, CodexSessionMeta};
use crate::adapter::{
    AdapterError, AdapterErrorClass, AssociationQuality, CatalogDiscoveryLimits,
    DiscoveredProject, DiscoveredSession, ProjectAssociationBasis, SourceCatalogDiscovery,
    SourceInstance,
};
use crate::source::{
    read_prefix_confined, read_stable_file_confined, DirectoryEntryKind, DirectoryScan, DirectorySelection,
    DirectorySnapshot, DirectorySnapshotConfig, SourceDriverError, StableRead,
};

const SESSIONS_ROOT: &str = "sessions";

pub(super) fn discover(
    instance: &SourceInstance,
    limits: &CatalogDiscoveryLimits,
) -> Result<SourceCatalogDiscovery, AdapterError> {
    let root = instance.root(SESSIONS_ROOT)?;
    let snapshot = DirectorySnapshot::new(DirectorySnapshotConfig {
        max_entries: limits.max_entries,
        max_entries_per_directory: limits.max_entries,
        max_depth: limits.max_depth,
    })
    .map_err(|error| scan_error("codex_catalog_bounds", error))?;

    let checkpoint = match snapshot
        .scan(root, None, &rollout_selection)
        .map_err(|error| scan_error("codex_catalog_scan", error))?
    {
        DirectoryScan::Snapshot { checkpoint, .. } => checkpoint,
        DirectoryScan::Unavailable => {
            return Ok(SourceCatalogDiscovery::degraded(
                "codex sessions root is unavailable",
            ));
        }
        DirectoryScan::RetryTransient => {
            return Ok(SourceCatalogDiscovery::degraded(
                "codex sessions root could not be read in this pass",
            ));
        }
    };

    let mut projects: BTreeMap<String, DiscoveredProject> = BTreeMap::new();
    let mut sessions = Vec::new();
    let mut unreadable_heads = 0_u64;

    for entry in checkpoint.entries.values() {
        if entry.kind != DirectoryEntryKind::File {
            continue;
        }
        let Some(head) = read_session_head(root, &entry.display_path, limits.max_head_bytes) else {
            unreadable_heads = unreadable_heads.saturating_add(1);
            continue;
        };
        let native_project_key = head.native_project_key.clone();
        projects
            .entry(native_project_key.clone())
            .or_insert_with(|| DiscoveredProject {
                native_project_key: native_project_key.clone(),
                display_name: None,
                display_path: Some(head.cwd.clone()),
            });
        sessions.push(DiscoveredSession {
            native_session_key: head.session_id.clone(),
            native_session_id: Some(head.session_id),
            native_project_key,
            association_basis: ProjectAssociationBasis::RolloutHeader,
            association_quality: AssociationQuality::NativeClaimed,
            association_provenance: entry.display_path.clone(),
            title: None,
            native_created_at: head.session_time,
            native_updated_at: None,
            native_message_count: None,
            transcript_locator: Some(entry.display_path.clone()),
            source_size_bytes: Some(entry.size_bytes),
            source_modified_ms: Some(modified_ms(entry.modified_ns)),
            transcript_present: true,
        });
    }

    Ok(SourceCatalogDiscovery {
        projects: projects.into_values().collect(),
        sessions,
        conflicts: Vec::new(),
        degraded_reason: (unreadable_heads > 0).then(|| {
            format!("{unreadable_heads} codex rollout heads did not declare a session identity")
        }),
    })
}

fn rollout_selection(path: &Path, kind: DirectoryEntryKind) -> DirectorySelection {
    match kind {
        DirectoryEntryKind::Directory => DirectorySelection::Recurse,
        DirectoryEntryKind::File => {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                DirectorySelection::Include
            } else {
                DirectorySelection::Ignore
            }
        }
    }
}

/// Read the first complete JSONL record of a rollout and accept it only when
/// it is the `session_meta` that declares identity. A truncated trailing line
/// inside the bound is discarded rather than guessed at.
fn read_session_head(
    root: &Path,
    relative_path: &str,
    max_head_bytes: u64,
) -> Option<CodexSessionMeta> {
    let bounded = usize::try_from(max_head_bytes).unwrap_or(usize::MAX);
    let read = read_stable_file_confined(root, Path::new(relative_path), bounded).ok()?;
    let bytes = match read {
        StableRead::Stable { bytes, .. } => bytes,
        // A rollout larger than the head bound still yields its head: the
        // driver reports oversize, so fall back to a prefix read.
        StableRead::Oversized(_) => {
            read_prefix_confined(root, Path::new(relative_path), bounded).ok()??
        }
        StableRead::Missing | StableRead::Unstable => return None,
    };
    let line = bytes.split(|byte| *byte == b'\n').next()?;
    let record: Map<String, Value> = serde_json::from_slice(line).ok()?;
    if record.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    normalize_session_meta(&record).ok()
}

fn scan_error(code: &'static str, error: SourceDriverError) -> AdapterError {
    AdapterError::new(AdapterErrorClass::Transient, code, error.to_string())
}

fn modified_ms(modified_ns: i128) -> i64 {
    i64::try_from(modified_ns / 1_000_000).unwrap_or(i64::MAX)
}
