//! Crate-private Grok catalog source/coverage producer.
//!
//! The current built-in support release remains Candidate and cannot authorize
//! this producer. The membership component retains the reviewed four-sidecar
//! admission policy and owns a disjoint catalog family rather than treating
//! legacy durable facts as semantic-tier parity. Tests execute an exact
//! synthetic composition only.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

use super::adapter::{
    candidate_catalog_path_coordinates, candidate_catalog_summary_coordinates, GrokAdapter,
    GrokCatalogCoordinates,
};
use crate::adapter::{
    AdapterId, AgentAdapter, DecodeDisposition, DriverSpec, Fact, FactSemanticContext,
    SourceInstance, SourceInstanceKey, SourceInstanceSpec, SourceObjectDescriptor, SourceRoot,
    StreamSpec,
};
use crate::catalog_contract::CatalogAccessPolicyDigest;
use crate::decode_runtime::{
    decode_record, DecodeRuntimeLimits, DecodeRuntimeRequest, DecoderDependenciesDenied,
};
use crate::source::catalog_composition::{
    CatalogBoundSourceAccess, CatalogCompletedCoverageObject, CatalogComponentCoverageCompletion,
    CatalogCompositionError, CatalogContribution, CatalogDecoderStateBoundary,
    CatalogDiscoveryBounds, CatalogLibraryCoverageAssembly, CatalogMemberRef,
    CatalogMembershipAuthorityEvidence, CatalogMembershipEntry, CatalogOverlapStrategy,
    CatalogPromotedBinding, CatalogSourceComponent, CatalogSourceComposition,
    CatalogSourcePrimitive, MAX_CATALOG_COVERAGE_POINTS,
};
use crate::source::{
    DirectoryCheckpoint, DirectoryEntryKind, DirectoryScan, DirectorySelection, DirectorySnapshot,
    DirectorySnapshotConfig, RecordOrigin, ReplaceCheckpoint, ReplaceDocument,
    ReplaceDocumentConfig, ReplaceRead, SourceDriverError, SourceMediaType,
};

const ADAPTER_ID: &str = "grok";
const SESSIONS_ROOT_ID: &str = "sessions";
const MEMBERSHIP_STREAM_ID: &str = "session-membership";
const SUMMARY_STREAM_ID: &str = "session-summaries";
const MEMBERSHIP_COMPONENT_ID: &str = "session-directory-membership";
const SUMMARY_COMPONENT_ID: &str = "session-summary-metadata";
const MEMBER_IDENTITY_CONTRACT: &str = "catalog-session-identity-v1";
const PLANNING_EVIDENCE_ID: &str = "phase0-catalog-census-2026-08-15";
const PLANNED_SUPPORT_RELEASE_ID: &str = "grok.catalog-candidate-2026-08-15";
const PLANNED_SOURCE_DECLARATION_ID: &str = "grok.catalog-sources-v1";
const CONFORMANCE_SUPPORT_RELEASE_ID: &str = "grok.catalog-conformance-support-v1";
const CONFORMANCE_SOURCE_DECLARATION_ID: &str = "grok.catalog-conformance-sources-v1";
const CONFORMANCE_SOURCE_DECLARATION: &[u8] =
    b"spaghetti/rfc012b/grok-catalog-conformance-declaration/v1";
