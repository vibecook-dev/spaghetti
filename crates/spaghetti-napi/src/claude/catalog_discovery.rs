//! Claude Code catalog discovery.
//!
//! Claude spells catalog membership on the filesystem, so a complete catalog
//! needs no transcript bytes at all:
//!
//! ```text
//! <projects>/<project-slug>/                     -> a project
//! <projects>/<project-slug>/<uuid>.jsonl         -> a transcript-backed session
//! <projects>/<project-slug>/<uuid>/              -> a session with side data
//!                                                   (subagents, workflows, tool
//!                                                   results) and possibly no
//!                                                   transcript of its own
//! <projects>/<project-slug>/sessions-index.json  -> sessions the agent knows about,
//!                                                   including ones with no transcript
//! ```
//!
//! A third of the projects on a real machine are discoverable only through
//! that session-directory form, so ignoring it would silently lose them.
//!
//! `<project-slug>` is byte-identical to the `native_project_key` the Claude
//! decoder derives from the same path, so a discovered row and its later
//! transcript-backed history share one identity.
//!
//! Nested `subagents/agent-*.jsonl` transcripts belong to their parent session
//! and are deliberately not catalog members.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::adapter::{
    AdapterError, AdapterErrorClass, AssociationQuality, CatalogDiscoveryLimits,
    DiscoveredAssociationConflict, DiscoveredProject, DiscoveredSession, ProjectAssociationBasis,
    SourceCatalogDiscovery, SourceInstance,
};
use crate::source::{
    read_stable_file_confined, DirectoryEntryKind, DirectoryScan, DirectorySelection,
    DirectorySnapshot, DirectorySnapshotConfig, SourceDriverError, StableRead,
};

const PROJECTS_ROOT: &str = "projects";
const SESSION_INDEX_NAME: &str = "sessions-index.json";

/// One `sessions-index.json` entry, reduced to the catalog-relevant fields.
struct IndexEntry {
    session_id: String,
    title: Option<String>,
    created: Option<String>,
    modified: Option<String>,
    message_count: Option<u64>,
    project_path: Option<String>,
}

pub(super) fn discover(
    instance: &SourceInstance,
    limits: &CatalogDiscoveryLimits,
) -> Result<SourceCatalogDiscovery, AdapterError> {
    let root = instance.root(PROJECTS_ROOT)?;
    let snapshot = DirectorySnapshot::new(DirectorySnapshotConfig {
        max_entries: limits.max_entries,
        max_entries_per_directory: limits.max_entries,
        max_depth: limits.max_depth.min(2).max(2),
    })
    .map_err(|error| catalog_scan_error("claude_catalog_bounds", error))?;

    let scan = snapshot
        .scan(root, None, &catalog_selection)
        .map_err(|error| catalog_scan_error("claude_catalog_scan", error))?;
    let checkpoint = match scan {
        DirectoryScan::Snapshot { checkpoint, .. } => checkpoint,
        DirectoryScan::Unavailable => {
            return Ok(SourceCatalogDiscovery::degraded(
                "claude projects root is unavailable",
            ));
        }
        DirectoryScan::RetryTransient => {
            return Ok(SourceCatalogDiscovery::degraded(
                "claude projects root could not be read in this pass",
            ));
        }
    };

    let mut projects: BTreeMap<String, DiscoveredProject> = BTreeMap::new();
    let mut sessions: BTreeMap<String, DiscoveredSession> = BTreeMap::new();
    let mut conflicts = Vec::new();
    let mut degraded_reason = None;
    let mut index_documents = Vec::new();

    for entry in checkpoint.entries.values() {
        let components = path_components(&entry.display_path);
        let [slug, name] = components.as_slice() else {
            continue;
        };
        if slug.is_empty() {
            continue;
        }
        projects
            .entry(slug.clone())
            .or_insert_with(|| DiscoveredProject {
                native_project_key: slug.clone(),
                display_name: None,
                display_path: None,
            });

        // A session directory names a session whose transcript may or may not
        // exist beside it. Discoverability and transcript availability are
        // different facts, so it is admitted either way.
        if entry.kind == DirectoryEntryKind::Directory {
            if !is_uuid(name) {
                continue;
            }
            insert_session(
                &mut sessions,
                &mut conflicts,
                session_row(slug, name, &entry.display_path, None),
            );
            continue;
        }

        if name == SESSION_INDEX_NAME {
            index_documents.push((slug.clone(), entry.display_path.clone()));
            continue;
        }
        let Some(session_id) = name.strip_suffix(".jsonl").filter(|id| is_uuid(id)) else {
            continue;
        };
        insert_session(
            &mut sessions,
            &mut conflicts,
            session_row(
                slug,
                session_id,
                &entry.display_path,
                Some((entry.size_bytes, modified_ms(entry.modified_ns))),
            ),
        );
    }

    for (slug, relative_path) in index_documents {
        match read_index_document(root, &relative_path, limits.max_document_bytes) {
            Ok(entries) => {
                for entry in entries {
                    if let Some(project) = projects.get_mut(&slug) {
                        if project.display_path.is_none() {
                            project.display_path.clone_from(&entry.project_path);
                        }
                    }
                    merge_index_entry(&mut sessions, &mut conflicts, &slug, &relative_path, entry);
                }
            }
            Err(reason) => {
                degraded_reason.get_or_insert(reason);
            }
        }
    }

    Ok(SourceCatalogDiscovery {
        projects: projects.into_values().collect(),
        sessions: sessions.into_values().collect(),
        conflicts,
        degraded_reason,
    })
}

