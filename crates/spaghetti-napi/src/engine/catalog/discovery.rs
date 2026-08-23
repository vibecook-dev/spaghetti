//! One bounded catalog discovery pass over one configured source.
//!
//! This module is pure with respect to the database: it drives the adapter,
//! derives identity, and hands [`SourceScan`] to [`super::store`] to commit.
//! Keeping the I/O and the transaction apart is what lets a rescan run on a
//! supervisor thread without holding the writer.

use crate::adapter::{
    AdapterError, AgentAdapter, CanonicalEntityKey, CanonicalSourceInstanceKey,
    CatalogDiscoveryLimits, EntityKey, SourceInstance,
};
use crate::core::timefmt::epoch_ms_to_iso8601;

use super::super::EngineError;

/// A project row ready to commit.
pub(crate) struct ScannedProject {
    pub(crate) project_key: Vec<u8>,
    pub(crate) external_ref: [u8; 32],
    pub(crate) native_project_key: String,
    pub(crate) display_name: Option<String>,
    pub(crate) display_path: Option<String>,
}

/// A session row ready to commit.
pub(crate) struct ScannedSession {
    pub(crate) session_key: Vec<u8>,
    pub(crate) project_key: Vec<u8>,
    pub(crate) external_ref: [u8; 32],
    pub(crate) native_session_key: String,
    pub(crate) native_session_id: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) association_basis: &'static str,
    pub(crate) association_quality: &'static str,
    pub(crate) association_provenance: String,
    pub(crate) native_created_at: Option<String>,
    pub(crate) native_updated_at: Option<String>,
    pub(crate) native_message_count: Option<u64>,
    pub(crate) transcript_present: bool,
    pub(crate) transcript_locator: Option<String>,
    pub(crate) source_size_bytes: Option<u64>,
    pub(crate) source_modified_ms: Option<i64>,
    /// Ordering key for activity-sorted pages. Prefers native times, falls
    /// back to the transcript's modification time, and is empty when the
    /// source declares no time at all — an unknown time never sorts as epoch.
    pub(crate) sort_time: String,
}

/// A losing association, retained so the conflict stays visible.
pub(crate) struct ScannedConflict {
    pub(crate) session_key: Vec<u8>,
    pub(crate) competing_native_project_key: String,
    pub(crate) basis: &'static str,
    pub(crate) provenance: String,
}

/// Everything one pass found, with identity already derived.
pub(crate) struct SourceScan {
    pub(crate) source_instance_id: u64,
    pub(crate) adapter_id: String,
    pub(crate) projects: Vec<ScannedProject>,
    pub(crate) sessions: Vec<ScannedSession>,
    pub(crate) conflicts: Vec<ScannedConflict>,
    pub(crate) degraded_reason: Option<String>,
}

impl SourceScan {
    /// A pass that reached no native evidence at all. Its rows are not
    /// authoritative, so [`super::store::commit_source_scan`] will not retract
    /// anything on its behalf.
    pub(crate) fn degraded(
        source_instance_id: u64,
        adapter_id: &str,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            source_instance_id,
            adapter_id: adapter_id.to_string(),
            projects: Vec::new(),
            sessions: Vec::new(),
            conflicts: Vec::new(),
            degraded_reason: Some(clamp_reason(reason.into())),
        }
    }
}

/// Longest `catalog_sources.degraded_reason` the schema accepts.
const MAX_DEGRADED_REASON_CHARS: usize = 512;

/// Keep a reason inside the column it is stored in.
///
/// The reason quotes an adapter error, and an adapter error can quote a long
/// native path. Storing it unclamped would fail the row's `CHECK`, which would
/// abort the whole commit — turning "this source is degraded" into "this
/// source has no catalog at all", which is exactly the outcome a degraded pass
/// exists to avoid.
fn clamp_reason(reason: String) -> String {
    if reason.chars().count() <= MAX_DEGRADED_REASON_CHARS {
        return reason;
    }
    reason
        .chars()
        .take(MAX_DEGRADED_REASON_CHARS - 1)
        .chain(std::iter::once('\u{2026}'))
        .collect()
}

