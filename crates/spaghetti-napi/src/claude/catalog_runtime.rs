//! Crate-private Claude catalog source-access/coverage producer.
//!
//! Candidate declarations describe 4/8 MiB `full_only` AppendDelimited streams.
//! This producer executes a distinct 64 KiB/idempotent catalog topology only
//! after a borrowed typed catalog authorization has bound the promoted release,
//! source declaration, selected contracts, and exact source streams. It cannot
//! mint catalog authorization or source access from a raw path. Native paths
//! stay out of returned types, Debug, and error messages.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use sha2::{Digest as _, Sha256};

use super::adapter::ClaudeCodeAdapter;
use crate::adapter::{
    AdapterId, AdapterObjectContext, AgentAdapter, AuthorizedCatalogAccess,
    CanonicalSourceInstanceKey, DecodeDisposition, DriverSpec, Fact, FactSemanticContext,
    SessionFact, SourceCoverageSet, SourceInstance, SourceObjectDescriptor, StreamSpec,
    TypedAccessAuthorization,
};
#[cfg(test)]
use crate::adapter::{SourceInstanceKey, SourceInstanceSpec, SourceRoot};
use crate::catalog_contract::evidence::{
    CatalogAvailability, CatalogEvidenceOwner, ProjectAssociationBasis,
};
use crate::catalog_contract::CatalogAccessPolicyDigest;
use crate::decode_runtime::{
    decode_record, DecodeRuntimeLimits, DecodeRuntimeRequest, DecodedFactBatch,
    DecoderDependenciesDenied,
};
#[cfg(test)]
use crate::source::catalog_composition::CatalogPromotedBinding;
use crate::source::catalog_composition::{
    CatalogBoundSourceAccess, CatalogCompletedCoverageObject, CatalogComponentCoverageCompletion,
    CatalogCompositionError, CatalogContribution, CatalogDecoderStateBoundary,
    CatalogDiscoveryBounds, CatalogExecutableComposition, CatalogLibraryCoverageAssembly,
    CatalogMemberRef, CatalogMembershipAuthorityEvidence, CatalogMembershipEntry,
    CatalogOverlapStrategy, CatalogSourceComponent, CatalogSourceComposition,
    CatalogSourcePrimitive, MAX_CATALOG_COVERAGE_POINTS, MAX_MEMBERSHIP_MEMBERS,
};
use crate::source::catalog_projection::{CatalogSourceMemberProjection, CatalogSourceProjection};
use crate::source::catalog_runtime_registry::CatalogSourceRuntime;
use crate::source::{
    AppendCheckpoint, AppendDelimitedConfig, AppendDelimitedFile, AppendItem, AppendRead,
    AppendTransition, DirectoryCheckpoint, DirectoryEntryKind, DirectoryEntryState, DirectoryScan,
    DirectorySelection, DirectorySnapshot, DirectorySnapshotConfig, RecordOrigin,
    ReplaceCheckpoint, ReplaceDocument, ReplaceDocumentConfig, ReplaceRead, SourceDriverError,
    SourceMediaType, SourceRecord,
};

const ADAPTER_ID: &str = "claude-code";
const MEMBER_IDENTITY_CONTRACT: &str = "catalog-session-identity-v1";
const PROJECTS_ROOT_ID: &str = "projects";
const INDEX_COMPONENT_ID: &str = "session-index-membership";
const TOP_LEVEL_COMPONENT_ID: &str = "top-level-transcript-membership";
const NESTED_COMPONENT_ID: &str = "nested-parent-membership";
const HEAD_COMPONENT_ID: &str = "transcript-head-fallback";
const INDEX_STREAM_ID: &str = "session-indexes";
const PARENT_STREAM_ID: &str = "session-transcripts";
const SOURCE_DECLARATION_ID: &str = "claude-code-sources-2026-08-21-candidate";
#[cfg(test)]
const PLANNING_EVIDENCE_ID: &str = "phase0-catalog-census-2026-08-15";
#[cfg(test)]
const PLANNED_SUPPORT_RELEASE_ID: &str = "claude-code.catalog-candidate-2026-08-15";
#[cfg(test)]
const PLANNED_SOURCE_DECLARATION_ID: &str = "claude-code.catalog-sources-v1";
#[cfg(test)]
const CONFORMANCE_SUPPORT_RELEASE_ID: &str = "claude-code.catalog-conformance-support-v1";
#[cfg(test)]
const CONFORMANCE_SOURCE_DECLARATION: &[u8] =
    b"spaghetti/rfc012b/claude-catalog-conformance-declaration/v1";
#[cfg(test)]
const CONFORMANCE_SUPPORT_RELEASE: &[u8] =
    b"spaghetti/rfc012b/claude-catalog-conformance-support/v1";
const CANDIDATE_HEAD_BYTES: u64 = 64 * 1024;
const HEAD_CHECKPOINT_ANCHOR_BYTES: usize = 4_096;

const INDEX_EVIDENCE: u8 = 1 << 0;
const TOP_LEVEL_EVIDENCE: u8 = 1 << 1;
const NESTED_EVIDENCE: u8 = 1 << 2;

type MemberKey = (String, String);

/// Privacy-reduced catalog identity produced from authorized Claude composition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeCatalogIdentity {
    pub(crate) adapter_id: String,
    pub(crate) project_count: u64,
    pub(crate) session_count: u64,
    pub(crate) project_identity_digest: String,
    pub(crate) session_identity_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeCatalogProduction {
    pub(crate) identity: ClaudeCatalogIdentity,
    pub(crate) assembly: CatalogLibraryCoverageAssembly,
    pub(crate) projection: CatalogSourceProjection,
}

pub(crate) struct ClaudeCatalogSourceRuntime;

impl CatalogSourceRuntime for ClaudeCatalogSourceRuntime {
    fn adapter_id(&self) -> &'static str {
        ADAPTER_ID
    }

    fn library_plan_source(
        &self,
        authorization: &TypedAccessAuthorization,
        instance: &SourceInstance,
        access_policy_digest: CatalogAccessPolicyDigest,
    ) -> Result<crate::catalog_contract::CatalogCoveragePlanSource, CatalogCompositionError> {
        let access = authorization
            .select_catalog_access()
            .map_err(|_| CatalogCompositionError::invalid("Claude catalog authority is invalid"))?;
        let composition = claude_authorized_catalog_composition(&access)?;
        let executable = composition.authorize_execution(access)?;
        executable
            .bind_source_instance(instance)?
            .library_plan_source(access_policy_digest)
    }

    fn produce_library_projection(
        &self,
        authorization: &TypedAccessAuthorization,
        instance: &SourceInstance,
        access_policy_digest: CatalogAccessPolicyDigest,
        prior_coverage: Option<&SourceCoverageSet>,
    ) -> Result<CatalogSourceProjection, CatalogCompositionError> {
        let access = authorization
            .select_catalog_access()
            .map_err(|_| CatalogCompositionError::invalid("Claude catalog authority is invalid"))?;
        let composition = claude_authorized_catalog_composition(&access)?;
        let executable = composition.authorize_execution(access)?;
        let bound = executable.bind_source_instance(instance)?;
        let production = match prior_coverage {
            Some(prior) => produce_claude_library_refresh(&bound, access_policy_digest, prior)?,
            None => produce_claude_library_coverage(&bound, access_policy_digest)?,
        };
        Ok(production.projection)
    }
}

