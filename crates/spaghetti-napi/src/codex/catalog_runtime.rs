//! Crate-private Codex catalog source/coverage producer.
//!
//! The current built-in support release remains Candidate and cannot authorize
//! this producer. Production execution requires a borrowed typed catalog
//! authorization that binds the reviewed release, source declaration, selected
//! contracts, and exact source streams. The catalog component owns a disjoint
//! catalog family rather than treating legacy durable facts as semantic-tier
//! parity. Native paths stay out of returned types, Debug, and error messages.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

use super::adapter::CodexAdapter;
use crate::adapter::{
    AdapterId, AgentAdapter, AuthorizedCatalogAccess, DecodeDisposition, DriverSpec, Fact,
    FactSemanticContext, SourceCoverageSet, SourceInstance, SourceObjectDescriptor, StreamSpec,
    TypedAccessAuthorization,
};
#[cfg(test)]
use crate::adapter::{SourceInstanceKey, SourceInstanceSpec, SourceRoot};
use crate::catalog_contract::evidence::{
    CatalogAvailability, CatalogEvidenceOwner, ProjectAssociationBasis,
};
use crate::catalog_contract::CatalogAccessPolicyDigest;
use crate::decode_runtime::{
    decode_record, DecodeRuntimeLimits, DecodeRuntimeRequest, DecoderDependenciesDenied,
};
#[cfg(test)]
use crate::source::catalog_composition::CatalogPromotedBinding;
use crate::source::catalog_composition::{
    CatalogBoundSourceAccess, CatalogCompletedCoverageObject, CatalogComponentCoverageCompletion,
    CatalogCompositionError, CatalogContribution, CatalogDecoderStateBoundary,
    CatalogDiscoveryBounds, CatalogLibraryCoverageAssembly, CatalogMemberRef,
    CatalogMembershipAuthorityEvidence, CatalogMembershipEntry, CatalogOverlapStrategy,
    CatalogSourceComponent, CatalogSourceComposition, CatalogSourcePrimitive,
};
use crate::source::catalog_projection::{CatalogSourceMemberProjection, CatalogSourceProjection};
use crate::source::catalog_runtime_registry::CatalogSourceRuntime;
use crate::source::{
    AppendCheckpoint, AppendDelimitedConfig, AppendDelimitedFile, AppendItem, AppendRead,
    AppendTransition, DirectoryEntryKind, DirectoryScan, DirectorySelection, DirectorySnapshot,
    DirectorySnapshotConfig, RecordOrigin, SourceDriverError, SourceMediaType,
};

const ADAPTER_ID: &str = "codex";
const STREAM_ID: &str = "rollout-sessions";
const SESSIONS_ROOT_ID: &str = "sessions";
const COMPONENT_ID: &str = "rollout-session-meta-head";
const MEMBER_IDENTITY_CONTRACT: &str = "catalog-session-identity-v1";
const SOURCE_DECLARATION_ID: &str = "codex-sources-2026-08-15-candidate";
#[cfg(test)]
const PLANNING_EVIDENCE_ID: &str = "phase0-catalog-census-2026-08-15";
#[cfg(test)]
const PLANNED_SUPPORT_RELEASE_ID: &str = "codex.catalog-candidate-2026-08-15";
#[cfg(test)]
const PLANNED_SOURCE_DECLARATION_ID: &str = "codex.catalog-sources-v1";
#[cfg(test)]
const CONFORMANCE_SUPPORT_RELEASE_ID: &str = "codex.catalog-conformance-support-v1";
#[cfg(test)]
const CONFORMANCE_SOURCE_DECLARATION: &[u8] =
    b"spaghetti/rfc012b/codex-catalog-conformance-declaration/v1";
#[cfg(test)]
const CONFORMANCE_SUPPORT_RELEASE: &[u8] =
    b"spaghetti/rfc012b/codex-catalog-conformance-support/v1";