const CONFORMANCE_SUPPORT_RELEASE: &[u8] = b"spaghetti/rfc012b/grok-catalog-conformance-support/v1";
const MAX_ENTRIES: usize = 100_000;
const MAX_DEPTH: usize = 8;
const MAX_SUMMARY_BYTES: usize = 1024 * 1024;
const ADMITTED_SIDECARS: [&str; 4] = [
    "chat_history.jsonl",
    "summary.json",
    "events.jsonl",
    "signals.json",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GrokCatalogIdentity {
    pub(crate) adapter_id: String,
    pub(crate) project_count: u64,
    pub(crate) session_count: u64,
    pub(crate) project_identity_digest: String,
    pub(crate) session_identity_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GrokCatalogProduction {
    pub(crate) identity: GrokCatalogIdentity,
    pub(crate) assembly: CatalogLibraryCoverageAssembly,
}

pub(crate) fn grok_catalog_components() -> Vec<CatalogSourceComponent> {
    vec![
        CatalogSourceComponent {
            component_id: MEMBERSHIP_COMPONENT_ID.to_owned(),
            source_stream_id: MEMBERSHIP_STREAM_ID.to_owned(),
            stream_id: MEMBERSHIP_STREAM_ID.to_owned(),
            root_id: SESSIONS_ROOT_ID.to_owned(),
            relative_selectors: ADMITTED_SIDECARS.map(|name| format!("**/{name}")).to_vec(),
            discovery_bounds: CatalogDiscoveryBounds::new(MAX_ENTRIES as u32, MAX_DEPTH as u32)
                .expect("Grok membership discovery bounds are valid"),
            primitive: CatalogSourcePrimitive::DirectoryMembership,
            contribution: CatalogContribution::Membership {
                member_identity_contract_id: MEMBER_IDENTITY_CONTRACT.to_owned(),
                admission_contract_id: "current-sidecar-session-admission-v1".to_owned(),
                provides_metadata: false,
            },
            overlap_strategy: CatalogOverlapStrategy::DisjointCatalogFamily {
                ownership_contract_id: "grok-session-membership-catalog-family-v1".to_owned(),
            },
            safe_decoder_state_boundary: CatalogDecoderStateBoundary::FullSnapshot,
            source_record_contract_version: 1,
            framing_contract_version: 1,
            decoder_contract_id: "grok-session-membership".to_owned(),
            decoder_contract_version: 1,
            disposition_ownership: vec!["native-family:session-membership".to_owned()],
        },
        CatalogSourceComponent {
            component_id: SUMMARY_COMPONENT_ID.to_owned(),
            source_stream_id: SUMMARY_STREAM_ID.to_owned(),
            stream_id: SUMMARY_STREAM_ID.to_owned(),
            root_id: SESSIONS_ROOT_ID.to_owned(),
            relative_selectors: vec!["**/summary.json".to_owned()],
            discovery_bounds: CatalogDiscoveryBounds::new(MAX_ENTRIES as u32, MAX_DEPTH as u32)
                .expect("Grok summary discovery bounds are valid"),
            primitive: CatalogSourcePrimitive::ReplaceDocument {
                max_object_bytes: MAX_SUMMARY_BYTES as u64,
            },
            contribution: CatalogContribution::MetadataForKnownMember {
                member_identity_contract_id: MEMBER_IDENTITY_CONTRACT.to_owned(),
                metadata_contract_id: "replaceable-session-summary-v1".to_owned(),
            },
            overlap_strategy: CatalogOverlapStrategy::IdempotentOverlap,
            safe_decoder_state_boundary: CatalogDecoderStateBoundary::ObjectGenerationRevision,
            source_record_contract_version: 1,
            framing_contract_version: 1,
            decoder_contract_id: "grok-summary".to_owned(),
            decoder_contract_version: 1,
            disposition_ownership: vec!["native-family:session-summary".to_owned()],
        },
    ]
}

pub(crate) fn grok_planned_catalog_composition(
) -> Result<CatalogSourceComposition, CatalogCompositionError> {
    CatalogSourceComposition::new_planned(
        ADAPTER_ID,
        PLANNED_SUPPORT_RELEASE_ID,
        PLANNED_SOURCE_DECLARATION_ID,
        PLANNING_EVIDENCE_ID,
        grok_catalog_components(),
    )
}

pub(crate) fn grok_conformance_promoted_composition(
) -> Result<CatalogSourceComposition, CatalogCompositionError> {
    CatalogSourceComposition::new_promoted(
        ADAPTER_ID,
        CONFORMANCE_SUPPORT_RELEASE_ID,
        CONFORMANCE_SOURCE_DECLARATION_ID,
        CatalogPromotedBinding::from_digests(
            Sha256::digest(CONFORMANCE_SOURCE_DECLARATION).into(),
            Sha256::digest(CONFORMANCE_SUPPORT_RELEASE).into(),
        )?,
        grok_catalog_components(),
    )
}

pub(crate) fn grok_conformance_source_declaration_bytes() -> &'static [u8] {
    CONFORMANCE_SOURCE_DECLARATION
}

pub(crate) fn grok_conformance_support_release_bytes() -> &'static [u8] {
    CONFORMANCE_SUPPORT_RELEASE
}

pub(crate) fn grok_conformance_support_release_id() -> &'static str {
    CONFORMANCE_SUPPORT_RELEASE_ID
}