pub(crate) fn claude_catalog_components() -> Vec<CatalogSourceComponent> {
    vec![
        catalog_component(
            (
                HEAD_COMPONENT_ID,
                "session-transcripts",
                "session-transcripts",
                PROJECTS_ROOT_ID,
            ),
            &["*/*.jsonl"],
            CatalogSourcePrimitive::DelimitedPrefix {
                max_record_bytes: CANDIDATE_HEAD_BYTES,
                max_window_bytes: CANDIDATE_HEAD_BYTES,
                max_records: 128,
            },
            CatalogContribution::MetadataForKnownMember {
                member_identity_contract_id: MEMBER_IDENTITY_CONTRACT.to_owned(),
                metadata_contract_id: "transcript-head-metadata-v1".to_owned(),
            },
            CatalogOverlapStrategy::IdempotentOverlap,
            CatalogDecoderStateBoundary::ObjectGenerationCursor,
            (
                "claude-session-record",
                &["native-family:session-transcript"],
            ),
        ),
        catalog_component(
            (
                NESTED_COMPONENT_ID,
                "nested-transcript-membership",
                "subagent-transcripts",
                PROJECTS_ROOT_ID,
            ),
            &["*/*/subagents/**/agent-*.jsonl"],
            CatalogSourcePrimitive::DirectoryMembership,
            CatalogContribution::Membership {
                member_identity_contract_id: MEMBER_IDENTITY_CONTRACT.to_owned(),
                admission_contract_id: "nested-parent-session-admission-v1".to_owned(),
                provides_metadata: false,
            },
            CatalogOverlapStrategy::DisjointCatalogFamily {
                ownership_contract_id: "nested-parent-membership-v1".to_owned(),
            },
            CatalogDecoderStateBoundary::FullSnapshot,
            (
                "claude-nested-parent-membership-v1",
                &["native-family:nested-parent-membership"],
            ),
        ),
        catalog_component(
            (
                INDEX_COMPONENT_ID,
                INDEX_STREAM_ID,
                INDEX_STREAM_ID,
                PROJECTS_ROOT_ID,
            ),
            &["*/sessions-index.json"],
            CatalogSourcePrimitive::ReplaceDocument {
                max_object_bytes: 1024 * 1024,
            },
            CatalogContribution::Membership {
                member_identity_contract_id: MEMBER_IDENTITY_CONTRACT.to_owned(),
                admission_contract_id: "session-index-entry-admission-v1".to_owned(),
                provides_metadata: true,
            },
            CatalogOverlapStrategy::CommitCatalogFacts,
            CatalogDecoderStateBoundary::ObjectGenerationRevision,
            ("claude-session-index", &["native-family:session-index"]),
        ),
        catalog_component(
            (
                TOP_LEVEL_COMPONENT_ID,
                "top-level-transcript-membership",
                "session-transcripts",
                PROJECTS_ROOT_ID,
            ),
            &["*/*.jsonl"],
            CatalogSourcePrimitive::DirectoryMembership,
            CatalogContribution::Membership {
                member_identity_contract_id: MEMBER_IDENTITY_CONTRACT.to_owned(),
                admission_contract_id: "top-level-transcript-admission-v1".to_owned(),
                provides_metadata: false,
            },
            CatalogOverlapStrategy::DisjointCatalogFamily {
                ownership_contract_id: "top-level-transcript-membership-v1".to_owned(),
            },
            CatalogDecoderStateBoundary::FullSnapshot,
            (
                "claude-top-level-transcript-membership-v1",
                &["native-family:top-level-transcript-membership"],
            ),
        ),
    ]
}

/// Build the exact compiled Claude catalog topology from a non-transferable
/// typed authorization. Candidate releases cannot produce this proof, and the
/// returned value must still consume it through `authorize_execution`.
pub(crate) fn claude_authorized_catalog_composition(
    authorization: &AuthorizedCatalogAccess<'_>,
) -> Result<CatalogSourceComposition, CatalogCompositionError> {
    CatalogSourceComposition::from_authorized_catalog_access(
        authorization,
        SOURCE_DECLARATION_ID,
        claude_catalog_components(),
    )
}

#[cfg(test)]
pub(crate) fn claude_planned_catalog_composition(
) -> Result<CatalogSourceComposition, CatalogCompositionError> {
    CatalogSourceComposition::new_planned(
        ADAPTER_ID,
        PLANNED_SUPPORT_RELEASE_ID,
        PLANNED_SOURCE_DECLARATION_ID,
        PLANNING_EVIDENCE_ID,
        claude_catalog_components(),
    )
}

#[cfg(test)]
pub(crate) fn claude_conformance_promoted_composition(
) -> Result<CatalogSourceComposition, CatalogCompositionError> {
    CatalogSourceComposition::new_promoted(
        ADAPTER_ID,
        CONFORMANCE_SUPPORT_RELEASE_ID,
        SOURCE_DECLARATION_ID,
        CatalogPromotedBinding::from_digests(
            Sha256::digest(CONFORMANCE_SOURCE_DECLARATION).into(),
            Sha256::digest(CONFORMANCE_SUPPORT_RELEASE).into(),
        )?,
        claude_catalog_components(),
    )
}

#[cfg(test)]
pub(crate) fn claude_conformance_source_declaration_bytes() -> &'static [u8] {
    CONFORMANCE_SOURCE_DECLARATION
}

#[cfg(test)]
pub(crate) fn claude_conformance_support_release_bytes() -> &'static [u8] {
    CONFORMANCE_SUPPORT_RELEASE
}

#[cfg(test)]
pub(crate) fn claude_conformance_support_release_id() -> &'static str {
    CONFORMANCE_SUPPORT_RELEASE_ID
}

#[cfg(test)]
pub(crate) fn claude_conformance_source_declaration_id() -> &'static str {
    SOURCE_DECLARATION_ID
}

pub(crate) fn produce_claude_library_coverage(
    access: &CatalogBoundSourceAccess<'_, '_, '_>,
    access_policy_digest: CatalogAccessPolicyDigest,
) -> Result<ClaudeCatalogProduction, CatalogCompositionError> {
    produce_claude_library_coverage_after_heads(access, access_policy_digest, None, |_| Ok(()))
}

pub(crate) fn produce_claude_library_refresh(
    access: &CatalogBoundSourceAccess<'_, '_, '_>,
    access_policy_digest: CatalogAccessPolicyDigest,
    prior_coverage: &SourceCoverageSet,
) -> Result<ClaudeCatalogProduction, CatalogCompositionError> {
    produce_claude_library_coverage_after_heads(
        access,
        access_policy_digest,
        Some(prior_coverage),
        |_| Ok(()),
    )
}

#[cfg(test)]
pub(crate) fn produce_claude_library_coverage_with_post_head_mutation(
    access: &CatalogBoundSourceAccess<'_, '_, '_>,
    access_policy_digest: CatalogAccessPolicyDigest,
    mutate: impl FnOnce(&Path),
) -> Result<ClaudeCatalogProduction, CatalogCompositionError> {
    produce_claude_library_coverage_after_heads(access, access_policy_digest, None, |root| {
        mutate(root);
        Ok(())
    })
}

#[cfg(test)]
pub(crate) fn claude_catalog_source_instance(
    catalog_root: &Path,
    source_instance_discriminator: &[u8],
) -> Result<SourceInstance, CatalogCompositionError> {
    producer_instance(
        &catalog_root.join(PROJECTS_ROOT_ID),
        source_instance_discriminator,
    )
}

