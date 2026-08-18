//! Portable page-contract conformance tests.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::*;
use crate::adapter::{
    CanonicalEntityKey, CanonicalFactId, CanonicalSourceInstanceKey, ContractCompleteness,
    ContractVersionOffer, ContractVersionRequest, CoverageAbsence, CoverageAbsenceKind,
    CoverageDeclarationDigest, CoverageDomain, CoverageError, CoverageMembershipRevision,
    CoverageObjectKey, CoveragePosition, CoveragePositionKind, CoverageProvenance, CoverageScope,
    CoverageSetCompleteness, CoverageStatus, CoverageStreamKey, FactRevisionId, QualifiedValue,
    SourceCoveragePoint, CONTRACT_VERSION_SELECTION_VERSION, SOURCE_COVERAGE_CONTRACT_VERSION,
};
use crate::catalog_contract::evidence::{
    CatalogDisclosureClass, CatalogProjectAssertion, CatalogQualifiedField, CatalogReducer,
    CatalogRetractionCause, CatalogRetractionEvidence, CatalogSessionAssertion,
    IdentityRelationFact, IdentityRelationKind,
};
use crate::catalog_contract::query::{
    negotiate_catalog_query_contract, CatalogQueryContractOffer, CatalogQueryContractRequest,
    CatalogTypedUnknownCapability, CATALOG_BASE_MODEL_MAJOR,
};
use crate::catalog_contract::{
    CatalogAccessPolicyDigest, CatalogCoveragePlanSource, CatalogIntegritySnapshotDisposition,
    CatalogReadinessMachine, CATALOG_PROJECTION_PACK_ID, CATALOG_QUERY_PACK_CONTRACT_VERSION,
};

const ADAPTER_ID: &str = "fixture-agent";
const DECLARATION: &[u8] = b"portable-catalog-page-declaration-v1";

fn source_key(label: &[u8]) -> CanonicalSourceInstanceKey {
    CanonicalSourceInstanceKey::derive(1, label).unwrap()
}

fn owner(label: &str, generation: u64) -> CatalogEvidenceOwner {
    CatalogEvidenceOwner::new(
        ADAPTER_ID,
        source_key(label.as_bytes()),
        CoverageStreamKey::derive(ADAPTER_ID, b"catalog").unwrap(),
        CoverageObjectKey::derive("portable-catalog", label.as_bytes()).unwrap(),
        generation,
    )
    .unwrap()
}

fn entity(owner: &CatalogEvidenceOwner, kind: CatalogEntityKind, label: &str) -> CatalogEntityRef {
    let entity_kind = match kind {
        CatalogEntityKind::Project => "project",
        CatalogEntityKind::Session => "session",
    };
    let key = CanonicalEntityKey::derive(
        &owner.adapter_id,
        &owner.source_instance_key,
        entity_kind,
        label.as_bytes(),
    )
    .unwrap();
    match kind {
        CatalogEntityKind::Project => CatalogEntityRef::project(key),
        CatalogEntityKind::Session => CatalogEntityRef::session(key),
    }
}

fn revision(owner: &CatalogEvidenceOwner, label: &str) -> SemanticRevisionRef {
    let fact = CanonicalFactId::native(
        &owner.adapter_id,
        &owner.source_instance_key,
        "catalog.page.fixture",
        label.as_bytes(),
    )
    .unwrap();
    SemanticRevisionRef::new(FactRevisionId::derive(&fact, 1, label.as_bytes()).unwrap())
}

fn authority(class_id: &str, precedence: u16) -> CatalogFieldAuthority {
    CatalogFieldAuthority::new(class_id, precedence, true).unwrap()
}

fn field<T>(
    owner: &CatalogEvidenceOwner,
    label: &str,
    value: T,
    quality: QualifiedValueQuality,
    authority: CatalogFieldAuthority,
    effective_at: Option<i64>,
    disclosure: CatalogDisclosureClass,
) -> CatalogQualifiedField<T> {
    CatalogQualifiedField::new(
        QualifiedValue::from_parts(
            Some(value),
            quality,
            authority,
            ContractCompleteness::Complete,
            None,
            effective_at,
            vec![revision(owner, label)],
        )
        .unwrap(),
        disclosure,
    )
    .unwrap()
}

fn availability(
    owner: &CatalogEvidenceOwner,
    label: &str,
) -> CatalogQualifiedField<CatalogAvailability> {
    field(
        owner,
        label,
        CatalogAvailability::HistoryReady,
        QualifiedValueQuality::Exact,
        authority("catalog-membership", 100),
        None,
        CatalogDisclosureClass::Public,
    )
}