pub(crate) fn grok_catalog_source_instance(
    catalog_root: &Path,
    source_instance_discriminator: &[u8],
) -> Result<SourceInstance, CatalogCompositionError> {
    Ok(SourceInstance {
        id: 1,
        spec: SourceInstanceSpec {
            identity_contract_version: 1,
            stable_key: SourceInstanceKey::new(source_instance_discriminator.to_vec()).map_err(
                |_| CatalogCompositionError::invalid("Grok catalog source identity is invalid"),
            )?,
            display_name: "Grok catalog producer".to_owned(),
            roots: vec![
                SourceRoot {
                    name: "home".to_owned(),
                    path: catalog_root.to_path_buf(),
                },
                SourceRoot {
                    name: SESSIONS_ROOT_ID.to_owned(),
                    path: catalog_root.join(SESSIONS_ROOT_ID),
                },
            ],
            discovery_reason: "crate-private Grok catalog producer".to_owned(),
        },
    })
}

pub(crate) fn produce_grok_library_coverage(
    access: &CatalogBoundSourceAccess<'_, '_, '_>,
    access_policy_digest: CatalogAccessPolicyDigest,
) -> Result<GrokCatalogProduction, CatalogCompositionError> {
    produce_grok_library_coverage_after_summaries(access, access_policy_digest, |_| Ok(()))
}

#[cfg(test)]
pub(crate) fn produce_grok_library_coverage_with_post_summary_mutation(
    access: &CatalogBoundSourceAccess<'_, '_, '_>,
    access_policy_digest: CatalogAccessPolicyDigest,
    mutate: impl FnOnce(&Path),
) -> Result<GrokCatalogProduction, CatalogCompositionError> {
    produce_grok_library_coverage_after_summaries(access, access_policy_digest, |sessions_root| {
        mutate(sessions_root);
        Ok(())
    })
}

