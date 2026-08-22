use std::collections::BTreeMap;

use super::super::evidence::{
    CatalogAvailability, CatalogDisclosureClass, CatalogFieldAuthority, CatalogLocatorKind,
    CatalogLocatorValue, CatalogMutation, CatalogQualifiedField, CatalogSessionAssertion,
    IdentityRelationFact, IdentityRelationKind, NativeLocatorClaim, ProjectAssociationBasis,
    SessionProjectAssociationFact,
};
use super::super::{CatalogAccessPolicyDigest, CatalogReadinessMachine};
use super::*;
use crate::adapter::{
    CanonicalEntityKey, CanonicalFactId, CanonicalSourceInstanceKey, ContractCompleteness,
    CoverageDeclarationDigest, CoverageMembershipRevision, CoverageObjectKey, CoveragePosition,
    CoveragePositionKind, CoverageProvenance, CoverageStatus, CoverageStreamKey, FactRevisionId,
    QualifiedValue, QualifiedValueQuality, SemanticRevisionRef,
};

const ADAPTER_ID: &str = "fixture-agent";
const MEMBER_IDENTITY_CONTRACT: &str = "catalog-session-identity-v1";

#[derive(Clone)]
struct FixtureSource {
    plan_source: CatalogCoveragePlanSource,
    assembly: CatalogCompleteSourceAssembly,
    member_ref: CatalogPublicationMemberRef,
    owner: CatalogEvidenceOwner,
    session_ref: CatalogEntityRef,
    assertion: CatalogSessionAssertion,
}

fn selection() -> ContractVersionSelection {
    ContractVersionSelection {
        selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
        model_major: 1,
        external_entity_reference_version: 1,
        semantic_revision_reference_version: 1,
        coverage_contract_version: SOURCE_COVERAGE_CONTRACT_VERSION,
        fact_family_versions: BTreeMap::from([("catalog.session".to_owned(), 1)]),
        query_pack_version: Some(CATALOG_QUERY_PACK_CONTRACT_VERSION),
        observation_contract_version: None,
    }
}

fn revision(owner: &CatalogEvidenceOwner, label: &str) -> SemanticRevisionRef {
    let fact_id = CanonicalFactId::native(
        &owner.adapter_id,
        &owner.source_instance_key,
        "catalog.publication.fixture",
        label.as_bytes(),
    )
    .unwrap();
    SemanticRevisionRef::new(FactRevisionId::derive(&fact_id, 1, label.as_bytes()).unwrap())
}

fn availability(
    owner: &CatalogEvidenceOwner,
    label: &str,
) -> CatalogQualifiedField<CatalogAvailability> {
    CatalogQualifiedField::new(
        QualifiedValue::from_parts(
            Some(CatalogAvailability::HistoryReady),
            QualifiedValueQuality::Exact,
            CatalogFieldAuthority::new("catalog-membership", 100, true).unwrap(),
            ContractCompleteness::Complete,
            None,
            None,
            vec![revision(owner, label)],
        )
        .unwrap(),
        CatalogDisclosureClass::Public,
    )
    .unwrap()
}

fn session_assertion(
    owner: CatalogEvidenceOwner,
    session_ref: CatalogEntityRef,
    label: &str,
) -> CatalogSessionAssertion {
    CatalogSessionAssertion::new(
        owner.clone(),
        label.as_bytes(),
        session_ref,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        availability(&owner, &format!("{label}-availability")),
        vec![revision(&owner, &format!("{label}-assertion"))],
    )
    .unwrap()
}