fn project_assertion(
    owner: CatalogEvidenceOwner,
    project_ref: CatalogEntityRef,
) -> CatalogProjectAssertion {
    CatalogProjectAssertion::new(
        owner.clone(),
        b"project",
        project_ref,
        Some(field(
            &owner,
            "project-native-id",
            NativeIdentity {
                native_namespace: "fixture.project".to_owned(),
                native_id: "secret-project-native-id".to_owned(),
            },
            QualifiedValueQuality::NativeClaimed,
            authority("native-project-id", 100),
            None,
            CatalogDisclosureClass::PolicyShareable,
        )),
        Some(field(
            &owner,
            "project-root",
            "/private/secret/project-root".to_owned(),
            QualifiedValueQuality::Exact,
            authority("project-root", 90),
            None,
            CatalogDisclosureClass::LocalSensitive,
        )),
        None,
        Some(field(
            &owner,
            "project-name",
            "Fixture Project".to_owned(),
            QualifiedValueQuality::NativeClaimed,
            authority("project-display", 80),
            None,
            CatalogDisclosureClass::Public,
        )),
        None,
        availability(&owner, "project-availability"),
        vec![revision(&owner, "project-assertion")],
    )
    .unwrap()
}

fn session_assertion(
    owner: CatalogEvidenceOwner,
    session_ref: CatalogEntityRef,
    native_key: &[u8],
    title: &str,
) -> CatalogSessionAssertion {
    CatalogSessionAssertion::new(
        owner.clone(),
        native_key,
        session_ref,
        Some(field(
            &owner,
            "session-native-id",
            NativeIdentity {
                native_namespace: "fixture.session".to_owned(),
                native_id: "secret-session-native-id".to_owned(),
            },
            QualifiedValueQuality::NativeClaimed,
            authority("native-session-id", 100),
            Some(1_700_000_000),
            CatalogDisclosureClass::PolicyShareable,
        )),
        Some(field(
            &owner,
            "session-title",
            title.to_owned(),
            QualifiedValueQuality::Exact,
            authority("transcript-title", 80),
            Some(1_700_000_000),
            CatalogDisclosureClass::Public,
        )),
        None,
        None,
        None,
        None,
        None,
        availability(&owner, "session-availability"),
        vec![revision(&owner, "session-assertion")],
    )
    .unwrap()
}

fn association(
    owner: CatalogEvidenceOwner,
    session_ref: CatalogEntityRef,
    project_ref: CatalogEntityRef,
) -> SessionProjectAssociationFact {
    SessionProjectAssociationFact::new(
        owner.clone(),
        b"session-project",
        session_ref,
        project_ref,
        ProjectAssociationBasis::NativeProjectIndex,
        None,
        None,
        authority("native-project-association", 70),
        QualifiedValueQuality::NativeClaimed,
        ContractCompleteness::Complete,
        Some(1_700_000_000),
        vec![revision(&owner, "session-project-association")],
    )
    .unwrap()
}

fn relation(
    owner: CatalogEvidenceOwner,
    prior_ref: CatalogEntityRef,
    replacement_ref: CatalogEntityRef,
) -> IdentityRelationFact {
    IdentityRelationFact::new(
        owner.clone(),
        b"replacement",
        IdentityRelationKind::ReplacedBy,
        prior_ref,
        replacement_ref,
        authority("native-replacement", 100),
        QualifiedValueQuality::Exact,
        ContractCompleteness::Complete,
        None,
        None,
        vec![revision(&owner, "replacement-relation")],
    )
    .unwrap()
}

fn selected_contract() -> CatalogQueryContractSelection {
    let request = CatalogQueryContractRequest::new(
        ContractVersionRequest {
            selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
            model_major: CATALOG_BASE_MODEL_MAJOR,
            external_entity_reference_version: EXTERNAL_ENTITY_REFERENCE_VERSION,
            semantic_revision_reference_version: SEMANTIC_REFERENCE_CONTRACT_VERSION,
            coverage_contract_versions: vec![SOURCE_COVERAGE_CONTRACT_VERSION],
            fact_family_versions: BTreeMap::new(),
            query_pack_versions: Some(vec![CATALOG_QUERY_PACK_CONTRACT_VERSION]),
            observation_contract_versions: None,
        },
        CatalogTypedUnknownCapability::preserving(4_096).unwrap(),
    )
    .unwrap();
    let offer = CatalogQueryContractOffer::new(
        ContractVersionOffer {
            selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
            model_major: CATALOG_BASE_MODEL_MAJOR,
            external_entity_reference_versions: vec![EXTERNAL_ENTITY_REFERENCE_VERSION],
            semantic_revision_reference_versions: vec![SEMANTIC_REFERENCE_CONTRACT_VERSION],
            coverage_contract_versions: vec![SOURCE_COVERAGE_CONTRACT_VERSION],
            fact_family_versions: BTreeMap::new(),
            query_pack_versions: vec![CATALOG_QUERY_PACK_CONTRACT_VERSION],
            observation_contract_versions: Vec::new(),
        },
        CatalogTypedUnknownCapability::preserving(8_192).unwrap(),
    )
    .unwrap();
    negotiate_catalog_query_contract(&request, &offer).unwrap()
}

fn plan_source(support_release_id: &str, policy: &[u8]) -> CatalogCoveragePlanSource {
    CatalogCoveragePlanSource::new(
        ADAPTER_ID,
        source_key(b"portable-page-source"),
        support_release_id,
        CoverageDeclarationDigest::derive(DECLARATION).unwrap(),
        CatalogAccessPolicyDigest::derive(1, policy).unwrap(),
    )
    .unwrap()
}

