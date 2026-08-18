use std::collections::BTreeMap;

use serde::Serialize;

use crate::adapter::{
    AuthorizedCatalogAccess, CanonicalSourceInstanceKey, CompatibilityClass,
    ContractVersionSelection, CoverageAbsenceKind, CoverageDeclarationDigest, CoverageDomain,
    CoverageObjectKey, CoveragePosition, CoveragePositionKind, CoverageProvenance,
    CoverageSetCompleteness, CoverageStatus, Sha256Digest, CONTRACT_VERSION_SELECTION_VERSION,
};
use crate::catalog_contract::CatalogAccessPolicyDigest;

use super::*;

const MEMBER_IDENTITY_CONTRACT: &str = "catalog-session-identity-v1";
const CANDIDATE_HEAD_BYTES: u64 = 64 * 1024;
const PLANNING_EVIDENCE_ID: &str = "phase0-catalog-census-2026-08-15";

fn catalog_contract_selection() -> ContractVersionSelection {
    ContractVersionSelection {
        selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
        model_major: 1,
        external_entity_reference_version: 1,
        semantic_revision_reference_version: 1,
        coverage_contract_version: 1,
        fact_family_versions: BTreeMap::from([
            ("catalog.project".to_owned(), 1),
            ("catalog.session".to_owned(), 1),
        ]),
        query_pack_version: Some(1),
        observation_contract_version: None,
    }
}

fn catalog_access<'a>(
    adapter_id: &'a str,
    support_release_id: &'a str,
    source_declaration: &[u8],
    support_release: &[u8],
    selection: &'a ContractVersionSelection,
) -> AuthorizedCatalogAccess<'a> {
    AuthorizedCatalogAccess::fixture(
        adapter_id,
        support_release_id,
        Sha256Digest::of(support_release),
        Sha256Digest::of(source_declaration),
        selection,
    )
}

fn catalog_access_with_compatibility<'a>(
    adapter_id: &'a str,
    support_release_id: &'a str,
    source_declaration: &[u8],
    support_release: &[u8],
    selection: &'a ContractVersionSelection,
    compatibility_class: CompatibilityClass,
) -> AuthorizedCatalogAccess<'a> {
    AuthorizedCatalogAccess::fixture_with_compatibility(
        adapter_id,
        support_release_id,
        Sha256Digest::of(support_release),
        Sha256Digest::of(source_declaration),
        selection,
        compatibility_class,
    )
}

fn planned_composition(
    adapter_id: &str,
    support_release_id: &str,
    source_declaration_id: &str,
    components: Vec<CatalogSourceComponent>,
) -> Result<CatalogSourceComposition, CatalogCompositionError> {
    CatalogSourceComposition::new_planned(
        adapter_id,
        support_release_id,
        source_declaration_id,
        PLANNING_EVIDENCE_ID,
        components,
    )
}

fn discovery(max_depth: u32) -> CatalogDiscoveryBounds {
    CatalogDiscoveryBounds::new(250_000, max_depth).unwrap()
}

