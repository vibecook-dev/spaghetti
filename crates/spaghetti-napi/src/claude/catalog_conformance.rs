//! RFC 012B Claude candidate conformance oracle.
//!
//! This module is test-only. It composes complete common-driver membership
//! evidence with existing Claude decoders, but does not promote the candidate
//! support bundle or expose a runtime catalog implementation.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::adapter::{
    candidate_catalog_index_project, candidate_catalog_nested_parent_coordinates,
    candidate_catalog_parent_coordinates, ClaudeCodeAdapter,
};
use crate::adapter::{
    AdapterId, AgentAdapter, CanonicalEntityKey, CanonicalFactId, CanonicalSourceInstanceKey,
    DecodeContext, DecodeDisposition, DriverSpec, Fact, FactBatch, FactRevisionId,
    FactSemanticContext, SessionFact, SessionIndexSnapshotFact, SourceInstance, SourceInstanceKey,
    SourceInstanceSpec, SourceObjectDescriptor, SourceRecordId, SourceRoot, StreamSpec,
};
use crate::source::{
    AppendDelimitedConfig, AppendDelimitedFile, AppendItem, AppendRead, AppendTransition,
    DirectoryCheckpoint, DirectoryEntryKind, DirectoryEntryState, DirectoryScan,
    DirectorySelection, DirectorySnapshot, DirectorySnapshotConfig, RecordOrigin, ReplaceDocument,
    ReplaceRead, SourceDriverError, SourceMediaType,
};

const FIXTURE_CONTRACT_VERSION: u32 = 1;
const ADAPTER_ID: &str = "claude-code";
const INDEX_STREAM_ID: &str = "session-indexes";
const PARENT_STREAM_ID: &str = "session-transcripts";
const SUBAGENT_STREAM_ID: &str = "subagent-transcripts";
const PROJECT_FACT_KIND: &str = "catalog.project-membership";
const SESSION_FACT_KIND: &str = "catalog.session-membership";
const ASSOCIATION_FACT_KIND: &str = "catalog.session-project-association";
const INDEX_METADATA_FACT_KIND: &str = "catalog.session-index-metadata";
const HEAD_METADATA_FACT_KIND: &str = "catalog.transcript-head-metadata";
const FIXTURE_SOURCE_INSTANCE: &[u8] = b"claude-small-candidate-fixture-root-v1";
const PLANNING_EVIDENCE_ID: &str = "phase0-catalog-census-2026-08-15";
const SUPPORT_RELEASE_ID: &str = "claude-code-support-2026-08-15-candidate";
const SOURCE_DECLARATION_ID: &str = "claude-code-sources-2026-08-15-candidate";
const PLANNED_SUPPORT_RELEASE_ID: &str = "claude-code.catalog-candidate-2026-08-15";
const PLANNED_SOURCE_DECLARATION_ID: &str = "claude-code.catalog-sources-v1";
const MEMBERSHIP_MAX_ENTRIES: usize = 250_000;
const MEMBERSHIP_MAX_DEPTH: usize = 64;
const INDEX_MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
const HEAD_RECORD_PAYLOAD_BYTES: usize = 65_536;
const HEAD_WINDOW_PAYLOAD_BYTES: usize = 65_536;
const HEAD_MAX_RECORDS: usize = 128;
const HEAD_DELIMITER_BYTES: usize = 1;
const HEAD_FRAMING_READ_AHEAD_BYTES: usize = 65_536;
const HEAD_CHECKPOINT_ANCHOR_BYTES: usize = 4_096;
// A 65,536-byte payload plus its LF can cross into a second 64-KiB operating-
// system framing read; the common driver then re-reads a 4-KiB checkpoint
// anchor. This 132-KiB value is candidate fixture evidence, not a global
// performance or access-policy bound.
const HEAD_PHYSICAL_READ_CEILING: u64 = (HEAD_RECORD_PAYLOAD_BYTES
    + HEAD_FRAMING_READ_AHEAD_BYTES
    + HEAD_CHECKPOINT_ANCHOR_BYTES) as u64;
const FRAMING_CONTRACT_VERSION: u32 = 1;
const FACT_REVISION_CONTRACT_VERSION: u32 = 1;

type ConformanceResult<T> = Result<T, Box<dyn Error + Send + Sync>>;
type MemberKey = (String, String);
type ProjectIdentity = (String, String);
type SessionIdentity = (String, String, String);