fn fixture_source(label: &str) -> FixtureSource {
    let source_instance_key = CanonicalSourceInstanceKey::derive(1, label.as_bytes()).unwrap();
    let stream_key =
        CoverageStreamKey::derive(ADAPTER_ID, format!("{label}-catalog").as_bytes()).unwrap();
    let object_key = CoverageObjectKey::derive("catalog-publication", label.as_bytes()).unwrap();
    let owner =
        CatalogEvidenceOwner::new(ADAPTER_ID, source_instance_key, stream_key, object_key, 1)
            .unwrap();
    let plan_source = CatalogCoveragePlanSource::new(
        ADAPTER_ID,
        source_instance_key,
        format!("{label}-support"),
        CoverageDeclarationDigest::derive(format!("{label}-declaration").as_bytes()).unwrap(),
        CatalogAccessPolicyDigest::derive(1, format!("{label}-policy").as_bytes()).unwrap(),
    )
    .unwrap();
    let domain = CoverageDomain::ProjectionPack {
        pack: CATALOG_PROJECTION_PACK_ID.to_owned(),
        version: CATALOG_QUERY_PACK_CONTRACT_VERSION,
    };
    let point = crate::adapter::SourceCoveragePoint::new(
        domain.clone(),
        ADAPTER_ID,
        source_instance_key,
        stream_key,
        object_key,
        1,
        Some(
            CoveragePosition::derive(
                CoveragePositionKind::SnapshotRevision,
                format!("{label}-position").as_bytes(),
                None,
            )
            .unwrap(),
        ),
        CoverageStatus::ExactSnapshot,
        CoverageProvenance::default(),
    )
    .unwrap();
    let source_coverage = SourceCoverageSet::new(
        domain,
        plan_source.coverage_scope(CatalogCoverageScope::Library),
        CoverageMembershipRevision::derive(format!("{label}-coverage-membership").as_bytes())
            .unwrap(),
        vec![point],
        Vec::new(),
        Vec::new(),
        CoverageSetCompleteness::Complete,
    )
    .unwrap();
    let member_ref = CatalogPublicationMemberRef::from_digest(
        *blake3::hash(format!("member:{label}").as_bytes()).as_bytes(),
    );
    let assembly = CatalogCompleteSourceAssembly::from_complete_library_coverage(
        plan_source.clone(),
        selection(),
        MEMBER_IDENTITY_CONTRACT,
        CatalogSourceMembershipRevision::from_digest(
            *blake3::hash(format!("membership:{label}").as_bytes()).as_bytes(),
        ),
        CatalogSourceCompletionRevision::from_digest(
            *blake3::hash(format!("completion:{label}").as_bytes()).as_bytes(),
        ),
        vec![member_ref],
        source_coverage,
    )
    .unwrap();
    let entity_key = CanonicalEntityKey::derive(
        ADAPTER_ID,
        &source_instance_key,
        "session",
        label.as_bytes(),
    )
    .unwrap();
    let session_ref = CatalogEntityRef::session(entity_key);
    let assertion = session_assertion(owner.clone(), session_ref, label);
    FixtureSource {
        plan_source,
        assembly,
        member_ref,
        owner,
        session_ref,
        assertion,
    }
}

fn rebuild_source_assembly(
    source: &FixtureSource,
    member_refs: Vec<CatalogPublicationMemberRef>,
    source_coverage: SourceCoverageSet,
) -> Result<CatalogCompleteSourceAssembly, CatalogContractError> {
    CatalogCompleteSourceAssembly::from_complete_library_coverage(
        source.plan_source.clone(),
        selection(),
        MEMBER_IDENTITY_CONTRACT,
        source.assembly.membership_revision(),
        source.assembly.component_completion_revision(),
        member_refs,
        source_coverage,
    )
}

fn building(plan: &CatalogCoveragePlan) -> CatalogReadinessSnapshot {
    let mut machine =
        CatalogReadinessMachine::register(plan.clone(), CATALOG_QUERY_PACK_CONTRACT_VERSION)
            .unwrap();
    machine.schedule_build().unwrap();
    machine.snapshot().clone()
}

fn reducer_with(sources: &[&FixtureSource], observation_commit: u64) -> CatalogReducer {
    let mut reducer = CatalogReducer::default();
    for source in sources {
        reducer
            .upsert_session_assertion(source.assertion.clone(), observation_commit)
            .unwrap();
    }
    reducer
}

fn bindings(sources: &[&FixtureSource]) -> Vec<CatalogPublicationMemberBinding> {
    sources
        .iter()
        .map(|source| {
            source
                .assembly
                .member_binding(
                    source.member_ref,
                    source.assertion.assertion_key,
                    source.session_ref,
                )
                .unwrap()
        })
        .collect()
}

fn plan(required: &[&FixtureSource], optional: &[&FixtureSource]) -> CatalogCoveragePlan {
    CatalogCoveragePlan::new(
        CatalogCoverageScope::Library,
        required
            .iter()
            .map(|source| source.plan_source.clone())
            .collect(),
        optional
            .iter()
            .map(|source| source.plan_source.clone())
            .collect(),
    )
    .unwrap()
}