fn plan(support_release_id: &str, policy: &[u8]) -> CatalogCoveragePlan {
    CatalogCoveragePlan::new(
        CatalogCoverageScope::Library,
        vec![plan_source(support_release_id, policy)],
        Vec::new(),
    )
    .unwrap()
}

fn coverage(plan: &CatalogCoveragePlan, order: u64) -> Vec<SourceCoverageSet> {
    let source = &plan.required_sources[0];
    let domain = CoverageDomain::ProjectionPack {
        pack: CATALOG_PROJECTION_PACK_ID.to_owned(),
        version: CATALOG_QUERY_PACK_CONTRACT_VERSION,
    };
    let point = SourceCoveragePoint::new(
        domain.clone(),
        source.adapter_id.clone(),
        source.source_instance_key,
        CoverageStreamKey::derive(ADAPTER_ID, b"page-stream").unwrap(),
        CoverageObjectKey::derive(ADAPTER_ID, b"page-object").unwrap(),
        1,
        Some(
            CoveragePosition::derive(
                CoveragePositionKind::AppendCursor,
                &order.to_be_bytes(),
                Some(order),
            )
            .unwrap(),
        ),
        CoverageStatus::CompleteThrough,
        CoverageProvenance::default(),
    )
    .unwrap();
    vec![SourceCoverageSet::new(
        domain,
        CoverageScope {
            adapter_id: source.adapter_id.clone(),
            source_instance_key: source.source_instance_key,
            root_entity_key: None,
            support_release_id: source.support_release_id.clone(),
            source_or_scope_declaration_digest: source.coverage_binding_digest(),
        },
        CoverageMembershipRevision::derive(b"portable-page-membership").unwrap(),
        vec![point],
        Vec::new(),
        Vec::new(),
        CoverageSetCompleteness::Complete,
    )
    .unwrap()]
}

struct FixtureState {
    selection: CatalogQueryContractSelection,
    published_plan: CatalogCoveragePlan,
    current_plan: CatalogCoveragePlan,
    published_readiness: CatalogReadinessSnapshot,
    current_readiness: CatalogReadinessSnapshot,
    snapshot: CatalogSnapshotId,
    latest_snapshot: CatalogSnapshotId,
    project_page: CatalogProjectPage,
    session_page: CatalogSessionPage,
    readiness_response: CatalogReadinessResponse,
    resolution_responses: BTreeMap<&'static str, CatalogEntityResolutionResponse>,
    live_resolution: CatalogResolvedEntity,
    continuation: CatalogContinuationRequest,
    expiration: CatalogSnapshotExpired,
}

