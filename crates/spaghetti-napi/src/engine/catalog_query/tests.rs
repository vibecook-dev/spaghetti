use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::*;
use crate::adapter::{
    CanonicalEntityKey, CanonicalFactId, CanonicalSourceInstanceKey, ContractCompleteness,
    ContractVersionSelection, CoverageAbsence, CoverageAbsenceKind, CoverageDeclarationDigest,
    CoverageDomain, CoverageMembershipRevision, CoverageObjectKey, CoveragePosition,
    CoveragePositionKind, CoverageProvenance, CoverageSetCompleteness, CoverageStatus,
    CoverageStreamKey, FactRevisionId, QualifiedUnknownReason, QualifiedValue,
    QualifiedValueQuality, SemanticRevisionRef, SourceCoveragePoint, SourceCoverageSet,
    CONTRACT_VERSION_SELECTION_VERSION, SOURCE_COVERAGE_CONTRACT_VERSION,
};
use crate::catalog_contract::evidence::{
    decode_durable_project_row, CatalogAvailability, CatalogDisclosureClass, CatalogEntityRef,
    CatalogEvidenceOwner, CatalogFieldAuthority, CatalogProjectAssertion, CatalogQualifiedField,
    CatalogReducer, CatalogRetractionCause, CatalogRetractionEvidence, CatalogSessionAssertion,
    CatalogUnknownReferenceReason, IdentityRelationFact, IdentityRelationKind,
};
use crate::catalog_contract::page::{
    CatalogCount, CatalogEntityResolution, CatalogOptionalField, CatalogPortableLiveRow,
    CatalogResolutionRequestBinding,
};
use crate::catalog_contract::publication::{
    CatalogCompleteSourceAssembly, CatalogInitialPublicationAssembly, CatalogPublicationLimits,
    CatalogPublicationMemberRef, CatalogRefreshPublicationAssembly,
    CatalogSourceCompletionRevision, CatalogSourceMembershipRevision,
};
use crate::catalog_contract::query::{
    CatalogTypedUnknownCapability, CATALOG_QUERY_CONTRACT_VERSION,
};
use crate::catalog_contract::{
    CatalogAccessPolicyDigest, CatalogCoveragePlan, CatalogCoveragePlanSource,
    CatalogReadinessPhase, CatalogSnapshotId, CATALOG_PROJECTION_PACK_ID,
    CATALOG_QUERY_PACK_CONTRACT_VERSION,
};
use crate::core::schema;
use crate::engine::catalog_publication::{
    apply_initial_catalog_publication, apply_refresh_catalog_publication,
    CatalogInitialPublicationCommand, CatalogRefreshPublicationCommand,
};
use crate::engine::catalog_state::{
    self, CatalogBuildStateCommand, CatalogCommitHook, CatalogCommitStage, DurableCatalogBuildState,
};

const FIXTURE_ADAPTER: &str = "retained-page-fixture";

struct PublishedCatalog {
    connection: Connection,
    state: DurableCatalogBuildState,
    selection: CatalogQueryContractSelection,
}

struct FailRefreshAt(CatalogCommitStage);

impl CatalogCommitHook for FailRefreshAt {
    fn reach(&self, stage: CatalogCommitStage) -> Result<(), EngineError> {
        if stage == self.0 {
            Err(EngineError::InjectedFailure {
                stage: "catalog refresh test seam",
            })
        } else {
            Ok(())
        }
    }
}

struct ObserveRefreshIsolation {
    database_path: PathBuf,
    snapshot_commit: i64,
    snapshot_entries: i64,
    observed: Cell<bool>,
}