#[test]
fn complete_initial_publication_is_canonical_and_redacts_native_evidence() {
    let alpha = fixture_source("alpha");
    let beta = fixture_source("beta");
    let coverage_plan = plan(&[&alpha], &[&beta]);
    let readiness = building(&coverage_plan);
    let first_reducer = reducer_with(&[&alpha, &beta], 10);
    let first = CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        &readiness,
        selection(),
        vec![beta.assembly.clone(), alpha.assembly.clone()],
        &first_reducer,
        bindings(&[&beta, &alpha]),
        CatalogPublicationLimits::new(10, 100, 10).unwrap(),
    )
    .unwrap();

    let second_reducer = reducer_with(&[&beta, &alpha], 10);
    let second = CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        &readiness,
        selection(),
        vec![alpha.assembly.clone(), beta.assembly.clone()],
        &second_reducer,
        bindings(&[&alpha, &beta]),
        CatalogPublicationLimits::new(10, 100, 10).unwrap(),
    )
    .unwrap();

    assert_eq!(first.digest(), second.digest());
    assert_eq!(first.source_count(), 2);
    assert_eq!(first.member_count(), 2);
    assert_eq!(first.session_row_count(), 2);
    assert_eq!(first.project_row_count(), 0);
    assert_eq!(first.tombstone_count(), 0);
    assert_eq!(
        first.build().coverage_plan_id,
        coverage_plan.coverage_plan_id
    );
    let debug = format!("{first:?}");
    assert!(!debug.contains("alpha-availability"));
    assert!(!debug.contains("catalog.publication.fixture"));
    assert!(!debug.contains(&format!("{:?}", alpha.session_ref)));
}

#[test]
fn durable_source_and_member_frames_rebind_to_the_exact_reducer() {
    let alpha = fixture_source("restart-alpha");
    let coverage_plan = plan(&[&alpha], &[]);
    let reducer = reducer_with(&[&alpha], 10);
    let assembly = CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        &building(&coverage_plan),
        selection(),
        vec![alpha.assembly.clone()],
        &reducer,
        bindings(&[&alpha]),
        CatalogPublicationLimits::default(),
    )
    .unwrap();
    let durable = assembly.prepare_durable().unwrap();
    let source_entry = durable
        .entries()
        .iter()
        .find(|entry| entry.kind() == CatalogDurablePublicationEntryKind::Source)
        .unwrap();
    let member_entry = durable
        .entries()
        .iter()
        .find(|entry| entry.kind() == CatalogDurablePublicationEntryKind::MemberBinding)
        .unwrap();
    let reducer_entry = durable
        .entries()
        .iter()
        .find(|entry| entry.kind() == CatalogDurablePublicationEntryKind::ReducerState)
        .unwrap();
    let source = decode_durable_source_frame(
        source_entry.payload(),
        source_entry.key(),
        MAX_DURABLE_PUBLICATION_BYTES,
    )
    .unwrap();
    let binding = decode_durable_member_binding_frame(
        member_entry.payload(),
        member_entry.key(),
        MAX_DURABLE_PUBLICATION_BYTES,
    )
    .unwrap();
    let restored_reducer = super::super::evidence::decode_durable_reducer_state(
        reducer_entry.payload(),
        MAX_DURABLE_PUBLICATION_BYTES,
    )
    .unwrap()
    .finish(
        Vec::new(),
        durable.reducer_revision(),
        CatalogReducerPublicationLimits::default(),
    )
    .unwrap();
    let coverage = validate_restarted_initial_publication(
        &coverage_plan,
        &selection(),
        Some(MEMBER_IDENTITY_CONTRACT),
        vec![source],
        vec![binding],
        &restored_reducer,
    )
    .unwrap();
    assert_eq!(coverage, vec![alpha.assembly.source_coverage().clone()]);

    let mut drifted: DurableMemberBindingWire =
        serde_json::from_slice(member_entry.payload()).unwrap();
    drifted.session_ref = CatalogEntityRef::session(
        CanonicalEntityKey::derive(
            ADAPTER_ID,
            &alpha.owner.source_instance_key,
            "session",
            b"other-session",
        )
        .unwrap(),
    );
    let drifted = serialize_private_json_bounded(
        &drifted,
        MAX_DURABLE_PUBLICATION_BYTES,
        "drifted member binding fixture",
    )
    .unwrap();
    let drifted_binding = decode_durable_member_binding_frame(
        &drifted,
        member_entry.key(),
        MAX_DURABLE_PUBLICATION_BYTES,
    )
    .unwrap();
    let source = decode_durable_source_frame(
        source_entry.payload(),
        source_entry.key(),
        MAX_DURABLE_PUBLICATION_BYTES,
    )
    .unwrap();
    assert!(validate_restarted_initial_publication(
        &coverage_plan,
        &selection(),
        Some(MEMBER_IDENTITY_CONTRACT),
        vec![source],
        vec![drifted_binding],
        &restored_reducer,
    )
    .is_err());
}