fn produce_grok_library_coverage_after_summaries(
    access: &CatalogBoundSourceAccess<'_, '_, '_>,
    access_policy_digest: CatalogAccessPolicyDigest,
    after_summaries: impl FnOnce(&Path) -> Result<(), CatalogCompositionError>,
) -> Result<GrokCatalogProduction, CatalogCompositionError> {
    let executable = access.executable();
    let composition = executable.composition();
    require_exact_conformance_composition(composition)?;
    let membership_component = expect_component(composition, MEMBERSHIP_COMPONENT_ID)?;
    let summary_component = expect_component(composition, SUMMARY_COMPONENT_ID)?;
    let source_instance_key = access.source_instance_key()?;
    let instance = access.instance();
    let sessions_root = access.root(SESSIONS_ROOT_ID)?;
    let membership_scan = scan_membership(sessions_root, membership_component)?;
    let summary_scan = scan_summaries(sessions_root, summary_component)?;

    let mut members = BTreeMap::<PathBuf, MemberState>::new();
    let mut coordinate_directories = BTreeMap::<(String, String), PathBuf>::new();
    let mut session_projects = BTreeMap::<String, String>::new();
    let mut projects = BTreeSet::new();
    let mut sessions = BTreeSet::new();
    for entry in membership_scan.entries.values() {
        let relative_path = PathBuf::from(&entry.display_path);
        let directory = relative_path.parent().ok_or_else(|| {
            CatalogCompositionError::invalid("Grok membership object has no session directory")
        })?;
        let coordinates = candidate_catalog_path_coordinates(&relative_path).map_err(|_| {
            CatalogCompositionError::invalid("Grok membership path is not a catalog coordinate")
        })?;
        if let Some(existing) = members.get(directory) {
            if existing.coordinates != coordinates {
                return Err(CatalogCompositionError::invalid(
                    "one Grok session directory produced conflicting coordinates",
                ));
            }
            continue;
        }
        let identity = (
            coordinates.native_project_key.clone(),
            coordinates.session_id.clone(),
        );
        if coordinate_directories
            .insert(identity, directory.to_path_buf())
            .is_some()
        {
            return Err(CatalogCompositionError::invalid(
                "distinct Grok directories produced one fallback identity",
            ));
        }
        if let Some(project) = session_projects.insert(
            coordinates.session_id.clone(),
            coordinates.native_project_key.clone(),
        ) {
            if project != coordinates.native_project_key {
                return Err(CatalogCompositionError::invalid(
                    "one Grok session ID appeared under competing projects",
                ));
            }
        }
        projects.insert((
            ADAPTER_ID.to_owned(),
            coordinates.native_project_key.clone(),
        ));
        sessions.insert((
            ADAPTER_ID.to_owned(),
            coordinates.native_project_key.clone(),
            coordinates.session_id.clone(),
        ));
        members.insert(
            directory.to_path_buf(),
            MemberState {
                coordinates,
                summary_metadata: false,
            },
        );
    }

    let adapter = GrokAdapter::new();
    let streams = adapter.streams(instance).map_err(|_| {
        CatalogCompositionError::invalid("Grok catalog producer could not load adapter streams")
    })?;
    let summary_stream = exact_stream(&streams, SUMMARY_STREAM_ID)?;
    let DriverSpec::ReplaceDocument(config) = &summary_stream.driver else {
        return Err(CatalogCompositionError::invalid(
            "Grok catalog summary stream is not replace-document",
        ));
    };
    if config.max_document_bytes != MAX_SUMMARY_BYTES {
        return Err(CatalogCompositionError::invalid(
            "Grok catalog summary bound drifted from the reviewed adapter stream",
        ));
    }
    let summary_driver = ReplaceDocument::new(ReplaceDocumentConfig {
        max_document_bytes: MAX_SUMMARY_BYTES,
    })
    .map_err(map_driver_error)?;
    let mut summary_objects = Vec::with_capacity(summary_scan.entries.len());
    let mut summary_reads = Vec::with_capacity(summary_scan.entries.len());
    let mut coverage_points = membership_scan.entries.len();
    if coverage_points > MAX_CATALOG_COVERAGE_POINTS {
        return Err(CatalogCompositionError::invalid(
            "catalog coverage exceeds the RFC 012A portable set bounds",
        ));
    }

    for (index, entry) in summary_scan.entries.values().enumerate() {
        let relative_path = PathBuf::from(&entry.display_path);
        let directory = relative_path.parent().ok_or_else(|| {
            CatalogCompositionError::invalid("Grok summary has no session directory")
        })?;
        let path_coordinates = members
            .get(directory)
            .ok_or_else(|| {
                CatalogCompositionError::invalid(
                    "Grok summary metadata cannot fabricate catalog membership",
                )
            })?
            .coordinates
            .clone();
        let read = summary_driver
            .read_confined(
                sessions_root,
                &relative_path,
                None,
                &object_origin(index)?,
                false,
            )
            .map_err(map_driver_error)?;
        let ReplaceRead::Record {
            record, checkpoint, ..
        } = read
        else {
            return Err(CatalogCompositionError::invalid(
                "Grok catalog summary was not completely readable",
            ));
        };
        let summary: serde_json::Value = serde_json::from_slice(&record.payload).map_err(|_| {
            CatalogCompositionError::invalid("Grok catalog summary JSON is invalid")
        })?;
        let summary_coordinates = candidate_catalog_summary_coordinates(&relative_path, &summary)
            .map_err(|_| {
            CatalogCompositionError::invalid("Grok catalog summary coordinates are invalid")
        })?;
        if summary_coordinates != path_coordinates {
            return Err(CatalogCompositionError::invalid(
                "Grok summary identity disagrees with catalog membership",
            ));
        }
        let descriptor = SourceObjectDescriptor {
            stream_id: summary_stream.id.clone(),
            object_key: entry.path_key.clone(),
            relative_path: relative_path.clone(),
        };
        let object_context = adapter
            .bootstrap_object(instance, &descriptor)
            .map_err(|_| {
                CatalogCompositionError::invalid(
                    "Grok catalog producer could not bootstrap a summary object",
                )
            })?;
        let semantic_context = FactSemanticContext::new(
            &AdapterId::new(ADAPTER_ID).map_err(|_| {
                CatalogCompositionError::invalid("Grok catalog adapter identity is invalid")
            })?,
            instance.spec.identity_contract_version,
            instance.spec.stable_key.as_bytes(),
            summary_stream.id.as_str().as_bytes(),
            &entry.path_key,
            1,
        )
        .map_err(|_| CatalogCompositionError::invalid("Grok semantic context is invalid"))?;
        let decoded = decode_record(DecodeRuntimeRequest {
            adapter: &adapter,
            decoder: &summary_stream.decoder,
            object_context: &object_context,
            source_access: &DecoderDependenciesDenied,
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
        .map_err(|_| {
            CatalogCompositionError::invalid(
                "Grok catalog summary failed at the common decode boundary",
            )
        })?;
        if decoded.disposition != DecodeDisposition::Applied || decoded.batch.facts().len() != 1 {
            return Err(CatalogCompositionError::invalid(
                "Grok catalog summary did not produce one complete session fact",
            ));
        }
        let Fact::Session(session) = &decoded.batch.facts()[0].value else {
            return Err(CatalogCompositionError::invalid(
                "Grok catalog summary emitted an unexpected fact family",
            ));
        };
        if session.native_session_id != path_coordinates.session_id
            || session.native_project_key != path_coordinates.native_project_key
            || session.cwd.as_deref() != Some(path_coordinates.cwd.as_str())
        {
            return Err(CatalogCompositionError::invalid(
                "Grok catalog summary attempted to retarget membership",
            ));
        }
        members
            .get_mut(directory)
            .expect("summary member checked above")
            .summary_metadata = true;
        retain_coverage_point(
            &mut summary_objects,
            &mut coverage_points,
            coverage_point(
                SUMMARY_STREAM_ID,
                &entry.path_key,
                checkpoint.generation,
                crate::adapter::CoveragePositionKind::DocumentRevision,
                checkpoint.revision.as_bytes(),
            )?,
        )?;
        summary_reads.push(SummaryRead {
            relative_path,
            checkpoint,
        });
    }

    after_summaries(sessions_root)?;
    revalidate_summary_revisions(&summary_driver, sessions_root, &summary_reads)?;
    if scan_summaries(sessions_root, summary_component)? != summary_scan {
        return Err(CatalogCompositionError::invalid(
            "Grok summary membership changed during complete production",
        ));
    }
    if scan_membership(sessions_root, membership_component)? != membership_scan {
        return Err(CatalogCompositionError::invalid(
            "Grok membership authority changed during complete production",
        ));
    }

    let membership_entries = members
        .values()
        .map(|state| {
            CatalogMembershipEntry::new(
                CatalogMemberRef::from_canonical_session(
                    composition.member_identity_contract_id(),
                    composition.adapter_id(),
                    source_instance_key,
                    state.coordinates.session_id.as_bytes(),
                )?,
                vec![MEMBERSHIP_COMPONENT_ID.to_owned()],
                if state.summary_metadata {
                    vec![SUMMARY_COMPONENT_ID.to_owned()]
                } else {
                    Vec::new()
                },
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let membership_completion = complete_directory(
        executable,
        source_instance_key,
        access_policy_digest,
        membership_component,
        &membership_scan,
    )?;
    let summary_completion = CatalogComponentCoverageCompletion::new(
        executable,
        source_instance_key,
        access_policy_digest,
        SUMMARY_COMPONENT_ID,
        snapshot_completion_position(SUMMARY_COMPONENT_ID, summary_scan.revision.as_bytes())?,
        summary_objects,
    )?;
    let authorities = vec![CatalogMembershipAuthorityEvidence::unbound(
        MEMBERSHIP_COMPONENT_ID,
        membership_scan.generation,
        *membership_scan.revision.as_bytes(),
    )?];
    let assembly = executable.assemble_produced_library_coverage(
        source_instance_key,
        access_policy_digest,
        authorities,
        membership_entries,
        vec![membership_completion, summary_completion],
    )?;

    Ok(GrokCatalogProduction {
        identity: GrokCatalogIdentity {
            adapter_id: ADAPTER_ID.to_owned(),
            project_count: projects.len() as u64,
            session_count: sessions.len() as u64,
            project_identity_digest: identity_digest(&projects),
            session_identity_digest: identity_digest(&sessions),
        },
        assembly,
    })
}

#[derive(Clone)]
struct MemberState {
    coordinates: GrokCatalogCoordinates,
    summary_metadata: bool,
}

struct SummaryRead {
    relative_path: PathBuf,
    checkpoint: ReplaceCheckpoint,
}

fn require_exact_conformance_composition(
    composition: &CatalogSourceComposition,
) -> Result<(), CatalogCompositionError> {
    if composition != &grok_conformance_promoted_composition()? {
        return Err(CatalogCompositionError::invalid(
            "Grok catalog producer requires the exact synthetic conformance composition",
        ));
    }
    Ok(())
}

fn expect_component<'a>(
    composition: &'a CatalogSourceComposition,
    component_id: &str,
) -> Result<&'a CatalogSourceComponent, CatalogCompositionError> {
    composition
        .components()
        .iter()
        .find(|component| component.component_id == component_id)
        .ok_or_else(|| CatalogCompositionError::invalid("Grok catalog component is missing"))
}

fn exact_stream(
    streams: &[StreamSpec],
    stream_id: &str,
) -> Result<StreamSpec, CatalogCompositionError> {
    streams
        .iter()
        .find(|stream| stream.id.as_str() == stream_id)
        .cloned()
        .ok_or_else(|| CatalogCompositionError::invalid("Grok catalog stream is missing"))
}

fn scan_membership(
    sessions_root: &Path,
    component: &CatalogSourceComponent,
) -> Result<DirectoryCheckpoint, CatalogCompositionError> {
    scan_directory(sessions_root, component, |relative, kind| match kind {
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
    })
}

fn scan_summaries(
    sessions_root: &Path,
    component: &CatalogSourceComponent,
) -> Result<DirectoryCheckpoint, CatalogCompositionError> {
    scan_directory(sessions_root, component, |relative, kind| match kind {
        DirectoryEntryKind::Directory => DirectorySelection::Recurse,
        DirectoryEntryKind::File
            if relative.file_name().and_then(|name| name.to_str()) == Some("summary.json") =>
        {
            DirectorySelection::Include
        }
        DirectoryEntryKind::File => DirectorySelection::Ignore,
    })
}

fn scan_directory(
    root: &Path,
    component: &CatalogSourceComponent,
    selector: impl Fn(&Path, DirectoryEntryKind) -> DirectorySelection,
) -> Result<DirectoryCheckpoint, CatalogCompositionError> {
    let max_entries = usize::try_from(component.discovery_bounds.max_entries).map_err(|_| {
        CatalogCompositionError::invalid("Grok discovery entry bound does not fit this platform")
    })?;
    let scan = DirectorySnapshot::new(DirectorySnapshotConfig {
        max_entries,
        max_entries_per_directory: max_entries,
        max_depth: usize::try_from(component.discovery_bounds.max_depth).map_err(|_| {
            CatalogCompositionError::invalid(
                "Grok discovery depth bound does not fit this platform",
            )
        })?,
    })
    .map_err(map_driver_error)?
    .scan(root, None, &selector)
    .map_err(map_driver_error)?;
    let DirectoryScan::Snapshot { checkpoint, .. } = scan else {
        return Err(CatalogCompositionError::invalid(
            "Grok catalog directory membership was not stably available",
        ));
    };
    if checkpoint.generation == 0
        || checkpoint
            .entries
            .values()
            .any(|entry| entry.generation == 0)
    {
        return Err(CatalogCompositionError::invalid(
            "Grok catalog membership generation must be positive",
        ));
    }
    Ok(checkpoint)
}

fn revalidate_summary_revisions(
    driver: &ReplaceDocument,
    sessions_root: &Path,
    reads: &[SummaryRead],
) -> Result<(), CatalogCompositionError> {
    for (index, evidence) in reads.iter().enumerate() {
        let read = driver
            .read_confined(
                sessions_root,
                &evidence.relative_path,
                None,
                &object_origin(index)?,
                false,
            )
            .map_err(map_driver_error)?;
        let checkpoint = match read {
            ReplaceRead::Record { checkpoint, .. } | ReplaceRead::Unchanged { checkpoint } => {
                checkpoint
            }
            _ => {
                return Err(CatalogCompositionError::invalid(
                    "Grok summary driver revision changed during production",
                ))
            }
        };
        if checkpoint.generation != evidence.checkpoint.generation
            || checkpoint.revision != evidence.checkpoint.revision
        {
            return Err(CatalogCompositionError::invalid(
                "Grok summary driver revision changed during production",
            ));
        }
    }
    Ok(())
}

fn complete_directory(
    executable: &crate::source::catalog_composition::CatalogExecutableComposition<'_, '_>,
    source_instance_key: crate::adapter::CanonicalSourceInstanceKey,
    access_policy_digest: CatalogAccessPolicyDigest,
    component: &CatalogSourceComponent,
    checkpoint: &DirectoryCheckpoint,
) -> Result<CatalogComponentCoverageCompletion, CatalogCompositionError> {
    let mut objects = Vec::with_capacity(checkpoint.entries.len());
    for entry in checkpoint.entries.values() {
        objects.push(coverage_point(
            &component.stream_id,
            &entry.path_key,
            entry.generation,
            crate::adapter::CoveragePositionKind::SnapshotRevision,
            entry.revision.as_bytes(),
        )?);
    }
    CatalogComponentCoverageCompletion::new(
        executable,
        source_instance_key,
        access_policy_digest,
        &component.component_id,
        snapshot_completion_position(&component.component_id, checkpoint.revision.as_bytes())?,
        objects,
    )
}

fn retain_coverage_point(
    objects: &mut Vec<CatalogCompletedCoverageObject>,
    aggregate: &mut usize,
    object: CatalogCompletedCoverageObject,
) -> Result<(), CatalogCompositionError> {
    if objects.len() >= MAX_CATALOG_COVERAGE_POINTS || *aggregate >= MAX_CATALOG_COVERAGE_POINTS {
        return Err(CatalogCompositionError::invalid(
            "catalog coverage exceeds the RFC 012A portable set bounds",
        ));
    }
    objects.push(object);
    *aggregate = aggregate
        .checked_add(1)
        .ok_or_else(|| CatalogCompositionError::invalid("coverage point count overflow"))?;
    Ok(())
}

fn coverage_point(
    stream_id: &str,
    object_key: &[u8],
    generation: u64,
    kind: crate::adapter::CoveragePositionKind,
    opaque: &[u8],
) -> Result<CatalogCompletedCoverageObject, CatalogCompositionError> {
    let position = crate::adapter::CoveragePosition::derive(kind, opaque, None)
        .map_err(|_| CatalogCompositionError::invalid("Grok coverage position is invalid"))?;
    CatalogCompletedCoverageObject::point(
        crate::adapter::CoverageObjectKey::derive(stream_id, object_key)
            .map_err(|_| CatalogCompositionError::invalid("Grok coverage key is invalid"))?,
        generation,
        position,
        crate::adapter::CoverageProvenance::default(),
    )
}

fn snapshot_completion_position(
    component_id: &str,
    revision: &[u8],
) -> Result<crate::adapter::CoveragePosition, CatalogCompositionError> {
    let mut material = Vec::new();
    material.extend_from_slice(&(component_id.len() as u64).to_be_bytes());
    material.extend_from_slice(component_id.as_bytes());
    material.extend_from_slice(&(revision.len() as u64).to_be_bytes());
    material.extend_from_slice(revision);
    crate::adapter::CoveragePosition::derive(
        crate::adapter::CoveragePositionKind::SnapshotRevision,
        &material,
        None,
    )
    .map_err(|_| CatalogCompositionError::invalid("Grok completion position is invalid"))
}

fn object_origin(index: usize) -> Result<RecordOrigin, CatalogCompositionError> {
    let object_id = u64::try_from(index)
        .ok()
        .and_then(|value| value.checked_add(3))
        .ok_or_else(|| CatalogCompositionError::invalid("Grok object index overflow"))?;
    Ok(RecordOrigin {
        source_instance_id: 1,
        stream_id: 2,
        object_id,
        observed_at: 1,
        source_timestamp_hint: None,
        media_type: SourceMediaType::new("application/json").map_err(map_driver_error)?,
    })
}

fn map_driver_error(error: SourceDriverError) -> CatalogCompositionError {
    CatalogCompositionError::invalid(match error {
        SourceDriverError::InvalidConfig(_) => {
            "Grok catalog producer received invalid driver configuration"
        }
        SourceDriverError::InvalidCursor(_) => {
            "Grok catalog producer received an invalid driver cursor"
        }
        SourceDriverError::PathEscape(_) => {
            "Grok catalog producer rejected a path outside its declared root"
        }
        SourceDriverError::LimitExceeded(_) => {
            "Grok catalog producer exceeded a declared source bound"
        }
        SourceDriverError::Unstable(_) => {
            "Grok catalog producer observed an unstable source snapshot"
        }
        SourceDriverError::Database(_) => "Grok catalog producer cannot use a database source",
        SourceDriverError::Io { .. } => {
            "Grok catalog producer failed to read a declared source object"
        }
    })
}

fn identity_digest<T: serde::Serialize + Ord>(values: &BTreeSet<T>) -> String {
    let mut digest = Sha256::new();
    for value in values {
        let encoded = serde_json::to_vec(value).expect("catalog identity is serializable");
        digest.update((encoded.len() as u64).to_be_bytes());
        digest.update(encoded);
    }
    let mut output = String::with_capacity(64);
    for byte in digest.finalize() {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}