fn fixture_state() -> FixtureState {
    let selection = selected_contract();
    let published_plan = plan("fixture-support-v1", b"fixture-policy-v1");
    let snapshot = CatalogSnapshotId::new(
        CATALOG_QUERY_PACK_CONTRACT_VERSION,
        published_plan.coverage_plan_id,
        1,
        42,
    )
    .unwrap();
    let mut readiness_machine =
        CatalogReadinessMachine::register(published_plan.clone(), 1).unwrap();
    readiness_machine.schedule_build().unwrap();
    readiness_machine
        .publish_ready(snapshot, coverage(&published_plan, 42))
        .unwrap();
    let published_readiness = readiness_machine.snapshot().clone();

    let current_plan = plan("fixture-support-v2", b"fixture-policy-v2");
    readiness_machine
        .replace_coverage_plan(current_plan.clone())
        .unwrap();
    let current_readiness = readiness_machine.snapshot().clone();
    let readiness_response = CatalogReadinessResponse::new(
        selection.clone(),
        current_readiness.clone(),
        BTreeMap::from([("future_readiness_hint".to_owned(), json!({"mode": "warm"}))]),
        &current_plan,
    )
    .unwrap();

    let evidence_owner = owner("page-live", 1);
    let project_ref = entity(&evidence_owner, CatalogEntityKind::Project, "project");
    let session_ref = entity(&evidence_owner, CatalogEntityKind::Session, "session");
    let mut reducer = CatalogReducer::default();
    reducer
        .upsert_project_assertion(project_assertion(evidence_owner.clone(), project_ref), 10)
        .unwrap();
    reducer
        .upsert_session_assertion(
            session_assertion(
                evidence_owner.clone(),
                session_ref,
                b"session",
                "Fixture Session",
            ),
            11,
        )
        .unwrap();
    reducer
        .upsert_association(
            association(evidence_owner.clone(), session_ref, project_ref),
            12,
        )
        .unwrap();
    let project_row = CatalogPortableProjectRow::from_evidence(
        &reducer.project_row(project_ref).unwrap(),
        CatalogPolicyView::WITHHELD,
    )
    .unwrap();
    let session_row = CatalogPortableSessionRow::from_evidence(
        &reducer.session_row(session_ref).unwrap(),
        CatalogPolicyView::WITHHELD,
    )
    .unwrap();

    let project_fingerprint = CatalogQueryFingerprint::derive(
        1,
        CatalogQueryKind::Projects,
        CatalogCoverageScope::Library,
        1,
        br#"{"availability":"any"}"#,
    )
    .unwrap();
    let project_sort = CatalogSortKey::new(b"fixture-project".to_vec()).unwrap();
    let project_cursor = CatalogCursor::new(
        snapshot,
        project_fingerprint,
        1,
        project_sort.clone(),
        project_ref.external_ref.entity_key,
    )
    .unwrap();
    let continuation = CatalogContinuationRequest::new(
        selection.clone(),
        snapshot,
        project_fingerprint,
        1,
        project_cursor,
        1,
    )
    .unwrap();
    let project_request = CatalogPageRequestBinding::new(
        selection.clone(),
        snapshot,
        CatalogQueryKind::Projects,
        project_fingerprint,
        1,
        1,
        None,
    )
    .unwrap();
    let project_page = CatalogProjectPage::new_projects(
        project_request,
        published_readiness.clone(),
        CatalogCount::known(2).unwrap(),
        true,
        vec![CatalogPageEntry::new(project_sort, project_row).unwrap()],
        Some(continuation.clone()),
        BTreeMap::from([("future_page_hint".to_owned(), json!({"cache": "warm"}))]),
        &published_plan,
    )
    .unwrap();

    let session_fingerprint = CatalogQueryFingerprint::derive(
        1,
        CatalogQueryKind::Sessions,
        CatalogCoverageScope::Library,
        1,
        br#"{"project":null}"#,
    )
    .unwrap();
    let session_request = CatalogPageRequestBinding::new(
        selection.clone(),
        snapshot,
        CatalogQueryKind::Sessions,
        session_fingerprint,
        1,
        50,
        None,
    )
    .unwrap();
    let session_page = CatalogSessionPage::new_sessions(
        session_request,
        published_readiness.clone(),
        CatalogCount::unknown(QualifiedUnknownReason::NotYetObserved),
        false,
        vec![CatalogPageEntry::new(
            CatalogSortKey::new(b"fixture-session".to_vec()).unwrap(),
            session_row,
        )
        .unwrap()],
        None,
        BTreeMap::new(),
        &published_plan,
    )
    .unwrap();

    let live_resolution = reducer.resolve_external_ref(session_ref.external_ref);
    let mut resolutions = BTreeMap::new();
    resolutions.insert("live", (session_ref.external_ref, live_resolution.clone()));

    let tombstone_owner = owner("tombstone", 1);
    let tombstone_ref = entity(
        &tombstone_owner,
        CatalogEntityKind::Session,
        "tombstoned-session",
    );
    let mut tombstone_reducer = CatalogReducer::default();
    tombstone_reducer
        .upsert_session_assertion(
            session_assertion(
                tombstone_owner.clone(),
                tombstone_ref,
                b"tombstoned-session",
                "Removed Session",
            ),
            20,
        )
        .unwrap();
    let retraction = CatalogRetractionEvidence::new(
        tombstone_owner.clone(),
        CatalogRetractionCause::ConfirmedDeletion,
        ContractCompleteness::Complete,
        vec![revision(&tombstone_owner, "confirmed-deletion")],
    )
    .unwrap();
    tombstone_reducer.retract_owner(&retraction, 21).unwrap();
    tombstone_reducer
        .confirm_absent(tombstone_ref, &retraction, 22)
        .unwrap();
    resolutions.insert(
        "tombstoned",
        (
            tombstone_ref.external_ref,
            tombstone_reducer.resolve_external_ref(tombstone_ref.external_ref),
        ),
    );

    let replacement_owner = owner("replacement", 1);
    let prior_ref = entity(
        &replacement_owner,
        CatalogEntityKind::Session,
        "prior-session",
    );
    let replacement_ref = entity(
        &replacement_owner,
        CatalogEntityKind::Session,
        "replacement-session",
    );
    let mut replacement_reducer = CatalogReducer::default();
    replacement_reducer
        .upsert_session_assertion(
            session_assertion(
                replacement_owner.clone(),
                prior_ref,
                b"prior-session",
                "Prior Session",
            ),
            30,
        )
        .unwrap();
    replacement_reducer
        .upsert_session_assertion(
            session_assertion(
                replacement_owner.clone(),
                replacement_ref,
                b"replacement-session",
                "Replacement Session",
            ),
            31,
        )
        .unwrap();
    replacement_reducer
        .upsert_identity_relation(relation(replacement_owner, prior_ref, replacement_ref), 32)
        .unwrap();
    resolutions.insert(
        "superseded",
        (
            prior_ref.external_ref,
            replacement_reducer.resolve_external_ref(prior_ref.external_ref),
        ),
    );

    let unknown_owner = owner("unknown", 1);
    let unknown_ref = entity(&unknown_owner, CatalogEntityKind::Session, "never-observed");
    resolutions.insert(
        "unknown",
        (
            unknown_ref.external_ref,
            CatalogReducer::default().resolve_external_ref(unknown_ref.external_ref),
        ),
    );

    let resolution_responses = resolutions
        .into_iter()
        .map(|(label, (external_ref, resolved))| {
            let request =
                CatalogResolutionRequestBinding::new(selection.clone(), snapshot, external_ref)
                    .unwrap();
            let portable = CatalogEntityResolution::from_evidence(
                external_ref,
                &resolved,
                CatalogPolicyView::WITHHELD,
            )
            .unwrap();
            let response =
                CatalogEntityResolutionResponse::new(request, portable, BTreeMap::new()).unwrap();
            (label, response)
        })
        .collect();

    let latest_snapshot = CatalogSnapshotId::new(
        CATALOG_QUERY_PACK_CONTRACT_VERSION,
        current_plan.coverage_plan_id,
        2,
        84,
    )
    .unwrap();
    let CatalogContinuationDisposition::SnapshotExpired(expiration) =
        validate_continuation_retention(
            serde_json::to_value(&continuation).unwrap(),
            &continuation,
            CatalogCoverageScope::Library,
            CatalogSnapshotRetention::Expired { latest_snapshot },
        )
        .unwrap()
    else {
        panic!("fixture continuation must expire")
    };

    FixtureState {
        selection,
        published_plan,
        current_plan,
        published_readiness,
        current_readiness,
        snapshot,
        latest_snapshot,
        project_page,
        session_page,
        readiness_response,
        resolution_responses,
        live_resolution,
        continuation,
        expiration,
    }
}

