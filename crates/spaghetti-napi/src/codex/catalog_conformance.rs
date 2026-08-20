//! RFC 012B Codex candidate conformance oracle.
//!
//! This module is test-only: it proves bounded common-driver and adapter
//! identity behavior without granting a candidate support package runtime
//! catalog authority.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::adapter::{normalize_session_meta, CodexAdapter};
use crate::adapter::{
    AdapterId, AgentAdapter, CanonicalEntityKey, CanonicalFactId, ContractVersionOffer,
    ContractVersionRequest, DecodeContext, DriverSpec, Fact, FactBatch, FactRevisionId,
    FactSemanticContext, NativeArtifactProbe, SourceInstance, SourceInstanceKey,
    SourceInstanceSpec, SourceObjectDescriptor, SourceRecordId, SourceRoot, SupportReleaseStatus,
};
use crate::source::{
    AppendDelimitedConfig, AppendDelimitedFile, AppendItem, AppendRead, AppendTransition,
    DirectoryEntryKind, DirectoryScan, DirectorySelection, DirectorySnapshot,
    DirectorySnapshotConfig, RecordOrigin, SourceMediaType, SourceRecord,
};

const FIXTURE_CONTRACT_VERSION: u32 = 1;
const ADAPTER_ID: &str = "codex";
const STREAM_ID: &str = "rollout-sessions";
const PROJECT_FACT_KIND: &str = "catalog.project-membership";
const SESSION_FACT_KIND: &str = "catalog.session-membership";
const FIXTURE_SOURCE_INSTANCE: &[u8] = b"codex-small-candidate-fixture-root-v1";
const PLANNING_EVIDENCE_ID: &str = "codex.catalog-candidate-2026-08-15";
const SUPPORT_RELEASE_ID: &str = "codex-support-2026-08-15-candidate";
const CANDIDATE_HEAD_PREFIX_BYTES: u64 = 64 * 1024;
// The common append driver's record bound excludes the LF delimiter. A
// 64-KiB Phase-0 head prefix therefore proves at most 65,535 payload bytes.
const CANDIDATE_HEAD_RECORD_PAYLOAD_BYTES: usize = 64 * 1024 - 1;
const CANDIDATE_CHECKPOINT_ANCHOR_BYTES: usize = 4 * 1024;
// The bounded common driver may read one 64-KiB framing chunk and then re-read
// the committed prefix anchor used by its checkpoint continuity proof.
const CANDIDATE_HEAD_PHYSICAL_BYTES: u64 =
    CANDIDATE_HEAD_PREFIX_BYTES + CANDIDATE_CHECKPOINT_ANCHOR_BYTES as u64;
const CANDIDATE_MAX_ENTRIES: usize = 250_000;
const CANDIDATE_MAX_DEPTH: usize = 64;
const FRAMING_CONTRACT_VERSION: u32 = 1;
const FACT_REVISION_CONTRACT_VERSION: u32 = 1;

type ConformanceResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexCandidateConformanceFixture {
    fixture_contract_version: u32,
    adapter_id: String,
    planning_evidence_id: String,
    support_release_id: String,
    support_release_status: String,
    catalog_execution_authorized: bool,
    head_bound_status: String,
    bounds: CodexCandidateBounds,
    independent_oracle: CodexIndependentOracle,
    rust_conformance: CodexRustConformance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexCandidateBounds {
    max_head_prefix_bytes: u64,
    max_record_payload_bytes: u64,
    max_checkpoint_anchor_bytes: u64,
    max_physical_read_bytes_per_object: u64,
    max_entries: u64,
    max_depth: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexIndependentOracle {
    project_count: u64,
    session_count: u64,
    project_identity_digest: String,
    session_identity_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexRustConformance {
    source_record_count: u64,
    project_entity_count: u64,
    session_entity_count: u64,
    fact_identity_count: u64,
    fact_revision_count: u64,
    total_physical_bytes_read: u64,
    source_record_identity_digest: String,
    entity_identity_digest: String,
    fact_identity_digest: String,
    fact_revision_digest: String,
}

#[derive(Default)]
struct CandidateProjection {
    projects: BTreeSet<(String, String)>,
    sessions: BTreeSet<(String, String, String)>,
    source_records: BTreeSet<SourceRecordId>,
    project_entities: BTreeSet<CanonicalEntityKey>,
    session_entities: BTreeSet<CanonicalEntityKey>,
    fact_ids: BTreeSet<(String, CanonicalFactId)>,
    fact_revisions: BTreeSet<(String, FactRevisionId)>,
    total_physical_bytes_read: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NativeCatalogIdentities {
    projects: BTreeSet<(String, String)>,
    sessions: BTreeSet<(String, String, String)>,
}

impl CandidateProjection {
    fn report(&self) -> CodexCandidateConformanceFixture {
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
        CodexCandidateConformanceFixture {
            fixture_contract_version: FIXTURE_CONTRACT_VERSION,
            adapter_id: ADAPTER_ID.to_string(),
            planning_evidence_id: PLANNING_EVIDENCE_ID.to_string(),
            support_release_id: SUPPORT_RELEASE_ID.to_string(),
            support_release_status: "candidate".to_string(),
            catalog_execution_authorized: false,
            head_bound_status: "candidate_fixture_evidence".to_string(),
            bounds: CodexCandidateBounds {
                max_head_prefix_bytes: CANDIDATE_HEAD_PREFIX_BYTES,
                max_record_payload_bytes: CANDIDATE_HEAD_RECORD_PAYLOAD_BYTES as u64,
                max_checkpoint_anchor_bytes: CANDIDATE_CHECKPOINT_ANCHOR_BYTES as u64,
                max_physical_read_bytes_per_object: CANDIDATE_HEAD_PHYSICAL_BYTES,
                max_entries: CANDIDATE_MAX_ENTRIES as u64,
                max_depth: CANDIDATE_MAX_DEPTH as u64,
            },
            independent_oracle: CodexIndependentOracle {
                project_count: self.projects.len() as u64,
                session_count: self.sessions.len() as u64,
                project_identity_digest: identity_digest(&self.projects),
                session_identity_digest: identity_digest(&self.sessions),
            },
            rust_conformance: CodexRustConformance {
                source_record_count: self.source_records.len() as u64,
                project_entity_count: self.project_entities.len() as u64,
                session_entity_count: self.session_entities.len() as u64,
                fact_identity_count: self.fact_ids.len() as u64,
                fact_revision_count: self.fact_revisions.len() as u64,
                total_physical_bytes_read: self.total_physical_bytes_read,
                source_record_identity_digest: semantic_digest(&self.source_records),
                entity_identity_digest: semantic_digest(&entity_ids),
                fact_identity_digest: semantic_digest(&self.fact_ids),
                fact_revision_digest: semantic_digest(&self.fact_revisions),
            },
        }
    }
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/small-codex/.codex")
}

fn candidate_files(root: &Path) -> ConformanceResult<Vec<(Vec<u8>, PathBuf)>> {
    let sessions = root.join("sessions");
    let scan = DirectorySnapshot::new(DirectorySnapshotConfig {
        max_entries: CANDIDATE_MAX_ENTRIES,
        max_entries_per_directory: CANDIDATE_MAX_ENTRIES,
        max_depth: CANDIDATE_MAX_DEPTH,
    })?
    .scan(&sessions, None, &|relative: &Path, kind| match kind {
        DirectoryEntryKind::Directory => DirectorySelection::Recurse,
        DirectoryEntryKind::File
            if relative
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl")) =>
        {
            DirectorySelection::Include
        }
        DirectoryEntryKind::File => DirectorySelection::Ignore,
    })?;
    let DirectoryScan::Snapshot { checkpoint, .. } = scan else {
        return Err("Codex fixture directory was not stably available".into());
    };
    Ok(checkpoint
        .entries
        .values()
        .map(|entry| (entry.path_key.clone(), PathBuf::from(&entry.display_path)))
        .collect())
}

fn head_projection(root: &Path, registration_seed: u64) -> ConformanceResult<CandidateProjection> {
    let sessions_root = root.join("sessions");
    let adapter_id = AdapterId::new(ADAPTER_ID)?;
    let mut projection = CandidateProjection::default();
    for (index, (object_key, relative_path)) in candidate_files(root)?.into_iter().enumerate() {
        let object_number = u64::try_from(index)?;
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
            media_type: SourceMediaType::new("application/x-ndjson")?,
        };
        let mut config = AppendDelimitedConfig::json_lines();
        config.max_record_bytes = CANDIDATE_HEAD_RECORD_PAYLOAD_BYTES;
        config.max_batch_bytes = CANDIDATE_HEAD_RECORD_PAYLOAD_BYTES;
        config.max_records_per_batch = 1;
        config.prefix_anchor_bytes = CANDIDATE_CHECKPOINT_ANCHOR_BYTES;
        let read = AppendDelimitedFile::new(config)?.read_confined_bounded(
            &sessions_root,
            &relative_path,
            None,
            &origin,
            false,
            CANDIDATE_HEAD_PHYSICAL_BYTES,
        )?;
        let AppendRead::Batch {
            items,
            transition,
            bytes_read,
            ..
        } = read
        else {
            return Err("Codex candidate head read did not produce a stable batch".into());
        };
        if transition != AppendTransition::Initial || items.len() != 1 {
            return Err("Codex candidate head must frame exactly the first logical record".into());
        }
        projection.total_physical_bytes_read = projection
            .total_physical_bytes_read
            .checked_add(bytes_read)
            .ok_or("physical read accounting overflow")?;
        let record = match items.into_iter().next().expect("one item checked") {
            AppendItem::Record(record) => record,
            AppendItem::Quarantined(_) => {
                return Err("Codex candidate head record exceeded its evidence bound".into())
            }
        };
        add_head_record(&adapter_id, &object_key, &record, &mut projection)?;
    }
    Ok(projection)
}

fn add_head_record(
    adapter_id: &AdapterId,
    object_key: &[u8],
    record: &SourceRecord,
    projection: &mut CandidateProjection,
) -> ConformanceResult<()> {
    let value: serde_json::Value = serde_json::from_slice(&record.payload)?;
    let root = value
        .as_object()
        .ok_or("Codex candidate head record must be an object")?;
    if root.get("type").and_then(serde_json::Value::as_str) != Some("session_meta") {
        return Err("Codex candidate head must begin with session_meta".into());
    }
    let metadata = normalize_session_meta(root).map_err(|error| format!("{error:?}"))?;
    if metadata.internal {
        return Ok(());
    }
    let semantic_context = FactSemanticContext::new(
        adapter_id,
        1,
        FIXTURE_SOURCE_INSTANCE,
        STREAM_ID.as_bytes(),
        object_key,
        FRAMING_CONTRACT_VERSION,
    )?;
    let source_record_id = semantic_context.source_record_id(record)?;
    let source_instance_key = semantic_context.source_instance_key();
    let project_entity = CanonicalEntityKey::derive(
        ADAPTER_ID,
        &source_instance_key,
        "project",
        metadata.native_project_key.as_bytes(),
    )?;
    let session_entity = CanonicalEntityKey::derive(
        ADAPTER_ID,
        &source_instance_key,
        "session",
        metadata.session_id.as_bytes(),
    )?;
    // These are conformance-oracle identities for the planned catalog fact
    // families. They do not construct a CatalogAssertion, assign field
    // authority, or enter the reducer; a promoted ADS/declaration must bind
    // those semantics before an execution composition can emit them.
    let project_fact = CanonicalFactId::native(
        ADAPTER_ID,
        &source_instance_key,
        PROJECT_FACT_KIND,
        metadata.native_project_key.as_bytes(),
    )?;
    let session_fact = CanonicalFactId::native(
        ADAPTER_ID,
        &source_instance_key,
        SESSION_FACT_KIND,
        metadata.session_id.as_bytes(),
    )?;
    let project_revision = FactRevisionId::derive(
        &project_fact,
        FACT_REVISION_CONTRACT_VERSION,
        source_record_id.as_bytes(),
    )?;
    let session_revision = FactRevisionId::derive(
        &session_fact,
        FACT_REVISION_CONTRACT_VERSION,
        source_record_id.as_bytes(),
    )?;

    projection
        .projects
        .insert((ADAPTER_ID.to_string(), metadata.native_project_key.clone()));
    projection.sessions.insert((
        ADAPTER_ID.to_string(),
        metadata.native_project_key,
        metadata.session_id,
    ));
    projection.source_records.insert(source_record_id);
    projection.project_entities.insert(project_entity);
    projection.session_entities.insert(session_entity);
    projection
        .fact_ids
        .insert((PROJECT_FACT_KIND.to_string(), project_fact));
    projection
        .fact_ids
        .insert((SESSION_FACT_KIND.to_string(), session_fact));
    projection
        .fact_revisions
        .insert((PROJECT_FACT_KIND.to_string(), project_revision));
    projection
        .fact_revisions
        .insert((SESSION_FACT_KIND.to_string(), session_revision));
    Ok(())
}

fn full_decoder_identities(
    root: &Path,
    registration_seed: u64,
) -> ConformanceResult<NativeCatalogIdentities> {
    let adapter = CodexAdapter::new();
    let instance = SourceInstance {
        id: registration_seed,
        spec: SourceInstanceSpec {
            identity_contract_version: 1,
            stable_key: SourceInstanceKey::new(FIXTURE_SOURCE_INSTANCE.to_vec())?,
            display_name: "Codex candidate conformance".to_string(),
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
    };
    let stream = adapter
        .streams(&instance)?
        .into_iter()
        .find(|stream| stream.id.as_str() == STREAM_ID)
        .ok_or("Codex candidate conformance stream is not declared")?;
    let DriverSpec::AppendDelimited(config) = stream.driver.clone() else {
        return Err("Codex durable stream no longer uses AppendDelimited".into());
    };
    let driver = AppendDelimitedFile::new(config)?;
    let sessions_root = root.join("sessions");
    let mut projects = BTreeSet::new();
    let mut sessions = BTreeSet::new();
    for (index, (object_key, relative_path)) in candidate_files(root)?.into_iter().enumerate() {
        let object_number = u64::try_from(index)?;
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
            media_type: SourceMediaType::new("application/x-ndjson")?,
        };
        let descriptor = SourceObjectDescriptor {
            stream_id: stream.id.clone(),
            object_key,
            relative_path: relative_path.clone(),
        };
        let object_context = adapter.bootstrap_object(&instance, &descriptor)?;
        let mut checkpoint = None;
        let mut decoder_state = None;
        loop {
            let read = driver.read_confined(
                &sessions_root,
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
                return Err("Codex full fixture read did not produce a stable batch".into());
            };
            for item in items {
                let record = match item {
                    AppendItem::Record(record) => record,
                    AppendItem::Quarantined(_) => {
                        return Err("Codex full fixture contains an oversized record".into())
                    }
                };
                let mut batch = FactBatch::new(64, 16)?;
                adapter.decode(
                    DecodeContext {
                        decoder: &stream.decoder,
                        object_context: &object_context,
                        decoder_state: decoder_state.as_deref(),
                    },
                    &record,
                    &mut batch,
                )?;
                decoder_state = batch.next_decoder_state().map(ToOwned::to_owned);
                for envelope in batch.facts() {
                    if let Fact::Session(session) = &envelope.value {
                        projects
                            .insert((ADAPTER_ID.to_string(), session.native_project_key.clone()));
                        sessions.insert((
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
    Ok(NativeCatalogIdentities { projects, sessions })
}

fn identity_digest<T>(values: &BTreeSet<T>) -> String
where
    T: Serialize + Ord,
{
    sha256_length_prefixed(values)
}

fn semantic_digest<T>(values: &BTreeSet<T>) -> String
where
    T: Serialize + Ord,
{
    format!("sha256:{}", sha256_length_prefixed(values))
}

fn sha256_length_prefixed<T>(values: &BTreeSet<T>) -> String
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

fn compatible_contracts() -> (ContractVersionRequest, ContractVersionOffer) {
    (
        ContractVersionRequest {
            selection_contract_version: 1,
            model_major: 1,
            external_entity_reference_version: 1,
            semantic_revision_reference_version: 1,
            coverage_contract_versions: vec![1],
            fact_family_versions: Default::default(),
            query_pack_versions: Some(vec![1]),
            observation_contract_versions: None,
        },
        ContractVersionOffer {
            selection_contract_version: 1,
            model_major: 1,
            external_entity_reference_versions: vec![1],
            semantic_revision_reference_versions: vec![1],
            coverage_contract_versions: vec![1],
            fact_family_versions: Default::default(),
            query_pack_versions: vec![1],
            observation_contract_versions: Vec::new(),
        },
    )
}

fn candidate_probe() -> NativeArtifactProbe {
    NativeArtifactProbe {
        family: ADAPTER_ID.to_string(),
        platform: "darwin".to_string(),
        version: Some("0.98.0".to_string()),
        markers: vec![
            "rollout.record-type".to_string(),
            "session-meta.cli-version".to_string(),
        ],
        contradictory_markers: false,
    }
}

fn assert_candidate_release(release_status: SupportReleaseStatus) {
    assert_eq!(release_status, SupportReleaseStatus::Candidate);
}

#[cfg(test)]
mod tests;