#[test]
fn complete_source_coverage_order_is_canonical_and_zero_member_refs_are_rejected() {
    let alpha = fixture_source("canonical-source");
    let mut coverage = alpha.assembly.source_coverage().clone();
    coverage.points.push(
        crate::adapter::SourceCoveragePoint::new(
            coverage.coverage_domain.clone(),
            ADAPTER_ID,
            alpha.owner.source_instance_key,
            alpha.owner.stream_key,
            CoverageObjectKey::derive("catalog-publication", b"canonical-extra").unwrap(),
            2,
            Some(
                CoveragePosition::derive(
                    CoveragePositionKind::SnapshotRevision,
                    b"canonical-extra-position",
                    None,
                )
                .unwrap(),
            ),
            CoverageStatus::ExactSnapshot,
            CoverageProvenance::default(),
        )
        .unwrap(),
    );
    let first = rebuild_source_assembly(&alpha, vec![alpha.member_ref], coverage.clone()).unwrap();
    coverage.points.reverse();
    let reordered = rebuild_source_assembly(&alpha, vec![alpha.member_ref], coverage).unwrap();
    assert_eq!(first.digest, reordered.digest);
    assert!(rebuild_source_assembly(
        &alpha,
        vec![CatalogPublicationMemberRef::from_digest([0; 32])],
        alpha.assembly.source_coverage().clone(),
    )
    .is_err());
}

#[test]
fn initial_publication_requires_an_exact_active_lineage_and_selection() {
    let alpha = fixture_source("alpha-lineage");
    let coverage_plan = plan(&[&alpha], &[]);
    let reducer = reducer_with(&[&alpha], 10);
    let member_bindings = bindings(&[&alpha]);
    let sources = vec![alpha.assembly.clone()];

    let pending = CatalogReadinessMachine::register(
        coverage_plan.clone(),
        CATALOG_QUERY_PACK_CONTRACT_VERSION,
    )
    .unwrap();
    assert!(CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        pending.snapshot(),
        selection(),
        sources.clone(),
        &reducer,
        member_bindings.clone(),
        CatalogPublicationLimits::default(),
    )
    .is_err());

    let mut replacement_machine =
        CatalogReadinessMachine::resume(coverage_plan.clone(), building(&coverage_plan)).unwrap();
    replacement_machine.invalidate_source_generation().unwrap();
    let replacement = CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        replacement_machine.snapshot(),
        selection(),
        sources.clone(),
        &reducer,
        member_bindings.clone(),
        CatalogPublicationLimits::default(),
    )
    .unwrap();
    assert_eq!(replacement.build().epoch, 2);
    assert_eq!(replacement.build().attempt, 1);

    let mut drifted_selection = selection();
    drifted_selection.model_major = 2;
    assert!(CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        &building(&coverage_plan),
        drifted_selection,
        sources,
        &reducer,
        member_bindings,
        CatalogPublicationLimits::default(),
    )
    .is_err());
}

#[test]
fn source_free_publication_still_requires_exact_base_and_reference_versions() {
    let coverage_plan =
        CatalogCoveragePlan::new(CatalogCoverageScope::Library, Vec::new(), Vec::new()).unwrap();
    let readiness = building(&coverage_plan);
    let reducer = CatalogReducer::default();

    CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        &readiness,
        selection(),
        Vec::new(),
        &reducer,
        Vec::new(),
        CatalogPublicationLimits::default(),
    )
    .unwrap();

    let mut model_drift = selection();
    model_drift.model_major = 2;
    let mut external_reference_drift = selection();
    external_reference_drift.external_entity_reference_version = 2;
    let mut semantic_reference_drift = selection();
    semantic_reference_drift.semantic_revision_reference_version = 2;
    for drifted in [
        model_drift,
        external_reference_drift,
        semantic_reference_drift,
    ] {
        assert!(CatalogInitialPublicationAssembly::assemble(
            &coverage_plan,
            &readiness,
            drifted,
            Vec::new(),
            &reducer,
            Vec::new(),
            CatalogPublicationLimits::default(),
        )
        .is_err());
    }
}