#[test]
fn pages_bind_the_exact_request_snapshot_order_and_next_cursor() {
    let fixture = fixture_state();
    fixture
        .project_page
        .validate_for_request(&fixture.project_page.request, &fixture.published_plan)
        .unwrap();
    fixture
        .session_page
        .validate_for_request(&fixture.session_page.request, &fixture.published_plan)
        .unwrap();

    let mut selection_drift = fixture.project_page.clone();
    selection_drift
        .request
        .contract_selection
        .typed_unknown
        .max_payload_bytes = 8_192;
    assert!(selection_drift
        .validate_for_request(&fixture.project_page.request, &fixture.published_plan)
        .is_err());

    let mut duplicate = fixture.project_page.clone();
    duplicate.rows.push(duplicate.rows[0].clone());
    duplicate.request.page_size = 2;
    duplicate.next_continuation.as_mut().unwrap().page_size = 2;
    assert!(duplicate
        .validate_for_request(&duplicate.request.clone(), &fixture.published_plan)
        .is_err());

    let mut duplicate_entity = fixture.project_page.clone();
    let mut second_entry = duplicate_entity.rows[0].clone();
    second_entry.sort_key = CatalogSortKey::new(b"fixture-project-z".to_vec()).unwrap();
    duplicate_entity.rows.push(second_entry.clone());
    duplicate_entity.request.page_size = 2;
    duplicate_entity.total_count = CatalogCount::known(3).unwrap();
    let next = duplicate_entity.next_continuation.as_mut().unwrap();
    next.page_size = 2;
    next.cursor.last_sort_key = second_entry.sort_key;
    assert!(duplicate_entity
        .validate_for_request(&duplicate_entity.request.clone(), &fixture.published_plan)
        .is_err());

    let mut impossible_total = fixture.project_page.clone();
    impossible_total.total_count = CatalogCount::known(1).unwrap();
    assert!(impossible_total
        .validate_for_request(&impossible_total.request.clone(), &fixture.published_plan)
        .is_err());

    let mut missing_next = fixture.project_page.clone();
    missing_next.next_continuation = None;
    assert!(missing_next
        .validate_for_request(&missing_next.request.clone(), &fixture.published_plan)
        .is_err());

    let mut empty_with_next = fixture.project_page.clone();
    empty_with_next.rows.clear();
    assert!(empty_with_next
        .validate_for_request(&empty_with_next.request.clone(), &fixture.published_plan)
        .is_err());

    let mut foreign_final_entity = fixture.project_page.clone();
    foreign_final_entity
        .next_continuation
        .as_mut()
        .unwrap()
        .cursor
        .last_entity_key = CanonicalEntityKey::derive(
        ADAPTER_ID,
        &source_key(b"foreign-final"),
        "project",
        b"foreign",
    )
    .unwrap();
    assert!(foreign_final_entity
        .validate_for_request(
            &foreign_final_entity.request.clone(),
            &fixture.published_plan
        )
        .is_err());
}

#[test]
fn portable_rows_withhold_native_identity_and_never_encode_unknown_counts_as_zero() {
    let fixture = fixture_state();
    let project = serde_json::to_value(&fixture.project_page).unwrap();
    let session = serde_json::to_value(&fixture.session_page).unwrap();
    let encoded = format!("{project}{session}");
    assert!(!encoded.contains("secret-project-native-id"));
    assert!(!encoded.contains("secret-session-native-id"));
    assert!(!encoded.contains("/private/secret/project-root"));
    assert_eq!(
        project["rows"][0]["row"]["native_identity"]["selection"]["field"]["unknown_reason"],
        json!("withheld")
    );
    assert_eq!(
        session["total_count"],
        json!({"state": "unknown", "reason": "not_yet_observed"})
    );
    assert_eq!(
        session["rows"][0]["row"]["native_message_count"],
        json!({"state": "unknown", "reason": "not_yet_observed"})
    );
    assert_ne!(
        CatalogCount::known(0).unwrap(),
        fixture.session_page.total_count
    );

    let mut foreign_conflict = fixture.project_page.clone();
    let row = &mut foreign_conflict.rows[0].row;
    let foreign_assertion =
        project_assertion(owner("foreign-field-conflict", 1), row.project_ref).assertion_key;
    let CatalogOptionalField::Selected { selection } = &mut row.display_name else {
        panic!("fixture display name must be selected")
    };
    selection.conflicting_assertion_keys = vec![foreign_assertion];
    assert!(foreign_conflict
        .validate_for_request(&foreign_conflict.request.clone(), &fixture.published_plan)
        .is_err());
}