/// One transcript-backed or directory-only session row.
fn session_row(
    slug: &str,
    session_id: &str,
    provenance: &str,
    transcript: Option<(u64, i64)>,
) -> DiscoveredSession {
    DiscoveredSession {
        native_session_key: session_id.to_string(),
        native_session_id: Some(session_id.to_string()),
        native_project_key: slug.to_string(),
        association_basis: ProjectAssociationBasis::SessionDirectory,
        association_quality: AssociationQuality::Exact,
        association_provenance: provenance.to_string(),
        title: None,
        native_created_at: None,
        native_updated_at: None,
        native_message_count: None,
        transcript_locator: transcript.map(|_| provenance.to_string()),
        source_size_bytes: transcript.map(|(size, _)| size),
        source_modified_ms: transcript.map(|(_, modified)| modified),
        transcript_present: transcript.is_some(),
    }
}

/// Depth-2 selection: project directories and, inside each, the transcripts,
/// session directories, and session index. Session directories are recorded
/// but never descended into, so a deep `subagents/` or `workflows/` tree
/// costs nothing.
fn catalog_selection(path: &Path, kind: DirectoryEntryKind) -> DirectorySelection {
    let components = path_components(&path.to_string_lossy());
    match (kind, components.len()) {
        (DirectoryEntryKind::Directory, 1) => DirectorySelection::Recurse,
        (DirectoryEntryKind::Directory, 2) if is_uuid(&components[1]) => {
            DirectorySelection::Include
        }
        (DirectoryEntryKind::File, 2) => {
            let name = &components[1];
            if name == SESSION_INDEX_NAME || name.ends_with(".jsonl") {
                DirectorySelection::Include
            } else {
                DirectorySelection::Ignore
            }
        }
        _ => DirectorySelection::Ignore,
    }
}

/// Index evidence refines a transcript-backed session and *introduces* one
/// that has no transcript. It never overwrites the directory association,
/// which is exact.
fn merge_index_entry(
    sessions: &mut BTreeMap<String, DiscoveredSession>,
    conflicts: &mut Vec<DiscoveredAssociationConflict>,
    slug: &str,
    provenance: &str,
    entry: IndexEntry,
) {
    if let Some(existing) = sessions.get_mut(&entry.session_id) {
        existing.title = existing.title.take().or(entry.title);
        existing.native_created_at = existing.native_created_at.take().or(entry.created);
        existing.native_updated_at = existing.native_updated_at.take().or(entry.modified);
        existing.native_message_count = existing.native_message_count.or(entry.message_count);
        if existing.native_project_key != slug {
            conflicts.push(DiscoveredAssociationConflict {
                native_session_key: entry.session_id,
                competing_native_project_key: slug.to_string(),
                basis: ProjectAssociationBasis::NativeProjectIndex,
                provenance: provenance.to_string(),
            });
        }
        return;
    }
    insert_session(
        sessions,
        conflicts,
        DiscoveredSession {
            native_session_key: entry.session_id.clone(),
            native_session_id: Some(entry.session_id),
            native_project_key: slug.to_string(),
            association_basis: ProjectAssociationBasis::NativeProjectIndex,
            association_quality: AssociationQuality::NativeClaimed,
            association_provenance: provenance.to_string(),
            title: entry.title,
            native_created_at: entry.created,
            native_updated_at: entry.modified,
            native_message_count: entry.message_count,
            transcript_locator: None,
            source_size_bytes: None,
            source_modified_ms: None,
            transcript_present: false,
        },
    );
}

