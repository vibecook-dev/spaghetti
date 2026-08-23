//! The observer's open request and the identity it settles before any I/O.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::ObserverError;

/// Default bounds. A caller may lower them; it cannot disable boundedness.
const DEFAULT_MAX_QUEUED_EVENTS: u32 = 4_096;
const DEFAULT_MAX_QUEUED_BYTES: u32 = 32 * 1024 * 1024;
const DEFAULT_POLL_INTERVAL_MS: u32 = 250;
const MAX_QUEUED_EVENTS_CEILING: u32 = 262_144;
const MAX_QUEUED_BYTES_CEILING: u32 = 512 * 1024 * 1024;

/// Open one store-free observer over a single native session tree.
///
/// The locator is adapter-specific by design: for `claude-code` it is the path
/// of the root transcript, which need not exist yet. Nothing here opens a
/// database, configures a whole-adapter host, or enumerates unrelated roots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ObserveSessionRequest {
    /// Currently `claude-code`.
    pub adapter_id: String,
    /// The agent's data root, e.g. the user's `.claude` directory. Must exist.
    pub agent_root: String,
    /// Absolute path of the root session transcript. It may not exist yet;
    /// attaching before native creation is supported and produces an empty
    /// complete bootstrap with an active watch.
    pub transcript_path: String,
    /// When supplied, must equal the session id implied by the locator.
    /// A mismatch fails the attach rather than emitting a provisional identity.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub native_session_id: Option<String>,
    /// Follow declared child/sidecar relations. Defaults to true.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub include_descendants: Option<bool>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub max_queued_events: Option<u32>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub max_queued_bytes: Option<u32>,
    /// Bounded reconciliation fallback for filesystem notifications, which are
    /// hints and are allowed to be lost or coalesced.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub poll_interval_ms: Option<u32>,
}

/// Caller-visible bounds after clamping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QueueLimits {
    pub max_events: usize,
    pub max_bytes: usize,
}

/// A validated request with the root identity already settled.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedRequest {
    /// Canonicalized agent root.
    pub agent_root: PathBuf,
    /// `projects`-relative directory name of the owning project.
    pub project_slug: String,
    /// Native session id, taken from the transcript file stem.
    pub native_session_id: String,
    pub include_descendants: bool,
    pub queue: QueueLimits,
    pub poll_interval: std::time::Duration,
}

impl ResolvedRequest {
    /// `projects`-relative path of the root transcript.
    pub(crate) fn root_transcript_relative(&self) -> PathBuf {
        PathBuf::from(&self.project_slug).join(format!("{}.jsonl", self.native_session_id))
    }
}

impl ObserveSessionRequest {
    pub(crate) fn resolve(&self, adapter_id: &str) -> Result<ResolvedRequest, ObserverError> {
        if self.adapter_id != adapter_id {
            return Err(ObserverError::UnsupportedAdapter(self.adapter_id.clone()));
        }
        let agent_root = std::fs::canonicalize(&self.agent_root).map_err(|error| {
            ObserverError::InvalidRequest(format!("agent_root is not readable: {error}"))
        })?;
        if !agent_root.is_dir() {
            return Err(ObserverError::InvalidRequest(
                "agent_root must be a directory".to_string(),
            ));
        }

        let (project_slug, native_session_id) =
            split_transcript_locator(&agent_root, Path::new(&self.transcript_path))?;
        if let Some(declared) = &self.native_session_id {
            if declared != &native_session_id {
                return Err(ObserverError::InvalidRootIdentity(format!(
                    "declared native_session_id does not match the transcript locator \
                     ({declared} vs {native_session_id})"
                )));
            }
        }

        let max_events = self
            .max_queued_events
            .unwrap_or(DEFAULT_MAX_QUEUED_EVENTS)
            .clamp(1, MAX_QUEUED_EVENTS_CEILING) as usize;
        let max_bytes = self
            .max_queued_bytes
            .unwrap_or(DEFAULT_MAX_QUEUED_BYTES)
            .clamp(1, MAX_QUEUED_BYTES_CEILING) as usize;
        let poll_interval = std::time::Duration::from_millis(u64::from(
            self.poll_interval_ms
                .unwrap_or(DEFAULT_POLL_INTERVAL_MS)
                .clamp(10, 60_000),
        ));

        Ok(ResolvedRequest {
            agent_root,
            project_slug,
            native_session_id,
            include_descendants: self.include_descendants.unwrap_or(true),
            queue: QueueLimits {
                max_events,
                max_bytes,
            },
            poll_interval,
        })
    }
}

/// Resolve symlinks in the part of `path` that exists, keeping the components
/// that do not exist yet.
fn canonicalize_existing_prefix(path: &Path) -> PathBuf {
    let mut suffix = Vec::new();
    let mut cursor = path.to_path_buf();
    loop {
        if let Ok(canonical) = std::fs::canonicalize(&cursor) {
            let mut resolved = canonical;
            for component in suffix.iter().rev() {
                resolved.push(component);
            }
            return resolved;
        }
        let Some(name) = cursor.file_name().map(std::ffi::OsString::from) else {
            return path.to_path_buf();
        };
        suffix.push(name);
        if !cursor.pop() {
            return path.to_path_buf();
        }
    }
}

/// Decompose `<agent_root>/projects/<project>/<session>.jsonl` without
/// requiring the transcript to exist, so an attach before native creation
/// still settles a final identity.
fn split_transcript_locator(
    agent_root: &Path,
    transcript_path: &Path,
) -> Result<(String, String), ObserverError> {
    if !transcript_path.is_absolute() {
        return Err(ObserverError::InvalidRequest(
            "transcript_path must be absolute".to_string(),
        ));
    }
    // Canonicalize the deepest ancestor that exists and re-attach the rest.
    // The session directory may be created after attach, and the agent root is
    // already canonical, so a purely lexical comparison would reject a valid
    // locator whenever the root path traverses a symlink.
    let normalized = canonicalize_existing_prefix(transcript_path);

    let projects_root = agent_root.join("projects");
    let relative = normalized.strip_prefix(&projects_root).map_err(|_| {
        ObserverError::InvalidRequest(format!(
            "transcript_path must live under {}",
            projects_root.display()
        ))
    })?;

    let mut components = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(value) => components.push(
                value
                    .to_str()
                    .ok_or_else(|| {
                        ObserverError::InvalidRequest(
                            "transcript_path contains non-UTF-8 components".to_string(),
                        )
                    })?
                    .to_string(),
            ),
            Component::CurDir => {}
            _ => {
                return Err(ObserverError::InvalidRequest(
                    "transcript_path must not escape the projects root".to_string(),
                ))
            }
        }
    }
    // The declared `session-transcripts` pattern is `*/*.jsonl`: exactly one
    // project directory and one transcript file.
    if components.len() != 2 {
        return Err(ObserverError::InvalidRequest(
            "transcript_path must be <projects>/<project>/<session>.jsonl".to_string(),
        ));
    }
    let file_name = components.pop().expect("two components were verified");
    let project_slug = components.pop().expect("two components were verified");
    let session_id = file_name.strip_suffix(".jsonl").ok_or_else(|| {
        ObserverError::InvalidRequest("transcript_path must end in .jsonl".to_string())
    })?;
    if session_id.is_empty() || project_slug.is_empty() {
        return Err(ObserverError::InvalidRequest(
            "transcript_path names an empty project or session".to_string(),
        ));
    }
    Ok((project_slug, session_id.to_string()))
}