#[test]
fn current_readiness_can_retain_a_prior_plan_snapshot_without_claiming_current_coverage() {
    let fixture = fixture_state();
    assert_eq!(
        fixture.published_readiness.state,
        CatalogReadinessPhase::Ready
    );
    assert_eq!(
        fixture.current_readiness.state,
        CatalogReadinessPhase::Building
    );
    assert_eq!(fixture.current_readiness.complete_through_commit, None);
    assert_eq!(
        fixture.current_readiness.last_complete_snapshot,
        Some(fixture.snapshot)
    );
    assert_ne!(
        fixture.current_readiness.coverage_plan_id,
        fixture.snapshot.coverage_plan_id
    );
    fixture
        .readiness_response
        .validate_for_request(&fixture.selection, &fixture.current_plan)
        .unwrap();

    let mut false_current = fixture.readiness_response.clone();
    false_current.readiness.complete_through_commit = Some(fixture.snapshot.complete_commit);
    assert!(false_current
        .validate_for_request(&fixture.selection, &fixture.current_plan)
        .is_err());

    let mut unsafe_error = fixture.readiness_response.clone();
    unsafe_error.readiness.state = CatalogReadinessPhase::Error;
    unsafe_error.readiness.reason = Some(super::super::CatalogReadinessReason::IntegrityFailure {
        code: "fixture_integrity_failure".to_owned(),
        snapshot_disposition: CatalogIntegritySnapshotDisposition::Discarded,
    });
    assert!(unsafe_error
        .validate_for_request(&fixture.selection, &fixture.current_plan)
        .is_err());
}

#[test]
fn policy_binding_generations_coverage_order_and_association_conflicts_fail_closed() {
    let old_plan = plan("fixture-support-v1", b"fixture-policy-old");
    let old_coverage = coverage(&old_plan, 10);
    let old_snapshot = CatalogSnapshotId::new(
        CATALOG_QUERY_PACK_CONTRACT_VERSION,
        old_plan.coverage_plan_id,
        1,
        10,
    )
    .unwrap();
    let mut machine = CatalogReadinessMachine::register(old_plan, 1).unwrap();
    machine.schedule_build().unwrap();
    machine
        .publish_ready(old_snapshot, old_coverage.clone())
        .unwrap();
    let new_plan = plan("fixture-support-v1", b"fixture-policy-new");
    machine.replace_coverage_plan(new_plan.clone()).unwrap();
    let new_snapshot = CatalogSnapshotId::new(
        CATALOG_QUERY_PACK_CONTRACT_VERSION,
        new_plan.coverage_plan_id,
        2,
        20,
    )
    .unwrap();
    assert!(machine.publish_ready(new_snapshot, old_coverage).is_err());
    assert_eq!(machine.snapshot().state, CatalogReadinessPhase::Building);
    machine
        .publish_ready(new_snapshot, coverage(&new_plan, 20))
        .unwrap();

    let mut source_wire = serde_json::to_value(&new_plan.required_sources[0]).unwrap();
    source_wire["future_source_identity"] = json!(true);
    assert!(serde_json::from_value::<CatalogCoveragePlanSource>(source_wire).is_err());
    assert!(serde_json::from_value::<CatalogCoverageScope>(json!({
        "kind": "library",
        "future_scope_meaning": true,
    }))
    .is_err());
    let entity_scope = CatalogCoverageScope::Entity {
        external_ref: entity(
            &owner("scope-entity", 1),
            CatalogEntityKind::Project,
            "scope-entity",
        )
        .external_ref,
    };
    let mut entity_scope_wire = serde_json::to_value(entity_scope).unwrap();
    entity_scope_wire["external_ref"]["future_identity_meaning"] = json!(true);
    assert!(serde_json::from_value::<CatalogCoverageScope>(entity_scope_wire).is_err());

    let fixture = fixture_state();
    let mut zero_owner = fixture.session_page.clone();
    let CatalogAssociationCoverage::Available { selection } =
        &mut zero_owner.rows[0].row.project_association
    else {
        panic!("fixture association must be available")
    };
    selection.association.owner.generation = 0;
    assert!(zero_owner
        .validate_for_request(&zero_owner.request.clone(), &fixture.published_plan)
        .is_err());

    let mut zero_point = coverage(&fixture.published_plan, 42);
    zero_point[0].points[0].generation = 0;
    assert!(validate_source_coverage_portable(&zero_point).is_err());

    let mut zero_absence = coverage(&fixture.published_plan, 42);
    let point = zero_absence[0].points.remove(0);
    zero_absence[0]
        .explicit_absence_or_deletion
        .push(CoverageAbsence {
            stream_key: point.stream_key,
            object_key: point.object_key,
            generation: 0,
            kind: CoverageAbsenceKind::Absent,
        });
    assert!(validate_source_coverage_portable(&zero_absence).is_err());

    let mut oversized_reason = coverage(&fixture.published_plan, 42);
    oversized_reason[0].completeness = CoverageSetCompleteness::Partial;
    oversized_reason[0].points[0].status = CoverageStatus::Unavailable {
        reason: "x".repeat(MAX_PORTABLE_COVERAGE_REASON_BYTES + 1),
    };
    assert!(validate_source_coverage_portable(&oversized_reason).is_err());

    let mut oversized_error = coverage(&fixture.published_plan, 42);
    oversized_error[0].completeness = CoverageSetCompleteness::Partial;
    oversized_error[0].explicit_errors.push(CoverageError {
        stream_key: None,
        object_key: None,
        code: "x".repeat(257),
    });
    assert!(validate_source_coverage_portable(&oversized_error).is_err());

    let mut reversed_points = coverage(&fixture.published_plan, 42);
    let mut second = reversed_points[0].points[0].clone();
    second.object_key = CoverageObjectKey::derive(ADAPTER_ID, b"second-page-object").unwrap();
    reversed_points[0].points.push(second);
    reversed_points[0]
        .points
        .sort_by_key(|point| (point.stream_key, point.object_key, point.generation));
    reversed_points[0].points.reverse();
    assert!(validate_source_coverage_portable(&reversed_points).is_err());

    let mut conflicts = fixture.session_page.clone();
    let row = &mut conflicts.rows[0].row;
    let other_owner = owner("association-conflict", 1);
    let other_project = entity(&other_owner, CatalogEntityKind::Project, "other-project");
    let competitor = association(other_owner, row.session_ref, other_project);
    let competitor_key = competitor.association_key;
    let CatalogAssociationCoverage::Available { selection } = &mut row.project_association else {
        unreachable!()
    };
    selection.competing_associations = vec![competitor];
    selection.conflicting_association_keys.clear();
    assert!(conflicts
        .validate_for_request(&conflicts.request.clone(), &fixture.published_plan)
        .is_err());
    let CatalogAssociationCoverage::Available { selection } =
        &mut conflicts.rows[0].row.project_association
    else {
        unreachable!()
    };
    selection.conflicting_association_keys = vec![competitor_key];
    conflicts
        .validate_for_request(&conflicts.request.clone(), &fixture.published_plan)
        .unwrap();
}