#[test]
fn required_and_optional_source_assemblies_are_exact() {
    let required = fixture_source("required");
    let optional = fixture_source("optional");
    let foreign = fixture_source("foreign");
    let coverage_plan = plan(&[&required], &[&optional]);
    let readiness = building(&coverage_plan);

    let optional_only_reducer = reducer_with(&[&optional], 10);
    assert!(CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        &readiness,
        selection(),
        vec![optional.assembly.clone()],
        &optional_only_reducer,
        bindings(&[&optional]),
        CatalogPublicationLimits::default(),
    )
    .is_err());

    let required_reducer = reducer_with(&[&required], 10);
    assert!(CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        &readiness,
        selection(),
        vec![required.assembly.clone(), required.assembly.clone()],
        &required_reducer,
        bindings(&[&required]),
        CatalogPublicationLimits::default(),
    )
    .is_err());

    let foreign_reducer = reducer_with(&[&required, &foreign], 10);
    assert!(CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        &readiness,
        selection(),
        vec![required.assembly.clone(), foreign.assembly.clone()],
        &foreign_reducer,
        bindings(&[&required, &foreign]),
        CatalogPublicationLimits::default(),
    )
    .is_err());

    CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        &readiness,
        selection(),
        vec![required.assembly.clone()],
        &required_reducer,
        bindings(&[&required]),
        CatalogPublicationLimits::default(),
    )
    .unwrap();
}

#[test]
fn complete_empty_required_source_composes_with_a_nonempty_source() {
    let populated = fixture_source("populated-required");
    let empty = fixture_source("empty-required");
    let empty_assembly =
        rebuild_source_assembly(&empty, Vec::new(), empty.assembly.source_coverage().clone())
            .unwrap();
    let coverage_plan = CatalogCoveragePlan::new(
        CatalogCoverageScope::Library,
        vec![populated.plan_source.clone(), empty.plan_source.clone()],
        Vec::new(),
    )
    .unwrap();
    let publication = CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        &building(&coverage_plan),
        selection(),
        vec![populated.assembly.clone(), empty_assembly],
        &reducer_with(&[&populated], 10),
        bindings(&[&populated]),
        CatalogPublicationLimits::default(),
    )
    .unwrap();
    assert_eq!(publication.source_count(), 2);
    assert_eq!(publication.member_count(), 1);
    assert_eq!(publication.session_row_count(), 1);
}

#[test]
fn shared_member_identity_must_converge_on_one_base_session_across_sources() {
    let alpha = fixture_source("converged-alpha");
    let mut beta = fixture_source("converged-beta");
    beta.member_ref = alpha.member_ref;
    beta.assembly = rebuild_source_assembly(
        &beta,
        vec![beta.member_ref],
        beta.assembly.source_coverage().clone(),
    )
    .unwrap();
    let coverage_plan = plan(&[&alpha, &beta], &[]);
    let readiness = building(&coverage_plan);

    assert!(CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        &readiness,
        selection(),
        vec![alpha.assembly.clone(), beta.assembly.clone()],
        &reducer_with(&[&alpha, &beta], 10),
        bindings(&[&alpha, &beta]),
        CatalogPublicationLimits::default(),
    )
    .is_err());

    beta.session_ref = alpha.session_ref;
    beta.assertion = session_assertion(beta.owner.clone(), beta.session_ref, "converged-beta");
    let publication = CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        &readiness,
        selection(),
        vec![alpha.assembly.clone(), beta.assembly.clone()],
        &reducer_with(&[&alpha, &beta], 10),
        bindings(&[&alpha, &beta]),
        CatalogPublicationLimits::default(),
    )
    .unwrap();
    assert_eq!(publication.member_count(), 2);
    assert_eq!(publication.session_row_count(), 1);
}

