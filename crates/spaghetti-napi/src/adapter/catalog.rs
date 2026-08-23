//! RFC 012B catalog discovery contract.
//!
//! Catalog membership is a first-class fact: a project or session is
//! *discoverable* from bounded native evidence (index documents, directory
//! membership, one bounded record head) long before its transcript is decoded.
//! Adapters own that native interpretation; they never touch SQLite, identity
//! digests, or readiness. The engine turns what an adapter returns here into
//! durable catalog rows.
//!
//! Everything an adapter returns is *relative* to a declared source root. No
//! absolute native path leaves this contract.

use std::fmt;

/// Bounds a catalog discovery pass may not exceed. The engine owns these; an
/// adapter may read less but never more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogDiscoveryLimits {
    /// Maximum native directory entries retained by one enumeration.
    pub max_entries: usize,
    /// Maximum enumeration depth below a declared root.
    pub max_depth: usize,
    /// Maximum bytes read from any one native metadata document.
    pub max_document_bytes: u64,
    /// Maximum bytes read from the head of any one transcript-shaped object.
    pub max_head_bytes: u64,
}

impl Default for CatalogDiscoveryLimits {
    fn default() -> Self {
        Self {
            max_entries: 200_000,
            max_depth: 8,
            max_document_bytes: 8 * 1024 * 1024,
            max_head_bytes: 64 * 1024,
        }
    }
}

/// Native evidence that relates one session to one project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProjectAssociationBasis {
    /// A native project-level session index named the session.
    NativeProjectIndex,
    /// The session object lives inside the project's native directory.
    SessionDirectory,
    /// A bounded record head declared the working directory.
    RolloutHeader,
    /// A decoded transcript field declared the working directory.
    TranscriptCwd,
}

impl ProjectAssociationBasis {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NativeProjectIndex => "native_project_index",
            Self::SessionDirectory => "session_directory",
            Self::RolloutHeader => "rollout_header",
            Self::TranscriptCwd => "transcript_cwd",
        }
    }
}

impl fmt::Display for ProjectAssociationBasis {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// How well the association evidence is known. Ordering is precedence:
/// `Exact` defeats `NativeClaimed` defeats `Derived`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AssociationQuality {
    Derived,
    NativeClaimed,
    Exact,
}

impl AssociationQuality {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::NativeClaimed => "native_claimed",
            Self::Derived => "derived",
        }
    }
}

/// One discoverable project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredProject {
    /// Adapter-declared stable native project key. This is the *same* byte
    /// string the adapter's decoder passes to `EntityKey::native(.., "project",
    /// ..)`, so a catalog row and its later transcript-backed history converge
    /// on one identity.
    pub native_project_key: String,
    /// Human display label when the native surface declares one.
    pub display_name: Option<String>,
    /// Native working directory or equivalent. Treated as LOCAL-sensitive.
    pub display_path: Option<String>,
}

/// One discoverable session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredSession {
    /// Adapter-declared stable native session key, matching the decoder's
    /// `EntityKey::native(.., "session", ..)` material.
    pub native_session_key: String,
    /// Native session identifier as the agent spells it, when one exists.
    pub native_session_id: Option<String>,
    /// The project this session is associated with.
    pub native_project_key: String,
    pub association_basis: ProjectAssociationBasis,
    pub association_quality: AssociationQuality,
    /// Root-relative locator of the evidence that produced this association.
    pub association_provenance: String,
    pub title: Option<String>,
    pub native_created_at: Option<String>,
    pub native_updated_at: Option<String>,
    pub native_message_count: Option<u64>,
    /// Root-relative transcript locator when a transcript object exists.
    pub transcript_locator: Option<String>,
    /// Size of the transcript object in bytes, when one exists.
    pub source_size_bytes: Option<u64>,
    /// Modification time of the transcript object in epoch milliseconds.
    pub source_modified_ms: Option<i64>,
    /// True when a transcript object exists on the native surface. False for
    /// index-only or metadata-only sessions — discoverability and transcript
    /// availability are different facts.
    pub transcript_present: bool,
}

/// One competing association that lost to the selected one. Conflicts are
/// surfaced, never silently merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredAssociationConflict {
    pub native_session_key: String,
    pub competing_native_project_key: String,
    pub basis: ProjectAssociationBasis,
    pub provenance: String,
}

/// Everything one catalog discovery pass found for one source instance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceCatalogDiscovery {
    pub projects: Vec<DiscoveredProject>,
    pub sessions: Vec<DiscoveredSession>,
    pub conflicts: Vec<DiscoveredAssociationConflict>,
    /// Set when the pass could not read the complete native surface. The rows
    /// it did find stay valid and the source is published as degraded.
    pub degraded_reason: Option<String>,
}

impl SourceCatalogDiscovery {
    pub fn degraded(reason: impl Into<String>) -> Self {
        Self {
            degraded_reason: Some(reason.into()),
            ..Self::default()
        }
    }
}