#[test]
fn external_resolution_preserves_exact_identity_and_canonical_bounded_evidence() {
    let fixture = fixture_state();
    for response in fixture.resolution_responses.values() {
        response.validate_for_request(&response.request).unwrap();
        assert_eq!(
            response.request.external_ref,
            response.resolution.external_ref()
        );
    }

    let tombstoned = fixture.resolution_responses.get("tombstoned").unwrap();
    let CatalogEntityResolution::Tombstoned { provenance, .. } = &tombstoned.resolution else {
        panic!("expected tombstoned resolution")
    };
    assert!(provenance.len() >= 2);
    let mut noncanonical = tombstoned.clone();
    let CatalogEntityResolution::Tombstoned { provenance, .. } = &mut noncanonical.resolution
    else {
        unreachable!()
    };
    provenance.reverse();
    assert!(noncanonical
        .validate_for_request(&noncanonical.request)
        .is_err());

    let live = fixture.resolution_responses.get("live").unwrap();
    let mut cross_identity = live.clone();
    cross_identity.request.external_ref = fixture
        .resolution_responses
        .get("unknown")
        .unwrap()
        .request
        .external_ref;
    assert!(cross_identity.validate_for_request(&live.request).is_err());

    let typed = CatalogEntityResolution::typed_unknown(
        live.request.external_ref,
        "future_resolution_state",
        BTreeMap::from([("bounded".to_owned(), json!(true))]),
        &fixture.selection,
    )
    .unwrap();
    assert!(matches!(
        typed,
        CatalogEntityResolution::TypedUnknown { .. }
    ));

    let mut mismatched_live_kind = fixture.live_resolution.clone();
    let CatalogResolvedEntity::Live { entity_ref, .. } = &mut mismatched_live_kind else {
        unreachable!()
    };
    entity_ref.kind = CatalogEntityKind::Project;
    assert!(CatalogEntityResolution::from_evidence(
        entity_ref.external_ref,
        &mismatched_live_kind,
        CatalogPolicyView::WITHHELD,
    )
    .is_err());
}