#[test]
fn admitted_members_require_exact_unique_live_session_assertions() {
    let alpha = fixture_source("member-alpha");
    let coverage_plan = plan(&[&alpha], &[]);
    let readiness = building(&coverage_plan);
    let reducer = reducer_with(&[&alpha], 10);

    assert!(CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        &readiness,
        selection(),
        vec![alpha.assembly.clone()],
        &reducer,
        Vec::new(),
        CatalogPublicationLimits::default(),
    )
    .is_err());

    let mut foreign_binding = bindings(&[&alpha]).remove(0);
    foreign_binding.session_ref = CatalogEntityRef::session(
        CanonicalEntityKey::derive(
            ADAPTER_ID,
            &alpha.owner.source_instance_key,
            "session",
            b"foreign",
        )
        .unwrap(),
    );
    assert!(CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        &readiness,
        selection(),
        vec![alpha.assembly.clone()],
        &reducer,
        vec![foreign_binding],
        CatalogPublicationLimits::default(),
    )
    .is_err());

    let mut extra_reducer = reducer.clone();
    let extra_ref = CatalogEntityRef::session(
        CanonicalEntityKey::derive(
            ADAPTER_ID,
            &alpha.owner.source_instance_key,
            "session",
            b"extra",
        )
        .unwrap(),
    );
    extra_reducer
        .upsert_session_assertion(
            session_assertion(alpha.owner.clone(), extra_ref, "extra"),
            10,
        )
        .unwrap();
    assert!(CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        &readiness,
        selection(),
        vec![alpha.assembly.clone()],
        &extra_reducer,
        bindings(&[&alpha]),
        CatalogPublicationLimits::default(),
    )
    .is_err());
}

#[test]
fn live_reducer_evidence_must_belong_to_an_exact_covered_source_coordinate() {
    let alpha = fixture_source("covered-alpha");
    let foreign = fixture_source("covered-foreign");
    let coverage_plan = plan(&[&alpha], &[]);
    let readiness = building(&coverage_plan);
    let mut reducer = reducer_with(&[&alpha], 10);
    reducer
        .upsert_session_assertion(foreign.assertion.clone(), 10)
        .unwrap();

    assert!(CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        &readiness,
        selection(),
        vec![alpha.assembly.clone()],
        &reducer,
        bindings(&[&alpha]),
        CatalogPublicationLimits::default(),
    )
    .is_err());

    let mut drifted_source = alpha.assembly.clone();
    drifted_source
        .contract_selection
        .fact_family_versions
        .insert("catalog.project".to_owned(), 1);
    assert!(CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        &readiness,
        selection(),
        vec![drifted_source],
        &reducer_with(&[&alpha], 10),
        bindings(&[&alpha]),
        CatalogPublicationLimits::default(),
    )
    .is_err());

    let mut policy_drift = alpha.assembly.clone();
    policy_drift.plan_source.access_policy_digest =
        CatalogAccessPolicyDigest::derive(1, b"different-policy").unwrap();
    assert!(CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        &readiness,
        selection(),
        vec![policy_drift],
        &reducer_with(&[&alpha], 10),
        bindings(&[&alpha]),
        CatalogPublicationLimits::default(),
    )
    .is_err());
}