const MAX_ENTRIES: usize = 250_000;
const MAX_DEPTH: usize = 64;
const HEAD_WINDOW_BYTES: usize = 64 * 1024;
const MAX_RECORD_BYTES: usize = HEAD_WINDOW_BYTES - 1;
const CHECKPOINT_ANCHOR_BYTES: usize = 4 * 1024;
const PHYSICAL_READ_CEILING: u64 = 64 * 1024 + CHECKPOINT_ANCHOR_BYTES as u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexCatalogIdentity {
    pub(crate) adapter_id: String,
    pub(crate) project_count: u64,
    pub(crate) session_count: u64,
    pub(crate) project_identity_digest: String,
    pub(crate) session_identity_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexCatalogProduction {
    pub(crate) identity: CodexCatalogIdentity,
    pub(crate) assembly: CatalogLibraryCoverageAssembly,
    pub(crate) projection: CatalogSourceProjection,
}

pub(crate) struct CodexCatalogSourceRuntime;

impl CatalogSourceRuntime for CodexCatalogSourceRuntime {
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
            .map_err(|_| CatalogCompositionError::invalid("Codex catalog authority is invalid"))?;
        let composition = codex_authorized_catalog_composition(&access)?;
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
            .map_err(|_| CatalogCompositionError::invalid("Codex catalog authority is invalid"))?;
        let composition = codex_authorized_catalog_composition(&access)?;
        let executable = composition.authorize_execution(access)?;
        let bound = executable.bind_source_instance(instance)?;
        let production = match prior_coverage {
            Some(prior) => produce_codex_library_refresh(&bound, access_policy_digest, prior)?,
            None => produce_codex_library_coverage(&bound, access_policy_digest)?,
        };
        Ok(production.projection)
    }
}

pub(crate) fn codex_catalog_components() -> Vec<CatalogSourceComponent> {
    vec![CatalogSourceComponent {
        component_id: COMPONENT_ID.to_owned(),
        source_stream_id: STREAM_ID.to_owned(),
        stream_id: STREAM_ID.to_owned(),
        root_id: SESSIONS_ROOT_ID.to_owned(),
        relative_selectors: vec!["**/rollout-*.jsonl".to_owned()],
        discovery_bounds: CatalogDiscoveryBounds::new(MAX_ENTRIES as u32, MAX_DEPTH as u32)
            .expect("Codex catalog discovery bounds are valid"),
        primitive: CatalogSourcePrimitive::DelimitedHead {
            max_record_bytes: HEAD_WINDOW_BYTES as u64,
        },
        contribution: CatalogContribution::Membership {
            member_identity_contract_id: MEMBER_IDENTITY_CONTRACT.to_owned(),
            admission_contract_id: "noninternal-session-meta-admission-v1".to_owned(),
            provides_metadata: true,
        },
        overlap_strategy: CatalogOverlapStrategy::DisjointCatalogFamily {
            ownership_contract_id: "codex-session-meta-catalog-family-v1".to_owned(),
        },
        safe_decoder_state_boundary: CatalogDecoderStateBoundary::ObjectGenerationCursor,
        source_record_contract_version: 1,
        framing_contract_version: 1,
        decoder_contract_id: "codex-rollout-record".to_owned(),
        decoder_contract_version: 1,
        disposition_ownership: vec!["native-family:rollout-transcript".to_owned()],
    }]
}

/// Build the exact compiled Codex catalog topology from a non-transferable
/// typed authorization. Candidate releases cannot produce this proof, and the
/// returned value must still consume it through `authorize_execution`.
pub(crate) fn codex_authorized_catalog_composition(
    authorization: &AuthorizedCatalogAccess<'_>,
) -> Result<CatalogSourceComposition, CatalogCompositionError> {
    CatalogSourceComposition::from_authorized_catalog_access(
        authorization,
        SOURCE_DECLARATION_ID,
        codex_catalog_components(),
    )
}