#[test]
fn expiration_requires_a_valid_exact_continuation_before_retention_is_consulted() {
    let fixture = fixture_state();
    let wire = serde_json::to_value(&fixture.continuation).unwrap();
    assert!(matches!(
        validate_continuation_retention(
            wire.clone(),
            &fixture.continuation,
            CatalogCoverageScope::Library,
            CatalogSnapshotRetention::Retained
        )
        .unwrap(),
        CatalogContinuationDisposition::Continue(_)
    ));

    let mut malformed = wire.clone();
    malformed["cursor"]["query_fingerprint"] =
        json!("v1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
    assert!(validate_continuation_retention(
        malformed,
        &fixture.continuation,
        CatalogCoverageScope::Library,
        CatalogSnapshotRetention::Expired {
            latest_snapshot: fixture.latest_snapshot,
        }
    )
    .is_err());

    let mut unknown_cursor_field = wire.clone();
    unknown_cursor_field["cursor"]["future_cursor_meaning"] = json!(true);
    assert!(validate_continuation_retention(
        unknown_cursor_field,
        &fixture.continuation,
        CatalogCoverageScope::Library,
        CatalogSnapshotRetention::Expired {
            latest_snapshot: fixture.latest_snapshot,
        }
    )
    .is_err());

    let mut unknown_snapshot_field = wire.clone();
    unknown_snapshot_field["cursor"]["snapshot_id"]["future_snapshot_meaning"] = json!(true);
    assert!(validate_continuation_retention(
        unknown_snapshot_field,
        &fixture.continuation,
        CatalogCoverageScope::Library,
        CatalogSnapshotRetention::Expired {
            latest_snapshot: fixture.latest_snapshot,
        }
    )
    .is_err());

    let mut foreign_but_valid = wire;
    foreign_but_valid["page_size"] = json!(2);
    assert!(validate_continuation_retention(
        foreign_but_valid,
        &fixture.continuation,
        CatalogCoverageScope::Library,
        CatalogSnapshotRetention::Expired {
            latest_snapshot: fixture.latest_snapshot,
        }
    )
    .is_err());

    assert!(validate_continuation_retention(
        serde_json::to_value(&fixture.continuation).unwrap(),
        &fixture.continuation,
        CatalogCoverageScope::Library,
        CatalogSnapshotRetention::Expired {
            latest_snapshot: fixture.snapshot,
        }
    )
    .is_err());

    let mut older_latest = fixture.latest_snapshot;
    older_latest.complete_commit = fixture.snapshot.complete_commit - 1;
    assert!(validate_continuation_retention(
        serde_json::to_value(&fixture.continuation).unwrap(),
        &fixture.continuation,
        CatalogCoverageScope::Library,
        CatalogSnapshotRetention::Expired {
            latest_snapshot: older_latest,
        }
    )
    .is_err());

    let mut different_pack = fixture.latest_snapshot;
    different_pack.pack_contract_version += 1;
    assert!(validate_continuation_retention(
        serde_json::to_value(&fixture.continuation).unwrap(),
        &fixture.continuation,
        CatalogCoverageScope::Library,
        CatalogSnapshotRetention::Expired {
            latest_snapshot: different_pack,
        }
    )
    .is_err());

    let mut same_epoch_different_plan = fixture.latest_snapshot;
    same_epoch_different_plan.readiness_epoch = fixture.snapshot.readiness_epoch;
    assert_ne!(
        same_epoch_different_plan.coverage_plan_id,
        fixture.snapshot.coverage_plan_id
    );
    assert!(validate_continuation_retention(
        serde_json::to_value(&fixture.continuation).unwrap(),
        &fixture.continuation,
        CatalogCoverageScope::Library,
        CatalogSnapshotRetention::Expired {
            latest_snapshot: same_epoch_different_plan,
        }
    )
    .is_err());
}

fn frozen_fixture() -> Value {
    let fixture = fixture_state();
    let published_portable_plan =
        CatalogPortableCoveragePlan::from_plan(&fixture.published_plan).unwrap();
    let current_portable_plan =
        CatalogPortableCoveragePlan::from_plan(&fixture.current_plan).unwrap();
    json!({
        "fixture_contract_version": 1,
        "contract_selection": fixture.selection,
        "published_plan": published_portable_plan,
        "current_plan": current_portable_plan,
        "project_page": fixture.project_page.to_wire_value(
            &fixture.project_page.request,
            &fixture.published_plan,
        ).unwrap(),
        "session_page": fixture.session_page.to_wire_value(
            &fixture.session_page.request,
            &fixture.published_plan,
        ).unwrap(),
        "readiness_response": fixture.readiness_response.to_wire_value(
            &fixture.selection,
            &fixture.current_plan,
        ).unwrap(),
        "resolutions": fixture.resolution_responses.into_iter().map(|(key, response)| {
            (key, response.to_wire_value(&response.request).unwrap())
        }).collect::<BTreeMap<_, _>>(),
        "continuation_request": fixture.continuation,
        "snapshot_expired": fixture.expiration,
        "expected": {
            "published_snapshot": fixture.snapshot,
            "latest_snapshot": fixture.latest_snapshot,
            "project_has_more": true,
            "session_total_count_state": "unknown",
            "current_readiness_state": "building",
            "retained_snapshot_is_prior_plan": true,
            "native_values_withheld": true,
        }
    })
}

#[test]
fn frozen_catalog_page_contract_matches_portable_fixture() {
    let actual = frozen_fixture();
    let expected: Value = serde_json::from_str(include_str!(
        "../../../fixtures/contracts/rfc012b-catalog-pages-v1.json"
    ))
    .unwrap();
    if actual != expected {
        eprintln!("{}", serde_json::to_string_pretty(&actual).unwrap());
    }
    assert_eq!(actual, expected);
}