impl CatalogCommitHook for ObserveRefreshIsolation {
    fn reach(&self, stage: CatalogCommitStage) -> Result<(), EngineError> {
        if stage != CatalogCommitStage::AfterBuildStateWrite {
            return Ok(());
        }
        let observer = Connection::open(&self.database_path).unwrap();
        let (last_commit, refreshing): (i64, Option<i64>) = observer
            .query_row(
                "SELECT last_commit_seq, refreshing_from_snapshot_commit FROM catalog_build_state WHERE scope_kind = 'library'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(last_commit, self.snapshot_commit);
        assert_eq!(refreshing, None);
        assert_eq!(
            observer
                .query_row("SELECT COUNT(*) FROM catalog_snapshots", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            observer
                .query_row("SELECT COUNT(*) FROM catalog_snapshot_entries", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            self.snapshot_entries
        );
        self.observed.set(true);
        Ok(())
    }
}

fn durable_selection() -> ContractVersionSelection {
    ContractVersionSelection {
        selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
        model_major: 1,
        external_entity_reference_version: 1,
        semantic_revision_reference_version: 1,
        coverage_contract_version: SOURCE_COVERAGE_CONTRACT_VERSION,
        fact_family_versions: BTreeMap::from([
            ("catalog.project".to_owned(), 1),
            ("catalog.session".to_owned(), 1),
        ]),
        query_pack_version: Some(CATALOG_QUERY_PACK_CONTRACT_VERSION),
        observation_contract_version: None,
    }
}

fn query_selection() -> CatalogQueryContractSelection {
    CatalogQueryContractSelection {
        catalog_query_contract_version: CATALOG_QUERY_CONTRACT_VERSION,
        contract_versions: durable_selection(),
        typed_unknown: CatalogTypedUnknownCapability::preserving(4_096).unwrap(),
    }
}

fn semantic_revision(owner: &CatalogEvidenceOwner, label: &str) -> SemanticRevisionRef {
    let fact_id = CanonicalFactId::native(
        &owner.adapter_id,
        &owner.source_instance_key,
        "catalog.retained-page.fixture",
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
            vec![semantic_revision(owner, label)],
        )
        .unwrap(),
        CatalogDisclosureClass::Public,
    )
    .unwrap()
}

fn sensitive_text(owner: &CatalogEvidenceOwner, label: &str) -> CatalogQualifiedField<String> {
    CatalogQualifiedField::new(
        QualifiedValue::from_parts(
            Some(format!("private-{label}")),
            QualifiedValueQuality::NativeClaimed,
            CatalogFieldAuthority::new("native-private", 90, true).unwrap(),
            ContractCompleteness::Complete,
            None,
            None,
            vec![semantic_revision(owner, label)],
        )
        .unwrap(),
        CatalogDisclosureClass::LocalSensitive,
    )
    .unwrap()
}

fn database() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    schema::initialize_schema(&connection).unwrap();
    connection
}

fn schedule_build(
    connection: &mut Connection,
    plan: &CatalogCoveragePlan,
) -> crate::catalog_contract::CatalogReadinessSnapshot {
    catalog_state::apply_catalog_build_state_commit(
        connection,
        &CatalogBuildStateCommand::register(plan.clone(), 1, 10, 11),
    )
    .unwrap()
    .unwrap();
    let pending = catalog_state::load_catalog_build_state(connection)
        .unwrap()
        .unwrap();
    catalog_state::apply_catalog_build_state_commit(
        connection,
        &CatalogBuildStateCommand::schedule(pending.expectation().unwrap(), 20, 21),
    )
    .unwrap()
    .unwrap()
    .readiness
}

fn publish_catalog(project_count: usize, session_count: usize) -> PublishedCatalog {
    publish_catalog_in(database(), project_count, session_count)
}

fn publish_catalog_in(
    mut connection: Connection,
    project_count: usize,
    session_count: usize,
) -> PublishedCatalog {
    let selection = query_selection();
    let (plan, assembly) = if project_count == 0 && session_count == 0 {
        let plan = CatalogCoveragePlan::new(CatalogCoverageScope::Library, Vec::new(), Vec::new())
            .unwrap();
        let building = schedule_build(&mut connection, &plan);
        let assembly = CatalogInitialPublicationAssembly::assemble(
            &plan,
            &building,
            durable_selection(),
            Vec::new(),
            &CatalogReducer::default(),
            Vec::new(),
            CatalogPublicationLimits::default(),
        )
        .unwrap();
        (plan, assembly)
    } else {
        let source_instance_key =
            CanonicalSourceInstanceKey::derive(1, b"retained-page-source").unwrap();
        let stream_key = CoverageStreamKey::derive(FIXTURE_ADAPTER, b"catalog-rows").unwrap();
        let object_key = CoverageObjectKey::derive("catalog-rows", b"all").unwrap();
        let owner = CatalogEvidenceOwner::new(
            FIXTURE_ADAPTER,
            source_instance_key,
            stream_key,
            object_key,
            1,
        )
        .unwrap();
        let plan_source = CatalogCoveragePlanSource::new(
            FIXTURE_ADAPTER,
            source_instance_key,
            "retained-page-support@candidate-v1",
            CoverageDeclarationDigest::derive(b"retained-page-declaration").unwrap(),
            CatalogAccessPolicyDigest::derive(1, b"retained-page-withheld-policy").unwrap(),
        )
        .unwrap();
        let plan = CatalogCoveragePlan::new(
            CatalogCoverageScope::Library,
            vec![plan_source.clone()],
            Vec::new(),
        )
        .unwrap();
        let building = schedule_build(&mut connection, &plan);
        let domain = CoverageDomain::ProjectionPack {
            pack: CATALOG_PROJECTION_PACK_ID.to_owned(),
            version: CATALOG_QUERY_PACK_CONTRACT_VERSION,
        };
        let coverage = SourceCoverageSet::new(
            domain.clone(),
            plan_source.coverage_scope(CatalogCoverageScope::Library),
            CoverageMembershipRevision::derive(b"retained-page-membership").unwrap(),
            vec![SourceCoveragePoint::new(
                domain,
                FIXTURE_ADAPTER,
                source_instance_key,
                stream_key,
                object_key,
                1,
                Some(
                    CoveragePosition::derive(
                        CoveragePositionKind::SnapshotRevision,
                        b"retained-page-position",
                        None,
                    )
                    .unwrap(),
                ),
                CoverageStatus::ExactSnapshot,
                CoverageProvenance::default(),
            )
            .unwrap()],
            Vec::new(),
            Vec::new(),
            CoverageSetCompleteness::Complete,
        )
        .unwrap();
        let member_refs = (0..session_count)
            .map(|index| {
                CatalogPublicationMemberRef::from_digest(
                    *blake3::hash(format!("retained-member-{index}").as_bytes()).as_bytes(),
                )
            })
            .collect::<Vec<_>>();
        let source = CatalogCompleteSourceAssembly::from_complete_library_coverage(
            plan_source,
            durable_selection(),
            "catalog-session-identity-v1",
            CatalogSourceMembershipRevision::from_digest(
                *blake3::hash(b"retained-source-membership").as_bytes(),
            ),
            CatalogSourceCompletionRevision::from_digest(
                *blake3::hash(b"retained-source-completion").as_bytes(),
            ),
            member_refs.clone(),
            coverage,
        )
        .unwrap();
        let mut reducer = CatalogReducer::default();
        for index in 0..project_count {
            let label = format!("project-{index}");
            let project_ref = CatalogEntityRef::project(
                CanonicalEntityKey::derive(
                    FIXTURE_ADAPTER,
                    &source_instance_key,
                    "project",
                    label.as_bytes(),
                )
                .unwrap(),
            );
            reducer
                .upsert_project_assertion(
                    CatalogProjectAssertion::new(
                        owner.clone(),
                        label.as_bytes(),
                        project_ref,
                        None,
                        None,
                        None,
                        Some(sensitive_text(&owner, &label)),
                        None,
                        availability(&owner, &format!("{label}-availability")),
                        vec![semantic_revision(&owner, &format!("{label}-assertion"))],
                    )
                    .unwrap(),
                    10,
                )
                .unwrap();
        }
        let mut bindings = Vec::new();
        for (index, member_ref) in member_refs.into_iter().enumerate() {
            let label = format!("session-{index}");
            let session_ref = CatalogEntityRef::session(
                CanonicalEntityKey::derive(
                    FIXTURE_ADAPTER,
                    &source_instance_key,
                    "session",
                    label.as_bytes(),
                )
                .unwrap(),
            );
            let assertion = CatalogSessionAssertion::new(
                owner.clone(),
                label.as_bytes(),
                session_ref,
                None,
                Some(sensitive_text(&owner, &label)),
                None,
                None,
                None,
                None,
                None,
                availability(&owner, &format!("{label}-availability")),
                vec![semantic_revision(&owner, &format!("{label}-assertion"))],
            )
            .unwrap();
            bindings.push(
                source
                    .member_binding(member_ref, assertion.assertion_key, session_ref)
                    .unwrap(),
            );
            reducer.upsert_session_assertion(assertion, 10).unwrap();
        }
        let assembly = CatalogInitialPublicationAssembly::assemble(
            &plan,
            &building,
            durable_selection(),
            vec![source],
            &reducer,
            bindings,
            CatalogPublicationLimits::default(),
        )
        .unwrap();
        (plan, assembly)
    };
    let building_state = catalog_state::load_catalog_build_state(&connection)
        .unwrap()
        .unwrap();
    let receipt = apply_initial_catalog_publication(
        &mut connection,
        &CatalogInitialPublicationCommand::new(assembly, building_state.last_commit_seq, 30, 31),
    )
    .unwrap()
    .unwrap();
    assert_eq!(receipt.readiness.state, CatalogReadinessPhase::Ready);
    let state = catalog_state::load_catalog_build_state(&connection)
        .unwrap()
        .unwrap();
    assert_eq!(state.plan, plan);
    PublishedCatalog {
        connection,
        state,
        selection,
    }
}

fn refresh_catalog(
    published: &mut PublishedCatalog,
    project_count: usize,
    session_count: usize,
) -> CatalogSnapshotId {
    let begin = CatalogBuildStateCommand::begin_refresh(
        published.state.refresh_expectation().unwrap(),
        40,
        41,
    );
    catalog_state::apply_catalog_build_state_commit(&mut published.connection, &begin)
        .unwrap()
        .unwrap();
    let active = catalog_state::load_catalog_build_state(&published.connection)
        .unwrap()
        .unwrap();
    let expected = active.refresh_publication_expectation().unwrap();
    let plan_source = active.plan.required_sources[0].clone();
    let source_instance_key = plan_source.source_instance_key;
    let stream_key = CoverageStreamKey::derive(FIXTURE_ADAPTER, b"catalog-rows").unwrap();
    let object_key = CoverageObjectKey::derive("catalog-rows", b"all").unwrap();
    let owner = CatalogEvidenceOwner::new(
        FIXTURE_ADAPTER,
        source_instance_key,
        stream_key,
        object_key,
        1,
    )
    .unwrap();
    let domain = CoverageDomain::ProjectionPack {
        pack: CATALOG_PROJECTION_PACK_ID.to_owned(),
        version: CATALOG_QUERY_PACK_CONTRACT_VERSION,
    };
    let coverage = SourceCoverageSet::new(
        domain.clone(),
        plan_source.coverage_scope(CatalogCoverageScope::Library),
        CoverageMembershipRevision::derive(b"retained-page-refresh-membership").unwrap(),
        vec![SourceCoveragePoint::new(
            domain,
            FIXTURE_ADAPTER,
            source_instance_key,
            stream_key,
            object_key,
            1,
            Some(
                CoveragePosition::derive(
                    CoveragePositionKind::SnapshotRevision,
                    b"retained-page-refresh-position",
                    None,
                )
                .unwrap(),
            ),
            CoverageStatus::ExactSnapshot,
            CoverageProvenance::default(),
        )
        .unwrap()],
        Vec::new(),
        Vec::new(),
        CoverageSetCompleteness::Complete,
    )
    .unwrap();
    let member_refs = (0..session_count)
        .map(|index| {
            CatalogPublicationMemberRef::from_digest(
                *blake3::hash(format!("retained-member-{index}").as_bytes()).as_bytes(),
            )
        })
        .collect::<Vec<_>>();
    let source = CatalogCompleteSourceAssembly::from_complete_library_coverage(
        plan_source,
        durable_selection(),
        "catalog-session-identity-v1",
        CatalogSourceMembershipRevision::from_digest(
            *blake3::hash(b"retained-source-refresh-membership").as_bytes(),
        ),
        CatalogSourceCompletionRevision::from_digest(
            *blake3::hash(b"retained-source-refresh-completion").as_bytes(),
        ),
        member_refs.clone(),
        coverage,
    )
    .unwrap();
    let mut reducer = expected.resume_reducer();
    for index in 0..project_count {
        let label = format!("project-{index}");
        let project_ref = CatalogEntityRef::project(
            CanonicalEntityKey::derive(
                FIXTURE_ADAPTER,
                &source_instance_key,
                "project",
                label.as_bytes(),
            )
            .unwrap(),
        );
        reducer
            .upsert_project_assertion(
                CatalogProjectAssertion::new(
                    owner.clone(),
                    label.as_bytes(),
                    project_ref,
                    None,
                    None,
                    None,
                    Some(sensitive_text(&owner, &label)),
                    None,
                    availability(&owner, &format!("{label}-availability")),
                    vec![semantic_revision(&owner, &format!("{label}-assertion"))],
                )
                .unwrap(),
                11,
            )
            .unwrap();
    }
    let mut bindings = Vec::with_capacity(session_count);
    for (index, member_ref) in member_refs.into_iter().enumerate() {
        let label = format!("session-{index}");
        let session_ref = CatalogEntityRef::session(
            CanonicalEntityKey::derive(
                FIXTURE_ADAPTER,
                &source_instance_key,
                "session",
                label.as_bytes(),
            )
            .unwrap(),
        );
        let assertion = CatalogSessionAssertion::new(
            owner.clone(),
            label.as_bytes(),
            session_ref,
            None,
            Some(sensitive_text(&owner, &label)),
            None,
            None,
            None,
            None,
            None,
            availability(&owner, &format!("{label}-availability")),
            vec![semantic_revision(&owner, &format!("{label}-assertion"))],
        )
        .unwrap();
        bindings.push(
            source
                .member_binding(member_ref, assertion.assertion_key, session_ref)
                .unwrap(),
        );
        reducer.upsert_session_assertion(assertion, 11).unwrap();
    }
    let assembly = CatalogRefreshPublicationAssembly::assemble(
        &active.plan,
        &active.readiness,
        expected.refresh_started_commit_seq(),
        expected.predecessor().unwrap(),
        expected.prior_reducer(),
        expected.prior_member_history(),
        durable_selection(),
        vec![source],
        &reducer,
        bindings,
        CatalogPublicationLimits::default(),
    )
    .unwrap();
    let receipt = apply_refresh_catalog_publication(
        &mut published.connection,
        &CatalogRefreshPublicationCommand::new(assembly, expected, 50, 51),
    )
    .unwrap()
    .unwrap();
    published.state = catalog_state::load_catalog_build_state(&published.connection)
        .unwrap()
        .unwrap();
    receipt.snapshot_id
}

fn lifecycle_session_assertion(
    owner: &CatalogEvidenceOwner,
    native_key: &str,
    session_ref: CatalogEntityRef,
) -> CatalogSessionAssertion {
    CatalogSessionAssertion::new(
        owner.clone(),
        native_key.as_bytes(),
        session_ref,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        availability(owner, &format!("{native_key}-availability")),
        vec![semantic_revision(owner, &format!("{native_key}-assertion"))],
    )
    .unwrap()
}

fn publish_lifecycle_catalog(connection: &mut Connection) -> [CatalogEntityRef; 3] {
    schema::initialize_schema(connection).unwrap();
    let selection = durable_selection();
    let source_instance_key =
        CanonicalSourceInstanceKey::derive(1, b"retained-lifecycle-source").unwrap();
    let stream_key = CoverageStreamKey::derive(FIXTURE_ADAPTER, b"catalog-lifecycle").unwrap();
    let live_object = CoverageObjectKey::derive("catalog-lifecycle", b"live").unwrap();
    let deleted_object = CoverageObjectKey::derive("catalog-lifecycle", b"deleted").unwrap();
    let live_owner = CatalogEvidenceOwner::new(
        FIXTURE_ADAPTER,
        source_instance_key,
        stream_key,
        live_object,
        1,
    )
    .unwrap();
    let deleted_owner = CatalogEvidenceOwner::new(
        FIXTURE_ADAPTER,
        source_instance_key,
        stream_key,
        deleted_object,
        1,
    )
    .unwrap();
    let plan_source = CatalogCoveragePlanSource::new(
        FIXTURE_ADAPTER,
        source_instance_key,
        "retained-lifecycle-support@candidate-v1",
        CoverageDeclarationDigest::derive(b"retained-lifecycle-declaration").unwrap(),
        CatalogAccessPolicyDigest::derive(1, b"retained-lifecycle-withheld-policy").unwrap(),
    )
    .unwrap();
    let plan = CatalogCoveragePlan::new(
        CatalogCoverageScope::Library,
        vec![plan_source.clone()],
        Vec::new(),
    )
    .unwrap();
    let building = schedule_build(connection, &plan);
    let domain = CoverageDomain::ProjectionPack {
        pack: CATALOG_PROJECTION_PACK_ID.to_owned(),
        version: CATALOG_QUERY_PACK_CONTRACT_VERSION,
    };
    let coverage = SourceCoverageSet::new(
        domain.clone(),
        plan_source.coverage_scope(CatalogCoverageScope::Library),
        CoverageMembershipRevision::derive(b"retained-lifecycle-membership").unwrap(),
        vec![SourceCoveragePoint::new(
            domain,
            FIXTURE_ADAPTER,
            source_instance_key,
            stream_key,
            live_object,
            1,
            Some(
                CoveragePosition::derive(
                    CoveragePositionKind::SnapshotRevision,
                    b"retained-lifecycle-position",
                    None,
                )
                .unwrap(),
            ),
            CoverageStatus::ExactSnapshot,
            CoverageProvenance::default(),
        )
        .unwrap()],
        vec![CoverageAbsence {
            stream_key,
            object_key: deleted_object,
            generation: 1,
            kind: CoverageAbsenceKind::Deleted,
        }],
        Vec::new(),
        CoverageSetCompleteness::Complete,
    )
    .unwrap();
    let member_refs = [
        CatalogPublicationMemberRef::from_digest(
            *blake3::hash(b"retained-lifecycle-prior-member").as_bytes(),
        ),
        CatalogPublicationMemberRef::from_digest(
            *blake3::hash(b"retained-lifecycle-replacement-member").as_bytes(),
        ),
    ];
    let source = CatalogCompleteSourceAssembly::from_complete_library_coverage(
        plan_source,
        selection.clone(),
        "catalog-session-identity-v1",
        CatalogSourceMembershipRevision::from_digest(
            *blake3::hash(b"retained-lifecycle-source-membership").as_bytes(),
        ),
        CatalogSourceCompletionRevision::from_digest(
            *blake3::hash(b"retained-lifecycle-source-completion").as_bytes(),
        ),
        member_refs.to_vec(),
        coverage,
    )
    .unwrap();

    let prior_ref = CatalogEntityRef::session(
        CanonicalEntityKey::derive(
            FIXTURE_ADAPTER,
            &source_instance_key,
            "session",
            b"retained-lifecycle-prior",
        )
        .unwrap(),
    );
    let replacement_ref = CatalogEntityRef::session(
        CanonicalEntityKey::derive(
            FIXTURE_ADAPTER,
            &source_instance_key,
            "session",
            b"retained-lifecycle-replacement",
        )
        .unwrap(),
    );
    let deleted_ref = CatalogEntityRef::session(
        CanonicalEntityKey::derive(
            FIXTURE_ADAPTER,
            &source_instance_key,
            "session",
            b"retained-lifecycle-deleted",
        )
        .unwrap(),
    );
    let prior = lifecycle_session_assertion(&live_owner, "lifecycle-prior", prior_ref);
    let replacement =
        lifecycle_session_assertion(&live_owner, "lifecycle-replacement", replacement_ref);
    let deleted = lifecycle_session_assertion(&deleted_owner, "lifecycle-deleted", deleted_ref);
    let bindings = vec![
        source
            .member_binding(member_refs[0], prior.assertion_key, prior_ref)
            .unwrap(),
        source
            .member_binding(member_refs[1], replacement.assertion_key, replacement_ref)
            .unwrap(),
    ];
    let replacement_relation = IdentityRelationFact::new(
        live_owner.clone(),
        b"retained-lifecycle-replacement-relation",
        IdentityRelationKind::ReplacedBy,
        prior_ref,
        replacement_ref,
        CatalogFieldAuthority::new("native-replacement", 100, true).unwrap(),
        QualifiedValueQuality::Exact,
        ContractCompleteness::Complete,
        None,
        None,
        vec![semantic_revision(
            &live_owner,
            "retained-lifecycle-replacement-relation",
        )],
    )
    .unwrap();
    let deletion = CatalogRetractionEvidence::new(
        deleted_owner.clone(),
        CatalogRetractionCause::ConfirmedDeletion,
        ContractCompleteness::Complete,
        vec![semantic_revision(
            &deleted_owner,
            "retained-lifecycle-deletion",
        )],
    )
    .unwrap();
    let mut reducer = CatalogReducer::default();
    reducer.upsert_session_assertion(prior, 10).unwrap();
    reducer.upsert_session_assertion(replacement, 11).unwrap();
    reducer.upsert_session_assertion(deleted, 12).unwrap();
    reducer
        .upsert_identity_relation(replacement_relation, 13)
        .unwrap();
    reducer.retract_owner(&deletion, 20).unwrap();
    reducer.confirm_absent(deleted_ref, &deletion, 21).unwrap();
    let assembly = CatalogInitialPublicationAssembly::assemble(
        &plan,
        &building,
        selection,
        vec![source],
        &reducer,
        bindings,
        CatalogPublicationLimits::default(),
    )
    .unwrap();
    let building_state = catalog_state::load_catalog_build_state(connection)
        .unwrap()
        .unwrap();
    apply_initial_catalog_publication(
        connection,
        &CatalogInitialPublicationCommand::new(assembly, building_state.last_commit_seq, 30, 31),
    )
    .unwrap()
    .unwrap();
    [deleted_ref, prior_ref, replacement_ref]
}

#[test]
fn ordinary_refresh_start_retains_exact_ready_pages_resolution_and_outbox() {
    let mut published = publish_catalog(2, 2);
    let snapshot_id = published
        .state
        .ready_read_authority()
        .unwrap()
        .snapshot_id();
    let source_coverage = published.state.readiness.source_coverage.clone();
    let snapshot_entries: i64 = published
        .connection
        .query_row("SELECT COUNT(*) FROM catalog_snapshot_entries", [], |row| {
            row.get(0)
        })
        .unwrap();
    let expected = published.state.refresh_expectation().unwrap();
    let command = CatalogBuildStateCommand::begin_refresh(expected, 40, 41);
    let receipt =
        catalog_state::apply_catalog_build_state_commit(&mut published.connection, &command)
            .unwrap()
            .unwrap();
    assert_eq!(receipt.commit_seq, snapshot_id.complete_commit + 1);
    assert_eq!(receipt.readiness.state, CatalogReadinessPhase::Ready);
    assert_eq!(receipt.readiness.last_complete_snapshot, Some(snapshot_id));
    assert_eq!(
        receipt.readiness.refreshing_from_snapshot,
        Some(snapshot_id)
    );
    assert_eq!(
        receipt.readiness.complete_through_commit,
        Some(snapshot_id.complete_commit)
    );

    let state = catalog_state::load_catalog_build_state(&published.connection)
        .unwrap()
        .unwrap();
    assert_eq!(state.last_commit_seq, receipt.commit_seq);
    assert_eq!(state.readiness, receipt.readiness);
    assert_eq!(state.readiness.source_coverage, source_coverage);
    assert_eq!(
        published
            .connection
            .query_row("SELECT COUNT(*) FROM catalog_snapshots", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        published
            .connection
            .query_row("SELECT COUNT(*) FROM catalog_snapshot_entries", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        snapshot_entries
    );

    let (source, reason, fact_count): (Option<i64>, String, i64) = published
        .connection
        .query_row(
            "SELECT source_instance_id, reason, fact_count FROM ingest_commits WHERE commit_seq = ?1",
            [i64::try_from(receipt.commit_seq).unwrap()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(source, None);
    assert_eq!(reason, "catalog.library.refresh.started");
    assert_eq!(fact_count, 0);
    let (schema_version, payload): (i64, Vec<u8>) = published
        .connection
        .query_row(
            "SELECT schema_version, payload FROM change_log WHERE commit_seq = ?1",
            [i64::try_from(receipt.commit_seq).unwrap()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(schema_version, 3);
    let payload: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(payload["state"], "ready");
    assert_eq!(payload["commit_seq"], receipt.commit_seq);
    assert_eq!(
        payload["complete_through_commit"],
        snapshot_id.complete_commit
    );
    assert_eq!(
        payload["last_complete_snapshot"],
        payload["refreshing_from_snapshot"]
    );
    assert!(payload.get("source_coverage").is_none());
    assert!(!payload.to_string().contains("private-"));

    let authority = state.ready_read_authority().unwrap();
    assert_eq!(authority.snapshot_id(), snapshot_id);
    assert_eq!(authority.readiness().refreshing_from_snapshot, None);
    let before_query_changes: i64 = published
        .connection
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();
    let page = read_retained_catalog_page(
        &published.connection,
        &authority,
        &CatalogRetainedPageRequest::projects_all(
            published.selection.clone(),
            snapshot_id,
            10,
            None,
        ),
    )
    .unwrap();
    let CatalogRetainedPage::Projects(page) = page else {
        panic!("expected retained project page during refresh");
    };
    assert_eq!(page.rows.len(), 2);
    assert_eq!(page.published_readiness.refreshing_from_snapshot, None);

    let source_instance_key =
        CanonicalSourceInstanceKey::derive(1, b"retained-page-source").unwrap();
    let project_ref = CatalogEntityRef::project(
        CanonicalEntityKey::derive(
            FIXTURE_ADAPTER,
            &source_instance_key,
            "project",
            b"project-0",
        )
        .unwrap(),
    );
    let resolution = resolve_retained_catalog_entity(
        &published.connection,
        &authority,
        &CatalogResolutionRequestBinding::new(
            published.selection.clone(),
            snapshot_id,
            project_ref.external_ref,
        )
        .unwrap(),
    )
    .unwrap();
    assert!(matches!(
        resolution.resolution,
        CatalogEntityResolution::Live { .. }
    ));
    assert_eq!(
        published
            .connection
            .query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        before_query_changes
    );

    assert!(
        catalog_state::apply_catalog_build_state_commit(&mut published.connection, &command,)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        published
            .connection
            .query_row("SELECT COUNT(*) FROM ingest_commits", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        4
    );
}

#[test]
fn ordinary_refresh_start_rolls_back_every_precommit_seam_and_replays_lost_ack() {
    const PRECOMMIT_STAGES: [CatalogCommitStage; 6] = [
        CatalogCommitStage::BeforeTransaction,
        CatalogCommitStage::AfterCommitInsert,
        CatalogCommitStage::AfterPlanWrite,
        CatalogCommitStage::AfterBuildStateWrite,
        CatalogCommitStage::AfterOutboxInsert,
        CatalogCommitStage::BeforeCommit,
    ];
    let mut published = publish_catalog(1, 1);
    let snapshot_id = published
        .state
        .ready_read_authority()
        .unwrap()
        .snapshot_id();
    let snapshot_entries: i64 = published
        .connection
        .query_row("SELECT COUNT(*) FROM catalog_snapshot_entries", [], |row| {
            row.get(0)
        })
        .unwrap();
    let command = CatalogBuildStateCommand::begin_refresh(
        published.state.refresh_expectation().unwrap(),
        40,
        41,
    );
    for stage in PRECOMMIT_STAGES {
        assert!(catalog_state::apply_catalog_build_state_commit_with_hook(
            &mut published.connection,
            &command,
            &FailRefreshAt(stage),
        )
        .is_err());
        let retained = catalog_state::load_catalog_build_state(&published.connection)
            .unwrap()
            .unwrap();
        assert_eq!(retained.last_commit_seq, snapshot_id.complete_commit);
        assert_eq!(retained.readiness.refreshing_from_snapshot, None);
        assert_eq!(
            published
                .connection
                .query_row("SELECT COUNT(*) FROM ingest_commits", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(
            published
                .connection
                .query_row("SELECT COUNT(*) FROM change_log", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(
            published
                .connection
                .query_row("SELECT COUNT(*) FROM catalog_snapshot_entries", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            snapshot_entries
        );
    }

    assert!(catalog_state::apply_catalog_build_state_commit_with_hook(
        &mut published.connection,
        &command,
        &FailRefreshAt(CatalogCommitStage::AfterCommit),
    )
    .is_err());
    let retained = catalog_state::load_catalog_build_state(&published.connection)
        .unwrap()
        .unwrap();
    assert_eq!(
        retained.readiness.refreshing_from_snapshot,
        Some(snapshot_id)
    );
    assert_eq!(retained.last_commit_seq, snapshot_id.complete_commit + 1);
    assert!(
        catalog_state::apply_catalog_build_state_commit(&mut published.connection, &command,)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        published
            .connection
            .query_row("SELECT COUNT(*) FROM ingest_commits", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        4
    );
}

#[test]
fn ordinary_refresh_start_is_isolated_and_restarts_with_exact_retained_authority() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("catalog-refresh.sqlite");
    let connection = Connection::open(&database_path).unwrap();
    schema::initialize_schema(&connection).unwrap();
    let mut published = publish_catalog_in(connection, 2, 1);
    let snapshot_id = published
        .state
        .ready_read_authority()
        .unwrap()
        .snapshot_id();
    let snapshot_entries: i64 = published
        .connection
        .query_row("SELECT COUNT(*) FROM catalog_snapshot_entries", [], |row| {
            row.get(0)
        })
        .unwrap();
    let command = CatalogBuildStateCommand::begin_refresh(
        published.state.refresh_expectation().unwrap(),
        40,
        41,
    );
    let hook = ObserveRefreshIsolation {
        database_path: database_path.clone(),
        snapshot_commit: i64::try_from(snapshot_id.complete_commit).unwrap(),
        snapshot_entries,
        observed: Cell::new(false),
    };
    catalog_state::apply_catalog_build_state_commit_with_hook(
        &mut published.connection,
        &command,
        &hook,
    )
    .unwrap()
    .unwrap();
    assert!(hook.observed.get());
    drop(published.connection);

    let connection = Connection::open(&database_path).unwrap();
    let state = catalog_state::load_catalog_build_state(&connection)
        .unwrap()
        .unwrap();
    assert_eq!(state.readiness.state, CatalogReadinessPhase::Ready);
    assert_eq!(state.readiness.last_complete_snapshot, Some(snapshot_id));
    assert_eq!(state.readiness.refreshing_from_snapshot, Some(snapshot_id));
    assert_eq!(state.last_commit_seq, snapshot_id.complete_commit + 1);
    let authority = state.ready_read_authority().unwrap();
    assert_eq!(authority.snapshot_id(), snapshot_id);
    assert_eq!(authority.readiness().refreshing_from_snapshot, None);
    let before_changes: i64 = connection
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();
    let page = read_retained_catalog_page(
        &connection,
        &authority,
        &CatalogRetainedPageRequest::sessions_all(published.selection, snapshot_id, 10, None),
    )
    .unwrap();
    let CatalogRetainedPage::Sessions(page) = page else {
        panic!("expected retained session page after refresh restart");
    };
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.published_readiness.refreshing_from_snapshot, None);
    assert_eq!(
        connection
            .query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        before_changes
    );
}

#[test]
fn ordinary_refresh_start_rejects_foreign_publication_and_invalid_lineage_without_mutation() {
    let mut published = publish_catalog(1, 1);
    let foreign = publish_catalog(2, 1);
    let foreign_plan = publish_catalog(0, 0);
    assert_eq!(
        published
            .state
            .ready_read_authority()
            .unwrap()
            .snapshot_id(),
        foreign.state.ready_read_authority().unwrap().snapshot_id()
    );
    let before_changes: i64 = published
        .connection
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();
    let foreign_command = CatalogBuildStateCommand::begin_refresh(
        foreign.state.refresh_expectation().unwrap(),
        40,
        41,
    );
    assert!(catalog_state::apply_catalog_build_state_commit(
        &mut published.connection,
        &foreign_command,
    )
    .is_err());
    assert_eq!(
        published
            .connection
            .query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        before_changes
    );
    assert_eq!(
        catalog_state::load_catalog_build_state(&published.connection)
            .unwrap()
            .unwrap()
            .readiness
            .refreshing_from_snapshot,
        None
    );

    let foreign_plan_command = CatalogBuildStateCommand::begin_refresh(
        foreign_plan.state.refresh_expectation().unwrap(),
        40,
        41,
    );
    assert!(catalog_state::apply_catalog_build_state_commit(
        &mut published.connection,
        &foreign_plan_command,
    )
    .is_err());

    let invalid_time = CatalogBuildStateCommand::begin_refresh(
        published.state.refresh_expectation().unwrap(),
        42,
        41,
    );
    assert!(catalog_state::apply_catalog_build_state_commit(
        &mut published.connection,
        &invalid_time,
    )
    .is_err());

    let mut pending = database();
    let pending_plan =
        CatalogCoveragePlan::new(CatalogCoverageScope::Library, Vec::new(), Vec::new()).unwrap();
    catalog_state::apply_catalog_build_state_commit(
        &mut pending,
        &CatalogBuildStateCommand::register(pending_plan, 1, 10, 11),
    )
    .unwrap();
    assert!(catalog_state::load_catalog_build_state(&pending)
        .unwrap()
        .unwrap()
        .refresh_expectation()
        .is_err());

    let exact = CatalogBuildStateCommand::begin_refresh(
        published.state.refresh_expectation().unwrap(),
        40,
        41,
    );
    catalog_state::apply_catalog_build_state_commit(&mut published.connection, &exact)
        .unwrap()
        .unwrap();
    assert!(
        catalog_state::load_catalog_build_state(&published.connection)
            .unwrap()
            .unwrap()
            .refresh_expectation()
            .is_err()
    );
    assert!(catalog_state::apply_catalog_build_state_commit(
        &mut published.connection,
        &foreign_command,
    )
    .is_err());
}

#[test]
fn ordinary_refresh_start_supports_source_free_ready_and_rejects_forged_restart_lineage() {
    let mut published = publish_catalog(0, 0);
    let snapshot_id = published
        .state
        .ready_read_authority()
        .unwrap()
        .snapshot_id();
    assert!(published
        .connection
        .execute(
            "UPDATE catalog_build_state SET refreshing_from_snapshot_commit = ?1 WHERE scope_kind = 'library'",
            [i64::try_from(snapshot_id.complete_commit).unwrap()],
        )
        .is_err());
    let command = CatalogBuildStateCommand::begin_refresh(
        published.state.refresh_expectation().unwrap(),
        40,
        41,
    );
    catalog_state::apply_catalog_build_state_commit(&mut published.connection, &command)
        .unwrap()
        .unwrap();
    let state = catalog_state::load_catalog_build_state(&published.connection)
        .unwrap()
        .unwrap();
    let authority = state.ready_read_authority().unwrap();
    let page = read_retained_catalog_page(
        &published.connection,
        &authority,
        &CatalogRetainedPageRequest::projects_all(published.selection, snapshot_id, 10, None),
    )
    .unwrap();
    let CatalogRetainedPage::Projects(page) = page else {
        panic!("expected source-free retained project page during refresh");
    };
    assert!(page.rows.is_empty());

    published
        .connection
        .execute_batch("PRAGMA ignore_check_constraints = ON")
        .unwrap();
    published
        .connection
        .execute(
            "UPDATE catalog_build_state SET last_commit_seq = ?1 WHERE scope_kind = 'library'",
            [i64::try_from(snapshot_id.complete_commit).unwrap()],
        )
        .unwrap();
    let error = catalog_state::load_catalog_build_state(&published.connection).unwrap_err();
    let error = error.to_string();
    assert!(
        error.contains("catalog build lineage is not owned by the expected source-neutral commit"),
        "unexpected restart rejection: {error}"
    );
}

#[test]
fn refresh_successor_keeps_old_authority_and_publishes_new_pages_and_resolution() {
    let mut published = publish_catalog(1, 2);
    let old_authority = published.state.ready_read_authority().unwrap();
    let old_snapshot = old_authority.snapshot_id();
    let first_old_page = read_retained_catalog_page(
        &published.connection,
        &old_authority,
        &CatalogRetainedPageRequest::sessions_all(
            published.selection.clone(),
            old_snapshot,
            1,
            None,
        ),
    )
    .unwrap();
    let CatalogRetainedPage::Sessions(first_old_page) = first_old_page else {
        panic!("expected old session page")
    };
    assert_eq!(first_old_page.rows.len(), 1);
    assert!(first_old_page.has_more);
    let old_continuation = first_old_page.next_continuation.unwrap();

    let new_snapshot = refresh_catalog(&mut published, 2, 3);
    assert!(new_snapshot.complete_commit > old_snapshot.complete_commit);
    assert_eq!(new_snapshot.coverage_plan_id, old_snapshot.coverage_plan_id);
    assert_eq!(new_snapshot.readiness_epoch, old_snapshot.readiness_epoch);

    let second_old_page = read_retained_catalog_page(
        &published.connection,
        &old_authority,
        &CatalogRetainedPageRequest::sessions_all(
            published.selection.clone(),
            old_snapshot,
            1,
            Some(old_continuation),
        ),
    )
    .unwrap();
    let CatalogRetainedPage::Sessions(second_old_page) = second_old_page else {
        panic!("expected retained predecessor session page")
    };
    assert_eq!(second_old_page.rows.len(), 1);
    assert!(!second_old_page.has_more);
    assert_eq!(
        second_old_page.total_count,
        CatalogCount::Known { value: 2 }
    );

    let new_authority = published.state.ready_read_authority().unwrap();
    assert_eq!(new_authority.snapshot_id(), new_snapshot);
    let new_sessions = read_retained_catalog_page(
        &published.connection,
        &new_authority,
        &CatalogRetainedPageRequest::sessions_all(
            published.selection.clone(),
            new_snapshot,
            10,
            None,
        ),
    )
    .unwrap();
    let CatalogRetainedPage::Sessions(new_sessions) = new_sessions else {
        panic!("expected successor session page")
    };
    assert_eq!(new_sessions.rows.len(), 3);
    assert_eq!(new_sessions.total_count, CatalogCount::Known { value: 3 });
    let new_projects = read_retained_catalog_page(
        &published.connection,
        &new_authority,
        &CatalogRetainedPageRequest::projects_all(
            published.selection.clone(),
            new_snapshot,
            10,
            None,
        ),
    )
    .unwrap();
    let CatalogRetainedPage::Projects(new_projects) = new_projects else {
        panic!("expected successor project page")
    };
    assert_eq!(new_projects.rows.len(), 2);

    let source_instance_key =
        CanonicalSourceInstanceKey::derive(1, b"retained-page-source").unwrap();
    let new_session = CatalogEntityRef::session(
        CanonicalEntityKey::derive(
            FIXTURE_ADAPTER,
            &source_instance_key,
            "session",
            b"session-2",
        )
        .unwrap(),
    );
    let current_request = CatalogResolutionRequestBinding::new(
        published.selection.clone(),
        new_snapshot,
        new_session.external_ref,
    )
    .unwrap();
    assert!(matches!(
        resolve_retained_catalog_entity(&published.connection, &new_authority, &current_request,)
            .unwrap()
            .resolution,
        CatalogEntityResolution::Live { .. }
    ));
    let old_request = CatalogResolutionRequestBinding::new(
        published.selection.clone(),
        old_snapshot,
        new_session.external_ref,
    )
    .unwrap();
    assert!(matches!(
        resolve_retained_catalog_entity(&published.connection, &old_authority, &old_request)
            .unwrap()
            .resolution,
        CatalogEntityResolution::Unknown {
            reason: CatalogUnknownReferenceReason::NeverObserved,
            ..
        }
    ));
}

#[test]
fn refresh_successor_reopens_only_the_current_authority_while_retaining_both_snapshots() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("catalog-refresh-successor.sqlite");
    let connection = Connection::open(&database_path).unwrap();
    schema::initialize_schema(&connection).unwrap();
    let mut published = publish_catalog_in(connection, 1, 1);
    let predecessor = published
        .state
        .ready_read_authority()
        .unwrap()
        .snapshot_id();
    let successor = refresh_catalog(&mut published, 2, 2);
    drop(published.connection);

    let connection = Connection::open(&database_path).unwrap();
    schema::initialize_schema(&connection).unwrap();
    let state = catalog_state::load_catalog_build_state(&connection)
        .unwrap()
        .unwrap();
    let authority = state.ready_read_authority().unwrap();
    assert_eq!(authority.snapshot_id(), successor);
    assert_eq!(state.readiness.refreshing_from_snapshot, None);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM catalog_snapshots", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        2
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT replaces_snapshot_commit_seq FROM catalog_snapshots WHERE snapshot_commit_seq = ?1",
                [i64::try_from(successor.complete_commit).unwrap()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        i64::try_from(predecessor.complete_commit).unwrap()
    );
    let page = read_retained_catalog_page(
        &connection,
        &authority,
        &CatalogRetainedPageRequest::sessions_all(query_selection(), successor, 10, None),
    )
    .unwrap();
    let CatalogRetainedPage::Sessions(page) = page else {
        panic!("expected reopened successor session page")
    };
    assert_eq!(page.rows.len(), 2);
}

#[test]
fn retained_project_and_session_pages_are_exact_keyset_walks_and_withheld() {
    let published = publish_catalog(3, 3);
    let authority = published.state.ready_read_authority().unwrap();
    let first = read_retained_catalog_page(
        &published.connection,
        &authority,
        &CatalogRetainedPageRequest::projects_all(
            published.selection.clone(),
            authority.snapshot_id(),
            2,
            None,
        ),
    )
    .unwrap();
    let CatalogRetainedPage::Projects(first) = first else {
        panic!("expected project page");
    };
    assert_eq!(first.rows.len(), 2);
    assert!(first.has_more);
    assert_eq!(first.total_count, CatalogCount::Known { value: 3 });
    for entry in &first.rows {
        let CatalogOptionalField::Selected { selection } = &entry.row.display_name else {
            panic!("expected selected-but-withheld display evidence");
        };
        assert_eq!(selection.field.value, None);
        assert_eq!(selection.field.quality, QualifiedValueQuality::Unknown);
        assert_eq!(
            selection.field.unknown_reason,
            Some(QualifiedUnknownReason::Withheld)
        );
    }
    let continuation = first.next_continuation.clone().unwrap();
    let second = read_retained_catalog_page(
        &published.connection,
        &authority,
        &CatalogRetainedPageRequest::projects_all(
            published.selection.clone(),
            authority.snapshot_id(),
            2,
            Some(continuation),
        ),
    )
    .unwrap();
    let CatalogRetainedPage::Projects(second) = second else {
        panic!("expected project page");
    };
    assert_eq!(second.rows.len(), 1);
    assert!(!second.has_more);
    assert!(second.next_continuation.is_none());
    let mut all_keys = first
        .rows
        .iter()
        .chain(second.rows.iter())
        .map(|entry| entry.row.project_ref.external_ref.entity_key)
        .collect::<Vec<_>>();
    let walked = all_keys.clone();
    all_keys.sort();
    all_keys.dedup();
    assert_eq!(walked, all_keys);

    let sessions = read_retained_catalog_page(
        &published.connection,
        &authority,
        &CatalogRetainedPageRequest::sessions_all(
            published.selection.clone(),
            authority.snapshot_id(),
            3,
            None,
        ),
    )
    .unwrap();
    let CatalogRetainedPage::Sessions(sessions) = sessions else {
        panic!("expected session page");
    };
    assert_eq!(sessions.rows.len(), 3);
    assert!(!sessions.has_more);
    for entry in &sessions.rows {
        let CatalogOptionalField::Selected { selection } = &entry.row.title else {
            panic!("expected selected-but-withheld title evidence");
        };
        assert_eq!(selection.field.value, None);
        assert_eq!(
            selection.field.unknown_reason,
            Some(QualifiedUnknownReason::Withheld)
        );
    }
}

#[test]
fn retained_external_resolution_is_exact_withheld_read_only_and_fail_closed() {
    let published = publish_catalog(1, 1);
    let authority = published.state.ready_read_authority().unwrap();
    let source_instance_key =
        CanonicalSourceInstanceKey::derive(1, b"retained-page-source").unwrap();
    let project_ref = CatalogEntityRef::project(
        CanonicalEntityKey::derive(
            FIXTURE_ADAPTER,
            &source_instance_key,
            "project",
            b"project-0",
        )
        .unwrap(),
    );
    let request = CatalogResolutionRequestBinding::new(
        published.selection.clone(),
        authority.snapshot_id(),
        project_ref.external_ref,
    )
    .unwrap();
    let before_changes: i64 = published
        .connection
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();
    let response =
        resolve_retained_catalog_entity(&published.connection, &authority, &request).unwrap();
    let CatalogEntityResolution::Live { row, .. } = response.resolution else {
        panic!("expected a live retained project resolution");
    };
    let CatalogPortableLiveRow::Project(row) = *row else {
        panic!("expected a project row");
    };
    let CatalogOptionalField::Selected { selection } = row.display_name else {
        panic!("expected selected project display evidence");
    };
    assert_eq!(selection.field.value, None);
    assert_eq!(
        selection.field.unknown_reason,
        Some(QualifiedUnknownReason::Withheld)
    );
    assert_eq!(
        published
            .connection
            .query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        before_changes
    );

    let unknown_ref = CatalogEntityRef::session(
        CanonicalEntityKey::derive(
            FIXTURE_ADAPTER,
            &source_instance_key,
            "session",
            b"never-observed",
        )
        .unwrap(),
    );
    let unknown_request = CatalogResolutionRequestBinding::new(
        published.selection.clone(),
        authority.snapshot_id(),
        unknown_ref.external_ref,
    )
    .unwrap();
    let unknown =
        resolve_retained_catalog_entity(&published.connection, &authority, &unknown_request)
            .unwrap();
    assert!(matches!(
        unknown.resolution,
        CatalogEntityResolution::Unknown {
            reason: CatalogUnknownReferenceReason::NeverObserved,
            ..
        }
    ));

    let mut selection_drift = request.clone();
    selection_drift
        .contract_selection
        .contract_versions
        .fact_family_versions
        .insert("catalog.message".to_owned(), 1);
    let selection_error =
        resolve_retained_catalog_entity(&published.connection, &authority, &selection_drift)
            .unwrap_err();
    assert!(selection_error
        .to_string()
        .contains("exact Ready snapshot selection"));

    let mut snapshot_drift = request;
    snapshot_drift.snapshot_id = crate::catalog_contract::CatalogSnapshotId::new(
        authority.snapshot_id().pack_contract_version,
        authority.snapshot_id().coverage_plan_id,
        authority.snapshot_id().readiness_epoch,
        authority.snapshot_id().complete_commit + 1,
    )
    .unwrap();
    let snapshot_error =
        resolve_retained_catalog_entity(&published.connection, &authority, &snapshot_drift)
            .unwrap_err();
    assert!(snapshot_error
        .to_string()
        .contains("exact Ready snapshot selection"));
}

#[test]
fn retained_external_resolution_reopens_tombstoned_and_superseded_lifecycle() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("retained-lifecycle.sqlite");
    let [deleted_ref, prior_ref, replacement_ref] = {
        let mut connection = Connection::open(&database_path).unwrap();
        publish_lifecycle_catalog(&mut connection)
    };

    let connection = Connection::open(&database_path).unwrap();
    let state = catalog_state::load_catalog_build_state(&connection)
        .unwrap()
        .unwrap();
    let authority = state.ready_read_authority().unwrap();
    let selection = query_selection();

    let deleted = resolve_retained_catalog_entity(
        &connection,
        &authority,
        &CatalogResolutionRequestBinding::new(
            selection.clone(),
            authority.snapshot_id(),
            deleted_ref.external_ref,
        )
        .unwrap(),
    )
    .unwrap();
    assert!(matches!(
        deleted.resolution,
        CatalogEntityResolution::Tombstoned {
            external_ref,
            ref provenance,
        } if external_ref == deleted_ref.external_ref && !provenance.is_empty()
    ));

    let prior = resolve_retained_catalog_entity(
        &connection,
        &authority,
        &CatalogResolutionRequestBinding::new(
            selection.clone(),
            authority.snapshot_id(),
            prior_ref.external_ref,
        )
        .unwrap(),
    )
    .unwrap();
    assert!(matches!(
        prior.resolution,
        CatalogEntityResolution::Superseded {
            external_ref,
            ref target_refs,
            ref provenance,
        } if external_ref == prior_ref.external_ref
            && target_refs == &vec![replacement_ref.external_ref]
            && !provenance.is_empty()
    ));

    let replacement = resolve_retained_catalog_entity(
        &connection,
        &authority,
        &CatalogResolutionRequestBinding::new(
            selection,
            authority.snapshot_id(),
            replacement_ref.external_ref,
        )
        .unwrap(),
    )
    .unwrap();
    assert!(matches!(
        replacement.resolution,
        CatalogEntityResolution::Live {
            external_ref,
            ref row,
        } if external_ref == replacement_ref.external_ref
            && matches!(row.as_ref(), CatalogPortableLiveRow::Session(_))
    ));
}

#[test]
fn retained_external_resolution_rejects_post_authority_row_substitution() {
    let published = publish_catalog(1, 1);
    let authority = published.state.ready_read_authority().unwrap();
    let source_instance_key =
        CanonicalSourceInstanceKey::derive(1, b"retained-page-source").unwrap();
    let project_ref = CatalogEntityRef::project(
        CanonicalEntityKey::derive(
            FIXTURE_ADAPTER,
            &source_instance_key,
            "project",
            b"project-0",
        )
        .unwrap(),
    );
    let (payload, key): (Vec<u8>, Vec<u8>) = published
        .connection
        .query_row(
            "SELECT payload, entry_key FROM catalog_snapshot_entries WHERE entry_kind = 'project_row'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let key_array: [u8; 32] = key.clone().try_into().unwrap();
    let mut row =
        decode_durable_project_row(&payload, &key_array, MAX_DURABLE_CATALOG_ROW_BYTES).unwrap();
    row.display_name.as_mut().unwrap().field.qualified.value =
        Some("private-resolution-substitution".to_owned());
    let payload = serde_json::to_vec(&row).unwrap();
    let digest = blake3::hash(&payload);
    published
        .connection
        .execute(
            "UPDATE catalog_snapshot_entries SET payload = ?1, payload_digest = ?2 WHERE entry_kind = 'project_row' AND entry_key = ?3",
            params![payload, digest.as_bytes().as_slice(), key],
        )
        .unwrap();
    let request = CatalogResolutionRequestBinding::new(
        published.selection,
        authority.snapshot_id(),
        project_ref.external_ref,
    )
    .unwrap();
    let error =
        resolve_retained_catalog_entity(&published.connection, &authority, &request).unwrap_err();
    assert!(error.to_string().contains("restart-validated commitment"));
}

#[test]
fn source_free_ready_snapshot_returns_complete_empty_pages() {
    let published = publish_catalog(0, 0);
    let authority = published.state.ready_read_authority().unwrap();
    for request in [
        CatalogRetainedPageRequest::projects_all(
            published.selection.clone(),
            authority.snapshot_id(),
            10,
            None,
        ),
        CatalogRetainedPageRequest::sessions_all(
            published.selection.clone(),
            authority.snapshot_id(),
            10,
            None,
        ),
    ] {
        match read_retained_catalog_page(&published.connection, &authority, &request).unwrap() {
            CatalogRetainedPage::Projects(page) => {
                assert!(page.rows.is_empty());
                assert_eq!(page.total_count, CatalogCount::Known { value: 0 });
                assert!(!page.has_more);
            }
            CatalogRetainedPage::Sessions(page) => {
                assert!(page.rows.is_empty());
                assert_eq!(page.total_count, CatalogCount::Known { value: 0 });
                assert!(!page.has_more);
            }
        }
    }
}

#[test]
fn ready_authority_rejects_a_different_coherent_database_publication() {
    let first = publish_catalog(1, 1);
    let substituted = publish_catalog(2, 1);
    let authority = first.state.ready_read_authority().unwrap();
    assert_eq!(
        authority.snapshot_id(),
        substituted
            .state
            .ready_read_authority()
            .unwrap()
            .snapshot_id()
    );
    let error = read_retained_catalog_page(
        &substituted.connection,
        &authority,
        &CatalogRetainedPageRequest::projects_all(
            first.selection,
            authority.snapshot_id(),
            10,
            None,
        ),
    )
    .err()
    .unwrap();
    assert!(error
        .to_string()
        .contains("restart-validated publication identity"));
}

#[test]
fn retained_page_rejects_snapshot_selection_and_continuation_escalation() {
    let published = publish_catalog(3, 1);
    let authority = published.state.ready_read_authority().unwrap();
    let first = read_retained_catalog_page(
        &published.connection,
        &authority,
        &CatalogRetainedPageRequest::projects_all(
            published.selection.clone(),
            authority.snapshot_id(),
            1,
            None,
        ),
    )
    .unwrap();
    let CatalogRetainedPage::Projects(first) = first else {
        panic!("expected project page");
    };
    let continuation = first.next_continuation.unwrap();

    let foreign_snapshot = crate::catalog_contract::CatalogSnapshotId::new(
        authority.snapshot_id().pack_contract_version,
        authority.snapshot_id().coverage_plan_id,
        authority.snapshot_id().readiness_epoch,
        authority.snapshot_id().complete_commit + 1,
    )
    .unwrap();
    assert!(read_retained_catalog_page(
        &published.connection,
        &authority,
        &CatalogRetainedPageRequest::projects_all(
            published.selection.clone(),
            foreign_snapshot,
            1,
            None,
        ),
    )
    .is_err());

    let mut selection_drift = published.selection.clone();
    selection_drift
        .contract_versions
        .fact_family_versions
        .insert("catalog.message".to_owned(), 1);
    assert!(read_retained_catalog_page(
        &published.connection,
        &authority,
        &CatalogRetainedPageRequest::projects_all(
            selection_drift,
            authority.snapshot_id(),
            1,
            None,
        ),
    )
    .is_err());

    let mut wrong_sort_key = continuation.clone();
    wrong_sort_key.cursor.last_sort_key =
        CatalogSortKey::new(b"not-an-entity-key".to_vec()).unwrap();
    assert!(read_retained_catalog_page(
        &published.connection,
        &authority,
        &CatalogRetainedPageRequest::projects_all(
            published.selection.clone(),
            authority.snapshot_id(),
            1,
            Some(wrong_sort_key),
        ),
    )
    .is_err());

    let mut wrong_fingerprint = continuation.clone();
    wrong_fingerprint.query_fingerprint = CatalogQueryFingerprint::derive(
        CATALOG_QUERY_PACK_CONTRACT_VERSION,
        CatalogQueryKind::Sessions,
        CatalogCoverageScope::Library,
        CATALOG_ENTITY_KEY_SORT_SPEC_VERSION,
        CATALOG_ALL_FILTER_V1,
    )
    .unwrap();
    wrong_fingerprint.cursor.query_fingerprint = wrong_fingerprint.query_fingerprint;
    assert!(read_retained_catalog_page(
        &published.connection,
        &authority,
        &CatalogRetainedPageRequest::projects_all(
            published.selection.clone(),
            authority.snapshot_id(),
            1,
            Some(wrong_fingerprint),
        ),
    )
    .is_err());

    let mut wrong_sort_version = continuation.clone();
    wrong_sort_version.sort_spec_version = 2;
    wrong_sort_version.cursor.sort_spec_version = 2;
    assert!(read_retained_catalog_page(
        &published.connection,
        &authority,
        &CatalogRetainedPageRequest::projects_all(
            published.selection.clone(),
            authority.snapshot_id(),
            1,
            Some(wrong_sort_version),
        ),
    )
    .is_err());

    assert!(read_retained_catalog_page(
        &published.connection,
        &authority,
        &CatalogRetainedPageRequest::projects_all(
            published.selection,
            authority.snapshot_id(),
            2,
            Some(continuation),
        ),
    )
    .is_err());
}

#[test]
fn query_and_restart_reject_digest_consistent_noncanonical_rows() {
    let published = publish_catalog(1, 1);
    let authority = published.state.ready_read_authority().unwrap();
    let (key, payload): (Vec<u8>, Vec<u8>) = published
        .connection
        .query_row(
            "SELECT entry_key, payload FROM catalog_snapshot_entries WHERE entry_kind = 'project_row'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("silently_dropped".to_owned(), serde_json::json!(true));
    let corrupted = serde_json::to_vec(&value).unwrap();
    let digest = blake3::hash(&corrupted);
    published
        .connection
        .execute(
            "UPDATE catalog_snapshot_entries SET payload = ?1, payload_digest = ?2 WHERE entry_kind = 'project_row' AND entry_key = ?3",
            params![corrupted, digest.as_bytes().as_slice(), key],
        )
        .unwrap();
    let error = read_retained_catalog_page(
        &published.connection,
        &authority,
        &CatalogRetainedPageRequest::projects_all(
            published.selection,
            authority.snapshot_id(),
            10,
            None,
        ),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("restart-validated commitment"));
    let restart_error = catalog_state::load_catalog_build_state(&published.connection).unwrap_err();
    assert!(restart_error.to_string().contains("canonical private JSON"));
}

#[test]
fn ready_authority_rejects_canonical_same_key_row_substitution() {
    let published = publish_catalog(1, 1);
    let authority = published.state.ready_read_authority().unwrap();
    let (key, payload): (Vec<u8>, Vec<u8>) = published
        .connection
        .query_row(
            "SELECT entry_key, payload FROM catalog_snapshot_entries WHERE entry_kind = 'project_row'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let key_array: [u8; 32] = key.clone().try_into().unwrap();
    let mut row =
        decode_durable_project_row(&payload, &key_array, MAX_DURABLE_CATALOG_ROW_BYTES).unwrap();
    row.display_name.as_mut().unwrap().field.qualified.value =
        Some("private-canonical-substitution".to_owned());
    let substituted = serde_json::to_vec(&row).unwrap();
    let digest = blake3::hash(&substituted);
    published
        .connection
        .execute(
            "UPDATE catalog_snapshot_entries SET payload = ?1, payload_digest = ?2 WHERE entry_kind = 'project_row' AND entry_key = ?3",
            params![substituted, digest.as_bytes().as_slice(), key],
        )
        .unwrap();

    let error = read_retained_catalog_page(
        &published.connection,
        &authority,
        &CatalogRetainedPageRequest::projects_all(
            published.selection,
            authority.snapshot_id(),
            10,
            None,
        ),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("restart-validated commitment"));
}

#[test]
fn ready_authority_rejects_missing_rows_and_forged_keyset_positions() {
    let deleted = publish_catalog(2, 1);
    let deleted_authority = deleted.state.ready_read_authority().unwrap();
    deleted
        .connection
        .execute(
            "DELETE FROM catalog_snapshot_entries WHERE entry_kind = 'project_row' AND entry_key = (SELECT MIN(entry_key) FROM catalog_snapshot_entries WHERE entry_kind = 'project_row')",
            [],
        )
        .unwrap();
    let deletion_error = read_retained_catalog_page(
        &deleted.connection,
        &deleted_authority,
        &CatalogRetainedPageRequest::projects_all(
            deleted.selection,
            deleted_authority.snapshot_id(),
            10,
            None,
        ),
    )
    .err()
    .unwrap();
    assert!(deletion_error.to_string().contains("PK range"));

    let published = publish_catalog(2, 1);
    let authority = published.state.ready_read_authority().unwrap();
    let first = read_retained_catalog_page(
        &published.connection,
        &authority,
        &CatalogRetainedPageRequest::projects_all(
            published.selection.clone(),
            authority.snapshot_id(),
            1,
            None,
        ),
    )
    .unwrap();
    let CatalogRetainedPage::Projects(first) = first else {
        panic!("expected project page");
    };
    let mut forged = first.next_continuation.unwrap();
    let source_instance_key =
        CanonicalSourceInstanceKey::derive(1, b"retained-page-source").unwrap();
    let foreign_key = CanonicalEntityKey::derive(
        FIXTURE_ADAPTER,
        &source_instance_key,
        "project",
        b"not-a-published-project",
    )
    .unwrap();
    forged.cursor.last_entity_key = foreign_key;
    forged.cursor.last_sort_key = CatalogSortKey::new(foreign_key.as_bytes().to_vec()).unwrap();
    let cursor_error = read_retained_catalog_page(
        &published.connection,
        &authority,
        &CatalogRetainedPageRequest::projects_all(
            published.selection,
            authority.snapshot_id(),
            1,
            Some(forged),
        ),
    )
    .err()
    .unwrap();
    assert!(cursor_error.to_string().contains("does not name a row"));
}

#[test]
fn retained_row_sql_preflights_payload_bounds_and_query_is_read_only() {
    let published = publish_catalog(2, 1);
    let authority = published.state.ready_read_authority().unwrap();
    let before_changes: i64 = published
        .connection
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();
    let before_entries: i64 = published
        .connection
        .query_row("SELECT COUNT(*) FROM catalog_snapshot_entries", [], |row| {
            row.get(0)
        })
        .unwrap();
    read_retained_catalog_page(
        &published.connection,
        &authority,
        &CatalogRetainedPageRequest::projects_all(
            published.selection.clone(),
            authority.snapshot_id(),
            10,
            None,
        ),
    )
    .unwrap();
    assert_eq!(
        published
            .connection
            .query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        before_changes
    );
    assert_eq!(
        published
            .connection
            .query_row("SELECT COUNT(*) FROM catalog_snapshot_entries", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        before_entries
    );

    published
        .connection
        .execute(
            "UPDATE catalog_snapshot_entries SET payload = zeroblob(?1) WHERE entry_kind = 'project_row'",
            [i64::try_from(MAX_DURABLE_CATALOG_ROW_BYTES + 1).unwrap()],
        )
        .unwrap();
    let error = read_retained_catalog_page(
        &published.connection,
        &authority,
        &CatalogRetainedPageRequest::projects_all(
            published.selection,
            authority.snapshot_id(),
            10,
            None,
        ),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("preflight byte bound"));
}

#[test]
fn retained_page_sql_uses_primary_key_ranges_without_temp_sort_or_offset() {
    let published = publish_catalog(2, 1);
    let snapshot_commit = i64::try_from(
        published
            .state
            .ready_read_authority()
            .unwrap()
            .snapshot_id()
            .complete_commit,
    )
    .unwrap();
    assert!(!FIRST_PAGE_SQL.to_ascii_uppercase().contains("OFFSET"));
    assert!(!CONTINUATION_PAGE_SQL
        .to_ascii_uppercase()
        .contains("OFFSET"));

    let explain = |sql: &str, values: &[&dyn rusqlite::ToSql]| -> Vec<String> {
        let mut statement = published
            .connection
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap();
        statement
            .query_map(values, |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let cap = i64::try_from(MAX_RETAINED_PAGE_PAYLOAD_BYTES).unwrap();
    let limit = 3_i64;
    let first = explain(
        FIRST_PAGE_SQL,
        &[&snapshot_commit, &"project_row", &cap, &limit],
    );
    let after = vec![1_u8; 32];
    let continued = explain(
        CONTINUATION_PAGE_SQL,
        &[&snapshot_commit, &"project_row", &after, &cap, &limit],
    );
    for details in [first, continued] {
        let joined = details.join("\n").to_ascii_uppercase();
        assert!(joined.contains("CATALOG_SNAPSHOT_ENTRIES"));
        assert!(joined.contains("USING INDEX"));
        assert!(!joined.contains("USE TEMP B-TREE"));
    }
}
