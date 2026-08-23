//! Grok catalog discovery.
//!
//! Grok spells both project and session on the filesystem, so membership costs
//! one bounded directory walk:
//!
//! ```text
//! <sessions>/<percent-encoded-cwd>/<session-id>/summary.json
//! <sessions>/<percent-encoded-cwd>/<session-id>/chat_history.jsonl
//! ```
//!
//! `summary.json` is small and authoritative for the title and timestamps, so
//! discovery reads it; the transcript is never opened. The project key is
//! `encode_project_key(percent_decode(<percent-encoded-cwd>))`, refined by
//! `summary.json`'s `info.cwd` exactly as the Grok decoder does.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use super::adapter::{grok_project_key, grok_percent_decode};
use crate::adapter::{
    AdapterError, AdapterErrorClass, AssociationQuality, CatalogDiscoveryLimits,
    DiscoveredProject, DiscoveredSession, ProjectAssociationBasis, SourceCatalogDiscovery,
    SourceInstance,
};
use crate::source::{
    read_stable_file_confined, DirectoryEntryKind, DirectoryScan, DirectorySelection,
    DirectorySnapshot, DirectorySnapshotConfig, SourceDriverError, StableRead,
};

const SESSIONS_ROOT: &str = "sessions";
const SUMMARY_NAME: &str = "summary.json";
const TRANSCRIPT_NAME: &str = "chat_history.jsonl";

pub(super) fn discover(
    instance: &SourceInstance,
    limits: &CatalogDiscoveryLimits,
) -> Result<SourceCatalogDiscovery, AdapterError> {
    let root = instance.root(SESSIONS_ROOT)?;
    let snapshot = DirectorySnapshot::new(DirectorySnapshotConfig {
        max_entries: limits.max_entries,
        max_entries_per_directory: limits.max_entries,
        max_depth: limits.max_depth.max(3),
    })
    .map_err(|error| scan_error("grok_catalog_bounds", error))?;

    let checkpoint = match snapshot
        .scan(root, None, &session_selection)
        .map_err(|error| scan_error("grok_catalog_scan", error))?
    {
        DirectoryScan::Snapshot { checkpoint, .. } => checkpoint,
        DirectoryScan::Unavailable => {
            return Ok(SourceCatalogDiscovery::degraded(
                "grok sessions root is unavailable",
            ));
        }
        DirectoryScan::RetryTransient => {
            return Ok(SourceCatalogDiscovery::degraded(
                "grok sessions root could not be read in this pass",
            ));
        }
    };

    let mut projects: BTreeMap<String, DiscoveredProject> = BTreeMap::new();
    let mut sessions: BTreeMap<String, DiscoveredSession> = BTreeMap::new();
    let mut undecodable_roots = 0_u64;

    for entry in checkpoint.entries.values() {
        if entry.kind != DirectoryEntryKind::File {
            continue;
        }
        let components = path_components(&entry.display_path);
        let [encoded_cwd, session_dir, file_name] = components.as_slice() else {
            continue;
        };
        let Some(cwd) = grok_percent_decode(encoded_cwd) else {
            undecodable_roots = undecodable_roots.saturating_add(1);
            continue;
        };
        if cwd.trim().is_empty() || session_dir.trim().is_empty() {
            continue;
        }

        let session = sessions
            .entry(session_dir.clone())
            .or_insert_with(|| DiscoveredSession {
                native_session_key: session_dir.clone(),
                native_session_id: Some(session_dir.clone()),
                native_project_key: grok_project_key(&cwd),
                association_basis: ProjectAssociationBasis::SessionDirectory,
                association_quality: AssociationQuality::Exact,
                association_provenance: format!("{encoded_cwd}/{session_dir}"),
                title: None,
                native_created_at: None,
                native_updated_at: None,
                native_message_count: None,
                transcript_locator: None,
                source_size_bytes: None,
                source_modified_ms: None,
                transcript_present: false,
            });

        if file_name == TRANSCRIPT_NAME {
            session.transcript_present = true;
            session.transcript_locator = Some(entry.display_path.clone());
            session.source_size_bytes = Some(entry.size_bytes);
            session.source_modified_ms = Some(modified_ms(entry.modified_ns));
        } else if file_name == SUMMARY_NAME {
            apply_summary(
                session,
                read_summary(root, &entry.display_path, limits.max_document_bytes),
            );
        }

        let native_project_key = session.native_project_key.clone();
        let display_path = cwd.clone();
        projects
            .entry(native_project_key.clone())
            .or_insert_with(|| DiscoveredProject {
                native_project_key,
                display_name: None,
                display_path: Some(display_path),
            });
    }

    Ok(SourceCatalogDiscovery {
        projects: projects.into_values().collect(),
        sessions: sessions.into_values().collect(),
        conflicts: Vec::new(),
        degraded_reason: (undecodable_roots > 0).then(|| {
            format!("{undecodable_roots} grok project directories are not decodable coordinates")
        }),
    })
}

/// Depth-3 selection: `<cwd>/<session>/{summary.json,chat_history.jsonl}`.
fn session_selection(path: &Path, kind: DirectoryEntryKind) -> DirectorySelection {
    let components = path_components(&path.to_string_lossy());
    match kind {
        DirectoryEntryKind::Directory if components.len() <= 2 => DirectorySelection::Recurse,
        DirectoryEntryKind::File if components.len() == 3 => {
            let name = &components[2];
            if name == SUMMARY_NAME || name == TRANSCRIPT_NAME {
                DirectorySelection::Include
            } else {
                DirectorySelection::Ignore
            }
        }
        _ => DirectorySelection::Ignore,
    }
}

fn apply_summary(session: &mut DiscoveredSession, summary: Option<Value>) {
    let Some(summary) = summary else { return };
    if let Some(info) = summary.get("info") {
        if let Some(cwd) = text(info.get("cwd")) {
            session.native_project_key = grok_project_key(&cwd);
        }
        if let Some(id) = text(info.get("id")) {
            session.native_session_id = Some(id);
        }
    }
    session.title = text(summary.get("generated_title"))
        .or_else(|| text(summary.get("session_summary")))
        .or_else(|| session.title.take());
    session.native_created_at = text(summary.get("created_at")).or_else(|| session.native_created_at.take());
    session.native_updated_at = text(summary.get("updated_at"))
        .or_else(|| text(summary.get("last_active_at")))
        .or_else(|| session.native_updated_at.take());
}

fn read_summary(root: &Path, relative_path: &str, max_bytes: u64) -> Option<Value> {
    let bounded = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    let read = read_stable_file_confined(root, Path::new(relative_path), bounded).ok()?;
    let StableRead::Stable { bytes, .. } = read else {
        return None;
    };
    serde_json::from_slice(&bytes).ok()
}

fn text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn path_components(display_path: &str) -> Vec<String> {
    display_path
        .split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .map(str::to_owned)
        .collect()
}

fn scan_error(code: &'static str, error: SourceDriverError) -> AdapterError {
    AdapterError::new(AdapterErrorClass::Transient, code, error.to_string())
}

fn modified_ms(modified_ns: i128) -> i64 {
    i64::try_from(modified_ns / 1_000_000).unwrap_or(i64::MAX)
}