/// Insert a session, keeping the higher-quality association and recording the
/// loser as an explicit conflict when the projects disagree.
///
/// A session commonly appears twice inside one project — once as its side-data
/// directory and once as its transcript file — so agreeing evidence merges
/// rather than racing: whichever arrives second still contributes its
/// transcript.
fn insert_session(
    sessions: &mut BTreeMap<String, DiscoveredSession>,
    conflicts: &mut Vec<DiscoveredAssociationConflict>,
    candidate: DiscoveredSession,
) {
    match sessions.get_mut(&candidate.native_session_key) {
        None => {
            sessions.insert(candidate.native_session_key.clone(), candidate);
        }
        Some(existing) => {
            if existing.native_project_key == candidate.native_project_key {
                if candidate.transcript_present && !existing.transcript_present {
                    existing.transcript_present = true;
                    existing.transcript_locator = candidate.transcript_locator;
                    existing.source_size_bytes = candidate.source_size_bytes;
                    existing.source_modified_ms = candidate.source_modified_ms;
                    existing.association_provenance = candidate.association_provenance;
                }
                return;
            }
            if candidate.association_quality > existing.association_quality {
                conflicts.push(DiscoveredAssociationConflict {
                    native_session_key: existing.native_session_key.clone(),
                    competing_native_project_key: existing.native_project_key.clone(),
                    basis: existing.association_basis,
                    provenance: existing.association_provenance.clone(),
                });
                *existing = candidate;
            } else {
                conflicts.push(DiscoveredAssociationConflict {
                    native_session_key: candidate.native_session_key,
                    competing_native_project_key: candidate.native_project_key,
                    basis: candidate.association_basis,
                    provenance: candidate.association_provenance,
                });
            }
        }
    }
}

fn read_index_document(
    root: &Path,
    relative_path: &str,
    max_bytes: u64,
) -> Result<Vec<IndexEntry>, String> {
    let bounded = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    let read = read_stable_file_confined(root, Path::new(relative_path), bounded)
        .map_err(|_| "claude session index could not be read".to_string())?;
    let bytes = match read {
        StableRead::Stable { bytes, .. } => bytes,
        StableRead::Missing => return Ok(Vec::new()),
        StableRead::Oversized(_) => {
            return Err("claude session index exceeds the catalog document bound".to_string())
        }
        StableRead::Unstable => {
            return Err("claude session index changed during the catalog pass".to_string())
        }
    };
    let document: Value = serde_json::from_slice(&bytes)
        .map_err(|_| "claude session index is not valid JSON".to_string())?;
    let Some(entries) = document.get("entries").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    Ok(entries
        .iter()
        .filter_map(|entry| {
            let session_id = text(entry.get("sessionId")).filter(|id| is_uuid(id))?;
            Some(IndexEntry {
                session_id,
                title: text(entry.get("summary")).or_else(|| text(entry.get("firstPrompt"))),
                created: text(entry.get("created")),
                modified: text(entry.get("modified")),
                message_count: entry.get("messageCount").and_then(Value::as_u64),
                project_path: text(entry.get("projectPath")),
            })
        })
        .collect())
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

fn modified_ms(modified_ns: i128) -> i64 {
    i64::try_from(modified_ns / 1_000_000).unwrap_or(i64::MAX)
}

fn is_uuid(value: &str) -> bool {
    let lengths = [8, 4, 4, 4, 12];
    let mut parts = value.split('-');
    lengths.iter().all(|length| {
        parts.next().is_some_and(|part| {
            part.len() == *length && part.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    }) && parts.next().is_none()
}

fn catalog_scan_error(code: &'static str, error: SourceDriverError) -> AdapterError {
    AdapterError::new(AdapterErrorClass::Transient, code, error.to_string())
}