/// Run one discovery pass and derive both identities for every row.
///
/// Two identities are derived from the same adapter-declared native key:
///
/// * `project_key`/`session_key` — the `EntityKey::native` bytes the adapter's
///   decoder emits, so a discovered row and its later transcript-backed
///   history are the same entity and join directly; and
/// * `external_ref` — the RFC 012A `CanonicalEntityKey` digest that downstream
///   consumers persist. It is machine-independent and survives restarts.
pub(crate) fn scan_source<A: AgentAdapter + ?Sized>(
    adapter: &A,
    instance: &SourceInstance,
    limits: &CatalogDiscoveryLimits,
) -> Result<SourceScan, EngineError> {
    let adapter_id = adapter.manifest().id.clone();
    let source_instance_key = CanonicalSourceInstanceKey::derive(
        instance.spec.identity_contract_version,
        instance.spec.stable_key.as_bytes(),
    )
    .map_err(|error| EngineError::InvalidConfig(error.to_string()))?;

    let discovery = match adapter.discover_catalog(instance, limits) {
        Ok(discovery) => discovery,
        Err(error) => {
            return Ok(SourceScan::degraded(
                instance.id,
                adapter_id.as_str(),
                degraded_reason(&error),
            ))
        }
    };

    let mut projects = Vec::with_capacity(discovery.projects.len());
    for project in &discovery.projects {
        projects.push(ScannedProject {
            project_key: native_key(
                &adapter_id,
                instance,
                "project",
                &project.native_project_key,
            )?,
            external_ref: external_ref(
                &adapter_id,
                &source_instance_key,
                "project",
                &project.native_project_key,
            )?,
            native_project_key: project.native_project_key.clone(),
            display_name: project.display_name.clone(),
            display_path: project.display_path.clone(),
        });
    }

    let mut sessions = Vec::with_capacity(discovery.sessions.len());
    for session in &discovery.sessions {
        sessions.push(ScannedSession {
            session_key: native_key(
                &adapter_id,
                instance,
                "session",
                &session.native_session_key,
            )?,
            project_key: native_key(
                &adapter_id,
                instance,
                "project",
                &session.native_project_key,
            )?,
            external_ref: external_ref(
                &adapter_id,
                &source_instance_key,
                "session",
                &session.native_session_key,
            )?,
            native_session_key: session.native_session_key.clone(),
            native_session_id: session.native_session_id.clone(),
            title: session.title.clone(),
            association_basis: session.association_basis.as_str(),
            association_quality: session.association_quality.as_str(),
            association_provenance: session.association_provenance.clone(),
            native_created_at: session.native_created_at.clone(),
            native_updated_at: session.native_updated_at.clone(),
            native_message_count: session.native_message_count,
            transcript_present: session.transcript_present,
            transcript_locator: session.transcript_locator.clone(),
            source_size_bytes: session.source_size_bytes,
            source_modified_ms: session.source_modified_ms,
            sort_time: sort_time(session),
        });
    }

    let mut conflicts = Vec::with_capacity(discovery.conflicts.len());
    for conflict in &discovery.conflicts {
        conflicts.push(ScannedConflict {
            session_key: native_key(
                &adapter_id,
                instance,
                "session",
                &conflict.native_session_key,
            )?,
            competing_native_project_key: conflict.competing_native_project_key.clone(),
            basis: conflict.basis.as_str(),
            provenance: conflict.provenance.clone(),
        });
    }

    Ok(SourceScan {
        source_instance_id: instance.id,
        adapter_id: adapter_id.as_str().to_string(),
        projects,
        sessions,
        conflicts,
        degraded_reason: discovery.degraded_reason.map(clamp_reason),
    })
}

fn native_key(
    adapter_id: &crate::adapter::AdapterId,
    instance: &SourceInstance,
    entity_kind: &str,
    native: &str,
) -> Result<Vec<u8>, EngineError> {
    EntityKey::native(adapter_id, instance.id, entity_kind, native.as_bytes())
        .map(|key| key.as_bytes().to_vec())
        .map_err(|error| EngineError::InvalidConfig(error.to_string()))
}

fn external_ref(
    adapter_id: &crate::adapter::AdapterId,
    source_instance_key: &CanonicalSourceInstanceKey,
    entity_kind: &str,
    native: &str,
) -> Result<[u8; 32], EngineError> {
    CanonicalEntityKey::derive(
        adapter_id.as_str(),
        source_instance_key,
        entity_kind,
        native.as_bytes(),
    )
    .map(|key| *key.as_bytes())
    .map_err(|error| EngineError::InvalidConfig(error.to_string()))
}

/// An unknown activity time stays empty rather than becoming the epoch: an
/// empty key sorts last under `DESC`, which is where an undated row belongs.
fn sort_time(session: &crate::adapter::DiscoveredSession) -> String {
    if let Some(updated) = session.native_updated_at.as_deref() {
        return updated.to_string();
    }
    if let Some(created) = session.native_created_at.as_deref() {
        return created.to_string();
    }
    match session.source_modified_ms {
        Some(ms) if ms > 0 => epoch_ms_to_iso8601(ms as f64),
        _ => String::new(),
    }
}

/// Adapter error text is written for operators, and adapters are contractually
/// forbidden from putting native paths in it.
fn degraded_reason(error: &AdapterError) -> String {
    format!("{}: {}", error.code, error.message)
}
