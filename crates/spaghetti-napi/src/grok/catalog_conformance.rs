//! RFC 012B Grok candidate conformance oracle.
//!
//! This module is test-only: it proves bounded common-driver and adapter
//! identity behavior without granting a candidate support package runtime
//! catalog authority.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::adapter::{
    candidate_catalog_path_coordinates, candidate_catalog_summary_coordinates, GrokAdapter,
    GrokCatalogCoordinates,
};
use crate::adapter::{
    AdapterError, AdapterId, AgentAdapter, CanonicalEntityKey, CanonicalFactId,
    CanonicalSourceInstanceKey, DecodeDisposition, DriverSpec, Fact, FactRevisionId,
    FactSemanticContext, SourceAccess, SourceInstance, SourceInstanceKey, SourceInstanceSpec,
    SourceObjectDescriptor, SourceObjectList, SourceObjectListRequest, SourceQuery, SourceRecordId,
    SourceRoot, SourceRows, SourceSnapshot, StreamSpec,
};
use crate::decode_runtime::{decode_record, DecodeRuntimeLimits, DecodeRuntimeRequest};
use crate::source::{
    DirectoryEntryKind, DirectoryEntryState, DirectoryScan, DirectorySelection, DirectorySnapshot,
    RecordOrigin, ReplaceDocument, ReplaceRead, SourceMediaType,
};

const FIXTURE_CONTRACT_VERSION: u32 = 1;
const ADAPTER_ID: &str = "grok";
const MEMBERSHIP_STREAM_ID: &str = "session-membership";
const SUMMARY_STREAM_ID: &str = "session-summaries";
const PROJECT_FACT_KIND: &str = "catalog.project-membership";
const SESSION_FACT_KIND: &str = "catalog.session-membership";
const ASSOCIATION_FACT_KIND: &str = "catalog.session-project-association";
const SUMMARY_FACT_KIND: &str = "catalog.session-summary-metadata";
const FIXTURE_SOURCE_INSTANCE: &[u8] = b"grok-small-candidate-fixture-root-v1";
const PLANNING_EVIDENCE_ID: &str = "grok.catalog-candidate-2026-08-15";
const SUPPORT_RELEASE_ID: &str = "grok-support-2026-08-15-candidate";
const SOURCE_DECLARATION_ID: &str = "grok-sources-2026-08-15-candidate";
const CANDIDATE_MEMBERSHIP_MAX_ENTRIES: usize = 100_000;
const CANDIDATE_MEMBERSHIP_MAX_DEPTH: usize = 8;
const CANDIDATE_SUMMARY_MAX_BYTES: usize = 1024 * 1024;
const FRAMING_CONTRACT_VERSION: u32 = 1;
const FACT_REVISION_CONTRACT_VERSION: u32 = 1;
const ADMITTED_SIDECARS: [&str; 4] = [
    "chat_history.jsonl",
    "summary.json",
    "events.jsonl",
    "signals.json",
];

type ConformanceResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrokCandidateConformanceFixture {
    fixture_contract_version: u32,
    adapter_id: String,
    planning_evidence_id: String,
    support_release_id: String,
    source_declaration_id: String,
    support_release_status: String,
    catalog_execution_authorized: bool,
    planned_composition_status: String,
    admission_policy_status: String,
    bounds: GrokCandidateBounds,
    independent_oracle: GrokIndependentOracle,
    rust_conformance: GrokRustConformance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrokCandidateBounds {
    membership_max_entries: u64,
    membership_max_depth: u64,
    admitted_sidecar_name_count: u64,
    summary_max_document_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrokIndependentOracle {
    project_count: u64,
    session_count: u64,
    project_identity_digest: String,
    session_identity_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrokRustConformance {
    membership_generation: u64,
    admitted_membership_object_count: u64,
    member_count: u64,
    summary_source_record_count: u64,
    summary_metadata_unavailable_count: u64,
    summary_bytes_read: u64,
    hydrated_project_count: u64,
    hydrated_session_count: u64,
    project_entity_count: u64,
    session_entity_count: u64,
    fact_identity_count: u64,
    fact_revision_count: u64,
    membership_identity_digest: String,
    source_record_identity_digest: String,
    entity_identity_digest: String,
    fact_identity_digest: String,
    fact_revision_digest: String,
    hydrated_identity_digest: String,
}

#[derive(Default)]
struct CandidateProjection {
    membership_generation: u64,
    admitted_membership_objects: u64,
    member_coordinates: BTreeMap<PathBuf, GrokCatalogCoordinates>,
    projects: BTreeSet<(String, String)>,
    sessions: BTreeSet<(String, String, String)>,
    hydrated_projects: BTreeSet<(String, String)>,
    hydrated_sessions: BTreeSet<(String, String, String)>,
    summary_source_records: BTreeSet<SourceRecordId>,
    project_entities: BTreeSet<CanonicalEntityKey>,
    session_entities: BTreeSet<CanonicalEntityKey>,
    fact_ids: BTreeSet<(String, CanonicalFactId)>,
    fact_revisions: BTreeSet<(String, FactRevisionId)>,
    summary_metadata_unavailable: u64,
    summary_bytes_read: u64,
}

impl CandidateProjection {
    fn report(&self) -> GrokCandidateConformanceFixture {
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
        let mut hydrated = BTreeSet::new();
        hydrated.extend(
            self.hydrated_projects
                .iter()
                .cloned()
                .map(|(adapter, project)| ("project".to_string(), adapter, project, String::new())),
        );
        hydrated.extend(
            self.hydrated_sessions
                .iter()
                .cloned()
                .map(|(adapter, project, session)| {
                    ("session".to_string(), adapter, project, session)
                }),
        );
        GrokCandidateConformanceFixture {
            fixture_contract_version: FIXTURE_CONTRACT_VERSION,
            adapter_id: ADAPTER_ID.to_string(),
            planning_evidence_id: PLANNING_EVIDENCE_ID.to_string(),
            support_release_id: SUPPORT_RELEASE_ID.to_string(),
            source_declaration_id: SOURCE_DECLARATION_ID.to_string(),
            support_release_status: "candidate".to_string(),
            catalog_execution_authorized: false,
            planned_composition_status: "planned_unbound".to_string(),
            admission_policy_status: "current_candidate_declaration".to_string(),
            bounds: GrokCandidateBounds {
                membership_max_entries: CANDIDATE_MEMBERSHIP_MAX_ENTRIES as u64,
                membership_max_depth: CANDIDATE_MEMBERSHIP_MAX_DEPTH as u64,
                admitted_sidecar_name_count: ADMITTED_SIDECARS.len() as u64,
                summary_max_document_bytes: CANDIDATE_SUMMARY_MAX_BYTES as u64,
            },
            independent_oracle: GrokIndependentOracle {
                project_count: self.projects.len() as u64,
                session_count: self.sessions.len() as u64,
                project_identity_digest: identity_digest(&self.projects),
                session_identity_digest: identity_digest(&self.sessions),
            },
            rust_conformance: GrokRustConformance {
                membership_generation: self.membership_generation,
                admitted_membership_object_count: self.admitted_membership_objects,
                member_count: self.member_coordinates.len() as u64,
                summary_source_record_count: self.summary_source_records.len() as u64,
                summary_metadata_unavailable_count: self.summary_metadata_unavailable,
                summary_bytes_read: self.summary_bytes_read,
                hydrated_project_count: self.hydrated_projects.len() as u64,
                hydrated_session_count: self.hydrated_sessions.len() as u64,
                project_entity_count: self.project_entities.len() as u64,
                session_entity_count: self.session_entities.len() as u64,
                fact_identity_count: self.fact_ids.len() as u64,
                fact_revision_count: self.fact_revisions.len() as u64,
                membership_identity_digest: semantic_digest(&self.sessions),
                source_record_identity_digest: semantic_digest(&self.summary_source_records),
                entity_identity_digest: semantic_digest(&entity_ids),
                fact_identity_digest: semantic_digest(&self.fact_ids),
                fact_revision_digest: semantic_digest(&self.fact_revisions),
                hydrated_identity_digest: semantic_digest(&hydrated),
            },
        }
    }
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/small-grok/.grok")
}

fn fixture_instance(root: &Path, registration_seed: u64) -> ConformanceResult<SourceInstance> {
    Ok(SourceInstance {
        id: registration_seed,
        spec: SourceInstanceSpec {
            identity_contract_version: 1,
            stable_key: SourceInstanceKey::new(FIXTURE_SOURCE_INSTANCE.to_vec())?,
            display_name: "Grok candidate conformance".to_string(),
            roots: vec![
                SourceRoot {
                    name: "home".to_string(),
                    path: root.to_path_buf(),
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
        .ok_or_else(|| format!("Grok candidate stream {stream_id} is not declared").into())
}

fn candidate_projection(
    root: &Path,
    registration_seed: u64,
) -> ConformanceResult<CandidateProjection> {
    let adapter = GrokAdapter::new();
    let instance = fixture_instance(root, registration_seed)?;
    let streams = adapter.streams(&instance)?;
    let membership_stream = exact_stream(&streams, MEMBERSHIP_STREAM_ID)?;
    let summary_stream = exact_stream(&streams, SUMMARY_STREAM_ID)?;
    let DriverSpec::DirectorySnapshot(membership_config) = membership_stream.driver.clone() else {
        return Err("Grok candidate membership stream is not DirectorySnapshot".into());
    };
    if membership_config.max_entries != CANDIDATE_MEMBERSHIP_MAX_ENTRIES
        || membership_config.max_depth != CANDIDATE_MEMBERSHIP_MAX_DEPTH
    {
        return Err("Grok candidate membership bounds drifted".into());
    }
    let DriverSpec::ReplaceDocument(summary_config) = summary_stream.driver.clone() else {
        return Err("Grok candidate summary stream is not ReplaceDocument".into());
    };
    if summary_config.max_document_bytes != CANDIDATE_SUMMARY_MAX_BYTES {
        return Err("Grok candidate summary bound drifted".into());
    }

    let sessions_root = root.join("sessions");
    let scan = DirectorySnapshot::new(membership_config)?.scan(
        &sessions_root,
        None,
        &|relative: &Path, kind| match kind {
            DirectoryEntryKind::Directory => DirectorySelection::Recurse,
            DirectoryEntryKind::File
                if relative
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| ADMITTED_SIDECARS.contains(&name)) =>
            {
                DirectorySelection::Include
            }
            DirectoryEntryKind::File => DirectorySelection::Ignore,
        },
    )?;
    let DirectoryScan::Snapshot { checkpoint, .. } = scan else {
        return Err("Grok candidate membership was not stably available".into());
    };
    if checkpoint.generation == 0 {
        return Err("Grok candidate membership generation must be positive".into());
    }

    let mut projection = CandidateProjection {
        membership_generation: checkpoint.generation,
        admitted_membership_objects: checkpoint.entries.len() as u64,
        ..CandidateProjection::default()
    };
    let mut coordinate_directories = BTreeMap::<(String, String), PathBuf>::new();
    let mut session_projects = BTreeMap::<String, String>::new();
    let mut summaries = Vec::<DirectoryEntryState>::new();
    for entry in checkpoint.entries.values() {
        let relative_path = PathBuf::from(&entry.display_path);
        let directory = relative_path
            .parent()
            .ok_or("Grok membership object has no session directory")?
            .to_path_buf();
        let coordinates = candidate_catalog_path_coordinates(&relative_path)?;
        if let Some(existing) = projection.member_coordinates.get(&directory) {
            if existing != &coordinates {
                return Err("one Grok session directory produced conflicting coordinates".into());
            }
        } else {
            let identity = (
                coordinates.native_project_key.clone(),
                coordinates.session_id.clone(),
            );
            if coordinate_directories
                .insert(identity, directory.clone())
                .is_some()
            {
                return Err("distinct Grok directories produced one fallback identity".into());
            }
            if let Some(project) = session_projects.insert(
                coordinates.session_id.clone(),
                coordinates.native_project_key.clone(),
            ) {
                if project != coordinates.native_project_key {
                    return Err("one Grok session ID appeared under competing projects".into());
                }
            }
            projection.member_coordinates.insert(directory, coordinates);
        }
        if relative_path.file_name().and_then(|name| name.to_str()) == Some("summary.json") {
            summaries.push(entry.clone());
        }
    }
    add_membership_identities(&mut projection)?;
    let summary_driver = ReplaceDocument::new(summary_config)?;
    for (index, entry) in summaries.iter().enumerate() {
        enrich_summary(
            &adapter,
            &instance,
            &summary_stream,
            &summary_driver,
            &sessions_root,
            entry,
            index,
            registration_seed,
            &mut projection,
        )?;
    }
    Ok(projection)
}

fn add_membership_identities(projection: &mut CandidateProjection) -> ConformanceResult<()> {
    let adapter_id = AdapterId::new(ADAPTER_ID)?;
    let source_instance_key = CanonicalSourceInstanceKey::derive(1, FIXTURE_SOURCE_INSTANCE)?;
    for coordinates in projection.member_coordinates.values() {
        projection.projects.insert((
            ADAPTER_ID.to_string(),
            coordinates.native_project_key.clone(),
        ));
        projection.sessions.insert((
            ADAPTER_ID.to_string(),
            coordinates.native_project_key.clone(),
            coordinates.session_id.clone(),
        ));
    }
    let membership_revision = sha256_length_prefixed_bytes(&projection.sessions);
    for (_, project_key) in &projection.projects {
        let entity = CanonicalEntityKey::derive(
            adapter_id.as_str(),
            &source_instance_key,
            "project",
            project_key.as_bytes(),
        )?;
        let fact = CanonicalFactId::native(
            adapter_id.as_str(),
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
            FactRevisionId::derive(&fact, FACT_REVISION_CONTRACT_VERSION, &membership_revision)?,
        ));
    }
    for (_, project_key, session_id) in &projection.sessions {
        let entity = CanonicalEntityKey::derive(
            adapter_id.as_str(),
            &source_instance_key,
            "session",
            session_id.as_bytes(),
        )?;
        let session_fact = CanonicalFactId::native(
            adapter_id.as_str(),
            &source_instance_key,
            SESSION_FACT_KIND,
            session_id.as_bytes(),
        )?;
        let association_fact = CanonicalFactId::native(
            adapter_id.as_str(),
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
                FactRevisionId::derive(
                    &fact,
                    FACT_REVISION_CONTRACT_VERSION,
                    &membership_revision,
                )?,
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn enrich_summary(
    adapter: &GrokAdapter,
    instance: &SourceInstance,
    summary_stream: &StreamSpec,
    driver: &ReplaceDocument,
    sessions_root: &Path,
    entry: &DirectoryEntryState,
    object_index: usize,
    registration_seed: u64,
    projection: &mut CandidateProjection,
) -> ConformanceResult<()> {
    let relative_path = PathBuf::from(&entry.display_path);
    let directory = relative_path
        .parent()
        .ok_or("Grok summary has no session directory")?;
    let path_coordinates = projection
        .member_coordinates
        .get(directory)
        .ok_or("Grok summary metadata cannot fabricate catalog membership")?
        .clone();
    let object_number = u64::try_from(object_index)?;
    let origin = RecordOrigin {
        source_instance_id: registration_seed,
        stream_id: registration_seed
            .checked_add(1)
            .ok_or("stream id overflow")?,
        object_id: registration_seed
            .checked_add(2)
            .and_then(|value| value.checked_add(object_number))
            .ok_or("object id overflow")?,
        observed_at: 1,
        source_timestamp_hint: None,
        media_type: SourceMediaType::new("application/json")?,
    };
    let read = driver.read_confined(sessions_root, &relative_path, None, &origin, false)?;
    let record = match read {
        ReplaceRead::Record { record, .. } => record,
        ReplaceRead::Quarantined { .. } => {
            projection.summary_metadata_unavailable = projection
                .summary_metadata_unavailable
                .checked_add(1)
                .ok_or("summary-unavailable count overflow")?;
            return Ok(());
        }
        ReplaceRead::Missing
        | ReplaceRead::RetryTransient
        | ReplaceRead::Unchanged { .. }
        | ReplaceRead::Removed { .. } => {
            return Err("Grok summary changed during the complete membership pass".into())
        }
    };
    projection.summary_bytes_read = projection
        .summary_bytes_read
        .checked_add(u64::try_from(record.payload.len())?)
        .ok_or("summary byte accounting overflow")?;
    let summary: serde_json::Value =
        match serde_json::from_slice::<serde_json::Value>(&record.payload) {
            Ok(summary) if summary.is_object() => summary,
            _ => {
                projection.summary_metadata_unavailable = projection
                    .summary_metadata_unavailable
                    .checked_add(1)
                    .ok_or("summary-unavailable count overflow")?;
                return Ok(());
            }
        };
    let summary_coordinates = candidate_catalog_summary_coordinates(&relative_path, &summary)?;
    if summary_coordinates != path_coordinates {
        return Err(
            "Grok summary identity disagrees with membership; explicit relation evidence is required"
                .into(),
        );
    }

    let adapter_id = AdapterId::new(ADAPTER_ID)?;
    let semantic_context = FactSemanticContext::new(
        &adapter_id,
        1,
        FIXTURE_SOURCE_INSTANCE,
        SUMMARY_STREAM_ID.as_bytes(),
        &entry.path_key,
        FRAMING_CONTRACT_VERSION,
    )?;
    let descriptor = SourceObjectDescriptor {
        stream_id: summary_stream.id.clone(),
        object_key: entry.path_key.clone(),
        relative_path: relative_path.clone(),
    };
    let object_context = adapter.bootstrap_object(instance, &descriptor)?;
    let decoded = decode_record(DecodeRuntimeRequest {
        adapter,
        decoder: &summary_stream.decoder,
        object_context: &object_context,
        source_access: &CatalogDecoderDependencyAccessDenied,
        record: &record,
        semantic_context: &semantic_context,
        decoder_state: None,
        retention: summary_stream.retention,
        limits: DecodeRuntimeLimits {
            max_facts: 8,
            max_diagnostics: 4,
        },
    })
    .result
    .map_err(|_| "Grok catalog record failed at the common decode boundary")?;
    if decoded.disposition != DecodeDisposition::Applied {
        return Err("valid Grok summary did not apply through the durable decoder".into());
    }
    let mut decoded_session = None;
    for envelope in decoded.batch.facts() {
        if let Fact::Session(session) = &envelope.value {
            if decoded_session.replace(session).is_some() {
                return Err("Grok summary emitted more than one session fact".into());
            }
        }
    }
    let decoded_session = decoded_session.ok_or("Grok summary emitted no session fact")?;
    if decoded_session.native_session_id != path_coordinates.session_id
        || decoded_session.native_project_key != path_coordinates.native_project_key
        || decoded_session.cwd.as_deref() != Some(path_coordinates.cwd.as_str())
    {
        return Err("Grok durable summary decode drifted from candidate coordinates".into());
    }
    projection.hydrated_projects.insert((
        ADAPTER_ID.to_string(),
        decoded_session.native_project_key.clone(),
    ));
    projection.hydrated_sessions.insert((
        ADAPTER_ID.to_string(),
        decoded_session.native_project_key.clone(),
        decoded_session.native_session_id.clone(),
    ));

    let source_record_id = semantic_context.source_record_id(&record)?;
    let summary_fact = CanonicalFactId::native(
        ADAPTER_ID,
        &semantic_context.source_instance_key(),
        SUMMARY_FACT_KIND,
        path_coordinates.session_id.as_bytes(),
    )?;
    projection.summary_source_records.insert(source_record_id);
    projection
        .fact_ids
        .insert((SUMMARY_FACT_KIND.to_string(), summary_fact));
    projection.fact_revisions.insert((
        SUMMARY_FACT_KIND.to_string(),
        FactRevisionId::derive(
            &summary_fact,
            FACT_REVISION_CONTRACT_VERSION,
            source_record_id.as_bytes(),
        )?,
    ));
    Ok(())
}

struct CatalogDecoderDependencyAccessDenied;

impl SourceAccess for CatalogDecoderDependencyAccessDenied {
    fn read_object(
        &self,
        _root_name: &str,
        _relative_path: &Path,
        _max_bytes: usize,
    ) -> Result<SourceSnapshot, AdapterError> {
        Err(AdapterError::invalid_contract(
            "Grok summary catalog decoder has no declared dependency read",
        ))
    }

    fn query_source_db(&self, _query: &SourceQuery) -> Result<SourceRows, AdapterError> {
        Err(AdapterError::invalid_contract(
            "Grok summary catalog decoder has no declared dependency query",
        ))
    }

    fn list_objects(
        &self,
        _request: &SourceObjectListRequest,
    ) -> Result<SourceObjectList, AdapterError> {
        Err(AdapterError::invalid_contract(
            "Grok summary catalog decoder has no declared dependency listing",
        ))
    }
}

fn pair_key(left: &[u8], right: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(left.len() + right.len() + 16);
    for value in [left, right] {
        output.extend_from_slice(&(value.len() as u64).to_be_bytes());
        output.extend_from_slice(value);
    }
    output
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
    let digest = sha256_length_prefixed_bytes(values);
    let mut output = String::with_capacity(64);
    for byte in digest {
        write!(output, "{byte:02x}").expect("writing to String is infallible");
    }
    output
}

fn sha256_length_prefixed_bytes<T>(values: &BTreeSet<T>) -> [u8; 32]
where
    T: Serialize + Ord,
{
    let mut digest = Sha256::new();
    for value in values {
        let encoded = serde_json::to_vec(value).expect("conformance value is serializable");
        digest.update((encoded.len() as u64).to_be_bytes());
        digest.update(encoded);
    }
    digest.finalize().into()
}

#[cfg(test)]
mod tests;