#[cfg(test)]
pub(crate) fn codex_planned_catalog_composition(
) -> Result<CatalogSourceComposition, CatalogCompositionError> {
    CatalogSourceComposition::new_planned(
        ADAPTER_ID,
        PLANNED_SUPPORT_RELEASE_ID,
        PLANNED_SOURCE_DECLARATION_ID,
        PLANNING_EVIDENCE_ID,
        codex_catalog_components(),
    )
}

#[cfg(test)]
pub(crate) fn codex_conformance_promoted_composition(
) -> Result<CatalogSourceComposition, CatalogCompositionError> {
    CatalogSourceComposition::new_promoted(
        ADAPTER_ID,
        CONFORMANCE_SUPPORT_RELEASE_ID,
        SOURCE_DECLARATION_ID,
        CatalogPromotedBinding::from_digests(
            Sha256::digest(CONFORMANCE_SOURCE_DECLARATION).into(),
            Sha256::digest(CONFORMANCE_SUPPORT_RELEASE).into(),
        )?,
        codex_catalog_components(),
    )
}

#[cfg(test)]
pub(crate) fn codex_conformance_source_declaration_bytes() -> &'static [u8] {
    CONFORMANCE_SOURCE_DECLARATION
}

#[cfg(test)]
pub(crate) fn codex_conformance_support_release_bytes() -> &'static [u8] {
    CONFORMANCE_SUPPORT_RELEASE
}

#[cfg(test)]
pub(crate) fn codex_conformance_support_release_id() -> &'static str {
    CONFORMANCE_SUPPORT_RELEASE_ID
}

#[cfg(test)]
pub(crate) fn codex_conformance_source_declaration_id() -> &'static str {
    SOURCE_DECLARATION_ID
}

#[cfg(test)]
pub(crate) fn codex_catalog_source_instance(
    catalog_root: &Path,
    source_instance_discriminator: &[u8],
) -> Result<SourceInstance, CatalogCompositionError> {
    Ok(SourceInstance {
        id: 1,
        spec: SourceInstanceSpec {
            identity_contract_version: 1,
            stable_key: SourceInstanceKey::new(source_instance_discriminator.to_vec()).map_err(
                |_| CatalogCompositionError::invalid("Codex catalog source identity is invalid"),
            )?,
            display_name: "Codex catalog producer".to_owned(),
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
            discovery_reason: "crate-private Codex catalog producer".to_owned(),
        },
    })
}

pub(crate) fn produce_codex_library_coverage(
    access: &CatalogBoundSourceAccess<'_, '_, '_>,
    access_policy_digest: CatalogAccessPolicyDigest,
) -> Result<CodexCatalogProduction, CatalogCompositionError> {
    produce_codex_library_coverage_after_heads(access, access_policy_digest, None, |_| Ok(()))
}

pub(crate) fn produce_codex_library_refresh(
    access: &CatalogBoundSourceAccess<'_, '_, '_>,
    access_policy_digest: CatalogAccessPolicyDigest,
    prior_coverage: &SourceCoverageSet,
) -> Result<CodexCatalogProduction, CatalogCompositionError> {
    produce_codex_library_coverage_after_heads(
        access,
        access_policy_digest,
        Some(prior_coverage),
        |_| Ok(()),
    )
}

#[cfg(test)]
pub(crate) fn produce_codex_library_coverage_with_post_head_mutation(
    access: &CatalogBoundSourceAccess<'_, '_, '_>,
    access_policy_digest: CatalogAccessPolicyDigest,
    mutate: impl FnOnce(&Path),
) -> Result<CodexCatalogProduction, CatalogCompositionError> {
    produce_codex_library_coverage_after_heads(
        access,
        access_policy_digest,
        None,
        |sessions_root| {
            mutate(sessions_root);
            Ok(())
        },
    )
}