#[test]
fn publication_rejects_unbound_association_locator_and_identity_endpoints() {
    let alpha = fixture_source("endpoint-alpha");
    let coverage_plan = plan(&[&alpha], &[]);
    let readiness = building(&coverage_plan);
    let assemble = |reducer: &CatalogReducer| {
        CatalogInitialPublicationAssembly::assemble(
            &coverage_plan,
            &readiness,
            selection(),
            vec![alpha.assembly.clone()],
            reducer,
            bindings(&[&alpha]),
            CatalogPublicationLimits::default(),
        )
    };
    let foreign_project = CatalogEntityRef::project(
        CanonicalEntityKey::derive(
            ADAPTER_ID,
            &alpha.owner.source_instance_key,
            "project",
            b"foreign-project",
        )
        .unwrap(),
    );
    let association = SessionProjectAssociationFact::new(
        alpha.owner.clone(),
        b"unbound-association",
        alpha.session_ref,
        foreign_project,
        ProjectAssociationBasis::TranscriptCwd,
        None,
        None,
        CatalogFieldAuthority::new("association", 50, true).unwrap(),
        QualifiedValueQuality::Exact,
        ContractCompleteness::Complete,
        None,
        vec![revision(&alpha.owner, "unbound-association")],
    )
    .unwrap();
    let mut association_reducer = reducer_with(&[&alpha], 10);
    association_reducer
        .upsert_association(association, 10)
        .unwrap();
    assert!(assemble(&association_reducer).is_err());

    let foreign_session = CatalogEntityRef::session(
        CanonicalEntityKey::derive(
            ADAPTER_ID,
            &alpha.owner.source_instance_key,
            "session",
            b"foreign-session",
        )
        .unwrap(),
    );
    let locator = NativeLocatorClaim::new(
        alpha.owner.clone(),
        b"unbound-locator",
        foreign_session,
        CatalogLocatorKind::Filesystem,
        CatalogQualifiedField::new(
            QualifiedValue::from_parts(
                Some(CatalogLocatorValue {
                    native_value: Some("private-native-locator".to_owned()),
                    canonical_local_path: Some("/private/catalog/session.jsonl".to_owned()),
                }),
                QualifiedValueQuality::Exact,
                CatalogFieldAuthority::new("locator", 50, true).unwrap(),
                ContractCompleteness::Complete,
                None,
                None,
                vec![revision(&alpha.owner, "unbound-locator-value")],
            )
            .unwrap(),
            CatalogDisclosureClass::LocalSensitive,
        )
        .unwrap(),
        ProjectAssociationBasis::SessionDirectory,
        vec![revision(&alpha.owner, "unbound-locator")],
    )
    .unwrap();
    let mut locator_reducer = reducer_with(&[&alpha], 10);
    locator_reducer.upsert_locator_claim(locator, 10).unwrap();
    assert!(assemble(&locator_reducer).is_err());

    let relation = IdentityRelationFact::new(
        alpha.owner.clone(),
        b"unbound-relation",
        IdentityRelationKind::SameEntity,
        alpha.session_ref,
        foreign_session,
        CatalogFieldAuthority::new("identity", 50, true).unwrap(),
        QualifiedValueQuality::Exact,
        ContractCompleteness::Complete,
        Some(foreign_session),
        Some("fixture-collision-v1".to_owned()),
        vec![revision(&alpha.owner, "unbound-relation")],
    )
    .unwrap();
    let mut relation_reducer = reducer_with(&[&alpha], 10);
    relation_reducer
        .upsert_identity_relation(relation, 10)
        .unwrap();
    let endpoint_error = assemble(&relation_reducer).unwrap_err();
    assert!(endpoint_error.to_string().contains("unevidenced endpoint"));
    let preflight_error = relation_reducer
        .freeze_for_initial_publication(CatalogReducerPublicationLimits::new(1, 1).unwrap())
        .unwrap_err();
    assert!(preflight_error
        .to_string()
        .contains("bounded evidence ceiling"));
}

#[test]
fn publication_bounds_and_observation_commit_are_part_of_the_frozen_identity() {
    let alpha = fixture_source("bounds-alpha");
    let coverage_plan = plan(&[&alpha], &[]);
    let readiness = building(&coverage_plan);
    let reducer = reducer_with(&[&alpha], 10);
    assert!(CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        &readiness,
        selection(),
        vec![alpha.assembly.clone()],
        &reducer,
        bindings(&[&alpha]),
        CatalogPublicationLimits::new(1, 1, 1).unwrap(),
    )
    .is_err());

    let first = CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        &readiness,
        selection(),
        vec![alpha.assembly.clone()],
        &reducer,
        bindings(&[&alpha]),
        CatalogPublicationLimits::new(1, 20, 1).unwrap(),
    )
    .unwrap();
    let refreshed = reducer_with(&[&alpha], 11);
    let second = CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        &readiness,
        selection(),
        vec![alpha.assembly.clone()],
        &refreshed,
        bindings(&[&alpha]),
        CatalogPublicationLimits::new(1, 20, 1).unwrap(),
    )
    .unwrap();
    assert_ne!(first.digest(), second.digest());

    let different_limits = CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        &readiness,
        selection(),
        vec![alpha.assembly.clone()],
        &reducer,
        bindings(&[&alpha]),
        CatalogPublicationLimits::new(2, 20, 1).unwrap(),
    )
    .unwrap();
    assert_ne!(first.digest(), different_limits.digest());
}