const INDEX_EVIDENCE: u8 = 1 << 0;
const TOP_LEVEL_EVIDENCE: u8 = 1 << 1;
const NESTED_EVIDENCE: u8 = 1 << 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaudeCandidateConformanceFixture {
    fixture_contract_version: u32,
    adapter_id: String,
    planning_evidence_id: String,
    support_release_id: String,
    source_declaration_id: String,
    support_release_status: String,
    catalog_execution_authorized: bool,
    planned_composition_status: String,
    bounds: ClaudeCandidateBounds,
    independent_oracle: ClaudeIndependentOracle,
    rust_conformance: ClaudeRustConformance,
    non_promotion_gaps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaudeCandidateBounds {
    membership_max_entries: u64,
    membership_max_depth: u64,
    index_max_document_bytes: u64,
    transcript_head_max_record_payload_bytes: u64,
    transcript_head_delimiter_bytes: u64,
    transcript_head_max_window_payload_bytes: u64,
    transcript_head_max_records: u64,
    transcript_head_framing_read_ahead_bytes: u64,
    transcript_head_delimiter_included_in_framing_read_ahead: bool,
    transcript_head_checkpoint_anchor_bytes: u64,
    transcript_head_physical_read_ceiling_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaudeIndependentOracle {
    project_count: u64,
    session_count: u64,
    project_identity_digest: String,
    session_identity_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaudeRustConformance {
    index_membership_generation: u64,
    top_level_membership_generation: u64,
    nested_membership_generation: u64,
    index_object_count: u64,
    index_entry_count: u64,
    top_level_member_count: u64,
    nested_membership_object_count: u64,
    nested_parent_member_count: u64,
    index_only_member_count: u64,
    top_level_only_member_count: u64,
    nested_only_member_count: u64,
    index_and_top_level_member_count: u64,
    index_and_nested_member_count: u64,
    top_level_and_nested_member_count: u64,
    all_sources_member_count: u64,
    transcript_head_object_count: u64,
    member_metadata_unavailable_count: u64,
    transcript_head_record_count: u64,
    index_payload_bytes_read: u64,
    transcript_head_payload_bytes_read: u64,
    transcript_head_physical_bytes_read: u64,
    association_base_count: u64,
    association_evidence_count: u64,
    association_conflict_count: u64,
    project_entity_count: u64,
    session_entity_count: u64,
    source_record_count: u64,
    fact_identity_count: u64,
    fact_revision_count: u64,
    membership_identity_digest: String,
    association_evidence_digest: String,
    association_conflict_digest: String,
    source_record_identity_digest: String,
    entity_identity_digest: String,
    fact_identity_digest: String,
    fact_revision_digest: String,
}

#[derive(Debug, Clone, Default)]
struct CandidateMetadata {
    cwd: Option<String>,
    first_prompt: Option<String>,
    title: Option<String>,
    created_at: Option<String>,
}

impl CandidateMetadata {
    fn complete(&self) -> bool {
        has_display_value(&self.cwd)
            && (has_display_value(&self.first_prompt) || has_display_value(&self.title))
            && has_display_value(&self.created_at)
    }

    fn merge_session(&mut self, session: &SessionFact) {
        self.cwd =
            canonical_display(self.cwd.take()).or_else(|| canonical_display(session.cwd.clone()));
        self.first_prompt = canonical_display(self.first_prompt.take())
            .or_else(|| canonical_display(session.first_prompt.clone()));
        self.title = canonical_display(self.title.take())
            .or_else(|| canonical_display(session.custom_title.clone()))
            .or_else(|| canonical_display(session.ai_title.clone()));
        self.created_at = canonical_display(self.created_at.take()).or_else(|| {
            canonical_display(session.source_time.as_ref().map(|time| time.value.clone()))
        });
    }
}

fn has_display_value(value: &Option<String>) -> bool {
    value
        .as_deref()
        .is_some_and(|candidate| !candidate.trim().is_empty())
}

fn canonical_display(value: Option<String>) -> Option<String> {
    let value = value?;
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[derive(Default)]
struct CandidateProjection {
    membership_generations: [u64; 3],
    index_objects: u64,
    index_entries: u64,
    nested_membership_objects: u64,
    member_evidence: BTreeMap<MemberKey, u8>,
    projects: BTreeSet<ProjectIdentity>,
    sessions: BTreeSet<SessionIdentity>,
    session_projects: BTreeMap<String, String>,
    metadata: BTreeMap<MemberKey, CandidateMetadata>,
    association_evidence: BTreeSet<(String, String, String, String, String)>,
    association_conflicts: BTreeSet<(String, String, String, String, String)>,
    project_entities: BTreeSet<CanonicalEntityKey>,
    session_entities: BTreeSet<CanonicalEntityKey>,
    source_records: BTreeSet<SourceRecordId>,
    fact_ids: BTreeSet<(String, CanonicalFactId)>,
    fact_revisions: BTreeSet<(String, FactRevisionId)>,
    transcript_head_objects: u64,
    transcript_head_records: u64,
    index_payload_bytes: u64,
    transcript_head_payload_bytes: u64,
    transcript_head_physical_bytes: u64,
}

impl CandidateProjection {
    fn member_metadata_unavailable_count(&self) -> u64 {
        self.member_evidence
            .keys()
            .filter(|member| {
                !self
                    .metadata
                    .get(*member)
                    .is_some_and(CandidateMetadata::complete)
            })
            .count() as u64
    }

    fn report(&self) -> ClaudeCandidateConformanceFixture {
        let mut entity_ids = BTreeSet::new();
        entity_ids.extend(
            self.project_entities
                .iter()
                .copied()
                .map(|key| ("project".to_string(), key)),
        );
        entity_ids.extend(
            self.session_entities
                .iter()
                .copied()
                .map(|key| ("session".to_string(), key)),
        );
        let source_count = |mask| {
            self.member_evidence
                .values()
                .filter(|evidence| **evidence == mask)
                .count() as u64
        };
        ClaudeCandidateConformanceFixture {
            fixture_contract_version: FIXTURE_CONTRACT_VERSION,
            adapter_id: ADAPTER_ID.to_string(),
            planning_evidence_id: PLANNING_EVIDENCE_ID.to_string(),
            support_release_id: SUPPORT_RELEASE_ID.to_string(),
            source_declaration_id: SOURCE_DECLARATION_ID.to_string(),
            support_release_status: "candidate".to_string(),
            catalog_execution_authorized: false,
            planned_composition_status: "planned_unbound".to_string(),
            bounds: ClaudeCandidateBounds {
                membership_max_entries: MEMBERSHIP_MAX_ENTRIES as u64,
                membership_max_depth: MEMBERSHIP_MAX_DEPTH as u64,
                index_max_document_bytes: INDEX_MAX_DOCUMENT_BYTES as u64,
                transcript_head_max_record_payload_bytes: HEAD_RECORD_PAYLOAD_BYTES as u64,
                transcript_head_delimiter_bytes: HEAD_DELIMITER_BYTES as u64,
                transcript_head_max_window_payload_bytes: HEAD_WINDOW_PAYLOAD_BYTES as u64,
                transcript_head_max_records: HEAD_MAX_RECORDS as u64,
                transcript_head_framing_read_ahead_bytes: HEAD_FRAMING_READ_AHEAD_BYTES as u64,
                transcript_head_delimiter_included_in_framing_read_ahead: true,
                transcript_head_checkpoint_anchor_bytes: HEAD_CHECKPOINT_ANCHOR_BYTES as u64,
                transcript_head_physical_read_ceiling_bytes: HEAD_PHYSICAL_READ_CEILING,
            },
            independent_oracle: ClaudeIndependentOracle {
                project_count: self.projects.len() as u64,
                session_count: self.sessions.len() as u64,
                project_identity_digest: identity_digest(&self.projects),
                session_identity_digest: identity_digest(&self.sessions),
            },
            rust_conformance: ClaudeRustConformance {
                index_membership_generation: self.membership_generations[0],
                top_level_membership_generation: self.membership_generations[1],
                nested_membership_generation: self.membership_generations[2],
                index_object_count: self.index_objects,
                index_entry_count: self.index_entries,
                top_level_member_count: self
                    .member_evidence
                    .values()
                    .filter(|evidence| **evidence & TOP_LEVEL_EVIDENCE != 0)
                    .count() as u64,
                nested_membership_object_count: self.nested_membership_objects,
                nested_parent_member_count: self
                    .member_evidence
                    .values()
                    .filter(|evidence| **evidence & NESTED_EVIDENCE != 0)
                    .count() as u64,
                index_only_member_count: source_count(INDEX_EVIDENCE),
                top_level_only_member_count: source_count(TOP_LEVEL_EVIDENCE),
                nested_only_member_count: source_count(NESTED_EVIDENCE),
                index_and_top_level_member_count: source_count(INDEX_EVIDENCE | TOP_LEVEL_EVIDENCE),
                index_and_nested_member_count: source_count(INDEX_EVIDENCE | NESTED_EVIDENCE),
                top_level_and_nested_member_count: source_count(
                    TOP_LEVEL_EVIDENCE | NESTED_EVIDENCE,
                ),
                all_sources_member_count: source_count(
                    INDEX_EVIDENCE | TOP_LEVEL_EVIDENCE | NESTED_EVIDENCE,
                ),
                transcript_head_object_count: self.transcript_head_objects,
                member_metadata_unavailable_count: self.member_metadata_unavailable_count(),
                transcript_head_record_count: self.transcript_head_records,
                index_payload_bytes_read: self.index_payload_bytes,
                transcript_head_payload_bytes_read: self.transcript_head_payload_bytes,
                transcript_head_physical_bytes_read: self.transcript_head_physical_bytes,
                association_base_count: self.sessions.len() as u64,
                association_evidence_count: self.association_evidence.len() as u64,
                association_conflict_count: self.association_conflicts.len() as u64,
                project_entity_count: self.project_entities.len() as u64,
                session_entity_count: self.session_entities.len() as u64,
                source_record_count: self.source_records.len() as u64,
                fact_identity_count: self.fact_ids.len() as u64,
                fact_revision_count: self.fact_revisions.len() as u64,
                membership_identity_digest: semantic_digest(&self.sessions),
                association_evidence_digest: semantic_digest(&self.association_evidence),
                association_conflict_digest: semantic_digest(&self.association_conflicts),
                source_record_identity_digest: semantic_digest(&self.source_records),
                entity_identity_digest: semantic_digest(&entity_ids),
                fact_identity_digest: semantic_digest(&self.fact_ids),
                fact_revision_digest: semantic_digest(&self.fact_revisions),
            },
            non_promotion_gaps: vec![
                "nested_parent_uuid_enforcement_is_not_yet_declared".to_string(),
                "physical_head_ceiling_is_fixture_evidence_not_ratified".to_string(),
            ],
        }
    }

    fn admit(
        &mut self,
        project: String,
        session: String,
        evidence: u8,
        occurrence_ref: String,
    ) -> ConformanceResult<()> {
        if project.is_empty() || session.is_empty() {
            return Err("Claude catalog member coordinates must not be empty".into());
        }
        if let Some(existing) = self
            .session_projects
            .insert(session.clone(), project.clone())
        {
            if existing != project {
                return Err("one Claude session ID appeared under competing projects".into());
            }
        }
        self.projects
            .insert((ADAPTER_ID.to_string(), project.clone()));
        self.sessions
            .insert((ADAPTER_ID.to_string(), project.clone(), session.clone()));
        let basis = [
            (INDEX_EVIDENCE, "session_index_membership"),
            (TOP_LEVEL_EVIDENCE, "top_level_transcript_membership"),
            (NESTED_EVIDENCE, "nested_parent_membership"),
        ]
        .into_iter()
        .find_map(|(flag, basis)| (evidence == flag).then_some(basis))
        .ok_or("Claude membership evidence must name exactly one source occurrence")?;
        self.association_evidence.insert((
            session.clone(),
            project.clone(),
            project.clone(),
            basis.to_string(),
            occurrence_ref,
        ));
        *self.member_evidence.entry((project, session)).or_default() |= evidence;
        Ok(())
    }

    fn observe_association(
        &mut self,
        member: &MemberKey,
        asserted_cwd: &str,
        basis: &str,
        occurrence_ref: String,
    ) {
        let asserted_project = encode_project_key(asserted_cwd);
        let evidence = (
            member.1.clone(),
            member.0.clone(),
            asserted_project.clone(),
            basis.to_string(),
            occurrence_ref,
        );
        self.association_evidence.insert(evidence.clone());
        if asserted_project != member.0 {
            self.association_conflicts.insert(evidence);
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct MembershipSources {
    index: DirectoryCheckpoint,
    top_level: DirectoryCheckpoint,
    nested: DirectoryCheckpoint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NativeCatalogIdentities {
    projects: BTreeSet<ProjectIdentity>,
    sessions: BTreeSet<SessionIdentity>,
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/small/.claude")
}

fn fixture_instance(root: &Path, registration_seed: u64) -> ConformanceResult<SourceInstance> {
    Ok(SourceInstance {
        id: registration_seed,
        spec: SourceInstanceSpec {
            identity_contract_version: 1,
            stable_key: SourceInstanceKey::new(FIXTURE_SOURCE_INSTANCE.to_vec())?,
            display_name: "Claude candidate conformance".to_string(),
            roots: vec![
                SourceRoot {
                    name: "home".to_string(),
                    path: root.to_path_buf(),
                },
                SourceRoot {
                    name: "projects".to_string(),
                    path: root.join("projects"),
                },
                SourceRoot {
                    name: "teams".to_string(),
                    path: root.join("teams"),
                },
                SourceRoot {
                    name: "sessions".to_string(),
                    path: root.join("sessions"),
                },
            ],
            discovery_reason: "sanitized candidate conformance fixture".to_string(),
        },
    })
}

fn exact_stream(streams: &[StreamSpec], stream_id: &str) -> ConformanceResult<StreamSpec> {
    streams
        .iter()
        .find(|stream| stream.id.as_str() == stream_id)
        .cloned()
        .ok_or_else(|| format!("Claude candidate stream {stream_id} is not declared").into())
}

fn scan_membership_sources(projects_root: &Path) -> ConformanceResult<MembershipSources> {
    let index = complete_scan(projects_root, &index_selection)?;
    let top_level = complete_scan(projects_root, &top_level_selection)?;
    let nested = complete_scan(projects_root, &nested_selection)?;
    Ok(MembershipSources {
        index,
        top_level,
        nested,
    })
}

fn complete_scan<F>(projects_root: &Path, selector: &F) -> ConformanceResult<DirectoryCheckpoint>
where
    F: Fn(&Path, DirectoryEntryKind) -> DirectorySelection,
{
    let scan = DirectorySnapshot::new(DirectorySnapshotConfig {
        max_entries: MEMBERSHIP_MAX_ENTRIES,
        max_depth: MEMBERSHIP_MAX_DEPTH,
    })?
    .scan(projects_root, None, selector)?;
    let DirectoryScan::Snapshot { checkpoint, .. } = scan else {
        return Err("Claude membership authority was not completely available".into());
    };
    if checkpoint.generation == 0
        || checkpoint
            .entries
            .values()
            .any(|entry| entry.generation == 0)
    {
        return Err("Claude membership authority generation must be positive".into());
    }
    Ok(checkpoint)
}

fn index_selection(path: &Path, kind: DirectoryEntryKind) -> DirectorySelection {
    let components = utf8_components(path);
    match kind {
        DirectoryEntryKind::Directory if components.len() == 1 => DirectorySelection::Recurse,
        DirectoryEntryKind::File
            if components.len() == 2 && components[1] == "sessions-index.json" =>
        {
            DirectorySelection::Include
        }
        _ => DirectorySelection::Ignore,
    }
}

fn top_level_selection(path: &Path, kind: DirectoryEntryKind) -> DirectorySelection {
    let components = utf8_components(path);
    match kind {
        DirectoryEntryKind::Directory if components.len() == 1 => DirectorySelection::Recurse,
        DirectoryEntryKind::File if components.len() == 2 && components[1].ends_with(".jsonl") => {
            DirectorySelection::Include
        }
        _ => DirectorySelection::Ignore,
    }
}

fn nested_selection(path: &Path, kind: DirectoryEntryKind) -> DirectorySelection {
    let components = utf8_components(path);
    match kind {
        DirectoryEntryKind::Directory
            if components.len() <= 2
                || components.get(2).map(String::as_str) == Some("subagents") =>
        {
            DirectorySelection::Recurse
        }
        DirectoryEntryKind::File
            if components.len() >= 4
                && components.get(2).map(String::as_str) == Some("subagents")
                && components
                    .last()
                    .is_some_and(|name| name.starts_with("agent-") && name.ends_with(".jsonl")) =>
        {
            DirectorySelection::Include
        }
        _ => DirectorySelection::Ignore,
    }
}

fn utf8_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str().map(str::to_owned),
            _ => None,
        })
        .collect()
}

fn candidate_projection(
    root: &Path,
    registration_seed: u64,
) -> ConformanceResult<CandidateProjection> {
    let projects_root = root.join("projects");
    let memberships = scan_membership_sources(&projects_root)?;
    let adapter = ClaudeCodeAdapter::new();
    let instance = fixture_instance(root, registration_seed)?;
    let streams = adapter.streams(&instance)?;
    let index_stream = exact_stream(&streams, INDEX_STREAM_ID)?;
    let parent_stream = exact_stream(&streams, PARENT_STREAM_ID)?;
    let DriverSpec::ReplaceDocument(index_config) = index_stream.driver.clone() else {
        return Err("Claude candidate index stream is not ReplaceDocument".into());
    };
    if index_config.max_document_bytes != INDEX_MAX_DOCUMENT_BYTES {
        return Err("Claude candidate session-index bound drifted".into());
    }

    let mut projection = CandidateProjection {
        membership_generations: [
            memberships.index.generation,
            memberships.top_level.generation,
            memberships.nested.generation,
        ],
        index_objects: memberships.index.entries.len() as u64,
        nested_membership_objects: memberships.nested.entries.len() as u64,
        ..CandidateProjection::default()
    };

    for entry in memberships.top_level.entries.values() {
        let relative_path = PathBuf::from(&entry.display_path);
        let coordinates = candidate_catalog_parent_coordinates(&relative_path)?;
        projection.admit(
            coordinates.project_slug,
            coordinates.session_id,
            TOP_LEVEL_EVIDENCE,
            membership_occurrence_ref("top_level", &memberships.top_level, entry),
        )?;
    }
    for entry in memberships.nested.entries.values() {
        let relative_path = PathBuf::from(&entry.display_path);
        let coordinates = candidate_catalog_nested_parent_coordinates(&relative_path)?;
        projection.admit(
            coordinates.project_slug,
            coordinates.session_id,
            NESTED_EVIDENCE,
            membership_occurrence_ref("nested", &memberships.nested, entry),
        )?;
    }

    let index_driver = ReplaceDocument::new(index_config)?;
    for (index, entry) in memberships.index.entries.values().enumerate() {
        add_index_document(
            &adapter,
            &instance,
            &index_stream,
            &index_driver,
            &projects_root,
            entry,
            index,
            registration_seed,
            &mut projection,
        )?;
    }

    let membership_revision = membership_revision(&projection);
    add_membership_identities(&mut projection, &membership_revision)?;

    for (index, entry) in memberships.top_level.entries.values().enumerate() {
        let relative_path = PathBuf::from(&entry.display_path);
        let coordinates = candidate_catalog_parent_coordinates(&relative_path)?;
        let member = (coordinates.project_slug, coordinates.session_id);
        if projection
            .metadata
            .get(&member)
            .is_some_and(CandidateMetadata::complete)
        {
            continue;
        }
        enrich_transcript_head(
            &adapter,
            &instance,
            &parent_stream,
            &projects_root,
            entry,
            &member,
            index,
            registration_seed,
            &mut projection,
        )?;
    }
    if scan_membership_sources(&projects_root)? != memberships {
        return Err("Claude membership authority changed during complete projection".into());
    }
    Ok(projection)
}

#[allow(clippy::too_many_arguments)]
fn add_index_document(
    adapter: &ClaudeCodeAdapter,
    instance: &SourceInstance,
    stream: &StreamSpec,
    driver: &ReplaceDocument,
    projects_root: &Path,
    entry: &DirectoryEntryState,
    object_index: usize,
    registration_seed: u64,
    projection: &mut CandidateProjection,
) -> ConformanceResult<()> {
    let relative_path = PathBuf::from(&entry.display_path);
    let project_slug = candidate_catalog_index_project(&relative_path)?;
    projection
        .projects
        .insert((ADAPTER_ID.to_string(), project_slug.clone()));
    let origin = origin(registration_seed, object_index, "application/json")?;
    let read = driver.read_confined(projects_root, &relative_path, None, &origin, false)?;
    let record = match read {
        ReplaceRead::Record { record, .. } => record,
        ReplaceRead::Quarantined { .. } => {
            return Err("Claude authoritative session index exceeds its declared bound".into())
        }
        ReplaceRead::Missing
        | ReplaceRead::RetryTransient
        | ReplaceRead::Unchanged { .. }
        | ReplaceRead::Removed { .. } => {
            return Err("Claude authoritative session index changed during projection".into())
        }
    };
    projection.index_payload_bytes = projection
        .index_payload_bytes
        .checked_add(u64::try_from(record.payload.len())?)
        .ok_or("Claude index byte accounting overflow")?;
    let semantic_context = semantic_context(stream.id.as_str(), entry)?;
    let source_record_id = semantic_context.source_record_id(&record)?;
    projection.source_records.insert(source_record_id);

    let descriptor = SourceObjectDescriptor {
        stream_id: stream.id.clone(),
        object_key: entry.path_key.clone(),
        relative_path,
    };
    let object_context = adapter.bootstrap_object(instance, &descriptor)?;
    let mut batch = FactBatch::new_with_semantic_context(8, 8, semantic_context)?;
    let disposition = adapter.decode(
        DecodeContext {
            decoder: &stream.decoder,
            object_context: &object_context,
            decoder_state: None,
        },
        &record,
        &mut batch,
    )?;
    if disposition != DecodeDisposition::Applied {
        return Err("Claude authoritative session index did not decode completely".into());
    }
    let mut snapshots = batch
        .facts()
        .iter()
        .filter_map(|envelope| match &envelope.value {
            Fact::SessionIndexSnapshot(snapshot) => Some(snapshot),
            _ => None,
        });
    let snapshot = snapshots
        .next()
        .ok_or("Claude session index emitted no snapshot")?;
    if snapshots.next().is_some() || snapshot.native_project_key != project_slug {
        return Err("Claude session-index path identity drifted".into());
    }
    add_index_snapshot(snapshot, source_record_id, projection)
}

fn add_index_snapshot(
    snapshot: &SessionIndexSnapshotFact,
    source_record_id: SourceRecordId,
    projection: &mut CandidateProjection,
) -> ConformanceResult<()> {
    let source_instance_key = CanonicalSourceInstanceKey::derive(1, FIXTURE_SOURCE_INSTANCE)?;
    for entry in &snapshot.entries {
        projection.index_entries = projection
            .index_entries
            .checked_add(1)
            .ok_or("Claude index-entry count overflow")?;
        projection.admit(
            snapshot.native_project_key.clone(),
            entry.native_session_id.clone(),
            INDEX_EVIDENCE,
            index_entry_occurrence_ref(&source_record_id, &entry.native_session_id),
        )?;
        let member = (
            snapshot.native_project_key.clone(),
            entry.native_session_id.clone(),
        );
        projection.observe_association(
            &member,
            &entry.project_path,
            "session_index_project_path",
            index_entry_occurrence_ref(&source_record_id, &entry.native_session_id),
        );
        projection
            .metadata
            .entry(member)
            .or_default()
            .clone_from(&CandidateMetadata {
                cwd: canonical_display(Some(entry.project_path.clone())),
                first_prompt: canonical_display(Some(entry.first_prompt.clone())),
                title: canonical_display(entry.summary.clone()),
                created_at: canonical_display(Some(entry.created_at.value.clone())),
            });
        let fact = CanonicalFactId::native(
            ADAPTER_ID,
            &source_instance_key,
            INDEX_METADATA_FACT_KIND,
            entry.native_session_id.as_bytes(),
        )?;
        projection
            .fact_ids
            .insert((INDEX_METADATA_FACT_KIND.to_string(), fact));
        projection.fact_revisions.insert((
            INDEX_METADATA_FACT_KIND.to_string(),
            FactRevisionId::derive(
                &fact,
                FACT_REVISION_CONTRACT_VERSION,
                source_record_id.as_bytes(),
            )?,
        ));
    }
    Ok(())
}

fn add_membership_identities(
    projection: &mut CandidateProjection,
    membership_revision: &[u8; 32],
) -> ConformanceResult<()> {
    let source_instance_key = CanonicalSourceInstanceKey::derive(1, FIXTURE_SOURCE_INSTANCE)?;
    for (_, project_key) in &projection.projects {
        let entity = CanonicalEntityKey::derive(
            ADAPTER_ID,
            &source_instance_key,
            "project",
            project_key.as_bytes(),
        )?;
        let fact = CanonicalFactId::native(
            ADAPTER_ID,
            &source_instance_key,
            PROJECT_FACT_KIND,
            project_key.as_bytes(),
        )?;
        projection.project_entities.insert(entity);
        projection
            .fact_ids
            .insert((PROJECT_FACT_KIND.to_string(), fact));
        projection.fact_revisions.insert((
            PROJECT_FACT_KIND.to_string(),
            FactRevisionId::derive(&fact, FACT_REVISION_CONTRACT_VERSION, membership_revision)?,
        ));
    }
    for (_, project_key, session_id) in &projection.sessions {
        let entity = CanonicalEntityKey::derive(
            ADAPTER_ID,
            &source_instance_key,
            "session",
            session_id.as_bytes(),
        )?;
        let session_fact = CanonicalFactId::native(
            ADAPTER_ID,
            &source_instance_key,
            SESSION_FACT_KIND,
            session_id.as_bytes(),
        )?;
        let association_fact = CanonicalFactId::native(
            ADAPTER_ID,
            &source_instance_key,
            ASSOCIATION_FACT_KIND,
            &pair_key(session_id.as_bytes(), project_key.as_bytes()),
        )?;
        projection.session_entities.insert(entity);
        for (kind, fact) in [
            (SESSION_FACT_KIND, session_fact),
            (ASSOCIATION_FACT_KIND, association_fact),
        ] {
            projection.fact_ids.insert((kind.to_string(), fact));
            projection.fact_revisions.insert((
                kind.to_string(),
                FactRevisionId::derive(&fact, FACT_REVISION_CONTRACT_VERSION, membership_revision)?,
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn enrich_transcript_head(
    adapter: &ClaudeCodeAdapter,
    instance: &SourceInstance,
    stream: &StreamSpec,
    projects_root: &Path,
    entry: &DirectoryEntryState,
    member: &MemberKey,
    object_index: usize,
    registration_seed: u64,
    projection: &mut CandidateProjection,
) -> ConformanceResult<()> {
    projection.transcript_head_objects = projection
        .transcript_head_objects
        .checked_add(1)
        .ok_or("Claude transcript-head object count overflow")?;
    let relative_path = PathBuf::from(&entry.display_path);
    let origin = origin(registration_seed, object_index, "application/x-ndjson")?;
    let driver = AppendDelimitedFile::new(AppendDelimitedConfig {
        delimiter: b'\n',
        normalize_crlf: true,
        max_record_bytes: HEAD_RECORD_PAYLOAD_BYTES,
        max_batch_bytes: HEAD_WINDOW_PAYLOAD_BYTES,
        max_records_per_batch: HEAD_MAX_RECORDS,
        prefix_anchor_bytes: HEAD_CHECKPOINT_ANCHOR_BYTES,
    })?;
    let read = match driver.read_confined_bounded(
        projects_root,
        &relative_path,
        None,
        &origin,
        false,
        HEAD_PHYSICAL_READ_CEILING,
    ) {
        Ok(read) => read,
        Err(SourceDriverError::LimitExceeded(_)) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let AppendRead::Batch {
        items,
        transition,
        bytes_read,
        ..
    } = read
    else {
        return Err("Claude transcript head changed during projection".into());
    };
    if transition != AppendTransition::Initial {
        return Err("Claude initial transcript head started from a continuation".into());
    }
    projection.transcript_head_physical_bytes = projection
        .transcript_head_physical_bytes
        .checked_add(bytes_read)
        .ok_or("Claude transcript-head physical byte accounting overflow")?;
    if items
        .iter()
        .any(|item| matches!(item, AppendItem::Quarantined(_)))
    {
        return Ok(());
    }

    let descriptor = SourceObjectDescriptor {
        stream_id: stream.id.clone(),
        object_key: entry.path_key.clone(),
        relative_path,
    };
    let object_context = adapter.bootstrap_object(instance, &descriptor)?;
    let source_instance_key = CanonicalSourceInstanceKey::derive(1, FIXTURE_SOURCE_INSTANCE)?;
    let semantic_context = semantic_context(stream.id.as_str(), entry)?;
    let mut decoder_state = None;
    let mut metadata = projection.metadata.get(member).cloned().unwrap_or_default();
    for item in items {
        let AppendItem::Record(record) = item else {
            unreachable!("quarantines were handled above");
        };
        projection.transcript_head_records = projection
            .transcript_head_records
            .checked_add(1)
            .ok_or("Claude transcript-head record count overflow")?;
        projection.transcript_head_payload_bytes = projection
            .transcript_head_payload_bytes
            .checked_add(u64::try_from(record.payload.len())?)
            .ok_or("Claude transcript-head payload accounting overflow")?;
        let source_record_id = semantic_context.source_record_id(&record)?;
        projection.source_records.insert(source_record_id);
        let mut batch = FactBatch::new_with_semantic_context(256, 64, semantic_context.clone())?;
        let disposition = adapter.decode(
            DecodeContext {
                decoder: &stream.decoder,
                object_context: &object_context,
                decoder_state: decoder_state.as_deref(),
            },
            &record,
            &mut batch,
        )?;
        if disposition == DecodeDisposition::RetryTransient {
            return Err(
                "Claude transcript decoder requested a retry for stable head evidence".into(),
            );
        }
        decoder_state = batch.next_decoder_state().map(ToOwned::to_owned);
        for envelope in batch.facts() {
            let Fact::Session(session) = &envelope.value else {
                continue;
            };
            if session.native_project_key != member.0 || session.native_session_id != member.1 {
                return Err("Claude transcript metadata attempted to retarget membership".into());
            }
            if let Some(cwd) = &session.cwd {
                projection.observe_association(
                    member,
                    cwd,
                    "transcript_cwd",
                    source_record_occurrence_ref(&source_record_id),
                );
            }
            metadata.merge_session(session);
            let fact = CanonicalFactId::native(
                ADAPTER_ID,
                &source_instance_key,
                HEAD_METADATA_FACT_KIND,
                member.1.as_bytes(),
            )?;
            projection
                .fact_ids
                .insert((HEAD_METADATA_FACT_KIND.to_string(), fact));
            projection.fact_revisions.insert((
                HEAD_METADATA_FACT_KIND.to_string(),
                FactRevisionId::derive(
                    &fact,
                    FACT_REVISION_CONTRACT_VERSION,
                    source_record_id.as_bytes(),
                )?,
            ));
        }
    }
    projection.metadata.insert(member.clone(), metadata);
    Ok(())
}

fn durable_decoder_identities(
    root: &Path,
    registration_seed: u64,
) -> ConformanceResult<NativeCatalogIdentities> {
    let projects_root = root.join("projects");
    let memberships = scan_membership_sources(&projects_root)?;
    let adapter = ClaudeCodeAdapter::new();
    let instance = fixture_instance(root, registration_seed)?;
    let streams = adapter.streams(&instance)?;
    let index_stream = exact_stream(&streams, INDEX_STREAM_ID)?;
    let parent_stream = exact_stream(&streams, PARENT_STREAM_ID)?;
    let subagent_stream = exact_stream(&streams, SUBAGENT_STREAM_ID)?;
    let mut identities = NativeCatalogIdentities {
        projects: BTreeSet::new(),
        sessions: BTreeSet::new(),
    };

    let DriverSpec::ReplaceDocument(index_config) = index_stream.driver.clone() else {
        return Err("Claude durable index stream is not ReplaceDocument".into());
    };
    let index_driver = ReplaceDocument::new(index_config)?;
    for (index, entry) in memberships.index.entries.values().enumerate() {
        let relative_path = PathBuf::from(&entry.display_path);
        let origin = origin(registration_seed, index, "application/json")?;
        let ReplaceRead::Record { record, .. } =
            index_driver.read_confined(&projects_root, &relative_path, None, &origin, false)?
        else {
            return Err("Claude durable index read was not stable".into());
        };
        let descriptor = SourceObjectDescriptor {
            stream_id: index_stream.id.clone(),
            object_key: entry.path_key.clone(),
            relative_path,
        };
        let context = adapter.bootstrap_object(&instance, &descriptor)?;
        let mut batch = FactBatch::new_with_semantic_context(
            8,
            8,
            semantic_context(index_stream.id.as_str(), entry)?,
        )?;
        if adapter.decode(
            DecodeContext {
                decoder: &index_stream.decoder,
                object_context: &context,
                decoder_state: None,
            },
            &record,
            &mut batch,
        )? != DecodeDisposition::Applied
        {
            return Err("Claude durable index did not apply".into());
        }
        for envelope in batch.facts() {
            if let Fact::SessionIndexSnapshot(snapshot) = &envelope.value {
                identities
                    .projects
                    .insert((ADAPTER_ID.to_string(), snapshot.native_project_key.clone()));
                for index_entry in &snapshot.entries {
                    identities.sessions.insert((
                        ADAPTER_ID.to_string(),
                        snapshot.native_project_key.clone(),
                        index_entry.native_session_id.clone(),
                    ));
                }
            }
        }
    }
    decode_full_append_members(
        &adapter,
        &instance,
        &parent_stream,
        &projects_root,
        memberships.top_level.entries.values(),
        registration_seed,
        &mut identities,
    )?;
    decode_full_append_members(
        &adapter,
        &instance,
        &subagent_stream,
        &projects_root,
        memberships.nested.entries.values(),
        registration_seed,
        &mut identities,
    )?;
    Ok(identities)
}

#[allow(clippy::too_many_arguments)]
fn decode_full_append_members<'a>(
    adapter: &ClaudeCodeAdapter,
    instance: &SourceInstance,
    stream: &StreamSpec,
    projects_root: &Path,
    entries: impl Iterator<Item = &'a DirectoryEntryState>,
    registration_seed: u64,
    identities: &mut NativeCatalogIdentities,
) -> ConformanceResult<()> {
    let DriverSpec::AppendDelimited(config) = stream.driver.clone() else {
        return Err("Claude durable transcript stream is not AppendDelimited".into());
    };
    let driver = AppendDelimitedFile::new(config)?;
    for (index, entry) in entries.enumerate() {
        let relative_path = PathBuf::from(&entry.display_path);
        let descriptor = SourceObjectDescriptor {
            stream_id: stream.id.clone(),
            object_key: entry.path_key.clone(),
            relative_path: relative_path.clone(),
        };
        let object_context = adapter.bootstrap_object(instance, &descriptor)?;
        let semantic_context = semantic_context(stream.id.as_str(), entry)?;
        let origin = origin(registration_seed, index, "application/x-ndjson")?;
        let mut checkpoint = None;
        let mut decoder_state = None;
        loop {
            let read = driver.read_confined(
                projects_root,
                &relative_path,
                checkpoint.as_ref(),
                &origin,
                false,
            )?;
            let AppendRead::Batch {
                items,
                checkpoint: next_checkpoint,
                more_available,
                ..
            } = read
            else {
                return Err("Claude durable transcript read was not stable".into());
            };
            for item in items {
                let AppendItem::Record(record) = item else {
                    return Err("Claude durable fixture contains an oversized record".into());
                };
                let mut batch =
                    FactBatch::new_with_semantic_context(256, 64, semantic_context.clone())?;
                let disposition = adapter.decode(
                    DecodeContext {
                        decoder: &stream.decoder,
                        object_context: &object_context,
                        decoder_state: decoder_state.as_deref(),
                    },
                    &record,
                    &mut batch,
                )?;
                if disposition == DecodeDisposition::RetryTransient {
                    return Err("Claude durable transcript decode requested retry".into());
                }
                decoder_state = batch.next_decoder_state().map(ToOwned::to_owned);
                for envelope in batch.facts() {
                    if let Fact::Session(session) = &envelope.value {
                        identities
                            .projects
                            .insert((ADAPTER_ID.to_string(), session.native_project_key.clone()));
                        identities.sessions.insert((
                            ADAPTER_ID.to_string(),
                            session.native_project_key.clone(),
                            session.native_session_id.clone(),
                        ));
                    }
                }
            }
            checkpoint = Some(next_checkpoint);
            if !more_available {
                break;
            }
        }
    }
    Ok(())
}

fn semantic_context(
    stream_id: &str,
    entry: &DirectoryEntryState,
) -> ConformanceResult<FactSemanticContext> {
    Ok(FactSemanticContext::new(
        &AdapterId::new(ADAPTER_ID)?,
        1,
        FIXTURE_SOURCE_INSTANCE,
        stream_id.as_bytes(),
        &entry.path_key,
        FRAMING_CONTRACT_VERSION,
    )?)
}

fn origin(
    registration_seed: u64,
    object_index: usize,
    media_type: &str,
) -> ConformanceResult<RecordOrigin> {
    let object_number = u64::try_from(object_index)?;
    Ok(RecordOrigin {
        source_instance_id: registration_seed,
        stream_id: registration_seed
            .checked_add(1)
            .ok_or("Claude conformance stream id overflow")?,
        object_id: registration_seed
            .checked_add(2)
            .and_then(|value| value.checked_add(object_number))
            .ok_or("Claude conformance object id overflow")?,
        observed_at: 1,
        source_timestamp_hint: None,
        media_type: SourceMediaType::new(media_type)?,
    })
}

fn membership_revision(projection: &CandidateProjection) -> [u8; 32] {
    let mut digest = Sha256::new();
    for project in &projection.projects {
        let encoded =
            serde_json::to_vec(&("project", project)).expect("membership project is serializable");
        digest.update((encoded.len() as u64).to_be_bytes());
        digest.update(encoded);
    }
    for (member, evidence) in &projection.member_evidence {
        let encoded = serde_json::to_vec(&("session", member, evidence))
            .expect("membership session is serializable");
        digest.update((encoded.len() as u64).to_be_bytes());
        digest.update(encoded);
    }
    digest.finalize().into()
}

fn membership_occurrence_ref(
    source: &str,
    checkpoint: &DirectoryCheckpoint,
    entry: &DirectoryEntryState,
) -> String {
    let mut digest = Sha256::new();
    hash_field(&mut digest, b"claude-catalog-membership-occurrence-v1");
    hash_field(&mut digest, source.as_bytes());
    hash_field(&mut digest, &checkpoint.generation.to_be_bytes());
    for member in checkpoint.entries.values() {
        hash_field(&mut digest, &member.path_key);
        hash_field(&mut digest, &member.generation.to_be_bytes());
    }
    hash_field(&mut digest, b"selected-entry");
    hash_field(&mut digest, &entry.path_key);
    hash_field(&mut digest, &entry.generation.to_be_bytes());
    format!("sha256:{}", finish_hex(digest))
}

fn index_entry_occurrence_ref(source_record_id: &SourceRecordId, session_id: &str) -> String {
    occurrence_ref(
        b"claude-catalog-index-entry-occurrence-v1",
        &[source_record_id.as_bytes(), session_id.as_bytes()],
    )
}

fn source_record_occurrence_ref(source_record_id: &SourceRecordId) -> String {
    occurrence_ref(
        b"claude-catalog-head-record-occurrence-v1",
        &[source_record_id.as_bytes()],
    )
}

fn occurrence_ref(contract: &[u8], fields: &[&[u8]]) -> String {
    let mut digest = Sha256::new();
    hash_field(&mut digest, contract);
    for field in fields {
        hash_field(&mut digest, field);
    }
    format!("sha256:{}", finish_hex(digest))
}

fn hash_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn finish_hex(digest: Sha256) -> String {
    let mut output = String::with_capacity(64);
    for byte in digest.finalize() {
        write!(output, "{byte:02x}").expect("writing to String is infallible");
    }
    output
}

fn pair_key(left: &[u8], right: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(left.len() + right.len() + 16);
    for value in [left, right] {
        output.extend_from_slice(&(value.len() as u64).to_be_bytes());
        output.extend_from_slice(value);
    }
    output
}

fn encode_project_key(cwd: &str) -> String {
    cwd.replace(['/', '\\'], "-")
}

fn identity_digest<T>(values: &BTreeSet<T>) -> String
where
    T: Serialize + Ord,
{
    hex_digest(values)
}

fn semantic_digest<T>(values: &BTreeSet<T>) -> String
where
    T: Serialize + Ord,
{
    format!("sha256:{}", hex_digest(values))
}

fn hex_digest<T>(values: &BTreeSet<T>) -> String
where
    T: Serialize + Ord,
{
    let mut digest = Sha256::new();
    for value in values {
        let encoded = serde_json::to_vec(value).expect("conformance value is serializable");
        digest.update((encoded.len() as u64).to_be_bytes());
        digest.update(encoded);
    }
    let mut output = String::with_capacity(64);
    for byte in digest.finalize() {
        write!(output, "{byte:02x}").expect("writing to String is infallible");
    }
    output
}

#[cfg(test)]
mod tests;