fn produce_codex_library_coverage_after_heads(
    access: &CatalogBoundSourceAccess<'_, '_, '_>,
    access_policy_digest: CatalogAccessPolicyDigest,
    prior_coverage: Option<&SourceCoverageSet>,
    after_heads: impl FnOnce(&Path) -> Result<(), CatalogCompositionError>,
) -> Result<CodexCatalogProduction, CatalogCompositionError> {
    let executable = access.executable();
    executable.validate_complete_coverage_authority()?;
    let composition = executable.composition();
    require_exact_runtime_composition(composition)?;
    let component = composition
        .components()
        .iter()
        .find(|component| component.component_id == COMPONENT_ID)
        .ok_or_else(|| {
            CatalogCompositionError::invalid("Codex catalog composition is missing its component")
        })?;
    let source_instance_key = access.source_instance_key()?;
    let instance = access.instance();
    let sessions_root = access.root(SESSIONS_ROOT_ID)?;
    let checkpoint = scan_membership(sessions_root, component)?;

    let adapter = CodexAdapter::new();
    let stream = exact_stream(&adapter.streams(instance).map_err(|_| {
        CatalogCompositionError::invalid("Codex catalog producer could not load adapter streams")
    })?)?;
    if !matches!(stream.driver, DriverSpec::AppendDelimited(_)) {
        return Err(CatalogCompositionError::invalid(
            "Codex catalog stream is not append-delimited",
        ));
    }
    let driver = AppendDelimitedFile::new(AppendDelimitedConfig {
        delimiter: b'\n',
        normalize_crlf: true,
        max_record_bytes: MAX_RECORD_BYTES,
        max_batch_bytes: MAX_RECORD_BYTES,
        max_records_per_batch: 1,
        prefix_anchor_bytes: CHECKPOINT_ANCHOR_BYTES,
    })
    .map_err(map_driver_error)?;

    let mut projects = BTreeSet::new();
    let mut sessions = BTreeSet::new();
    let mut session_projects = BTreeMap::<String, String>::new();
    let mut members = BTreeSet::<String>::new();
    let mut coverage_objects = Vec::with_capacity(checkpoint.entries.len());
    let mut reads = Vec::with_capacity(checkpoint.entries.len());
    let mut projection_members = Vec::with_capacity(checkpoint.entries.len());

    for (index, entry) in checkpoint.entries.values().enumerate() {
        let relative_path = PathBuf::from(&entry.display_path);
        let read = driver
            .read_confined_bounded(
                sessions_root,
                &relative_path,
                None,
                &object_origin(index)?,
                false,
                PHYSICAL_READ_CEILING,
            )
            .map_err(map_driver_error)?;
        let AppendRead::Batch {
            items,
            checkpoint: head_checkpoint,
            transition,
            ..
        } = read
        else {
            return Err(CatalogCompositionError::invalid(
                "Codex catalog head was not stably available",
            ));
        };
        if transition != AppendTransition::Initial || items.len() != 1 {
            return Err(CatalogCompositionError::invalid(
                "Codex catalog head must contain exactly one initial record",
            ));
        }
        let record = match items.into_iter().next().expect("one item checked") {
            AppendItem::Record(record) => record,
            AppendItem::Quarantined(_) => {
                return Err(CatalogCompositionError::invalid(
                    "Codex catalog head exceeded its declared record bound",
                ))
            }
        };
        let descriptor = SourceObjectDescriptor {
            stream_id: stream.id.clone(),
            object_key: entry.path_key.clone(),
            relative_path: relative_path.clone(),
        };
        let object_context = adapter
            .bootstrap_object(instance, &descriptor)
            .map_err(|_| {
                CatalogCompositionError::invalid(
                    "Codex catalog producer could not bootstrap a rollout object",
                )
            })?;
        let semantic_context = FactSemanticContext::new(
            &AdapterId::new(ADAPTER_ID).map_err(|_| {
                CatalogCompositionError::invalid("Codex catalog adapter identity is invalid")
            })?,
            instance.spec.identity_contract_version,
            instance.spec.stable_key.as_bytes(),
            stream.id.as_str().as_bytes(),
            &entry.path_key,
            1,
        )
        .map_err(|_| {
            CatalogCompositionError::invalid("Codex catalog semantic context is invalid")
        })?;
        let decoded = decode_record(DecodeRuntimeRequest {
            adapter: &adapter,
            decoder: &stream.decoder,
            object_context: &object_context,
            source_access: &DecoderDependenciesDenied,
            record: &record,
            semantic_context: &semantic_context,
            decoder_state: None,
            retention: stream.retention,
            limits: DecodeRuntimeLimits {
                max_facts: 64,
                max_diagnostics: 16,
            },
        })
        .result
        .map_err(|_| {
            CatalogCompositionError::invalid(
                "Codex catalog record failed at the common decode boundary",
            )
        })?;
        match decoded.disposition {
            DecodeDisposition::Applied => {
                let mut decoded_sessions = decoded.batch.facts().iter().filter_map(|envelope| {
                    let Fact::Session(session) = &envelope.value else {
                        return None;
                    };
                    Some(session)
                });
                let session = decoded_sessions.next().ok_or_else(|| {
                    CatalogCompositionError::invalid(
                        "Codex catalog session_meta emitted no session fact",
                    )
                })?;
                if decoded_sessions.next().is_some() {
                    return Err(CatalogCompositionError::invalid(
                        "Codex catalog session_meta emitted multiple session facts",
                    ));
                }
                if let Some(existing) = session_projects.insert(
                    session.native_session_id.clone(),
                    session.native_project_key.clone(),
                ) {
                    if existing != session.native_project_key {
                        return Err(CatalogCompositionError::invalid(
                            "one Codex session ID appeared under competing projects",
                        ));
                    }
                }
                members.insert(session.native_session_id.clone());
                projects.insert((ADAPTER_ID.to_owned(), session.native_project_key.clone()));
                sessions.insert((
                    ADAPTER_ID.to_owned(),
                    session.native_project_key.clone(),
                    session.native_session_id.clone(),
                ));
                projection_members.push(CatalogSourceMemberProjection::new(
                    projection_owner(
                        source_instance_key,
                        &entry.path_key,
                        head_checkpoint.generation,
                    )?,
                    session.native_project_key.clone(),
                    session.native_session_id.clone(),
                    CatalogAvailability::TranscriptDiscovered,
                    ProjectAssociationBasis::RolloutHeader,
                ));
            }
            DecodeDisposition::IgnoredKnown => {}
            _ => {
                return Err(CatalogCompositionError::invalid(
                    "Codex catalog head did not produce a complete known disposition",
                ))
            }
        }

        coverage_objects.push(coverage_point(
            &entry.path_key,
            head_checkpoint.generation,
            &head_checkpoint,
        )?);
        reads.push((relative_path, head_checkpoint));
    }

    after_heads(sessions_root)?;
    revalidate_heads(&driver, sessions_root, &reads)?;
    if scan_membership(sessions_root, component)? != checkpoint {
        return Err(CatalogCompositionError::invalid(
            "Codex catalog membership changed during head revalidation",
        ));
    }

    let membership_entries = members
        .iter()
        .map(|session_id| {
            CatalogMembershipEntry::new(
                CatalogMemberRef::from_canonical_session(
                    composition.member_identity_contract_id(),
                    composition.adapter_id(),
                    source_instance_key,
                    session_id.as_bytes(),
                )?,
                vec![COMPONENT_ID.to_owned()],
                vec![COMPONENT_ID.to_owned()],
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let completion = CatalogComponentCoverageCompletion::new(
        executable,
        source_instance_key,
        access_policy_digest,
        COMPONENT_ID,
        completion_position(checkpoint.revision.as_bytes())?,
        coverage_objects,
    )?;
    let authorities = vec![CatalogMembershipAuthorityEvidence::unbound(
        COMPONENT_ID,
        checkpoint.generation,
        *checkpoint.revision.as_bytes(),
    )?];
    let assembly = executable.assemble_produced_library_coverage(
        source_instance_key,
        access_policy_digest,
        authorities,
        membership_entries,
        vec![completion],
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
            "Codex catalog coverage could not form a complete publication source",
        )
    })?;
    let projection = CatalogSourceProjection::assemble(publication_source, projection_members)?;

    Ok(CodexCatalogProduction {
        identity: CodexCatalogIdentity {
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

fn projection_owner(
    source_instance_key: crate::adapter::CanonicalSourceInstanceKey,
    object_key: &[u8],
    generation: u64,
) -> Result<CatalogEvidenceOwner, CatalogCompositionError> {
    CatalogEvidenceOwner::new(
        ADAPTER_ID,
        source_instance_key,
        crate::adapter::CoverageStreamKey::derive(ADAPTER_ID, STREAM_ID.as_bytes()).map_err(
            |_| CatalogCompositionError::invalid("Codex catalog stream identity is invalid"),
        )?,
        crate::adapter::CoverageObjectKey::derive(STREAM_ID, object_key).map_err(|_| {
            CatalogCompositionError::invalid("Codex catalog object identity is invalid")
        })?,
        generation,
    )
    .map_err(|_| CatalogCompositionError::invalid("Codex catalog evidence owner is invalid"))
}

fn require_exact_runtime_composition(
    composition: &CatalogSourceComposition,
) -> Result<(), CatalogCompositionError> {
    let mut expected_components = codex_catalog_components()
        .into_iter()
        .map(CatalogSourceComponent::normalize)
        .collect::<Result<Vec<_>, _>>()?;
    expected_components.sort_by(|left, right| left.component_id.cmp(&right.component_id));
    if composition.adapter_id() != ADAPTER_ID
        || composition.source_declaration_id() != SOURCE_DECLARATION_ID
        || composition.components() != expected_components
    {
        return Err(CatalogCompositionError::invalid(
            "Codex catalog producer requires the exact compiled source declaration and component topology",
        ));
    }
    Ok(())
}

fn exact_stream(streams: &[StreamSpec]) -> Result<StreamSpec, CatalogCompositionError> {
    streams
        .iter()
        .find(|stream| stream.id.as_str() == STREAM_ID)
        .cloned()
        .ok_or_else(|| CatalogCompositionError::invalid("Codex catalog stream is missing"))
}

fn scan_membership(
    sessions_root: &Path,
    component: &CatalogSourceComponent,
) -> Result<crate::source::DirectoryCheckpoint, CatalogCompositionError> {
    let scan = DirectorySnapshot::new(DirectorySnapshotConfig {
        max_entries: component.discovery_bounds.max_entries as usize,
        max_entries_per_directory: component.discovery_bounds.max_entries as usize,
        max_depth: component.discovery_bounds.max_depth as usize,
    })
    .map_err(map_driver_error)?
    .scan(sessions_root, None, &|relative: &Path, kind| match kind {
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
    })
    .map_err(map_driver_error)?;
    let DirectoryScan::Snapshot { checkpoint, .. } = scan else {
        return Err(CatalogCompositionError::invalid(
            "Codex catalog membership was not stably available",
        ));
    };
    if checkpoint.generation == 0
        || checkpoint
            .entries
            .values()
            .any(|entry| entry.generation == 0)
    {
        return Err(CatalogCompositionError::invalid(
            "Codex catalog membership generation must be positive",
        ));
    }
    Ok(checkpoint)
}

fn revalidate_heads(
    driver: &AppendDelimitedFile,
    sessions_root: &Path,
    reads: &[(PathBuf, AppendCheckpoint)],
) -> Result<(), CatalogCompositionError> {
    for (index, (relative_path, expected)) in reads.iter().enumerate() {
        let read = driver
            .read_confined_bounded(
                sessions_root,
                relative_path,
                None,
                &object_origin(index)?,
                false,
                PHYSICAL_READ_CEILING,
            )
            .map_err(map_driver_error)?;
        let AppendRead::Batch {
            items, checkpoint, ..
        } = read
        else {
            return Err(CatalogCompositionError::invalid(
                "Codex catalog head changed during revalidation",
            ));
        };
        if items
            .iter()
            .any(|item| matches!(item, AppendItem::Quarantined(_)))
            || checkpoint.generation != expected.generation
            || checkpoint.encode() != expected.encode()
        {
            return Err(CatalogCompositionError::invalid(
                "Codex catalog head changed during revalidation",
            ));
        }
    }
    Ok(())
}

fn coverage_point(
    object_key: &[u8],
    generation: u64,
    checkpoint: &AppendCheckpoint,
) -> Result<CatalogCompletedCoverageObject, CatalogCompositionError> {
    let position = crate::adapter::CoveragePosition::derive(
        crate::adapter::CoveragePositionKind::AppendCursor,
        &checkpoint.encode(),
        (checkpoint.committed_offset != 0).then_some(checkpoint.committed_offset),
    )
    .map_err(|_| CatalogCompositionError::invalid("Codex coverage position is invalid"))?;
    CatalogCompletedCoverageObject::point(
        crate::adapter::CoverageObjectKey::derive(STREAM_ID, object_key)
            .map_err(|_| CatalogCompositionError::invalid("Codex coverage key is invalid"))?,
        generation,
        position,
        crate::adapter::CoverageProvenance::default(),
    )
}

fn completion_position(
    revision: &[u8],
) -> Result<crate::adapter::CoveragePosition, CatalogCompositionError> {
    let mut material = Vec::new();
    material.extend_from_slice(&(COMPONENT_ID.len() as u64).to_be_bytes());
    material.extend_from_slice(COMPONENT_ID.as_bytes());
    material.extend_from_slice(&(revision.len() as u64).to_be_bytes());
    material.extend_from_slice(revision);
    crate::adapter::CoveragePosition::derive(
        crate::adapter::CoveragePositionKind::SnapshotRevision,
        &material,
        None,
    )
    .map_err(|_| CatalogCompositionError::invalid("Codex completion position is invalid"))
}

fn object_origin(index: usize) -> Result<RecordOrigin, CatalogCompositionError> {
    let object_id = u64::try_from(index)
        .ok()
        .and_then(|value| value.checked_add(3))
        .ok_or_else(|| CatalogCompositionError::invalid("Codex object index overflow"))?;
    Ok(RecordOrigin {
        source_instance_id: 1,
        stream_id: 2,
        object_id,
        observed_at: 1,
        source_timestamp_hint: None,
        media_type: SourceMediaType::new("application/x-ndjson").map_err(map_driver_error)?,
    })
}

fn map_driver_error(error: SourceDriverError) -> CatalogCompositionError {
    CatalogCompositionError::invalid(match error {
        SourceDriverError::InvalidConfig(_) => {
            "Codex catalog producer received invalid driver configuration"
        }
        SourceDriverError::InvalidCursor(_) => {
            "Codex catalog producer received an invalid driver cursor"
        }
        SourceDriverError::PathEscape(_) => {
            "Codex catalog producer rejected a path outside its declared root"
        }
        SourceDriverError::LimitExceeded(_) => {
            "Codex catalog producer exceeded a declared source bound"
        }
        SourceDriverError::Unstable(_) => {
            "Codex catalog producer observed an unstable source snapshot"
        }
        SourceDriverError::Database(_) => "Codex catalog producer cannot use a database source",
        SourceDriverError::Io { .. } => {
            "Codex catalog producer failed to read a declared source object"
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