fn decode_catalog_record(
    adapter: &ClaudeCodeAdapter,
    stream: &StreamSpec,
    object_context: &AdapterObjectContext,
    semantic_context: &FactSemanticContext,
    record: &SourceRecord,
    decoder_state: Option<&[u8]>,
    limits: DecodeRuntimeLimits,
) -> Result<DecodedFactBatch, CatalogCompositionError> {
    decode_record(DecodeRuntimeRequest {
        adapter,
        decoder: &stream.decoder,
        object_context,
        source_access: &DecoderDependenciesDenied,
        record,
        semantic_context,
        decoder_state,
        retention: stream.retention,
        limits,
    })
    .result
    .map_err(|_| {
        CatalogCompositionError::invalid(
            "Claude catalog record failed at the common decode boundary",
        )
    })
}

fn produce_claude_library_coverage_after_heads(
    access: &CatalogBoundSourceAccess<'_, '_, '_>,
    access_policy_digest: CatalogAccessPolicyDigest,
    prior_coverage: Option<&SourceCoverageSet>,
    after_heads: impl FnOnce(&Path) -> Result<(), CatalogCompositionError>,
) -> Result<ClaudeCatalogProduction, CatalogCompositionError> {
    let executable = access.executable();
    executable.validate_complete_coverage_authority()?;
    let composition = executable.composition();
    require_exact_runtime_composition(composition)?;
    let source_instance_key = access.source_instance_key()?;
    let instance = access.instance();
    let index_component = expect_component(composition, INDEX_COMPONENT_ID)?;
    let top_level_component = expect_component(composition, TOP_LEVEL_COMPONENT_ID)?;
    let nested_component = expect_component(composition, NESTED_COMPONENT_ID)?;
    let head_component = expect_component(composition, HEAD_COMPONENT_ID)?;
    let CatalogSourcePrimitive::DelimitedPrefix {
        max_record_bytes,
        max_window_bytes,
        max_records,
    } = &head_component.primitive
    else {
        return Err(CatalogCompositionError::invalid(
            "reviewed Claude transcript-head component is not a delimited prefix",
        ));
    };
    let max_record_bytes = *max_record_bytes;
    let max_window_bytes = *max_window_bytes;
    let max_records = *max_records;

    let projects_root = access.root(PROJECTS_ROOT_ID)?;
    let top_level_scan = scan_membership(projects_root, top_level_component, top_level_selection)?;
    let nested_scan = scan_membership(projects_root, nested_component, nested_selection)?;
    let index_scan = scan_membership(projects_root, index_component, index_selection)?;

    let mut members: BTreeMap<MemberKey, MemberState> = BTreeMap::new();
    let mut projects = BTreeSet::new();
    let mut sessions = BTreeSet::new();
    let mut session_projects = BTreeMap::new();

    for entry in top_level_scan.entries.values() {
        let coordinates = parent_coordinates(&entry.display_path)?;
        let member = admit_member(
            &mut MemberAdmission {
                members: &mut members,
                projects: &mut projects,
                sessions: &mut sessions,
                session_projects: &mut session_projects,
            },
            coordinates.project_slug,
            coordinates.session_id,
            TOP_LEVEL_COMPONENT_ID,
        )?;
        retain_projection_owner(
            &mut members,
            &member,
            ProjectionOwner::new(
                0,
                projection_owner(
                    source_instance_key,
                    top_level_component,
                    &entry.path_key,
                    entry.generation,
                )?,
                ProjectAssociationBasis::SessionDirectory,
            ),
        );
    }
    for entry in nested_scan.entries.values() {
        let coordinates = nested_coordinates(&entry.display_path)?;
        let member = admit_member(
            &mut MemberAdmission {
                members: &mut members,
                projects: &mut projects,
                sessions: &mut sessions,
                session_projects: &mut session_projects,
            },
            coordinates.project_slug,
            coordinates.session_id,
            NESTED_COMPONENT_ID,
        )?;
        retain_projection_owner(
            &mut members,
            &member,
            ProjectionOwner::new(
                1,
                projection_owner(
                    source_instance_key,
                    nested_component,
                    &entry.path_key,
                    entry.generation,
                )?,
                ProjectAssociationBasis::SessionDirectory,
            ),
        );
    }

    let adapter = ClaudeCodeAdapter::new();
    let mut coverage_points = 0_usize;
    let streams = adapter.streams(instance).map_err(|_| {
        CatalogCompositionError::invalid("Claude catalog producer could not load adapter streams")
    })?;
    let index_stream = exact_stream(&streams, INDEX_STREAM_ID)?;
    let parent_stream = exact_stream(&streams, PARENT_STREAM_ID)?;
    let CatalogSourcePrimitive::ReplaceDocument { max_object_bytes } = &index_component.primitive
    else {
        unreachable!("index primitive verified above");
    };
    let max_object_bytes = *max_object_bytes;
    let max_index_bytes = usize::try_from(max_object_bytes).map_err(|_| {
        CatalogCompositionError::invalid("Claude catalog index bound does not fit this platform")
    })?;
    if let DriverSpec::ReplaceDocument(config) = &index_stream.driver {
        if config.max_document_bytes != max_index_bytes {
            return Err(CatalogCompositionError::invalid(
                "Claude catalog index bound drifted from the reviewed adapter stream",
            ));
        }
    } else {
        return Err(CatalogCompositionError::invalid(
            "Claude catalog index stream is not ReplaceDocument",
        ));
    }
    let index_driver = ReplaceDocument::new(ReplaceDocumentConfig {
        max_document_bytes: max_index_bytes,
    })
    .map_err(map_driver_error)?;

    let mut index_objects = Vec::new();
    let mut index_reads = Vec::new();
    for (object_index, entry) in index_scan.entries.values().enumerate() {
        let relative_path = PathBuf::from(&entry.display_path);
        let project_slug = index_project(&entry.display_path)?;
        projects.insert((ADAPTER_ID.to_string(), project_slug.clone()));
        let origin = object_origin(object_index, "application/json")?;
        let read = index_driver
            .read_confined(projects_root, &relative_path, None, &origin, false)
            .map_err(map_driver_error)?;
        let ReplaceRead::Record {
            record, checkpoint, ..
        } = read
        else {
            return Err(CatalogCompositionError::invalid(
                "Claude catalog index object was not stably available",
            ));
        };
        let descriptor = SourceObjectDescriptor {
            stream_id: index_stream.id.clone(),
            object_key: entry.path_key.clone(),
            relative_path: relative_path.clone(),
        };
        let object_context = adapter
            .bootstrap_object(instance, &descriptor)
            .map_err(|_| {
                CatalogCompositionError::invalid(
                    "Claude catalog producer could not bootstrap an index object",
                )
            })?;
        let semantic_context = decode_context(&index_stream, entry, instance)?;
        let decoded = decode_catalog_record(
            &adapter,
            &index_stream,
            &object_context,
            &semantic_context,
            &record,
            None,
            DecodeRuntimeLimits {
                max_facts: 8,
                max_diagnostics: 8,
            },
        )?;
        if decoded.disposition != DecodeDisposition::Applied {
            return Err(CatalogCompositionError::invalid(
                "Claude catalog index did not decode completely",
            ));
        }
        let mut snapshots =
            decoded
                .batch
                .facts()
                .iter()
                .filter_map(|envelope| match &envelope.value {
                    Fact::SessionIndexSnapshot(snapshot) => Some(snapshot),
                    _ => None,
                });
        let snapshot = snapshots.next().ok_or_else(|| {
            CatalogCompositionError::invalid("Claude catalog index emitted no snapshot")
        })?;
        if snapshots.next().is_some() || snapshot.native_project_key != project_slug {
            return Err(CatalogCompositionError::invalid(
                "Claude catalog index path identity drifted",
            ));
        }
        for index_entry in &snapshot.entries {
            let member = admit_member(
                &mut MemberAdmission {
                    members: &mut members,
                    projects: &mut projects,
                    sessions: &mut sessions,
                    session_projects: &mut session_projects,
                },
                snapshot.native_project_key.clone(),
                index_entry.native_session_id.clone(),
                INDEX_COMPONENT_ID,
            )?;
            retain_projection_owner(
                &mut members,
                &member,
                ProjectionOwner::new(
                    2,
                    projection_owner(
                        source_instance_key,
                        index_component,
                        &entry.path_key,
                        checkpoint.generation,
                    )?,
                    ProjectAssociationBasis::NativeProjectIndex,
                ),
            );
            let metadata = members.get_mut(&member).expect("admitted member exists");
            metadata.merge_index(
                canonical_display(Some(index_entry.project_path.clone())),
                canonical_display(Some(index_entry.first_prompt.clone())),
                canonical_display(index_entry.summary.clone()),
                canonical_display(Some(index_entry.created_at.value.clone())),
            );
            if metadata.complete() {
                metadata
                    .metadata_component_ids
                    .insert(INDEX_COMPONENT_ID.to_owned());
            }
        }
        retain_coverage_point(
            &mut index_objects,
            &mut coverage_points,
            coverage_point(
                &index_component.stream_id,
                &entry.path_key,
                checkpoint.generation,
                crate::adapter::CoveragePositionKind::DocumentRevision,
                checkpoint.revision.as_bytes(),
                None,
            )?,
        )?;
        index_reads.push(ConfinedIndexRead {
            relative_path,
            checkpoint,
        });
    }

    require_stable_membership_checkpoints(
        projects_root,
        top_level_component,
        nested_component,
        index_component,
        &top_level_scan,
        &nested_scan,
        &index_scan,
    )?;

    let mut head_objects = Vec::new();
    let mut head_reads = Vec::new();
    let max_record_bytes_usize = usize::try_from(max_record_bytes).map_err(|_| {
        CatalogCompositionError::invalid(
            "Claude catalog head record bound does not fit this platform",
        )
    })?;
    let max_window_bytes_usize = usize::try_from(max_window_bytes).map_err(|_| {
        CatalogCompositionError::invalid(
            "Claude catalog head window bound does not fit this platform",
        )
    })?;
    let max_records_usize = usize::try_from(max_records).map_err(|_| {
        CatalogCompositionError::invalid(
            "Claude catalog head record count does not fit this platform",
        )
    })?;
    let physical_ceiling = prefix_physical_read_ceiling(max_record_bytes);
    let head_driver = AppendDelimitedFile::new(AppendDelimitedConfig {
        delimiter: b'\n',
        normalize_crlf: true,
        max_record_bytes: max_record_bytes_usize,
        max_batch_bytes: max_window_bytes_usize,
        max_records_per_batch: max_records_usize,
        prefix_anchor_bytes: HEAD_CHECKPOINT_ANCHOR_BYTES,
    })
    .map_err(map_driver_error)?;

    for (object_index, entry) in top_level_scan.entries.values().enumerate() {
        let coordinates = parent_coordinates(&entry.display_path)?;
        let member = (coordinates.project_slug, coordinates.session_id);
        if members.get(&member).is_some_and(MemberState::complete) {
            continue;
        }
        let relative_path = PathBuf::from(&entry.display_path);
        let origin = object_origin(object_index, "application/x-ndjson")?;
        let read = match head_driver.read_confined_bounded(
            projects_root,
            &relative_path,
            None,
            &origin,
            false,
            physical_ceiling,
        ) {
            Ok(read) => read,
            Err(SourceDriverError::LimitExceeded(_)) => {
                return Err(CatalogCompositionError::invalid(
                    "Claude catalog transcript head exceeded its declared bound",
                ));
            }
            Err(error) => return Err(map_driver_error(error)),
        };
        let AppendRead::Batch {
            items,
            checkpoint,
            transition,
            ..
        } = read
        else {
            return Err(CatalogCompositionError::invalid(
                "Claude catalog transcript head changed during production",
            ));
        };
        if transition != AppendTransition::Initial {
            return Err(CatalogCompositionError::invalid(
                "Claude catalog transcript head started from a continuation",
            ));
        }
        if items
            .iter()
            .any(|item| matches!(item, AppendItem::Quarantined(_)))
        {
            return Err(CatalogCompositionError::invalid(
                "Claude catalog transcript head is quarantined",
            ));
        }
        let descriptor = SourceObjectDescriptor {
            stream_id: parent_stream.id.clone(),
            object_key: entry.path_key.clone(),
            relative_path: relative_path.clone(),
        };
        let object_context = adapter
            .bootstrap_object(instance, &descriptor)
            .map_err(|_| {
                CatalogCompositionError::invalid(
                    "Claude catalog producer could not bootstrap a transcript object",
                )
            })?;
        let semantic_context = decode_context(&parent_stream, entry, instance)?;
        let mut decoder_state = None;
        let mut supplied_metadata = false;
        for item in items {
            let AppendItem::Record(record) = item else {
                continue;
            };
            let decoded = decode_catalog_record(
                &adapter,
                &parent_stream,
                &object_context,
                &semantic_context,
                &record,
                decoder_state.as_deref(),
                DecodeRuntimeLimits {
                    max_facts: 256,
                    max_diagnostics: 64,
                },
            )?;
            if decoded.disposition == DecodeDisposition::RetryTransient {
                return Err(CatalogCompositionError::invalid(
                    "Claude catalog transcript decoder requested a retry for stable head evidence",
                ));
            }
            decoder_state = decoded.next_decoder_state;
            for envelope in decoded.batch.facts() {
                let Fact::Session(session) = &envelope.value else {
                    continue;
                };
                if session.native_project_key != member.0 || session.native_session_id != member.1 {
                    return Err(CatalogCompositionError::invalid(
                        "Claude catalog transcript metadata attempted to retarget membership",
                    ));
                }
                if let Some(state) = members.get_mut(&member) {
                    state.merge_session(session);
                    supplied_metadata = true;
                }
            }
        }
        if supplied_metadata {
            if let Some(state) = members.get_mut(&member) {
                state
                    .metadata_component_ids
                    .insert(HEAD_COMPONENT_ID.to_owned());
            }
        }
        let monotonic = if checkpoint.committed_offset == 0 {
            None
        } else {
            Some(checkpoint.committed_offset)
        };
        retain_coverage_point(
            &mut head_objects,
            &mut coverage_points,
            coverage_point(
                &head_component.stream_id,
                &entry.path_key,
                checkpoint.generation,
                crate::adapter::CoveragePositionKind::AppendCursor,
                &checkpoint.encode(),
                monotonic,
            )?,
        )?;
        head_reads.push(ConfinedHeadRead {
            relative_path,
            checkpoint,
        });
    }

    let catalog_root = projects_root.parent().ok_or_else(|| {
        CatalogCompositionError::invalid("Claude catalog producer is missing its catalog root")
    })?;
    after_heads(catalog_root)?;
    revalidate_index_revisions(&index_driver, projects_root, &index_reads)?;
    revalidate_head_revisions(&head_driver, projects_root, physical_ceiling, &head_reads)?;
    require_stable_membership_checkpoints(
        projects_root,
        top_level_component,
        nested_component,
        index_component,
        &top_level_scan,
        &nested_scan,
        &index_scan,
    )?;

    let membership_entries = members
        .iter()
        .map(|(member, state)| {
            CatalogMembershipEntry::new(
                member_ref(composition, source_instance_key, &member.1)?,
                state.admitting_component_ids.iter().cloned().collect(),
                state.metadata_component_ids.iter().cloned().collect(),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut projection_members = members
        .iter()
        .map(|(member, state)| {
            let owner = state.projection_owner.as_ref().ok_or_else(|| {
                CatalogCompositionError::invalid(
                    "Claude catalog member has no covered projection owner",
                )
            })?;
            let availability = if state.admitting_component_ids.iter().any(|component| {
                component == TOP_LEVEL_COMPONENT_ID || component == NESTED_COMPONENT_ID
            }) {
                CatalogAvailability::TranscriptDiscovered
            } else {
                CatalogAvailability::MetadataOnly
            };
            Ok(CatalogSourceMemberProjection::new(
                owner.owner.clone(),
                member.0.clone(),
                member.1.clone(),
                availability,
                owner.association_basis,
            ))
        })
        .collect::<Result<Vec<_>, CatalogCompositionError>>()?;

    let completions = vec![
        complete_directory(
            executable,
            source_instance_key,
            access_policy_digest,
            nested_component,
            &nested_scan,
            &mut coverage_points,
        )?,
        complete_directory(
            executable,
            source_instance_key,
            access_policy_digest,
            top_level_component,
            &top_level_scan,
            &mut coverage_points,
        )?,
        CatalogComponentCoverageCompletion::new(
            executable,
            source_instance_key,
            access_policy_digest,
            INDEX_COMPONENT_ID,
            snapshot_completion_position(INDEX_COMPONENT_ID, index_scan.revision.as_bytes())?,
            index_objects,
        )?,
        CatalogComponentCoverageCompletion::new(
            executable,
            source_instance_key,
            access_policy_digest,
            HEAD_COMPONENT_ID,
            snapshot_completion_position(HEAD_COMPONENT_ID, b"transcript-head-complete")?,
            head_objects,
        )?,
    ];

    let authorities = vec![
        CatalogMembershipAuthorityEvidence::unbound(
            INDEX_COMPONENT_ID,
            index_scan.generation,
            *index_scan.revision.as_bytes(),
        )?,
        CatalogMembershipAuthorityEvidence::unbound(
            TOP_LEVEL_COMPONENT_ID,
            top_level_scan.generation,
            *top_level_scan.revision.as_bytes(),
        )?,
        CatalogMembershipAuthorityEvidence::unbound(
            NESTED_COMPONENT_ID,
            nested_scan.generation,
            *nested_scan.revision.as_bytes(),
        )?,
    ];

    let assembly = executable.assemble_produced_library_coverage(
        source_instance_key,
        access_policy_digest,
        authorities,
        membership_entries,
        completions,
    )?;
    let assembly = if let Some(prior_coverage) = prior_coverage {
        let (assembly, generations) = assembly.reconcile_refresh_coverage(prior_coverage)?;
        for member in &mut projection_members {
            member.reconcile_refresh_generation(&generations)?;
        }
        assembly
    } else {
        assembly
    };
    let publication_source = assembly.complete_publication_source().map_err(|_| {
        CatalogCompositionError::invalid(
            "Claude catalog coverage could not form a complete publication source",
        )
    })?;
    let projection = CatalogSourceProjection::assemble(publication_source, projection_members)?;

    Ok(ClaudeCatalogProduction {
        identity: ClaudeCatalogIdentity {
            adapter_id: ADAPTER_ID.to_owned(),
            project_count: projects.len() as u64,
            session_count: sessions.len() as u64,
            project_identity_digest: identity_digest(&projects),
            session_identity_digest: identity_digest(&sessions),
        },
        assembly,
        projection,
    })
}

#[derive(Default)]
struct MemberState {
    admitting_component_ids: BTreeSet<String>,
    metadata_component_ids: BTreeSet<String>,
    cwd: Option<String>,
    first_prompt: Option<String>,
    title: Option<String>,
    created_at: Option<String>,
    projection_owner: Option<ProjectionOwner>,
}

#[derive(Clone)]
struct ProjectionOwner {
    priority: u8,
    owner: CatalogEvidenceOwner,
    association_basis: ProjectAssociationBasis,
}

impl ProjectionOwner {
    fn new(
        priority: u8,
        owner: CatalogEvidenceOwner,
        association_basis: ProjectAssociationBasis,
    ) -> Self {
        Self {
            priority,
            owner,
            association_basis,
        }
    }
}

fn retain_projection_owner(
    members: &mut BTreeMap<MemberKey, MemberState>,
    member: &MemberKey,
    candidate: ProjectionOwner,
) {
    let state = members.get_mut(member).expect("admitted member exists");
    if state
        .projection_owner
        .as_ref()
        .is_none_or(|current| candidate.priority < current.priority)
    {
        state.projection_owner = Some(candidate);
    }
}

fn projection_owner(
    source_instance_key: CanonicalSourceInstanceKey,
    component: &CatalogSourceComponent,
    object_key: &[u8],
    generation: u64,
) -> Result<CatalogEvidenceOwner, CatalogCompositionError> {
    CatalogEvidenceOwner::new(
        ADAPTER_ID,
        source_instance_key,
        crate::adapter::CoverageStreamKey::derive(ADAPTER_ID, component.stream_id.as_bytes())
            .map_err(|_| {
                CatalogCompositionError::invalid("Claude catalog stream identity is invalid")
            })?,
        crate::adapter::CoverageObjectKey::derive(&component.stream_id, object_key).map_err(
            |_| CatalogCompositionError::invalid("Claude catalog object identity is invalid"),
        )?,
        generation,
    )
    .map_err(|_| CatalogCompositionError::invalid("Claude catalog evidence owner is invalid"))
}

impl MemberState {
    fn complete(&self) -> bool {
        has_display_value(&self.cwd)
            && (has_display_value(&self.first_prompt) || has_display_value(&self.title))
            && has_display_value(&self.created_at)
    }

    fn merge_index(
        &mut self,
        cwd: Option<String>,
        first_prompt: Option<String>,
        title: Option<String>,
        created_at: Option<String>,
    ) {
        self.cwd = canonical_display(self.cwd.take()).or(cwd);
        self.first_prompt = canonical_display(self.first_prompt.take()).or(first_prompt);
        self.title = canonical_display(self.title.take()).or(title);
        self.created_at = canonical_display(self.created_at.take()).or(created_at);
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

struct PathCoordinates {
    project_slug: String,
    session_id: String,
}

fn catalog_component(
    identifiers: (&str, &str, &str, &str),
    relative_selectors: &[&str],
    primitive: CatalogSourcePrimitive,
    contribution: CatalogContribution,
    overlap_strategy: CatalogOverlapStrategy,
    boundary: CatalogDecoderStateBoundary,
    contract_axes: (&str, &[&str]),
) -> CatalogSourceComponent {
    let (component_id, stream_id, source_stream_id, root_id) = identifiers;
    CatalogSourceComponent {
        component_id: component_id.to_owned(),
        source_stream_id: source_stream_id.to_owned(),
        stream_id: stream_id.to_owned(),
        root_id: root_id.to_owned(),
        relative_selectors: relative_selectors
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        discovery_bounds: CatalogDiscoveryBounds::new(250_000, 64)
            .expect("Claude catalog discovery bounds are valid"),
        primitive,
        contribution,
        overlap_strategy,
        safe_decoder_state_boundary: boundary,
        source_record_contract_version: 1,
        framing_contract_version: 1,
        decoder_contract_id: contract_axes.0.to_owned(),
        decoder_contract_version: 1,
        disposition_ownership: contract_axes
            .1
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
    }
}

fn require_exact_runtime_composition(
    composition: &CatalogSourceComposition,
) -> Result<(), CatalogCompositionError> {
    let mut expected_components = claude_catalog_components();
    expected_components = expected_components
        .into_iter()
        .map(CatalogSourceComponent::normalize)
        .collect::<Result<Vec<_>, _>>()?;
    expected_components.sort_by(|left, right| left.component_id.cmp(&right.component_id));
    if composition.adapter_id() != ADAPTER_ID
        || composition.source_declaration_id() != SOURCE_DECLARATION_ID
        || composition.components() != expected_components
    {
        return Err(CatalogCompositionError::invalid(
            "Claude catalog producer requires the exact compiled source declaration and component topology",
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
        .ok_or_else(|| {
            CatalogCompositionError::invalid(
                "Claude catalog composition is missing a required component",
            )
        })
}

fn require_stable_membership_checkpoints(
    projects_root: &Path,
    top_level_component: &CatalogSourceComponent,
    nested_component: &CatalogSourceComponent,
    index_component: &CatalogSourceComponent,
    top_level_scan: &DirectoryCheckpoint,
    nested_scan: &DirectoryCheckpoint,
    index_scan: &DirectoryCheckpoint,
) -> Result<(), CatalogCompositionError> {
    if scan_membership(projects_root, top_level_component, top_level_selection)? != *top_level_scan
        || scan_membership(projects_root, nested_component, nested_selection)? != *nested_scan
        || scan_membership(projects_root, index_component, index_selection)? != *index_scan
    {
        return Err(CatalogCompositionError::invalid(
            "Claude membership authority changed during complete production",
        ));
    }
    Ok(())
}

struct ConfinedIndexRead {
    relative_path: PathBuf,
    checkpoint: ReplaceCheckpoint,
}

struct ConfinedHeadRead {
    relative_path: PathBuf,
    checkpoint: AppendCheckpoint,
}

fn retain_coverage_point(
    objects: &mut Vec<CatalogCompletedCoverageObject>,
    aggregate: &mut usize,
    object: CatalogCompletedCoverageObject,
) -> Result<(), CatalogCompositionError> {
    retain_coverage_point_with_limit(objects, aggregate, object, MAX_CATALOG_COVERAGE_POINTS)
}

fn retain_coverage_point_with_limit(
    objects: &mut Vec<CatalogCompletedCoverageObject>,
    aggregate: &mut usize,
    object: CatalogCompletedCoverageObject,
    limit: usize,
) -> Result<(), CatalogCompositionError> {
    if objects.len() >= limit || *aggregate >= limit {
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

fn revalidate_index_revisions(
    driver: &ReplaceDocument,
    projects_root: &Path,
    reads: &[ConfinedIndexRead],
) -> Result<(), CatalogCompositionError> {
    for (object_index, evidence) in reads.iter().enumerate() {
        let origin = object_origin(object_index, "application/json")?;
        let read = driver
            .read_confined(projects_root, &evidence.relative_path, None, &origin, false)
            .map_err(map_driver_error)?;
        let checkpoint = match read {
            ReplaceRead::Record { checkpoint, .. } | ReplaceRead::Unchanged { checkpoint } => {
                checkpoint
            }
            _ => {
                return Err(CatalogCompositionError::invalid(
                    "Claude catalog index driver revision changed during production",
                ));
            }
        };
        if checkpoint.generation != evidence.checkpoint.generation
            || checkpoint.revision != evidence.checkpoint.revision
        {
            return Err(CatalogCompositionError::invalid(
                "Claude catalog index driver revision changed during production",
            ));
        }
    }
    Ok(())
}

fn revalidate_head_revisions(
    driver: &AppendDelimitedFile,
    projects_root: &Path,
    physical_ceiling: u64,
    reads: &[ConfinedHeadRead],
) -> Result<(), CatalogCompositionError> {
    for (object_index, evidence) in reads.iter().enumerate() {
        let origin = object_origin(object_index, "application/x-ndjson")?;
        let read = match driver.read_confined_bounded(
            projects_root,
            &evidence.relative_path,
            None,
            &origin,
            false,
            physical_ceiling,
        ) {
            Ok(read) => read,
            Err(SourceDriverError::LimitExceeded(_)) => {
                return Err(CatalogCompositionError::invalid(
                    "Claude catalog transcript head exceeded its declared bound",
                ));
            }
            Err(error) => return Err(map_driver_error(error)),
        };
        let AppendRead::Batch {
            checkpoint, items, ..
        } = read
        else {
            return Err(CatalogCompositionError::invalid(
                "Claude catalog transcript-head driver revision changed during production",
            ));
        };
        if items
            .iter()
            .any(|item| matches!(item, AppendItem::Quarantined(_)))
        {
            return Err(CatalogCompositionError::invalid(
                "Claude catalog transcript head is quarantined",
            ));
        }
        if checkpoint.generation != evidence.checkpoint.generation
            || checkpoint.encode() != evidence.checkpoint.encode()
        {
            return Err(CatalogCompositionError::invalid(
                "Claude catalog transcript-head driver revision changed during production",
            ));
        }
    }
    Ok(())
}

fn scan_membership(
    projects_root: &Path,
    component: &CatalogSourceComponent,
    selector: impl Fn(&Path, DirectoryEntryKind) -> DirectorySelection,
) -> Result<DirectoryCheckpoint, CatalogCompositionError> {
    let max_entries = usize::try_from(component.discovery_bounds.max_entries).map_err(|_| {
        CatalogCompositionError::invalid(
            "Claude catalog discovery entry bound does not fit this platform",
        )
    })?;
    let scan = DirectorySnapshot::new(DirectorySnapshotConfig {
        max_entries,
        max_entries_per_directory: max_entries,
        max_depth: usize::try_from(component.discovery_bounds.max_depth).map_err(|_| {
            CatalogCompositionError::invalid(
                "Claude catalog discovery depth bound does not fit this platform",
            )
        })?,
    })
    .map_err(map_driver_error)?
    .scan(projects_root, None, &selector)
    .map_err(map_driver_error)?;
    let DirectoryScan::Snapshot { checkpoint, .. } = scan else {
        return Err(CatalogCompositionError::invalid(
            "Claude membership authority was not completely available",
        ));
    };
    if checkpoint.generation == 0
        || checkpoint
            .entries
            .values()
            .any(|entry| entry.generation == 0)
    {
        return Err(CatalogCompositionError::invalid(
            "Claude membership authority generation must be positive",
        ));
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

fn parent_coordinates(display_path: &str) -> Result<PathCoordinates, CatalogCompositionError> {
    let components = utf8_components(Path::new(display_path));
    if components.len() != 2 {
        return Err(CatalogCompositionError::invalid(
            "Claude top-level transcript path is not a catalog coordinate",
        ));
    }
    let session_id = components[1].strip_suffix(".jsonl").ok_or_else(|| {
        CatalogCompositionError::invalid("Claude top-level transcript is not a JSONL object")
    })?;
    if session_id.is_empty() || !is_uuid(session_id) {
        return Err(CatalogCompositionError::invalid(
            "Claude top-level transcript name is not a UUID session coordinate",
        ));
    }
    if components[0].is_empty() {
        return Err(CatalogCompositionError::invalid(
            "Claude catalog member coordinates must not be empty",
        ));
    }
    Ok(PathCoordinates {
        project_slug: components[0].clone(),
        session_id: session_id.to_owned(),
    })
}

fn nested_coordinates(display_path: &str) -> Result<PathCoordinates, CatalogCompositionError> {
    let components = utf8_components(Path::new(display_path));
    if components.len() < 4 || components.get(2).map(String::as_str) != Some("subagents") {
        return Err(CatalogCompositionError::invalid(
            "Claude nested-parent path is not a catalog coordinate",
        ));
    }
    let file_name = components.last().expect("minimum component count checked");
    if !file_name.starts_with("agent-") || !file_name.ends_with(".jsonl") {
        return Err(CatalogCompositionError::invalid(
            "Claude nested-parent object name is not a catalog coordinate",
        ));
    }
    if components[0].is_empty() || components[1].is_empty() {
        return Err(CatalogCompositionError::invalid(
            "Claude catalog member coordinates must not be empty",
        ));
    }
    Ok(PathCoordinates {
        project_slug: components[0].clone(),
        session_id: components[1].clone(),
    })
}

fn index_project(display_path: &str) -> Result<String, CatalogCompositionError> {
    let components = utf8_components(Path::new(display_path));
    if components.len() != 2 || components[0].is_empty() || components[1] != "sessions-index.json" {
        return Err(CatalogCompositionError::invalid(
            "Claude session-index path is not a catalog coordinate",
        ));
    }
    Ok(components[0].clone())
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

struct MemberAdmission<'a> {
    members: &'a mut BTreeMap<MemberKey, MemberState>,
    projects: &'a mut BTreeSet<(String, String)>,
    sessions: &'a mut BTreeSet<(String, String, String)>,
    session_projects: &'a mut BTreeMap<String, String>,
}

fn admit_member(
    admission: &mut MemberAdmission<'_>,
    project: String,
    session: String,
    component_id: &str,
) -> Result<MemberKey, CatalogCompositionError> {
    admit_member_with_limit(
        admission,
        project,
        session,
        component_id,
        MAX_MEMBERSHIP_MEMBERS,
    )
}

fn admit_member_with_limit(
    admission: &mut MemberAdmission<'_>,
    project: String,
    session: String,
    component_id: &str,
    member_limit: usize,
) -> Result<MemberKey, CatalogCompositionError> {
    if project.is_empty() || session.is_empty() {
        return Err(CatalogCompositionError::invalid(
            "Claude catalog member coordinates must not be empty",
        ));
    }
    let key = (project.clone(), session.clone());
    if !admission.members.contains_key(&key) && admission.members.len() >= member_limit {
        return Err(CatalogCompositionError::invalid(format!(
            "catalog membership exceeds {member_limit} members"
        )));
    }
    if let Some(existing) = admission
        .session_projects
        .insert(session.clone(), project.clone())
    {
        if existing != project {
            return Err(CatalogCompositionError::invalid(
                "one Claude session ID appeared under competing projects",
            ));
        }
    }
    admission.projects.insert((ADAPTER_ID.to_string(), project));
    admission
        .sessions
        .insert((ADAPTER_ID.to_string(), key.0.clone(), key.1.clone()));
    admission
        .members
        .entry(key.clone())
        .or_default()
        .admitting_component_ids
        .insert(component_id.to_owned());
    Ok(key)
}

fn member_ref(
    composition: &CatalogSourceComposition,
    source_instance_key: CanonicalSourceInstanceKey,
    native_session_id: &str,
) -> Result<CatalogMemberRef, CatalogCompositionError> {
    CatalogMemberRef::from_canonical_session(
        composition.member_identity_contract_id(),
        composition.adapter_id(),
        source_instance_key,
        native_session_id.as_bytes(),
    )
}

fn complete_directory(
    executable: &CatalogExecutableComposition<'_, '_>,
    source_instance_key: CanonicalSourceInstanceKey,
    access_policy_digest: CatalogAccessPolicyDigest,
    component: &CatalogSourceComponent,
    checkpoint: &DirectoryCheckpoint,
    coverage_points: &mut usize,
) -> Result<CatalogComponentCoverageCompletion, CatalogCompositionError> {
    let mut objects = Vec::with_capacity(checkpoint.entries.len());
    for entry in checkpoint.entries.values() {
        retain_coverage_point(
            &mut objects,
            coverage_points,
            coverage_point(
                &component.stream_id,
                &entry.path_key,
                entry.generation,
                crate::adapter::CoveragePositionKind::SnapshotRevision,
                entry.revision.as_bytes(),
                None,
            )?,
        )?;
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

fn coverage_point(
    stream_id: &str,
    object_key: &[u8],
    generation: u64,
    kind: crate::adapter::CoveragePositionKind,
    opaque: &[u8],
    monotonic_order: Option<u64>,
) -> Result<CatalogCompletedCoverageObject, CatalogCompositionError> {
    let position = crate::adapter::CoveragePosition::derive(kind, opaque, monotonic_order)
        .map_err(|_| {
            CatalogCompositionError::invalid("Claude catalog coverage position is invalid")
        })?;
    CatalogCompletedCoverageObject::point(
        crate::adapter::CoverageObjectKey::derive(stream_id, object_key).map_err(|_| {
            CatalogCompositionError::invalid("Claude catalog coverage object key is invalid")
        })?,
        generation,
        position,
        crate::adapter::CoverageProvenance::default(),
    )
}

fn snapshot_completion_position(
    component_id: &str,
    opaque: &[u8],
) -> Result<crate::adapter::CoveragePosition, CatalogCompositionError> {
    let mut material = Vec::new();
    material.extend_from_slice(&(component_id.len() as u64).to_be_bytes());
    material.extend_from_slice(component_id.as_bytes());
    material.extend_from_slice(&(opaque.len() as u64).to_be_bytes());
    material.extend_from_slice(opaque);
    crate::adapter::CoveragePosition::derive(
        crate::adapter::CoveragePositionKind::SnapshotRevision,
        &material,
        None,
    )
    .map_err(|_| {
        CatalogCompositionError::invalid("Claude catalog component completion position is invalid")
    })
}

fn prefix_physical_read_ceiling(max_record_bytes: u64) -> u64 {
    // Conservative candidate fixture evidence: one maximum logical record, one
    // matching framing read-ahead, and the 4 KiB checkpoint anchor. This is
    // not a ratified global access-policy bound.
    max_record_bytes
        .saturating_add(max_record_bytes)
        .saturating_add(HEAD_CHECKPOINT_ANCHOR_BYTES as u64)
}

fn exact_stream(
    streams: &[StreamSpec],
    stream_id: &str,
) -> Result<StreamSpec, CatalogCompositionError> {
    streams
        .iter()
        .find(|stream| stream.id.as_str() == stream_id)
        .cloned()
        .ok_or_else(|| {
            CatalogCompositionError::invalid("Claude catalog producer is missing a required stream")
        })
}

#[cfg(test)]
fn producer_instance(
    projects_root: &Path,
    source_instance_discriminator: &[u8],
) -> Result<SourceInstance, CatalogCompositionError> {
    let catalog_root = projects_root.parent().ok_or_else(|| {
        CatalogCompositionError::invalid("Claude catalog producer is missing its catalog root")
    })?;
    Ok(SourceInstance {
        id: 1,
        spec: SourceInstanceSpec {
            identity_contract_version: 1,
            stable_key: SourceInstanceKey::new(source_instance_discriminator.to_vec()).map_err(
                |_| {
                    CatalogCompositionError::invalid(
                        "Claude catalog producer source instance key is invalid",
                    )
                },
            )?,
            display_name: "Claude catalog producer".to_string(),
            roots: vec![
                named_root("home", catalog_root),
                named_root("projects", projects_root),
                named_root("teams", catalog_root.join("teams")),
                named_root("sessions", catalog_root.join("sessions")),
            ],
            discovery_reason: "crate-private Claude catalog producer".to_string(),
        },
    })
}

#[cfg(test)]
fn named_root(name: &str, path: impl Into<PathBuf>) -> SourceRoot {
    SourceRoot {
        name: name.to_string(),
        path: path.into(),
    }
}

fn decode_context(
    stream: &StreamSpec,
    entry: &DirectoryEntryState,
    instance: &SourceInstance,
) -> Result<FactSemanticContext, CatalogCompositionError> {
    FactSemanticContext::new(
        &AdapterId::new(ADAPTER_ID).map_err(|_| {
            CatalogCompositionError::invalid("Claude catalog producer adapter id is invalid")
        })?,
        instance.spec.identity_contract_version,
        instance.spec.stable_key.as_bytes(),
        stream.id.as_str().as_bytes(),
        &entry.path_key,
        1,
    )
    .map_err(|_| {
        CatalogCompositionError::invalid("Claude catalog producer semantic context is invalid")
    })
}

fn object_origin(
    object_index: usize,
    media_type: &str,
) -> Result<RecordOrigin, CatalogCompositionError> {
    let object_number = u64::try_from(object_index).map_err(|_| {
        CatalogCompositionError::invalid("Claude catalog producer object index overflow")
    })?;
    Ok(RecordOrigin {
        source_instance_id: 1,
        stream_id: 2,
        object_id: object_number.checked_add(3).ok_or_else(|| {
            CatalogCompositionError::invalid("Claude catalog producer object id overflow")
        })?,
        observed_at: 1,
        source_timestamp_hint: None,
        media_type: SourceMediaType::new(media_type).map_err(map_driver_error)?,
    })
}

fn map_driver_error(error: SourceDriverError) -> CatalogCompositionError {
    CatalogCompositionError::invalid(match error {
        SourceDriverError::InvalidConfig(_) => {
            "Claude catalog producer received invalid driver configuration"
        }
        SourceDriverError::InvalidCursor(_) => {
            "Claude catalog producer received an invalid driver cursor"
        }
        SourceDriverError::PathEscape(_) => {
            "Claude catalog producer rejected a path that escaped its declared root"
        }
        SourceDriverError::LimitExceeded(_) => {
            "Claude catalog producer exceeded a declared source bound"
        }
        SourceDriverError::Unstable(_) => {
            "Claude catalog producer observed an unstable source snapshot"
        }
        SourceDriverError::Database(_) => "Claude catalog producer cannot use a database source",
        SourceDriverError::Io { .. } => {
            "Claude catalog producer failed to read a declared source object"
        }
    })
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

fn identity_digest<T>(values: &BTreeSet<T>) -> String
where
    T: serde::Serialize + Ord,
{
    let mut digest = Sha256::new();
    for value in values {
        let encoded = serde_json::to_vec(value).expect("catalog identity is serializable");
        digest.update((encoded.len() as u64).to_be_bytes());
        digest.update(encoded);
    }
    let mut output = String::with_capacity(64);
    for byte in digest.finalize() {
        use std::fmt::Write as _;
        write!(output, "{byte:02x}").expect("writing to String is infallible");
    }
    output
}

#[cfg(test)]
mod limit_tests {
    use super::*;
    use crate::adapter::CoveragePositionKind;

    #[test]
    fn catalog_producer_cannot_bypass_the_common_decode_boundary() {
        let source = include_str!("catalog_runtime.rs");
        assert!(source.contains("decode_record(DecodeRuntimeRequest"));
        let direct_decode = [".de", "code("].concat();
        let direct_batch = ["FactBatch::new_with_", "semantic_context"].concat();
        assert!(!source.contains(&direct_decode));
        assert!(!source.contains(&direct_batch));
    }

    #[test]
    fn admit_member_rejects_the_first_excess_item_before_session_project_mutation() {
        let mut members = BTreeMap::new();
        let mut projects = BTreeSet::new();
        let mut sessions = BTreeSet::new();
        let mut session_projects = BTreeMap::new();
        let first = admit_member_with_limit(
            &mut MemberAdmission {
                members: &mut members,
                projects: &mut projects,
                sessions: &mut sessions,
                session_projects: &mut session_projects,
            },
            "-tmp-project".to_owned(),
            "session-one".to_owned(),
            TOP_LEVEL_COMPONENT_ID,
            1,
        )
        .unwrap();
        assert_eq!(first.1, "session-one");
        assert_eq!(members.len(), 1);
        assert_eq!(session_projects.len(), 1);
        let error = admit_member_with_limit(
            &mut MemberAdmission {
                members: &mut members,
                projects: &mut projects,
                sessions: &mut sessions,
                session_projects: &mut session_projects,
            },
            "-tmp-project".to_owned(),
            "session-two".to_owned(),
            TOP_LEVEL_COMPONENT_ID,
            1,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("catalog membership exceeds 1 members"));
        assert_eq!(members.len(), 1);
        assert_eq!(session_projects.len(), 1);
        assert!(!session_projects.contains_key("session-two"));
        assert!(!members.contains_key(&("-tmp-project".to_owned(), "session-two".to_owned())));
    }

    #[test]
    fn retain_coverage_point_rejects_the_first_excess_aggregate_item() {
        let mut objects = Vec::new();
        let mut aggregate = 0_usize;
        let first = coverage_point(
            INDEX_STREAM_ID,
            b"object-one",
            1,
            CoveragePositionKind::DocumentRevision,
            b"revision-one",
            None,
        )
        .unwrap();
        retain_coverage_point_with_limit(&mut objects, &mut aggregate, first, 1).unwrap();
        assert_eq!(objects.len(), 1);
        assert_eq!(aggregate, 1);
        let second = coverage_point(
            INDEX_STREAM_ID,
            b"object-two",
            1,
            CoveragePositionKind::DocumentRevision,
            b"revision-two",
            None,
        )
        .unwrap();
        let error =
            retain_coverage_point_with_limit(&mut objects, &mut aggregate, second, 1).unwrap_err();
        assert!(error
            .to_string()
            .contains("catalog coverage exceeds the RFC 012A portable set bounds"));
        assert_eq!(objects.len(), 1);
        assert_eq!(aggregate, 1);
    }
}