#[test]
fn refresh_binds_exact_predecessor_reducer_and_cumulative_member_history() {
    let alpha = fixture_source("refresh-alpha");
    let beta = fixture_source("refresh-beta");
    let coverage_plan = plan(&[&alpha, &beta], &[]);
    let initial_reducer = reducer_with(&[&alpha, &beta], 10);
    let initial = CatalogInitialPublicationAssembly::assemble(
        &coverage_plan,
        &building(&coverage_plan),
        selection(),
        vec![alpha.assembly.clone(), beta.assembly.clone()],
        &initial_reducer,
        bindings(&[&alpha, &beta]),
        CatalogPublicationLimits::default(),
    )
    .unwrap();
    let durable_initial = initial.prepare_durable().unwrap();
    let prior_history = initial.member_history().unwrap();
    let snapshot = CatalogSnapshotId::new(
        CATALOG_QUERY_PACK_CONTRACT_VERSION,
        coverage_plan.coverage_plan_id,
        1,
        30,
    )
    .unwrap();
    let predecessor = CatalogRefreshPredecessor::new(
        snapshot,
        *durable_initial.publication_digest().storage_bytes(),
        *durable_initial.content_digest(),
        selection(),
        durable_initial
            .member_identity_contract_id()
            .map(str::to_owned),
        durable_initial.reducer_revision(),
        prior_history.revision(),
    )
    .unwrap();
    let mut readiness = CatalogReadinessMachine::register(
        coverage_plan.clone(),
        CATALOG_QUERY_PACK_CONTRACT_VERSION,
    )
    .unwrap();
    readiness.schedule_build().unwrap();
    let mut ready_coverage = vec![
        alpha.assembly.source_coverage().clone(),
        beta.assembly.source_coverage().clone(),
    ];
    ready_coverage.sort_by_key(|coverage| {
        (
            coverage.scope.adapter_id.clone(),
            coverage.scope.source_instance_key,
        )
    });
    readiness.publish_ready(snapshot, ready_coverage).unwrap();
    readiness.begin_refresh().unwrap();

    let mut next_reducer = initial.reducer_publication().resume_for_refresh();
    assert!(matches!(
        next_reducer
            .upsert_session_assertion(alpha.assertion.clone(), 11)
            .unwrap(),
        CatalogMutation::Updated
    ));
    let refresh = CatalogRefreshPublicationAssembly::assemble(
        &coverage_plan,
        readiness.snapshot(),
        31,
        predecessor.clone(),
        initial.reducer_publication(),
        &prior_history,
        selection(),
        vec![alpha.assembly.clone(), beta.assembly.clone()],
        &next_reducer,
        bindings(&[&alpha, &beta]),
        CatalogPublicationLimits::default(),
    )
    .unwrap();
    let durable_refresh = refresh.prepare_durable().unwrap();
    let reordered_refresh = CatalogRefreshPublicationAssembly::assemble(
        &coverage_plan,
        readiness.snapshot(),
        31,
        predecessor.clone(),
        initial.reducer_publication(),
        &prior_history,
        selection(),
        vec![beta.assembly.clone(), alpha.assembly.clone()],
        &next_reducer,
        bindings(&[&beta, &alpha]),
        CatalogPublicationLimits::default(),
    )
    .unwrap();
    assert_eq!(
        durable_refresh,
        reordered_refresh.prepare_durable().unwrap()
    );
    assert_eq!(
        durable_refresh.contract_version(),
        CATALOG_DURABLE_REFRESH_PUBLICATION_CONTRACT_VERSION
    );
    assert_eq!(durable_refresh.predecessor(), &predecessor);
    assert_eq!(durable_refresh.member_history(), &prior_history);

    // Empty history is itself canonical and valid, but it is not the history
    // authenticated by this predecessor. The mismatch must fail during pure
    // assembly, before a writer or SQLite transaction exists.
    let incomplete_history = CatalogPublicationMemberHistory::from_bindings(&[]).unwrap();
    let error = CatalogRefreshPublicationAssembly::assemble(
        &coverage_plan,
        readiness.snapshot(),
        31,
        predecessor,
        initial.reducer_publication(),
        &incomplete_history,
        selection(),
        vec![alpha.assembly.clone(), beta.assembly.clone()],
        &next_reducer,
        bindings(&[&alpha, &beta]),
        CatalogPublicationLimits::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("predecessor member history"));
}