fn component(
    identifiers: (&str, &str, &str),
    relative_selectors: &[&str],
    primitive: CatalogSourcePrimitive,
    contribution: CatalogContribution,
    overlap_strategy: CatalogOverlapStrategy,
    boundary: CatalogDecoderStateBoundary,
    contract_axes: (&str, &[&str]),
) -> CatalogSourceComponent {
    let (component_id, stream_id, root_id) = identifiers;
    CatalogSourceComponent {
        component_id: component_id.to_owned(),
        stream_id: stream_id.to_owned(),
        root_id: root_id.to_owned(),
        relative_selectors: relative_selectors
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        discovery_bounds: discovery(64),
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

fn membership(admission_contract_id: &str, provides_metadata: bool) -> CatalogContribution {
    CatalogContribution::Membership {
        member_identity_contract_id: MEMBER_IDENTITY_CONTRACT.to_owned(),
        admission_contract_id: admission_contract_id.to_owned(),
        provides_metadata,
    }
}

fn metadata(metadata_contract_id: &str) -> CatalogContribution {
    CatalogContribution::MetadataForKnownMember {
        member_identity_contract_id: MEMBER_IDENTITY_CONTRACT.to_owned(),
        metadata_contract_id: metadata_contract_id.to_owned(),
    }
}

fn claude_components() -> Vec<CatalogSourceComponent> {
    vec![
        component(
            (
                "transcript-head-fallback",
                "session-transcripts",
                "projects",
            ),
            &["*/*.jsonl"],
            CatalogSourcePrimitive::DelimitedPrefix {
                max_record_bytes: CANDIDATE_HEAD_BYTES,
                max_window_bytes: CANDIDATE_HEAD_BYTES,
                max_records: 128,
            },
            metadata("transcript-head-metadata-v1"),
            CatalogOverlapStrategy::IdempotentOverlap,
            CatalogDecoderStateBoundary::ObjectGenerationCursor,
            (
                "claude-session-record",
                &["native-family:session-transcript"],
            ),
        ),
        component(
            (
                "nested-parent-membership",
                "nested-transcript-membership",
                "projects",
            ),
            &["*/*/subagents/**/agent-*.jsonl"],
            CatalogSourcePrimitive::DirectoryMembership,
            membership("nested-parent-session-admission-v1", false),
            CatalogOverlapStrategy::DisjointCatalogFamily {
                ownership_contract_id: "nested-parent-membership-v1".to_owned(),
            },
            CatalogDecoderStateBoundary::FullSnapshot,
            (
                "claude-nested-parent-membership-v1",
                &["native-family:nested-parent-membership"],
            ),
        ),
        component(
            ("session-index-membership", "session-indexes", "projects"),
            &["*/sessions-index.json"],
            CatalogSourcePrimitive::ReplaceDocument {
                max_object_bytes: 1024 * 1024,
            },
            membership("session-index-entry-admission-v1", true),
            CatalogOverlapStrategy::CommitCatalogFacts,
            CatalogDecoderStateBoundary::ObjectGenerationRevision,
            ("claude-session-index", &["native-family:session-index"]),
        ),
        component(
            (
                "top-level-transcript-membership",
                "top-level-transcript-membership",
                "projects",
            ),
            &["*/*.jsonl"],
            CatalogSourcePrimitive::DirectoryMembership,
            membership("top-level-transcript-admission-v1", false),
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

fn claude_composition() -> CatalogSourceComposition {
    planned_composition(
        "claude-code",
        "claude-code.catalog-candidate-2026-08-15",
        "claude-code.catalog-sources-v1",
        claude_components(),
    )
    .unwrap()
}

fn codex_composition() -> CatalogSourceComposition {
    planned_composition(
        "codex",
        "codex.catalog-candidate-2026-08-15",
        "codex.catalog-sources-v1",
        vec![component(
            ("rollout-session-meta-head", "rollout-sessions", "sessions"),
            &["**/rollout-*.jsonl"],
            CatalogSourcePrimitive::DelimitedHead {
                max_record_bytes: CANDIDATE_HEAD_BYTES,
            },
            membership("noninternal-session-meta-admission-v1", true),
            CatalogOverlapStrategy::IdempotentOverlap,
            CatalogDecoderStateBoundary::ObjectGenerationCursor,
            (
                "codex-rollout-record",
                &["native-family:rollout-transcript"],
            ),
        )],
    )
    .unwrap()
}

fn grok_composition() -> CatalogSourceComposition {
    planned_composition(
        "grok",
        "grok.catalog-candidate-2026-08-15",
        "grok.catalog-sources-v1",
        vec![
            component(
                (
                    "session-directory-membership",
                    "session-membership",
                    "sessions",
                ),
                &["*/*"],
                CatalogSourcePrimitive::DirectoryMembership,
                membership("session-directory-admission-v1", false),
                CatalogOverlapStrategy::CommitCatalogFacts,
                CatalogDecoderStateBoundary::FullSnapshot,
                (
                    "grok-session-membership",
                    &["native-family:session-membership"],
                ),
            ),
            component(
                ("session-summary-metadata", "session-summaries", "sessions"),
                &["*/*/summary.json"],
                CatalogSourcePrimitive::ReplaceDocument {
                    max_object_bytes: 1024 * 1024,
                },
                metadata("replaceable-session-summary-v1"),
                CatalogOverlapStrategy::IdempotentOverlap,
                CatalogDecoderStateBoundary::ObjectGenerationRevision,
                ("grok-summary", &["native-family:session-summary"]),
            ),
        ],
    )
    .unwrap()
}

fn member(label: &str) -> CatalogMemberRef {
    CatalogMemberRef::fixture_from_semantic_identity(MEMBER_IDENTITY_CONTRACT, label.as_bytes())
        .unwrap()
}

fn complete_authorities(
    composition: &CatalogSourceComposition,
) -> Vec<CatalogMembershipAuthorityEvidence> {
    composition
        .components
        .iter()
        .filter(|component| component.contribution.can_admit_member())
        .map(|component| {
            CatalogMembershipAuthorityEvidence::fixture(
                &component.component_id,
                1,
                CatalogMembershipAuthorityCompleteness::Complete,
            )
        })
        .collect()
}

fn claude_members() -> Vec<CatalogMembershipEntry> {
    vec![
        CatalogMembershipEntry::new(
            member("fixture-semantic-session-charlie"),
            vec!["nested-parent-membership".to_owned()],
            vec![],
        )
        .unwrap(),
        CatalogMembershipEntry::new(
            member("fixture-semantic-session-alpha"),
            vec![
                "top-level-transcript-membership".to_owned(),
                "session-index-membership".to_owned(),
            ],
            vec![
                "transcript-head-fallback".to_owned(),
                "session-index-membership".to_owned(),
            ],
        )
        .unwrap(),
        CatalogMembershipEntry::new(
            member("fixture-semantic-session-bravo"),
            vec!["session-index-membership".to_owned()],
            vec!["session-index-membership".to_owned()],
        )
        .unwrap(),
    ]
}

fn claude_membership(composition: &CatalogSourceComposition) -> CatalogMembershipSnapshot {
    CatalogMembershipSnapshot::new(
        composition,
        complete_authorities(composition),
        claude_members(),
    )
    .unwrap()
}

fn codex_membership(composition: &CatalogSourceComposition) -> CatalogMembershipSnapshot {
    CatalogMembershipSnapshot::new(
        composition,
        complete_authorities(composition),
        vec![CatalogMembershipEntry::new(
            member("fixture-semantic-session-delta"),
            vec!["rollout-session-meta-head".to_owned()],
            vec!["rollout-session-meta-head".to_owned()],
        )
        .unwrap()],
    )
    .unwrap()
}

fn grok_membership(composition: &CatalogSourceComposition) -> CatalogMembershipSnapshot {
    CatalogMembershipSnapshot::new(
        composition,
        complete_authorities(composition),
        vec![CatalogMembershipEntry::new(
            member("fixture-semantic-session-echo"),
            vec!["session-directory-membership".to_owned()],
            vec!["session-summary-metadata".to_owned()],
        )
        .unwrap()],
    )
    .unwrap()
}

fn catalog_coverage_source_key(label: &[u8]) -> CanonicalSourceInstanceKey {
    CanonicalSourceInstanceKey::derive(1, label).unwrap()
}

fn catalog_coverage_policy(label: &[u8]) -> CatalogAccessPolicyDigest {
    CatalogAccessPolicyDigest::derive(1, label).unwrap()
}

fn component_completion_position(component_id: &str) -> CoveragePosition {
    CoveragePosition::derive(
        CoveragePositionKind::SnapshotRevision,
        format!("{component_id}/opaque-completion-revision").as_bytes(),
        None,
    )
    .unwrap()
}

fn complete_component_coverage(
    executable: &CatalogExecutableComposition<'_, '_>,
    source_instance_key: CanonicalSourceInstanceKey,
    access_policy_digest: CatalogAccessPolicyDigest,
) -> Vec<CatalogComponentCoverageCompletion> {
    executable
        .composition
        .components
        .iter()
        .enumerate()
        .map(|(index, component)| {
            let position_kind = component.primitive.complete_coverage_semantics().0;
            let position = CoveragePosition::derive(
                position_kind,
                format!("{}/opaque-position", component.component_id).as_bytes(),
                (position_kind == CoveragePositionKind::AppendCursor).then_some((index + 1) as u64),
            )
            .unwrap();
            let object = CatalogCompletedCoverageObject::point(
                CoverageObjectKey::derive(
                    &component.stream_id,
                    format!("{}/opaque-object", component.component_id).as_bytes(),
                )
                .unwrap(),
                (index + 1) as u64,
                position,
                CoverageProvenance::default(),
            )
            .unwrap();
            CatalogComponentCoverageCompletion::new(
                executable,
                source_instance_key,
                access_policy_digest,
                &component.component_id,
                component_completion_position(&component.component_id),
                vec![object],
            )
            .unwrap()
        })
        .collect()
}

fn coverage_bound_membership(
    composition: &CatalogSourceComposition,
    members: Vec<CatalogMembershipEntry>,
    completions: &[CatalogComponentCoverageCompletion],
) -> CatalogMembershipSnapshot {
    let mut authorities = complete_authorities(composition);
    for authority in &mut authorities {
        let completion = completions
            .iter()
            .find(|completion| completion.component_id == authority.component_id)
            .unwrap();
        authority.coverage_proof =
            calculate_component_coverage_proof(composition, &members, completion);
    }
    CatalogMembershipSnapshot::new(composition, authorities, members).unwrap()
}

fn digest(label: &str) -> [u8; DIGEST_BYTES] {
    *blake3::hash(label.as_bytes()).as_bytes()
}

fn witnesses(label: &str, record_count: usize) -> Vec<CatalogDecodeWitness> {
    (0..record_count)
        .map(|index| {
            CatalogDecodeWitness::from_digests(
                digest(&format!("{label}/record/{index}")),
                digest(&format!("{label}/disposition/{index}")),
                digest(&format!("{label}/facts/{index}")),
                digest(&format!("{label}/payload/{index}")),
                digest(&format!("{label}/provenance/{index}")),
                digest(&format!("{label}/state/{}", index + 1)),
            )
        })
        .collect()
}

fn trace_summary(
    composition: &CatalogSourceComposition,
    component_id: &str,
    records: &[CatalogDecodeWitness],
) -> CatalogDecodeTraceSummary {
    let mut trace = CatalogDecodeTraceAccumulator::new(composition, component_id).unwrap();
    for record in records {
        trace.push(*record).unwrap();
    }
    let final_state = records.last().map_or_else(
        || digest("empty-decoder-state"),
        |record| record.decoder_state_after,
    );
    trace.finish(final_state)
}

#[derive(Debug, Serialize)]
pub(super) struct WindowConformanceFixture {
    complete_record_bytes: Vec<u64>,
    windows: Vec<CatalogRecordWindowStep>,
    full_only: CatalogDecodeTraceSummary,
    head_or_prefix_plus_continuation: CatalogDecodeTraceSummary,
}

fn window_conformance(
    composition: &CatalogSourceComposition,
    component_id: &str,
    complete_record_bytes: Vec<u64>,
) -> WindowConformanceFixture {
    let records = witnesses(component_id, complete_record_bytes.len());
    let full_only = trace_summary(composition, component_id, &records);
    let mut tiered = CatalogDecodeTraceAccumulator::new(composition, component_id).unwrap();
    let mut planner = CatalogRecordWindowPlanner::new(composition, component_id).unwrap();
    let mut windows = Vec::new();
    let mut step = planner
        .plan_initial(&complete_record_bytes, 0)
        .expect("fixture record layout is bounded");
    loop {
        let window = step.window;
        for record in &records[window.start_record as usize..window.end_record() as usize] {
            tiered.push(*record).unwrap();
        }
        let continuation = step.continuation;
        windows.push(step);
        match (window.remainder, continuation) {
            (CatalogRecordWindowRemainder::ContinueAt { .. }, Some(token)) => {
                step = planner
                    .plan_continuation(token, &complete_record_bytes, 0)
                    .expect("fixture continuation is exact");
            }
            (CatalogRecordWindowRemainder::AtSnapshotBoundary { .. }, None) => break,
            (
                CatalogRecordWindowRemainder::AwaitingCompleteRecord { .. }
                | CatalogRecordWindowRemainder::OversizedRecord { .. },
                _,
            ) => {
                panic!("frozen complete fixture cannot be partial or oversized")
            }
            _ => panic!("window continuation shape is inconsistent with its remainder"),
        }
    }
    let final_state = records.last().unwrap().decoder_state_after;
    let head_or_prefix_plus_continuation = tiered.finish(final_state);
    assert_eq!(head_or_prefix_plus_continuation, full_only);
    WindowConformanceFixture {
        complete_record_bytes,
        windows,
        full_only,
        head_or_prefix_plus_continuation,
    }
}

#[derive(Debug, Serialize)]
pub(super) struct FrozenCatalogCompositionFixture {
    fixture_contract_version: u32,
    contract_status: &'static str,
    release_gate: &'static str,
    candidate_head_record_bytes: u64,
    compositions: BTreeMap<&'static str, CatalogSourceComposition>,
    membership_snapshots: BTreeMap<&'static str, CatalogMembershipSnapshot>,
    overlap_conformance: BTreeMap<&'static str, WindowConformanceFixture>,
}

pub(super) fn frozen_fixture() -> FrozenCatalogCompositionFixture {
    let claude = claude_composition();
    let codex = codex_composition();
    let grok = grok_composition();
    let memberships = BTreeMap::from([
        ("claude_code", claude_membership(&claude)),
        ("codex", codex_membership(&codex)),
        ("grok", grok_membership(&grok)),
    ]);
    let conformance = BTreeMap::from([
        (
            "claude_code_transcript_prefix",
            window_conformance(
                &claude,
                "transcript-head-fallback",
                vec![32 * 1024, 30 * 1024, 20 * 1024],
            ),
        ),
        (
            "codex_session_meta_head",
            window_conformance(
                &codex,
                "rollout-session-meta-head",
                vec![48 * 1024, 10 * 1024, 8 * 1024],
            ),
        ),
    ]);
    FrozenCatalogCompositionFixture {
        fixture_contract_version: 1,
        contract_status: "planned_adapter_neutral",
        release_gate: "identity_oracle_and_promoted_declarations_pending",
        candidate_head_record_bytes: CANDIDATE_HEAD_BYTES,
        compositions: BTreeMap::from([("claude_code", claude), ("codex", codex), ("grok", grok)]),
        membership_snapshots: memberships,
        overlap_conformance: conformance,
    }
}

#[test]
fn composition_identity_is_canonical_and_binds_contract_content() {
    let canonical = claude_composition();
    let mut reordered = claude_components();
    reordered.reverse();
    let reordered = planned_composition(
        "claude-code",
        "claude-code.catalog-candidate-2026-08-15",
        "claude-code.catalog-sources-v1",
        reordered,
    )
    .unwrap();
    assert_eq!(reordered, canonical);

    let mut changed = claude_components();
    let CatalogSourcePrimitive::DelimitedPrefix { max_records, .. } = &mut changed[0].primitive
    else {
        panic!("fixture component must be the prefix view")
    };
    *max_records -= 1;
    let changed = planned_composition(
        "claude-code",
        "claude-code.catalog-candidate-2026-08-15",
        "claude-code.catalog-sources-v1",
        changed,
    )
    .unwrap();
    assert_ne!(changed.composition_id, canonical.composition_id);

    let mut identity_drift = claude_components();
    identity_drift[0].contribution = CatalogContribution::MetadataForKnownMember {
        member_identity_contract_id: "catalog-session-identity-v2".to_owned(),
        metadata_contract_id: "transcript-head-metadata-v1".to_owned(),
    };
    assert!(planned_composition(
        "claude-code",
        "claude-code.catalog-candidate-2026-08-15",
        "claude-code.catalog-sources-v1",
        identity_drift,
    )
    .is_err());
}

#[test]
fn planned_compositions_bind_existing_decoder_and_disposition_axes_without_promotion() {
    fn assert_axes(
        composition: &CatalogSourceComposition,
        component_id: &str,
        declaration_json: &str,
        stream_id: &str,
    ) {
        let declaration: serde_json::Value = serde_json::from_str(declaration_json).unwrap();
        let stream = declaration["streams"]
            .as_array()
            .unwrap()
            .iter()
            .find(|stream| stream["stream_id"] == stream_id)
            .unwrap();
        let component = composition.component(component_id).unwrap();
        assert_eq!(component.stream_id, stream["stream_id"].as_str().unwrap());
        assert_eq!(
            component.decoder_contract_id,
            stream["decoder_id"].as_str().unwrap()
        );
        assert_eq!(
            serde_json::to_value(&component.disposition_ownership).unwrap(),
            stream["disposition_ownership"]
        );
        assert_eq!(stream["topologies"], serde_json::json!(["durable"]));
        assert_eq!(stream["overlap_strategy"], "full_only");
        assert!(matches!(
            composition.binding,
            CatalogCompositionBinding::PlannedUnbound { .. }
        ));
    }

    let claude = claude_composition();
    let claude_declaration = include_str!(
        "../../../../../agent-support/claude-code/candidate-2026-08-15/source-declarations.json"
    );
    assert_axes(
        &claude,
        "transcript-head-fallback",
        claude_declaration,
        "session-transcripts",
    );
    assert_axes(
        &claude,
        "session-index-membership",
        claude_declaration,
        "session-indexes",
    );
    assert_axes(
        &codex_composition(),
        "rollout-session-meta-head",
        include_str!(
            "../../../../../agent-support/codex/candidate-2026-08-15/source-declarations.json"
        ),
        "rollout-sessions",
    );
    let grok = grok_composition();
    let grok_declaration = include_str!(
        "../../../../../agent-support/grok/candidate-2026-08-15/source-declarations.json"
    );
    assert_axes(
        &grok,
        "session-directory-membership",
        grok_declaration,
        "session-membership",
    );
    assert_axes(
        &grok,
        "session-summary-metadata",
        grok_declaration,
        "session-summaries",
    );
}

#[test]
fn only_an_exact_rust_authorization_can_execute_a_promoted_composition() {
    const ADAPTER_ID: &str = "fixture-agent";
    const SUPPORT_RELEASE_ID: &str = "fixture-catalog-support-v1";
    const SOURCE_DECLARATION_ID: &str = "fixture-catalog-sources-v1";
    const SOURCE_DECLARATION: &[u8] = b"fixture/catalog-source-declaration/v1";
    const SUPPORT_RELEASE: &[u8] = b"fixture/catalog-support-release/v1";

    let planned = planned_composition(
        ADAPTER_ID,
        SUPPORT_RELEASE_ID,
        SOURCE_DECLARATION_ID,
        claude_components(),
    )
    .unwrap();
    let selection = catalog_contract_selection();
    let access = catalog_access(
        ADAPTER_ID,
        SUPPORT_RELEASE_ID,
        SOURCE_DECLARATION,
        SUPPORT_RELEASE,
        &selection,
    );
    assert!(planned.authorize_execution(access).is_err());

    let promoted_binding = CatalogPromotedBinding::fixture(SOURCE_DECLARATION, SUPPORT_RELEASE);

    let promoted = CatalogSourceComposition::new_promoted(
        ADAPTER_ID,
        SUPPORT_RELEASE_ID,
        SOURCE_DECLARATION_ID,
        promoted_binding,
        claude_components(),
    )
    .unwrap();
    assert_ne!(promoted.composition_id, planned.composition_id);
    let executable = promoted
        .authorize_execution(catalog_access(
            ADAPTER_ID,
            SUPPORT_RELEASE_ID,
            SOURCE_DECLARATION,
            SUPPORT_RELEASE,
            &selection,
        ))
        .unwrap();
    assert_eq!(executable.composition_id(), promoted.composition_id);
    assert_eq!(executable.contract_selection(), &selection);

    let drifted_adapter = catalog_access(
        "other-agent",
        SUPPORT_RELEASE_ID,
        SOURCE_DECLARATION,
        SUPPORT_RELEASE,
        &selection,
    );
    assert!(promoted.authorize_execution(drifted_adapter).is_err());

    let drifted_release = catalog_access(
        ADAPTER_ID,
        "claude-code.other-support-v1",
        SOURCE_DECLARATION,
        SUPPORT_RELEASE,
        &selection,
    );
    assert!(promoted.authorize_execution(drifted_release).is_err());

    let drifted_declaration = catalog_access(
        ADAPTER_ID,
        SUPPORT_RELEASE_ID,
        b"other/source-declaration",
        SUPPORT_RELEASE,
        &selection,
    );
    assert!(promoted.authorize_execution(drifted_declaration).is_err());

    let drifted_release_digest = catalog_access(
        ADAPTER_ID,
        SUPPORT_RELEASE_ID,
        SOURCE_DECLARATION,
        b"other/support-release",
        &selection,
    );
    assert!(promoted
        .authorize_execution(drifted_release_digest)
        .is_err());

    assert!(CatalogPromotedBinding::from_digests([0; DIGEST_BYTES], [1; DIGEST_BYTES]).is_err());
}

#[test]
fn executable_composition_assembles_canonical_complete_library_coverage() {
    const ADAPTER_ID: &str = "fixture-agent";
    const SUPPORT_RELEASE_ID: &str = "fixture-catalog-support-v1";
    const SOURCE_DECLARATION_ID: &str = "fixture-catalog-sources-v1";
    const SOURCE_DECLARATION: &[u8] = b"fixture/catalog-source-declaration/v1";
    const SUPPORT_RELEASE: &[u8] = b"fixture/catalog-support-release/v1";

    let selection = catalog_contract_selection();
    let composition = CatalogSourceComposition::new_promoted(
        ADAPTER_ID,
        SUPPORT_RELEASE_ID,
        SOURCE_DECLARATION_ID,
        CatalogPromotedBinding::fixture(SOURCE_DECLARATION, SUPPORT_RELEASE),
        claude_components(),
    )
    .unwrap();
    let executable = composition
        .authorize_execution(catalog_access(
            ADAPTER_ID,
            SUPPORT_RELEASE_ID,
            SOURCE_DECLARATION,
            SUPPORT_RELEASE,
            &selection,
        ))
        .unwrap();
    let source_instance_key = catalog_coverage_source_key(b"fixture-device/catalog-root");
    let policy = catalog_coverage_policy(b"fixture-local-catalog-policy");
    let completions = complete_component_coverage(&executable, source_instance_key, policy);
    let membership = coverage_bound_membership(&composition, claude_members(), &completions);

    let ordered_assembly = executable
        .assemble_library_coverage(
            source_instance_key,
            policy,
            &membership,
            completions.clone(),
        )
        .unwrap();
    let mut reversed = completions.clone();
    reversed.reverse();
    let assembly = executable
        .assemble_library_coverage(source_instance_key, policy, &membership, reversed)
        .unwrap();
    assert_eq!(
        ordered_assembly.component_completion_revision(),
        assembly.component_completion_revision(),
        "canonical component completion identity must ignore caller input order"
    );
    assert_eq!(ordered_assembly, assembly);
    assert_eq!(
        assembly.catalog_membership_revision(),
        membership.membership_revision
    );
    assert_ne!(
        assembly.catalog_membership_revision().as_bytes(),
        assembly.source_coverage().membership_revision.as_bytes(),
        "composition-conformance membership must never substitute for RFC 012A source membership"
    );
    assert_eq!(assembly.plan_source().adapter_id, ADAPTER_ID);
    assert_eq!(
        assembly.plan_source().support_release_id,
        SUPPORT_RELEASE_ID
    );
    assert_eq!(
        assembly.plan_source().catalog_declaration_digest,
        CoverageDeclarationDigest::derive(Sha256Digest::of(SOURCE_DECLARATION).as_bytes()).unwrap()
    );
    assert_eq!(assembly.plan_source().access_policy_digest, policy);
    assert_eq!(assembly.contract_selection(), &selection);
    assert!(assembly
        .plan_source()
        .matches_coverage(assembly.source_coverage()));
    assert_eq!(
        assembly.source_coverage().coverage_domain,
        CoverageDomain::ProjectionPack {
            pack: "library.catalog".to_owned(),
            version: 1,
        }
    );
    assert_eq!(assembly.source_coverage().scope.root_entity_key, None);
    assert_eq!(
        assembly.source_coverage().completeness,
        CoverageSetCompleteness::Complete
    );
    assert_eq!(assembly.source_coverage().points.len(), 4);
    assert!(assembly
        .source_coverage()
        .explicit_absence_or_deletion
        .is_empty());
    assert!(assembly.source_coverage().explicit_errors.is_empty());
    assert_eq!(
        assembly
            .source_coverage()
            .points
            .iter()
            .filter(|point| matches!(&point.status, CoverageStatus::ExactSnapshot))
            .count(),
        3
    );
    assert_eq!(
        assembly
            .source_coverage()
            .points
            .iter()
            .filter(|point| matches!(&point.status, CoverageStatus::CompleteThrough))
            .count(),
        1
    );
    let debug = format!("{completions:?}");
    assert!(!debug.contains("opaque-object"));
    assert!(!debug.contains("opaque-position"));
    assert!(!debug.contains("opaque-completion-revision"));

    let range_executable = composition
        .authorize_execution(catalog_access_with_compatibility(
            ADAPTER_ID,
            SUPPORT_RELEASE_ID,
            SOURCE_DECLARATION,
            SUPPORT_RELEASE,
            &selection,
            CompatibilityClass::RangeSupported,
        ))
        .unwrap();
    let range_assembly = range_executable
        .assemble_library_coverage(source_instance_key, policy, &membership, completions)
        .unwrap();
    assert_eq!(assembly, range_assembly);
}

#[test]
fn coverage_assembly_rejects_authority_selection_and_binding_drift() {
    const ADAPTER_ID: &str = "fixture-agent";
    const SUPPORT_RELEASE_ID: &str = "fixture-catalog-support-v1";
    const SOURCE_DECLARATION_ID: &str = "fixture-catalog-sources-v1";
    const SOURCE_DECLARATION: &[u8] = b"fixture/catalog-source-declaration/v1";
    const SUPPORT_RELEASE: &[u8] = b"fixture/catalog-support-release/v1";

    let selection = catalog_contract_selection();
    let composition = CatalogSourceComposition::new_promoted(
        ADAPTER_ID,
        SUPPORT_RELEASE_ID,
        SOURCE_DECLARATION_ID,
        CatalogPromotedBinding::fixture(SOURCE_DECLARATION, SUPPORT_RELEASE),
        claude_components(),
    )
    .unwrap();
    let executable = composition
        .authorize_execution(catalog_access(
            ADAPTER_ID,
            SUPPORT_RELEASE_ID,
            SOURCE_DECLARATION,
            SUPPORT_RELEASE,
            &selection,
        ))
        .unwrap();
    let source_instance_key = catalog_coverage_source_key(b"fixture-device/catalog-root");
    let policy = catalog_coverage_policy(b"fixture-local-catalog-policy");
    let completions = complete_component_coverage(&executable, source_instance_key, policy);
    let membership = coverage_bound_membership(&composition, claude_members(), &completions);

    assert!(executable
        .assemble_library_coverage(
            catalog_coverage_source_key(b"fixture-device/other-root"),
            policy,
            &membership,
            completions.clone(),
        )
        .is_err());
    assert!(executable
        .assemble_library_coverage(
            source_instance_key,
            catalog_coverage_policy(b"other-policy"),
            &membership,
            completions.clone(),
        )
        .is_err());

    let forward_access = catalog_access_with_compatibility(
        ADAPTER_ID,
        SUPPORT_RELEASE_ID,
        SOURCE_DECLARATION,
        SUPPORT_RELEASE,
        &selection,
        CompatibilityClass::RecognizedUnverified,
    );
    assert!(!format!("{forward_access:?}").contains("RecognizedUnverified"));
    let forward = composition.authorize_execution(forward_access).unwrap();
    assert!(forward
        .assemble_library_coverage(
            source_instance_key,
            policy,
            &membership,
            completions.clone(),
        )
        .is_err());
    assert!(CatalogComponentCoverageCompletion::new(
        &forward,
        source_instance_key,
        policy,
        "transcript-head-fallback",
        component_completion_position("transcript-head-fallback"),
        Vec::new(),
    )
    .is_err());

    let mut family_drift = selection.clone();
    family_drift
        .fact_family_versions
        .insert("catalog.session".to_owned(), 2);
    for drifted_selection in [
        ContractVersionSelection {
            model_major: selection.model_major + 1,
            ..selection.clone()
        },
        family_drift,
    ] {
        let drifted = composition
            .authorize_execution(catalog_access(
                ADAPTER_ID,
                SUPPORT_RELEASE_ID,
                SOURCE_DECLARATION,
                SUPPORT_RELEASE,
                &drifted_selection,
            ))
            .unwrap();
        assert!(drifted
            .assemble_library_coverage(
                source_instance_key,
                policy,
                &membership,
                completions.clone(),
            )
            .is_err());
    }

    for drifted_selection in [
        ContractVersionSelection {
            coverage_contract_version: 2,
            ..selection.clone()
        },
        ContractVersionSelection {
            query_pack_version: Some(2),
            ..selection.clone()
        },
        ContractVersionSelection {
            query_pack_version: None,
            ..selection.clone()
        },
    ] {
        let drifted = composition
            .authorize_execution(catalog_access(
                ADAPTER_ID,
                SUPPORT_RELEASE_ID,
                SOURCE_DECLARATION,
                SUPPORT_RELEASE,
                &drifted_selection,
            ))
            .unwrap();
        assert!(CatalogComponentCoverageCompletion::new(
            &drifted,
            source_instance_key,
            policy,
            "transcript-head-fallback",
            component_completion_position("transcript-head-fallback"),
            Vec::new(),
        )
        .is_err());
    }

    let foreign_composition = claude_composition();
    let foreign_membership = claude_membership(&foreign_composition);
    assert!(executable
        .assemble_library_coverage(
            source_instance_key,
            policy,
            &foreign_membership,
            completions,
        )
        .is_err());
}

#[test]
fn coverage_assembly_requires_exact_complete_component_evidence() {
    const ADAPTER_ID: &str = "fixture-agent";
    const SUPPORT_RELEASE_ID: &str = "fixture-catalog-support-v1";
    const SOURCE_DECLARATION: &[u8] = b"fixture/catalog-source-declaration/v1";
    const SUPPORT_RELEASE: &[u8] = b"fixture/catalog-support-release/v1";

    let selection = catalog_contract_selection();
    let composition = CatalogSourceComposition::new_promoted(
        ADAPTER_ID,
        SUPPORT_RELEASE_ID,
        "fixture-catalog-sources-v1",
        CatalogPromotedBinding::fixture(SOURCE_DECLARATION, SUPPORT_RELEASE),
        claude_components(),
    )
    .unwrap();
    let executable = composition
        .authorize_execution(catalog_access(
            ADAPTER_ID,
            SUPPORT_RELEASE_ID,
            SOURCE_DECLARATION,
            SUPPORT_RELEASE,
            &selection,
        ))
        .unwrap();
    let source_instance_key = catalog_coverage_source_key(b"fixture-device/catalog-root");
    let policy = catalog_coverage_policy(b"fixture-local-catalog-policy");
    let completions = complete_component_coverage(&executable, source_instance_key, policy);
    let membership = coverage_bound_membership(&composition, claude_members(), &completions);

    let mut missing_metadata = completions.clone();
    missing_metadata.retain(|completion| completion.component_id != "transcript-head-fallback");
    assert!(executable
        .assemble_library_coverage(source_instance_key, policy, &membership, missing_metadata,)
        .is_err());

    let mut duplicate = completions.clone();
    duplicate.push(completions[0].clone());
    assert!(executable
        .assemble_library_coverage(source_instance_key, policy, &membership, duplicate)
        .is_err());

    let mut unknown = completions.clone();
    unknown[0].component_id = "unknown-component".to_owned();
    assert!(executable
        .assemble_library_coverage(source_instance_key, policy, &membership, unknown)
        .is_err());

    let admitting_index = composition
        .components
        .iter()
        .position(|component| component.contribution.can_admit_member())
        .unwrap();
    let admitting_component = &composition.components[admitting_index];
    let drifted_object = CatalogCompletedCoverageObject::point(
        CoverageObjectKey::derive(&admitting_component.stream_id, b"drifted-object").unwrap(),
        1,
        CoveragePosition::derive(
            admitting_component
                .primitive
                .complete_coverage_semantics()
                .0,
            b"drifted-position",
            None,
        )
        .unwrap(),
        CoverageProvenance::default(),
    )
    .unwrap();
    let mut proof_drift = completions.clone();
    proof_drift[admitting_index] = CatalogComponentCoverageCompletion::new(
        &executable,
        source_instance_key,
        policy,
        &admitting_component.component_id,
        component_completion_position(&admitting_component.component_id),
        vec![drifted_object],
    )
    .unwrap();
    assert!(executable
        .assemble_library_coverage(source_instance_key, policy, &membership, proof_drift,)
        .is_err());

    let first_component = &composition.components[0];
    let duplicate_object = CatalogCompletedCoverageObject::point(
        CoverageObjectKey::derive(&first_component.stream_id, b"duplicate-object").unwrap(),
        1,
        CoveragePosition::derive(
            first_component.primitive.complete_coverage_semantics().0,
            b"duplicate-position",
            None,
        )
        .unwrap(),
        CoverageProvenance::default(),
    )
    .unwrap();
    assert!(CatalogComponentCoverageCompletion::new(
        &executable,
        source_instance_key,
        policy,
        &first_component.component_id,
        component_completion_position(&first_component.component_id),
        vec![duplicate_object.clone(), duplicate_object],
    )
    .is_err());

    let lineage_key =
        CoverageObjectKey::derive(&first_component.stream_id, b"lineage-object").unwrap();
    let point_at = |generation| {
        CatalogCompletedCoverageObject::point(
            lineage_key,
            generation,
            CoveragePosition::derive(
                first_component.primitive.complete_coverage_semantics().0,
                format!("lineage-position-{generation}").as_bytes(),
                None,
            )
            .unwrap(),
            CoverageProvenance::default(),
        )
        .unwrap()
    };
    assert!(CatalogComponentCoverageCompletion::new(
        &executable,
        source_instance_key,
        policy,
        &first_component.component_id,
        component_completion_position(&first_component.component_id),
        vec![point_at(1), point_at(2)],
    )
    .is_err());
    let old_absence = CatalogCompletedCoverageObject::explicit_absence(
        lineage_key,
        1,
        CoverageAbsenceKind::Deleted,
    )
    .unwrap();
    assert!(CatalogComponentCoverageCompletion::new(
        &executable,
        source_instance_key,
        policy,
        &first_component.component_id,
        component_completion_position(&first_component.component_id),
        vec![old_absence, point_at(2)],
    )
    .is_ok());
    let future_absence = CatalogCompletedCoverageObject::explicit_absence(
        lineage_key,
        3,
        CoverageAbsenceKind::Deleted,
    )
    .unwrap();
    assert!(CatalogComponentCoverageCompletion::new(
        &executable,
        source_instance_key,
        policy,
        &first_component.component_id,
        component_completion_position(&first_component.component_id),
        vec![point_at(2), future_absence],
    )
    .is_err());
    assert!(CatalogCompletedCoverageObject::point(
        CoverageObjectKey::derive("fixture", b"zero-generation").unwrap(),
        0,
        CoveragePosition::derive(CoveragePositionKind::AppendCursor, b"cursor", Some(1)).unwrap(),
        CoverageProvenance::default(),
    )
    .is_err());
    assert!(CatalogCompletedCoverageObject::explicit_absence(
        CoverageObjectKey::derive("fixture", b"oversized-generation").unwrap(),
        MAX_PORTABLE_GENERATION + 1,
        CoverageAbsenceKind::Absent,
    )
    .is_err());

    let directory_component = composition
        .components
        .iter()
        .find(|component| {
            matches!(
                component.primitive,
                CatalogSourcePrimitive::DirectoryMembership
            )
        })
        .unwrap();
    let wrong_position = CatalogCompletedCoverageObject::point(
        CoverageObjectKey::derive(&directory_component.stream_id, b"wrong-position").unwrap(),
        1,
        CoveragePosition::derive(CoveragePositionKind::AppendCursor, b"cursor", Some(1)).unwrap(),
        CoverageProvenance::default(),
    )
    .unwrap();
    assert!(CatalogComponentCoverageCompletion::new(
        &executable,
        source_instance_key,
        policy,
        &directory_component.component_id,
        component_completion_position(&directory_component.component_id),
        vec![wrong_position],
    )
    .is_err());

    assert!(CatalogComponentCoverageCompletion::new(
        &executable,
        source_instance_key,
        policy,
        "transcript-head-fallback",
        CoveragePosition::derive(
            CoveragePositionKind::AppendCursor,
            b"not-a-component-snapshot",
            Some(1),
        )
        .unwrap(),
        Vec::new(),
    )
    .is_err());

    let mut empty_metadata = completions.clone();
    let metadata_index = composition
        .components
        .iter()
        .position(|component| component.component_id == "transcript-head-fallback")
        .unwrap();
    empty_metadata[metadata_index] = CatalogComponentCoverageCompletion::new(
        &executable,
        source_instance_key,
        policy,
        "transcript-head-fallback",
        component_completion_position("transcript-head-fallback"),
        Vec::new(),
    )
    .unwrap();
    let mut members_without_head_metadata = claude_members();
    for member in &mut members_without_head_metadata {
        member
            .metadata_component_ids
            .retain(|component_id| component_id != "transcript-head-fallback");
    }
    let empty_metadata_membership =
        coverage_bound_membership(&composition, members_without_head_metadata, &empty_metadata);
    let empty_metadata_assembly = executable
        .assemble_library_coverage(
            source_instance_key,
            policy,
            &empty_metadata_membership,
            empty_metadata.clone(),
        )
        .unwrap();
    assert_eq!(empty_metadata_assembly.source_coverage().points.len(), 3);
    assert_eq!(
        empty_metadata_assembly.source_coverage().completeness,
        CoverageSetCompleteness::Complete
    );
    let mut drifted_empty_metadata = empty_metadata;
    drifted_empty_metadata[metadata_index] = CatalogComponentCoverageCompletion::new(
        &executable,
        source_instance_key,
        policy,
        "transcript-head-fallback",
        CoveragePosition::derive(
            CoveragePositionKind::SnapshotRevision,
            b"transcript-head-fallback/other-completion-revision",
            None,
        )
        .unwrap(),
        Vec::new(),
    )
    .unwrap();
    let drifted_empty_metadata_assembly = executable
        .assemble_library_coverage(
            source_instance_key,
            policy,
            &empty_metadata_membership,
            drifted_empty_metadata,
        )
        .unwrap();
    assert_eq!(
        empty_metadata_assembly.source_coverage(),
        drifted_empty_metadata_assembly.source_coverage(),
        "metadata completion revisions must not overload RFC 012A object membership"
    );
    assert_ne!(
        empty_metadata_assembly.component_completion_revision(),
        drifted_empty_metadata_assembly.component_completion_revision(),
        "metadata-only enumeration revision drift must change restart identity"
    );

    let mut with_absence = completions;
    with_absence[metadata_index] = CatalogComponentCoverageCompletion::new(
        &executable,
        source_instance_key,
        policy,
        "transcript-head-fallback",
        component_completion_position("transcript-head-fallback"),
        vec![CatalogCompletedCoverageObject::explicit_absence(
            CoverageObjectKey::derive("session-transcripts", b"missing-head").unwrap(),
            1,
            CoverageAbsenceKind::Absent,
        )
        .unwrap()],
    )
    .unwrap();
    let absence_membership =
        coverage_bound_membership(&composition, claude_members(), &with_absence);
    let with_absence = executable
        .assemble_library_coverage(
            source_instance_key,
            policy,
            &absence_membership,
            with_absence,
        )
        .unwrap();
    assert_eq!(
        with_absence
            .source_coverage()
            .explicit_absence_or_deletion
            .len(),
        1
    );
    assert_eq!(
        with_absence.source_coverage().completeness,
        CoverageSetCompleteness::Complete
    );
}

#[test]
fn membership_requires_admission_authority_and_metadata_never_fabricates_it() {
    let composition = claude_composition();
    let metadata_only = planned_composition(
        "fixture-agent",
        "fixture-support-v1",
        "fixture-declaration-v1",
        vec![component(
            ("metadata-only", "metadata", "root"),
            &["*.jsonl"],
            CatalogSourcePrimitive::DelimitedHead {
                max_record_bytes: CANDIDATE_HEAD_BYTES,
            },
            metadata("metadata-only-v1"),
            CatalogOverlapStrategy::IdempotentOverlap,
            CatalogDecoderStateBoundary::ObjectGenerationCursor,
            ("fixture-metadata", &["native-family:fixture-metadata"]),
        )],
    );
    assert!(metadata_only.is_err());

    let falsely_admitted = CatalogMembershipEntry::new(
        member("fixture-semantic-session-false"),
        vec!["transcript-head-fallback".to_owned()],
        vec![],
    )
    .unwrap();
    assert!(CatalogMembershipSnapshot::new(
        &composition,
        complete_authorities(&composition),
        vec![falsely_admitted],
    )
    .is_err());

    let unauthorized_metadata = CatalogMembershipEntry::new(
        member("fixture-semantic-session-metadata"),
        vec!["top-level-transcript-membership".to_owned()],
        vec!["top-level-transcript-membership".to_owned()],
    )
    .unwrap();
    assert!(CatalogMembershipSnapshot::new(
        &composition,
        complete_authorities(&composition),
        vec![unauthorized_metadata],
    )
    .is_err());

    let snapshot = claude_membership(&composition);
    snapshot.validate_for(&composition).unwrap();
    assert_eq!(snapshot.members.len(), 3);
}

#[test]
fn membership_absence_requires_complete_authority_for_every_admitting_component() {
    let composition = claude_composition();
    let authorities = complete_authorities(&composition);
    let empty = CatalogMembershipSnapshot::new(&composition, authorities.clone(), vec![]).unwrap();
    assert!(empty.members.is_empty());

    let mut missing = authorities.clone();
    missing.pop();
    assert!(CatalogMembershipSnapshot::new(&composition, missing, vec![]).is_err());

    let mut partial = authorities.clone();
    partial[0].completeness = CatalogMembershipAuthorityCompleteness::Partial;
    assert!(CatalogMembershipSnapshot::new(&composition, partial, vec![]).is_err());

    let mut unavailable = authorities.clone();
    unavailable[0].completeness = CatalogMembershipAuthorityCompleteness::Unavailable;
    assert!(CatalogMembershipSnapshot::new(&composition, unavailable, vec![]).is_err());

    let mut zero_generation = authorities.clone();
    zero_generation[0].generation = 0;
    assert!(CatalogMembershipSnapshot::new(&composition, zero_generation, vec![]).is_err());

    let mut zero_revision = authorities;
    zero_revision[0].authority_revision = CatalogAuthorityRevision::from_digest([0; DIGEST_BYTES]);
    assert!(CatalogMembershipSnapshot::new(&composition, zero_revision, vec![]).is_err());
}

#[test]
fn membership_revision_is_order_invariant_but_duplicate_evidence_is_rejected() {
    let composition = claude_composition();
    let alpha = CatalogMembershipEntry::new(
        member("fixture-alpha"),
        vec![
            "top-level-transcript-membership".to_owned(),
            "session-index-membership".to_owned(),
        ],
        vec![
            "transcript-head-fallback".to_owned(),
            "session-index-membership".to_owned(),
        ],
    )
    .unwrap();
    let bravo = CatalogMembershipEntry::new(
        member("fixture-bravo"),
        vec!["nested-parent-membership".to_owned()],
        vec![],
    )
    .unwrap();
    let forward = CatalogMembershipSnapshot::new(
        &composition,
        complete_authorities(&composition),
        vec![alpha.clone(), bravo.clone()],
    )
    .unwrap();
    let reversed = CatalogMembershipSnapshot::new(
        &composition,
        complete_authorities(&composition),
        vec![bravo, alpha.clone()],
    )
    .unwrap();
    assert_eq!(forward, reversed);
    assert!(CatalogMembershipSnapshot::new(
        &composition,
        complete_authorities(&composition),
        vec![alpha.clone(), alpha],
    )
    .is_err());

    assert!(CatalogMembershipEntry::new(
        member("fixture-duplicate-evidence"),
        vec![
            "session-index-membership".to_owned(),
            "session-index-membership".to_owned(),
        ],
        vec![],
    )
    .is_err());
    let opaque = member("fixture-semantic-member-must-not-leak");
    assert!(!format!("{opaque:?}").contains("fixture-semantic-member-must-not-leak"));
}

#[test]
fn record_windows_never_split_skip_or_turn_partial_evidence_into_absence() {
    let composition = codex_composition();
    let mut planner =
        CatalogRecordWindowPlanner::new(&composition, "rollout-session-meta-head").unwrap();
    let first_step = planner.plan_initial(&[48 * 1024, 10 * 1024], 0).unwrap();
    let first = first_step.window;
    assert_eq!(first.selected_records, 1);
    assert_eq!(first.selected_bytes, 48 * 1024);
    assert_eq!(
        first.remainder,
        CatalogRecordWindowRemainder::ContinueAt { next_record: 1 }
    );
    let continuation = planner
        .plan_continuation(first_step.continuation.unwrap(), &[48 * 1024, 10 * 1024], 0)
        .unwrap()
        .window;
    assert_eq!(continuation.selected_records, 1);
    assert_eq!(continuation.end_record(), 2);

    let mut awaiting_planner =
        CatalogRecordWindowPlanner::new(&composition, "rollout-session-meta-head").unwrap();
    let awaiting = awaiting_planner
        .plan_initial(&[], 12 * 1024)
        .unwrap()
        .window;
    assert_eq!(awaiting.selected_records, 0);
    assert_eq!(awaiting.remainder.continuation_record(), None);
    assert_eq!(
        awaiting.remainder,
        CatalogRecordWindowRemainder::AwaitingCompleteRecord {
            next_record: 0,
            observed_bytes: 12 * 1024,
        }
    );
    let mut partial_planner =
        CatalogRecordWindowPlanner::new(&composition, "rollout-session-meta-head").unwrap();
    let oversized_partial = partial_planner
        .plan_initial(&[], CANDIDATE_HEAD_BYTES + 1)
        .unwrap()
        .window;
    assert_eq!(
        oversized_partial.remainder,
        CatalogRecordWindowRemainder::OversizedRecord {
            record_index: 0,
            observed_bytes: CANDIDATE_HEAD_BYTES + 1,
            complete: false,
        }
    );
    let mut oversized_planner =
        CatalogRecordWindowPlanner::new(&composition, "rollout-session-meta-head").unwrap();
    let oversized_complete = oversized_planner
        .plan_initial(&[CANDIDATE_HEAD_BYTES + 1], 0)
        .unwrap()
        .window;
    assert_eq!(
        oversized_complete.remainder,
        CatalogRecordWindowRemainder::OversizedRecord {
            record_index: 0,
            observed_bytes: CANDIDATE_HEAD_BYTES + 1,
            complete: true,
        }
    );
    let mut empty_planner =
        CatalogRecordWindowPlanner::new(&composition, "rollout-session-meta-head").unwrap();
    let empty_record = empty_planner.plan_initial(&[0], 0).unwrap().window;
    assert_eq!(empty_record.selected_records, 1);
    assert_eq!(empty_record.selected_bytes, 0);
}

#[test]
fn record_window_continuations_bind_the_exact_chain_and_cannot_skip_or_replay() {
    let layout = [48 * 1024, 10 * 1024, 8 * 1024];
    let composition = codex_composition();
    let mut planner =
        CatalogRecordWindowPlanner::new(&composition, "rollout-session-meta-head").unwrap();
    let first = planner.plan_initial(&layout, 0).unwrap();
    let first_token = first.continuation.unwrap();
    assert!(planner.plan_initial(&layout, 0).is_err());
    assert!(planner
        .plan_continuation(first_token, &[48 * 1024, 10 * 1024, 8 * 1024 + 1], 0)
        .is_err());

    let second = planner.plan_continuation(first_token, &layout, 0).unwrap();
    assert_eq!(second.window.start_record, 1);
    let second_token = second.continuation.unwrap();
    assert!(planner.plan_continuation(first_token, &layout, 0).is_err());
    assert!(planner
        .plan_continuation(
            CatalogWindowContinuation::from_digest([7; DIGEST_BYTES]),
            &layout,
            0,
        )
        .is_err());

    let mut skipped =
        CatalogRecordWindowPlanner::new(&composition, "rollout-session-meta-head").unwrap();
    skipped.plan_initial(&layout, 0).unwrap();
    assert!(skipped.plan_continuation(second_token, &layout, 0).is_err());

    let claude = claude_composition();
    let claude_layout = [32 * 1024, 30 * 1024, 20 * 1024];
    let mut wrong_component =
        CatalogRecordWindowPlanner::new(&claude, "transcript-head-fallback").unwrap();
    wrong_component.plan_initial(&claude_layout, 0).unwrap();
    assert!(wrong_component
        .plan_continuation(first_token, &claude_layout, 0)
        .is_err());

    let mut changed_components = codex_composition().components;
    let CatalogSourcePrimitive::DelimitedHead { max_record_bytes } =
        &mut changed_components[0].primitive
    else {
        panic!("Codex fixture component must remain a delimited head")
    };
    *max_record_bytes -= 1;
    let changed_spec = planned_composition(
        "codex",
        "codex.catalog-candidate-2026-08-15",
        "codex.catalog-sources-v1",
        changed_components,
    )
    .unwrap();
    let mut wrong_spec =
        CatalogRecordWindowPlanner::new(&changed_spec, "rollout-session-meta-head").unwrap();
    wrong_spec.plan_initial(&layout, 0).unwrap();
    assert!(wrong_spec
        .plan_continuation(first_token, &layout, 0)
        .is_err());

    let final_step = planner.plan_continuation(second_token, &layout, 0).unwrap();
    assert_eq!(final_step.window.start_record, 2);
    assert!(final_step.continuation.is_none());
    assert!(planner.plan_continuation(second_token, &layout, 0).is_err());
}

#[test]
fn head_or_prefix_plus_continuation_has_the_full_only_trace() {
    let claude = claude_composition();
    let prefix = window_conformance(
        &claude,
        "transcript-head-fallback",
        vec![32 * 1024, 30 * 1024, 20 * 1024],
    );
    assert_eq!(prefix.windows.len(), 2);
    assert_eq!(prefix.full_only, prefix.head_or_prefix_plus_continuation);
    assert_eq!(prefix.windows[0].window.end_record(), 2);
    assert_eq!(prefix.windows[1].window.start_record, 2);

    let codex = codex_composition();
    let head = window_conformance(
        &codex,
        "rollout-session-meta-head",
        vec![48 * 1024, 10 * 1024, 8 * 1024],
    );
    assert_eq!(head.windows.len(), 3);
    assert_eq!(head.full_only, head.head_or_prefix_plus_continuation);

    let records = witnesses("rollout-session-meta-head", 2);
    let mut wrong_order =
        CatalogDecodeTraceAccumulator::new(&codex, "rollout-session-meta-head").unwrap();
    wrong_order.push(records[1]).unwrap();
    wrong_order.push(records[0]).unwrap();
    assert_ne!(
        wrong_order.finish(records[0].decoder_state_after),
        trace_summary(&codex, "rollout-session-meta-head", &records)
    );

    let mut wrong_state =
        CatalogDecodeTraceAccumulator::new(&codex, "rollout-session-meta-head").unwrap();
    wrong_state.push(records[0]).unwrap();
    assert_ne!(
        wrong_state.finish(digest("different-final-state")),
        trace_summary(&codex, "rollout-session-meta-head", &records[..1])
    );

    assert!(CatalogDecodeTraceAccumulator::new(&codex, "missing-component").is_err());
    let grok = grok_composition();
    assert!(CatalogDecodeTraceAccumulator::new(&grok, "session-directory-membership").is_err());
}

#[test]
fn invalid_bounds_paths_overlap_and_state_boundaries_fail_closed() {
    let invalid_prefix = CatalogSourcePrimitive::DelimitedPrefix {
        max_record_bytes: 128,
        max_window_bytes: 64,
        max_records: 1,
    };
    assert!(invalid_prefix.validate().is_err());

    let mut duplicate_selector = claude_components();
    duplicate_selector[0].relative_selectors = vec!["*/*.jsonl".to_owned(); 2];
    assert!(planned_composition(
        "claude-code",
        "claude-code.catalog-candidate-2026-08-15",
        "claude-code.catalog-sources-v1",
        duplicate_selector,
    )
    .is_err());

    let duplicate_component = claude_components()[0].clone();
    assert!(planned_composition(
        "claude-code",
        "claude-code.catalog-candidate-2026-08-15",
        "claude-code.catalog-sources-v1",
        vec![duplicate_component.clone(), duplicate_component],
    )
    .is_err());

    let mut traversal = claude_components();
    traversal[0].relative_selectors = vec!["../../escape".to_owned()];
    assert!(planned_composition(
        "claude-code",
        "claude-code.catalog-candidate-2026-08-15",
        "claude-code.catalog-sources-v1",
        traversal,
    )
    .is_err());

    for invalid_selector in ["* /session.jsonl", "*/session\n.jsonl"] {
        let mut invalid = claude_components();
        invalid[0].relative_selectors = vec![invalid_selector.to_owned()];
        assert!(planned_composition(
            "claude-code",
            "claude-code.catalog-candidate-2026-08-15",
            "claude-code.catalog-sources-v1",
            invalid,
        )
        .is_err());
    }

    let mut full_only = claude_components();
    full_only[0].overlap_strategy = CatalogOverlapStrategy::FullOnly;
    assert!(planned_composition(
        "claude-code",
        "claude-code.catalog-candidate-2026-08-15",
        "claude-code.catalog-sources-v1",
        full_only,
    )
    .is_err());

    let mut full_primitives = grok_composition().components;
    for component in &mut full_primitives {
        component.overlap_strategy = CatalogOverlapStrategy::FullOnly;
    }
    assert!(planned_composition(
        "grok",
        "grok.catalog-candidate-2026-08-15",
        "grok.catalog-sources-v1",
        full_primitives,
    )
    .is_ok());

    let mut wrong_boundary = claude_components();
    wrong_boundary[1].safe_decoder_state_boundary =
        CatalogDecoderStateBoundary::ObjectGenerationCursor;
    assert!(planned_composition(
        "claude-code",
        "claude-code.catalog-candidate-2026-08-15",
        "claude-code.catalog-sources-v1",
        wrong_boundary,
    )
    .is_err());
}
