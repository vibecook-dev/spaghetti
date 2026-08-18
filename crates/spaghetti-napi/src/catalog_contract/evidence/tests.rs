use serde_json::{json, Value};

use super::*;
use crate::adapter::{CanonicalFactId, FactRevisionId};

const ADAPTER_ID: &str = "fixture-agent";

fn source_key(label: &[u8]) -> CanonicalSourceInstanceKey {
    CanonicalSourceInstanceKey::derive(1, label).unwrap()
}

fn owner(label: &str, generation: u64) -> CatalogEvidenceOwner {
    CatalogEvidenceOwner::new(
        ADAPTER_ID,
        source_key(label.as_bytes()),
        CoverageStreamKey::derive(ADAPTER_ID, b"catalog").unwrap(),
        CoverageObjectKey::derive("fixture-catalog", label.as_bytes()).unwrap(),
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
        "catalog.fixture",
        label.as_bytes(),
    )
    .unwrap();
    SemanticRevisionRef::new(FactRevisionId::derive(&fact, 1, label.as_bytes()).unwrap())
}

fn retraction_evidence(owner: &CatalogEvidenceOwner, label: &str) -> CatalogRetractionEvidence {
    CatalogRetractionEvidence::new(
        owner.clone(),
        CatalogRetractionCause::ConfirmedDeletion,
        ContractCompleteness::Complete,
        vec![revision(owner, label)],
    )
    .unwrap()
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

fn unknown_field<T>(
    owner: &CatalogEvidenceOwner,
    label: &str,
    authority: CatalogFieldAuthority,
) -> CatalogQualifiedField<T> {
    CatalogQualifiedField::new(
        QualifiedValue::from_parts(
            None,
            QualifiedValueQuality::Unknown,
            authority,
            ContractCompleteness::Unknown,
            Some(QualifiedUnknownReason::Missing),
            None,
            vec![revision(owner, label)],
        )
        .unwrap(),
        CatalogDisclosureClass::Public,
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

fn session_assertion(
    owner: CatalogEvidenceOwner,
    native_key: &str,
    session_ref: CatalogEntityRef,
    title: &str,
    title_quality: QualifiedValueQuality,
    title_authority: CatalogFieldAuthority,
    effective_at: Option<i64>,
) -> CatalogSessionAssertion {
    let provenance = vec![revision(&owner, &format!("{native_key}-assertion"))];
    CatalogSessionAssertion::new(
        owner.clone(),
        native_key.as_bytes(),
        session_ref,
        Some(field(
            &owner,
            &format!("{native_key}-identity"),
            NativeIdentity {
                native_namespace: "fixture.session".to_owned(),
                native_id: native_key.to_owned(),
            },
            QualifiedValueQuality::NativeClaimed,
            authority("native-session-id", 100),
            effective_at,
            CatalogDisclosureClass::PolicyShareable,
        )),
        Some(field(
            &owner,
            &format!("{native_key}-title"),
            title.to_owned(),
            title_quality,
            title_authority,
            effective_at,
            CatalogDisclosureClass::Public,
        )),
        None,
        None,
        None,
        None,
        None,
        availability(&owner, &format!("{native_key}-availability")),
        provenance,
    )
    .unwrap()
}

fn project_assertion(
    owner: CatalogEvidenceOwner,
    native_key: &str,
    project_ref: CatalogEntityRef,
    display_name: &str,
) -> CatalogProjectAssertion {
    let provenance = vec![revision(&owner, &format!("{native_key}-assertion"))];
    CatalogProjectAssertion::new(
        owner.clone(),
        native_key.as_bytes(),
        project_ref,
        Some(field(
            &owner,
            &format!("{native_key}-identity"),
            NativeIdentity {
                native_namespace: "fixture.project".to_owned(),
                native_id: native_key.to_owned(),
            },
            QualifiedValueQuality::NativeClaimed,
            authority("native-project-id", 100),
            None,
            CatalogDisclosureClass::PolicyShareable,
        )),
        Some(field(
            &owner,
            &format!("{native_key}-root"),
            format!("root:{native_key}"),
            QualifiedValueQuality::Exact,
            authority("root-identity", 90),
            None,
            CatalogDisclosureClass::LocalSensitive,
        )),
        None,
        Some(field(
            &owner,
            &format!("{native_key}-name"),
            display_name.to_owned(),
            QualifiedValueQuality::NativeClaimed,
            authority("native-project-index", 80),
            None,
            CatalogDisclosureClass::Public,
        )),
        None,
        availability(&owner, &format!("{native_key}-availability")),
        provenance,
    )
    .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn association(
    owner: CatalogEvidenceOwner,
    native_key: &str,
    session_ref: CatalogEntityRef,
    project_ref: CatalogEntityRef,
    basis: ProjectAssociationBasis,
    quality: QualifiedValueQuality,
    effective_at: Option<i64>,
) -> Result<SessionProjectAssociationFact, CatalogContractError> {
    let derivation = (basis == ProjectAssociationBasis::DeclaredDerivedAncestor)
        .then(|| "fixture.declared-ancestor-v1".to_owned());
    SessionProjectAssociationFact::new(
        owner.clone(),
        native_key.as_bytes(),
        session_ref,
        project_ref,
        basis,
        derivation,
        None,
        authority("native-project-association", 70),
        quality,
        ContractCompleteness::Complete,
        effective_at,
        vec![revision(&owner, &format!("{native_key}-association"))],
    )
}

fn locator(
    owner: CatalogEvidenceOwner,
    native_key: &str,
    subject_ref: CatalogEntityRef,
    path: &str,
) -> NativeLocatorClaim {
    NativeLocatorClaim::new(
        owner.clone(),
        native_key.as_bytes(),
        subject_ref,
        CatalogLocatorKind::Filesystem,
        field(
            &owner,
            &format!("{native_key}-locator"),
            CatalogLocatorValue {
                native_value: Some(path.to_owned()),
                canonical_local_path: Some(path.to_owned()),
            },
            QualifiedValueQuality::Exact,
            authority("transcript-path", 100),
            None,
            CatalogDisclosureClass::LocalSensitive,
        ),
        ProjectAssociationBasis::SessionDirectory,
        vec![revision(&owner, &format!("{native_key}-claim"))],
    )
    .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn relation(
    owner: CatalogEvidenceOwner,
    native_key: &str,
    kind: IdentityRelationKind,
    left_ref: CatalogEntityRef,
    right_ref: CatalogEntityRef,
    quality: QualifiedValueQuality,
    completeness: ContractCompleteness,
    canonical_winner: Option<CatalogEntityRef>,
    collision_policy_id: Option<&str>,
) -> Result<IdentityRelationFact, CatalogContractError> {
    IdentityRelationFact::new(
        owner.clone(),
        native_key.as_bytes(),
        kind,
        left_ref,
        right_ref,
        authority("native-identity-relation", 100),
        quality,
        completeness,
        canonical_winner,
        collision_policy_id.map(str::to_owned),
        vec![revision(&owner, &format!("{native_key}-relation"))],
    )
}

#[test]
fn assertion_identity_is_owner_generation_bound_and_rejects_false_kind_strengthening() {
    let first_owner = owner("source-a", 1);
    let next_generation = owner("source-a", 2);
    let shared_key = CanonicalEntityKey::derive(
        ADAPTER_ID,
        &first_owner.source_instance_key,
        "ambiguous-fixture",
        b"shared-key",
    )
    .unwrap();
    let project_ref = CatalogEntityRef::project(shared_key);
    let session_ref = CatalogEntityRef::session(shared_key);

    let first = project_assertion(first_owner.clone(), "project-a", project_ref, "Project A");
    let repeat = project_assertion(first_owner, "project-a", project_ref, "Project A");
    let next = project_assertion(next_generation, "project-a", project_ref, "Project A");
    assert_eq!(first.assertion_key, repeat.assertion_key);
    assert_ne!(first.assertion_key, next.assertion_key);

    let mut reducer = CatalogReducer::default();
    assert_eq!(
        reducer.upsert_project_assertion(first, 1).unwrap(),
        CatalogMutation::Inserted
    );
    let session = session_assertion(
        owner("source-a", 1),
        "session-a",
        session_ref,
        "Session A",
        QualifiedValueQuality::Exact,
        authority("transcript-header", 80),
        None,
    );
    assert!(reducer.upsert_session_assertion(session, 2).is_err());

    let public_identity_owner = owner("public-native-id", 1);
    let public_identity_ref = entity(
        &public_identity_owner,
        CatalogEntityKind::Project,
        "public-native-id",
    );
    assert!(CatalogProjectAssertion::new(
        public_identity_owner.clone(),
        b"public-native-id",
        public_identity_ref,
        Some(field(
            &public_identity_owner,
            "public-native-id",
            NativeIdentity {
                native_namespace: "fixture.project".to_owned(),
                native_id: "must-be-policy-bound".to_owned(),
            },
            QualifiedValueQuality::Exact,
            authority("native-project-id", 100),
            None,
            CatalogDisclosureClass::Public,
        )),
        None,
        None,
        None,
        None,
        availability(&public_identity_owner, "public-native-id-availability"),
        vec![revision(
            &public_identity_owner,
            "public-native-id-assertion"
        )],
    )
    .is_err());
}

#[test]
fn reducer_uses_declared_precedence_and_retains_equal_authority_conflicts() {
    let base_owner = owner("base", 1);
    let session_ref = entity(&base_owner, CatalogEntityKind::Session, "session-a");
    let exact_old = session_assertion(
        owner("exact-old", 1),
        "exact-old",
        session_ref,
        "Exact old",
        QualifiedValueQuality::Exact,
        authority("transcript-title", 80),
        Some(10),
    );
    let claimed_late = session_assertion(
        owner("claimed-late", 1),
        "claimed-late",
        session_ref,
        "Claimed late",
        QualifiedValueQuality::NativeClaimed,
        authority("transcript-title", 80),
        Some(100),
    );
    let claimed_late_key = claimed_late.assertion_key;
    let exact_new = session_assertion(
        owner("exact-new", 1),
        "exact-new",
        session_ref,
        "Exact new",
        QualifiedValueQuality::Exact,
        authority("transcript-title", 80),
        Some(20),
    );
    let exact_old_key = exact_old.assertion_key;
    let exact_new_key = exact_new.assertion_key;

    let mut reducer = CatalogReducer::default();
    reducer.upsert_session_assertion(exact_old, 10).unwrap();
    reducer.upsert_session_assertion(claimed_late, 100).unwrap();
    let row = reducer.session_row(session_ref).unwrap();
    assert_eq!(
        row.title.as_ref().unwrap().field.qualified.value.as_deref(),
        Some("Exact old")
    );

    reducer.upsert_session_assertion(exact_new, 20).unwrap();
    let title = reducer.session_row(session_ref).unwrap().title.unwrap();
    assert_eq!(title.selected_assertion_key, exact_new_key);
    assert_eq!(title.field.qualified.value.as_deref(), Some("Exact new"));
    let mut expected_conflicts = vec![exact_old_key, claimed_late_key];
    expected_conflicts.sort();
    assert_eq!(title.conflicting_assertion_keys, expected_conflicts);

    let unknown_owner = owner("unknown-title", 1);
    let unknown = CatalogSessionAssertion::new(
        unknown_owner.clone(),
        b"unknown-title",
        session_ref,
        None,
        Some(unknown_field(
            &unknown_owner,
            "unknown-title",
            authority("unknown-high-authority", 1_000),
        )),
        None,
        None,
        None,
        None,
        None,
        availability(&unknown_owner, "unknown-title-availability"),
        vec![revision(&unknown_owner, "unknown-title-assertion")],
    )
    .unwrap();
    reducer.upsert_session_assertion(unknown, 1_000).unwrap();
    assert!(reducer
        .session_row(session_ref)
        .unwrap()
        .title
        .unwrap()
        .field
        .qualified
        .value
        .is_some());

    let mutable_owner = owner("mutable", 1);
    let original = session_assertion(
        mutable_owner.clone(),
        "mutable",
        session_ref,
        "Before",
        QualifiedValueQuality::Exact,
        authority("transcript-title", 80),
        None,
    );
    let corrected = session_assertion(
        mutable_owner,
        "mutable",
        session_ref,
        "After",
        QualifiedValueQuality::Exact,
        authority("transcript-title", 80),
        None,
    );
    reducer.upsert_session_assertion(original, 200).unwrap();
    assert!(reducer
        .upsert_session_assertion(corrected.clone(), 200)
        .is_err());
    assert_eq!(
        reducer
            .upsert_session_assertion(corrected.clone(), 201)
            .unwrap(),
        CatalogMutation::Updated
    );
    assert_eq!(
        reducer.upsert_session_assertion(corrected, 1).unwrap(),
        CatalogMutation::Noop
    );
}

#[test]
fn identical_newer_observations_refresh_precedence_and_retraction_guards() {
    let first_owner = owner("refresh-first", 1);
    let second_owner = owner("refresh-second", 1);
    let session_ref = entity(&first_owner, CatalogEntityKind::Session, "refresh-session");
    let related_ref = entity(&first_owner, CatalogEntityKind::Session, "refresh-related");
    let first_project_ref = entity(
        &first_owner,
        CatalogEntityKind::Project,
        "refresh-project-a",
    );
    let second_project_ref = entity(
        &first_owner,
        CatalogEntityKind::Project,
        "refresh-project-b",
    );
    let project = project_assertion(
        first_owner.clone(),
        "refresh-project",
        first_project_ref,
        "Refresh project",
    );
    let first_session = session_assertion(
        first_owner.clone(),
        "refresh-session-first",
        session_ref,
        "First observation",
        QualifiedValueQuality::Exact,
        authority("refresh-title", 80),
        None,
    );
    let second_session = session_assertion(
        second_owner,
        "refresh-session-second",
        session_ref,
        "Second observation",
        QualifiedValueQuality::Exact,
        authority("refresh-title", 80),
        None,
    );
    let first_association = association(
        first_owner.clone(),
        "refresh-association-first",
        session_ref,
        first_project_ref,
        ProjectAssociationBasis::NativeProjectIndex,
        QualifiedValueQuality::Exact,
        None,
    )
    .unwrap();
    let second_association = association(
        owner("refresh-association-second", 1),
        "refresh-association-second",
        session_ref,
        second_project_ref,
        ProjectAssociationBasis::NativeProjectIndex,
        QualifiedValueQuality::Exact,
        None,
    )
    .unwrap();
    let locator = locator(
        first_owner.clone(),
        "refresh-locator",
        session_ref,
        "/private/fixture/refresh.jsonl",
    );
    let identity_relation = relation(
        first_owner.clone(),
        "refresh-relation",
        IdentityRelationKind::Alias,
        session_ref,
        related_ref,
        QualifiedValueQuality::NativeClaimed,
        ContractCompleteness::Partial,
        None,
        None,
    )
    .unwrap();
    let project_key = project.assertion_key;
    let first_session_key = first_session.assertion_key;
    let second_session_key = second_session.assertion_key;
    let first_association_key = first_association.association_key;
    let second_association_key = second_association.association_key;
    let locator_key = locator.locator_claim_key;
    let relation_key = identity_relation.relation_key;

    let mut reducer = CatalogReducer::default();
    reducer
        .upsert_project_assertion(project.clone(), 10)
        .unwrap();
    reducer
        .upsert_session_assertion(first_session.clone(), 10)
        .unwrap();
    reducer
        .upsert_session_assertion(second_session, 20)
        .unwrap();
    reducer
        .upsert_association(first_association.clone(), 10)
        .unwrap();
    reducer.upsert_association(second_association, 20).unwrap();
    reducer.upsert_locator_claim(locator.clone(), 10).unwrap();
    reducer
        .upsert_identity_relation(identity_relation.clone(), 10)
        .unwrap();
    assert_eq!(
        reducer
            .session_row(session_ref)
            .unwrap()
            .title
            .unwrap()
            .selected_assertion_key,
        second_session_key
    );
    let CatalogAssociationCoverage::Available { selection } =
        reducer.association_for_session(session_ref)
    else {
        panic!("session associations must be available");
    };
    assert_eq!(
        selection.association.association_key,
        second_association_key
    );

    assert_eq!(
        reducer
            .upsert_project_assertion(project.clone(), 40)
            .unwrap(),
        CatalogMutation::Updated
    );
    assert_eq!(
        reducer
            .upsert_session_assertion(first_session.clone(), 40)
            .unwrap(),
        CatalogMutation::Updated
    );
    assert_eq!(
        reducer
            .upsert_association(first_association.clone(), 40)
            .unwrap(),
        CatalogMutation::Updated
    );
    assert_eq!(
        reducer.upsert_locator_claim(locator.clone(), 40).unwrap(),
        CatalogMutation::Updated
    );
    assert_eq!(
        reducer
            .upsert_identity_relation(identity_relation.clone(), 40)
            .unwrap(),
        CatalogMutation::Updated
    );
    assert_eq!(
        reducer.upsert_project_assertion(project, 40).unwrap(),
        CatalogMutation::Noop
    );
    assert_eq!(
        reducer.upsert_session_assertion(first_session, 39).unwrap(),
        CatalogMutation::Noop
    );
    assert_eq!(
        reducer.upsert_association(first_association, 39).unwrap(),
        CatalogMutation::Noop
    );
    assert_eq!(
        reducer.upsert_locator_claim(locator, 40).unwrap(),
        CatalogMutation::Noop
    );
    assert_eq!(
        reducer
            .upsert_identity_relation(identity_relation, 39)
            .unwrap(),
        CatalogMutation::Noop
    );

    assert_eq!(
        reducer
            .session_row(session_ref)
            .unwrap()
            .title
            .unwrap()
            .selected_assertion_key,
        first_session_key
    );
    let CatalogAssociationCoverage::Available { selection } =
        reducer.association_for_session(session_ref)
    else {
        panic!("session associations must be available");
    };
    assert_eq!(selection.association.association_key, first_association_key);
    assert_eq!(
        reducer
            .projects
            .get(&project_key)
            .unwrap()
            .observation_commit,
        40
    );
    assert_eq!(
        reducer
            .sessions
            .get(&first_session_key)
            .unwrap()
            .observation_commit,
        40
    );
    assert_eq!(
        reducer
            .associations
            .get(&first_association_key)
            .unwrap()
            .observation_commit,
        40
    );
    assert_eq!(
        reducer
            .locators
            .get(&locator_key)
            .unwrap()
            .observation_commit,
        40
    );
    assert_eq!(
        reducer
            .identity_relations
            .get(&relation_key)
            .unwrap()
            .observation_commit,
        40
    );

    let retraction = retraction_evidence(&first_owner, "refresh-deletion");
    assert!(reducer.retract_owner(&retraction, 30).is_err());
    let applied = reducer.retract_owner(&retraction, 41).unwrap();
    assert_eq!(applied.assertion_count, 2);
    assert_eq!(applied.association_count, 1);
    assert_eq!(applied.locator_count, 1);
    assert_eq!(applied.identity_relation_count, 1);
}

#[test]
fn retraction_is_owner_scoped_and_tombstones_require_complete_later_coverage() {
    let source_a = owner("source-a", 1);
    let source_b = owner("source-b", 1);
    let evidence_a = retraction_evidence(&source_a, "source-a-deletion");
    let evidence_b = retraction_evidence(&source_b, "source-b-deletion");
    let session_ref = entity(&source_a, CatalogEntityKind::Session, "shared-session");
    let assertion_a = session_assertion(
        source_a.clone(),
        "source-a-session",
        session_ref,
        "From A",
        QualifiedValueQuality::Exact,
        authority("transcript-title", 80),
        None,
    );
    let assertion_b = session_assertion(
        source_b.clone(),
        "source-b-session",
        session_ref,
        "From B",
        QualifiedValueQuality::Exact,
        authority("transcript-title", 80),
        None,
    );
    let mut reducer = CatalogReducer::default();
    reducer
        .upsert_session_assertion(assertion_a.clone(), 10)
        .unwrap();
    reducer.upsert_session_assertion(assertion_b, 11).unwrap();

    assert!(CatalogRetractionEvidence::new(
        source_a.clone(),
        CatalogRetractionCause::ConfirmedDeletion,
        ContractCompleteness::Partial,
        vec![revision(&source_a, "partial-deletion")],
    )
    .is_err());
    assert!(CatalogRetractionEvidence::new(
        source_a.clone(),
        CatalogRetractionCause::TemporarilyUnavailable,
        ContractCompleteness::Complete,
        vec![revision(&source_a, "temporary-unavailability")],
    )
    .is_err());
    assert!(reducer.retract_owner(&evidence_a, 10).is_err());
    assert!(reducer.session_row(session_ref).is_some());

    let first = reducer.retract_owner(&evidence_a, 20).unwrap();
    assert_eq!(first.assertion_count, 1);
    assert!(first.orphaned_entities.is_empty());
    assert!(reducer.session_row(session_ref).is_some());
    assert!(reducer
        .upsert_session_assertion(assertion_a.clone(), 21)
        .is_err());
    let retargeted = session_assertion(
        source_a.clone(),
        "source-a-session",
        entity(&source_a, CatalogEntityKind::Session, "retargeted-session"),
        "Retargeted",
        QualifiedValueQuality::Exact,
        authority("transcript-title", 80),
        None,
    );
    assert!(reducer.upsert_session_assertion(retargeted, 21).is_err());

    let second = reducer.retract_owner(&evidence_b, 30).unwrap();
    assert_eq!(second.orphaned_entities, vec![session_ref]);
    assert!(matches!(
        reducer.resolve_external_ref(session_ref.external_ref),
        CatalogResolvedEntity::Unknown {
            reason: CatalogUnknownReferenceReason::RetractedPendingPublication,
            ..
        }
    ));
    assert!(reducer
        .confirm_absent(session_ref, &evidence_a, 25)
        .is_err());
    assert!(reducer
        .confirm_absent(session_ref, &evidence_b, 30)
        .is_err());
    let mut reversed_caller = reducer.clone();
    assert_eq!(
        reducer
            .confirm_absent(session_ref, &evidence_b, 31)
            .unwrap(),
        CatalogMutation::Inserted
    );
    let CatalogResolvedEntity::Tombstoned { tombstone } =
        reducer.resolve_external_ref(session_ref.external_ref)
    else {
        panic!("complete confirmed deletion must publish a tombstone");
    };
    reversed_caller
        .confirm_absent(session_ref, &evidence_a, 31)
        .unwrap();
    let CatalogResolvedEntity::Tombstoned {
        tombstone: reversed_tombstone,
    } = reversed_caller.resolve_external_ref(session_ref.external_ref)
    else {
        panic!("either complete owner proof must publish the same tombstone");
    };
    assert_eq!(tombstone, reversed_tombstone);
    let mut expected_absence_evidence = vec![evidence_a.clone(), evidence_b.clone()];
    expected_absence_evidence.sort_by(|left, right| left.owner.cmp(&right.owner));
    assert_eq!(tombstone.absence_evidence, expected_absence_evidence);
    assert!(!tombstone.provenance.is_empty());
    assert!(reducer
        .upsert_session_assertion(assertion_a.clone(), 31)
        .is_err());
    assert!(reducer.upsert_session_assertion(assertion_a, 32).is_err());
    let next_generation = owner("source-a", 2);
    let revived = session_assertion(
        next_generation,
        "source-a-session",
        session_ref,
        "From A, generation 2",
        QualifiedValueQuality::Exact,
        authority("transcript-title", 80),
        None,
    );
    reducer.upsert_session_assertion(revived, 33).unwrap();
    assert!(matches!(
        reducer.resolve_external_ref(session_ref.external_ref),
        CatalogResolvedEntity::Live {
            entity_ref,
            row,
        } if entity_ref == session_ref
            && matches!(*row, CatalogLiveRow::Session(_))
    ));
}

#[test]
fn associations_remain_explicit_conflicting_evidence_and_retract_independently() {
    let source_a = owner("association-a", 1);
    let source_b = owner("association-b", 1);
    let evidence_a = retraction_evidence(&source_a, "association-a-deletion");
    let evidence_b = retraction_evidence(&source_b, "association-b-deletion");
    let session_ref = entity(&source_a, CatalogEntityKind::Session, "session-a");
    let project_a = entity(&source_a, CatalogEntityKind::Project, "project-a");
    let project_b = entity(&source_a, CatalogEntityKind::Project, "project-b");
    let older = association(
        source_a.clone(),
        "association-a",
        session_ref,
        project_a,
        ProjectAssociationBasis::NativeProjectIndex,
        QualifiedValueQuality::Exact,
        Some(10),
    )
    .unwrap();
    let older_key = older.association_key;
    let newer = association(
        source_b.clone(),
        "association-b",
        session_ref,
        project_b,
        ProjectAssociationBasis::TranscriptCwd,
        QualifiedValueQuality::Exact,
        Some(20),
    )
    .unwrap();

    let mut reducer = CatalogReducer::default();
    reducer
        .upsert_session_assertion(
            session_assertion(
                owner("association-membership", 1),
                "association-membership",
                session_ref,
                "Associated session",
                QualifiedValueQuality::Exact,
                authority("transcript-title", 80),
                None,
            ),
            5,
        )
        .unwrap();
    reducer.upsert_association(older, 10).unwrap();
    reducer.upsert_association(newer, 20).unwrap();
    let CatalogAssociationCoverage::Available { selection } =
        reducer.association_for_session(session_ref)
    else {
        panic!("association evidence should remain available");
    };
    assert_eq!(selection.association.project_ref, project_b);
    assert_eq!(selection.competing_associations.len(), 1);
    assert_eq!(selection.competing_associations[0].project_ref, project_a);
    assert_eq!(selection.conflicting_association_keys, vec![older_key]);
    let CatalogAssociationCoverage::Available {
        selection: row_selection,
    } = reducer
        .session_row(session_ref)
        .unwrap()
        .project_association
    else {
        panic!("the session row must compose association coverage");
    };
    assert_eq!(row_selection.association.project_ref, project_b);
    assert_eq!(row_selection.competing_associations.len(), 1);

    reducer.retract_owner(&evidence_b, 21).unwrap();
    let CatalogAssociationCoverage::Available { selection } =
        reducer.association_for_session(session_ref)
    else {
        panic!("the independently owned association must remain");
    };
    assert_eq!(selection.association.project_ref, project_a);
    reducer.retract_owner(&evidence_a, 22).unwrap();
    assert_eq!(
        reducer.association_for_session(session_ref),
        CatalogAssociationCoverage::Unknown
    );

    assert!(SessionProjectAssociationFact::new(
        owner("invalid-derived", 1),
        b"invalid-derived",
        session_ref,
        project_a,
        ProjectAssociationBasis::DeclaredDerivedAncestor,
        None,
        None,
        authority("native-project-association", 70),
        QualifiedValueQuality::Derived,
        ContractCompleteness::Complete,
        None,
        vec![revision(&owner("invalid-derived", 1), "invalid-derived")],
    )
    .is_err());
}

#[test]
fn retracted_evidence_keys_cannot_retarget_any_immutable_coordinates() {
    let source = owner("immutable-history", 1);
    let session_a = entity(&source, CatalogEntityKind::Session, "session-a");
    let session_b = entity(&source, CatalogEntityKind::Session, "session-b");
    let project_a = entity(&source, CatalogEntityKind::Project, "project-a");
    let project_b = entity(&source, CatalogEntityKind::Project, "project-b");
    let assertion = session_assertion(
        source.clone(),
        "assertion-key",
        session_a,
        "Session A",
        QualifiedValueQuality::Exact,
        authority("transcript-title", 80),
        None,
    );
    let original_association = association(
        source.clone(),
        "association-key",
        session_a,
        project_a,
        ProjectAssociationBasis::NativeProjectIndex,
        QualifiedValueQuality::Exact,
        None,
    )
    .unwrap();
    let original_locator = locator(
        source.clone(),
        "locator-key",
        session_a,
        "/private/fixture/a.jsonl",
    );
    let original_relation = relation(
        source.clone(),
        "relation-key",
        IdentityRelationKind::Alias,
        session_a,
        session_b,
        QualifiedValueQuality::NativeClaimed,
        ContractCompleteness::Partial,
        None,
        None,
    )
    .unwrap();
    let mut reducer = CatalogReducer::default();
    reducer.upsert_session_assertion(assertion, 1).unwrap();
    reducer.upsert_association(original_association, 2).unwrap();
    reducer.upsert_locator_claim(original_locator, 3).unwrap();
    reducer
        .upsert_identity_relation(original_relation, 4)
        .unwrap();
    let deletion = retraction_evidence(&source, "immutable-history-deletion");
    reducer.retract_owner(&deletion, 10).unwrap();
    let repeated = reducer.retract_owner(&deletion, 11).unwrap();
    assert_eq!(repeated.assertion_count, 0);
    assert_eq!(repeated.association_count, 0);
    assert_eq!(repeated.locator_count, 0);
    assert_eq!(repeated.identity_relation_count, 0);
    let conflicting_retraction = CatalogRetractionEvidence::new(
        source.clone(),
        CatalogRetractionCause::ConfirmedReplacement,
        ContractCompleteness::Complete,
        vec![revision(&source, "conflicting-retraction")],
    )
    .unwrap();
    assert!(reducer.retract_owner(&conflicting_retraction, 12).is_err());
    assert_eq!(
        reducer
            .retract_owner(&deletion, 13)
            .unwrap()
            .assertion_count,
        0
    );

    let same_generation_assertion = session_assertion(
        source.clone(),
        "assertion-key",
        session_a,
        "Session A",
        QualifiedValueQuality::Exact,
        authority("transcript-title", 80),
        None,
    );
    assert!(reducer
        .upsert_session_assertion(same_generation_assertion, 11)
        .is_err());
    let same_generation_locator = locator(
        source.clone(),
        "locator-key",
        session_a,
        "/private/fixture/a.jsonl",
    );
    assert!(reducer
        .upsert_locator_claim(same_generation_locator, 11)
        .is_err());

    let retargeted_assertion = session_assertion(
        source.clone(),
        "assertion-key",
        session_b,
        "Session B",
        QualifiedValueQuality::Exact,
        authority("transcript-title", 80),
        None,
    );
    assert!(reducer
        .upsert_session_assertion(retargeted_assertion, 11)
        .is_err());
    let retargeted_association = association(
        source.clone(),
        "association-key",
        session_b,
        project_b,
        ProjectAssociationBasis::NativeProjectIndex,
        QualifiedValueQuality::Exact,
        None,
    )
    .unwrap();
    assert!(reducer
        .upsert_association(retargeted_association, 11)
        .is_err());
    let retargeted_locator = locator(
        source.clone(),
        "locator-key",
        session_b,
        "/private/fixture/b.jsonl",
    );
    assert!(reducer
        .upsert_locator_claim(retargeted_locator, 11)
        .is_err());
    let retargeted_relation = relation(
        source,
        "relation-key",
        IdentityRelationKind::ReplacedBy,
        session_a,
        session_b,
        QualifiedValueQuality::Exact,
        ContractCompleteness::Complete,
        None,
        None,
    )
    .unwrap();
    assert!(reducer
        .upsert_identity_relation(retargeted_relation, 11)
        .is_err());
}

#[test]
fn identity_relations_never_heuristically_merge_or_silently_retarget() {
    let relation_owner = owner("relations", 1);
    let old_ref = entity(&relation_owner, CatalogEntityKind::Session, "old");
    let new_ref = entity(&relation_owner, CatalogEntityKind::Session, "new");
    let alternate_ref = entity(&relation_owner, CatalogEntityKind::Session, "alternate");

    assert!(relation(
        relation_owner.clone(),
        "false-same-derived",
        IdentityRelationKind::SameEntity,
        old_ref,
        new_ref,
        QualifiedValueQuality::Derived,
        ContractCompleteness::Complete,
        Some(old_ref),
        Some("lowest-key-v1"),
    )
    .is_err());
    assert!(relation(
        relation_owner.clone(),
        "false-same-no-winner",
        IdentityRelationKind::SameEntity,
        old_ref,
        new_ref,
        QualifiedValueQuality::Exact,
        ContractCompleteness::Complete,
        None,
        Some("lowest-key-v1"),
    )
    .is_err());

    let first_same = relation(
        relation_owner.clone(),
        "same-first",
        IdentityRelationKind::SameEntity,
        old_ref,
        new_ref,
        QualifiedValueQuality::Exact,
        ContractCompleteness::Complete,
        Some(old_ref),
        Some("lowest-key-v1"),
    )
    .unwrap();
    let contradictory_same = relation(
        relation_owner.clone(),
        "same-contradictory",
        IdentityRelationKind::SameEntity,
        old_ref,
        alternate_ref,
        QualifiedValueQuality::Exact,
        ContractCompleteness::Complete,
        Some(alternate_ref),
        Some("prefer-newest-v1"),
    )
    .unwrap();
    let mut ambiguous_reducer = CatalogReducer::default();
    ambiguous_reducer
        .upsert_identity_relation(first_same, 1)
        .unwrap();
    assert!(ambiguous_reducer
        .upsert_identity_relation(contradictory_same, 2)
        .is_err());

    let alias = relation(
        relation_owner.clone(),
        "alias",
        IdentityRelationKind::Alias,
        old_ref,
        new_ref,
        QualifiedValueQuality::NativeClaimed,
        ContractCompleteness::Partial,
        None,
        None,
    )
    .unwrap();
    let mut reducer = CatalogReducer::default();
    reducer.upsert_identity_relation(alias, 10).unwrap();
    assert!(matches!(
        reducer.resolve_external_ref(old_ref.external_ref),
        CatalogResolvedEntity::Unknown {
            reason: CatalogUnknownReferenceReason::RelatedIdentityOnly,
            related_refs,
            ..
        } if related_refs == vec![new_ref]
    ));

    let new_assertion = session_assertion(
        owner("new-session", 1),
        "new-session",
        new_ref,
        "New",
        QualifiedValueQuality::Exact,
        authority("transcript-title", 80),
        None,
    );
    reducer.upsert_session_assertion(new_assertion, 11).unwrap();
    let replacement = relation(
        relation_owner.clone(),
        "replacement",
        IdentityRelationKind::ReplacedBy,
        old_ref,
        new_ref,
        QualifiedValueQuality::Exact,
        ContractCompleteness::Complete,
        None,
        None,
    )
    .unwrap();
    reducer.upsert_identity_relation(replacement, 12).unwrap();
    let CatalogResolvedEntity::Superseded {
        prior_ref,
        replacement_refs,
        provenance,
        ..
    } = reducer.resolve_external_ref(old_ref.external_ref)
    else {
        panic!("replacement evidence must resolve explicitly as superseded");
    };
    assert_eq!(prior_ref, old_ref);
    assert_eq!(replacement_refs, vec![new_ref]);
    assert!(!provenance.is_empty());

    let second_replacement = relation(
        relation_owner.clone(),
        "alternate-replacement",
        IdentityRelationKind::ReplacedBy,
        old_ref,
        alternate_ref,
        QualifiedValueQuality::Exact,
        ContractCompleteness::Complete,
        None,
        None,
    )
    .unwrap();
    reducer
        .upsert_identity_relation(second_replacement, 13)
        .unwrap();
    assert!(matches!(
        reducer.resolve_external_ref(old_ref.external_ref),
        CatalogResolvedEntity::Superseded { replacement_refs, .. }
            if replacement_refs == vec![new_ref, alternate_ref]
                || replacement_refs == vec![alternate_ref, new_ref]
    ));

    let cycle = relation(
        relation_owner,
        "cycle",
        IdentityRelationKind::ReplacedBy,
        new_ref,
        old_ref,
        QualifiedValueQuality::Exact,
        ContractCompleteness::Complete,
        None,
        None,
    )
    .unwrap();
    assert!(reducer.upsert_identity_relation(cycle, 14).is_err());
    assert!(matches!(
        reducer.resolve_external_ref(new_ref.external_ref),
        CatalogResolvedEntity::Live { entity_ref, row }
            if entity_ref == new_ref && matches!(*row, CatalogLiveRow::Session(_))
    ));
}

#[test]
fn locator_disclosure_and_attach_require_one_live_disclosed_base_member() {
    let source = owner("attach", 1);
    let selected_ref = entity(&source, CatalogEntityKind::Session, "selected");
    let presentation_ref = entity(&source, CatalogEntityKind::Session, "presentation");
    let mut assertion = session_assertion(
        source.clone(),
        "selected",
        selected_ref,
        "Selected",
        QualifiedValueQuality::Exact,
        authority("transcript-title", 80),
        None,
    );
    let claim = locator(
        source.clone(),
        "selected-transcript",
        selected_ref,
        "/private/fixture/session.jsonl",
    );
    let locator_key = claim.locator_claim_key;
    assertion.transcript_locator_claim = Some(locator_key);
    let same_entity = relation(
        source.clone(),
        "presentation-membership",
        IdentityRelationKind::SameEntity,
        presentation_ref,
        selected_ref,
        QualifiedValueQuality::Exact,
        ContractCompleteness::Complete,
        Some(presentation_ref),
        Some("presentation-winner-v1"),
    )
    .unwrap();
    let relation_key = same_entity.relation_key;
    let mut reducer = CatalogReducer::default();
    reducer.upsert_session_assertion(assertion, 10).unwrap();
    reducer.upsert_locator_claim(claim, 10).unwrap();
    assert_eq!(
        reducer
            .session_row(selected_ref)
            .unwrap()
            .project_association,
        CatalogAssociationCoverage::Unknown
    );
    assert_eq!(
        reducer
            .session_row(selected_ref)
            .unwrap()
            .transcript_locator_claim_keys,
        vec![locator_key]
    );

    assert!(CatalogSessionAttachHandoff::new(
        presentation_ref,
        vec![selected_ref],
        Vec::new(),
        selected_ref,
        locator_key,
    )
    .is_err());
    assert!(CatalogSessionAttachHandoff::new(
        presentation_ref,
        vec![selected_ref, presentation_ref],
        Vec::new(),
        selected_ref,
        locator_key,
    )
    .is_err());
    let handoff = CatalogSessionAttachHandoff::new(
        presentation_ref,
        vec![selected_ref, presentation_ref],
        vec![relation_key],
        selected_ref,
        locator_key,
    )
    .unwrap();
    assert!(reducer
        .resolve_attach_target(&handoff, CatalogPolicyView::LOCAL)
        .is_err());
    reducer.upsert_identity_relation(same_entity, 11).unwrap();
    let target = reducer
        .resolve_attach_target(&handoff, CatalogPolicyView::LOCAL)
        .unwrap();
    assert_eq!(target.session_ref, selected_ref);
    assert_eq!(
        target
            .locator
            .value
            .as_ref()
            .unwrap()
            .canonical_local_path
            .as_deref(),
        Some("/private/fixture/session.jsonl")
    );
    assert!(reducer
        .resolve_attach_target(&handoff, CatalogPolicyView::WITHHELD)
        .is_err());

    let withheld = reducer
        .locators
        .get(&locator_key)
        .unwrap()
        .fact
        .for_view(CatalogPolicyView::WITHHELD);
    assert_eq!(withheld.quality, QualifiedValueQuality::Unknown);
    assert_eq!(
        withheld.unknown_reason,
        Some(QualifiedUnknownReason::Withheld)
    );
    assert!(withheld.value.is_none());
}

#[test]
fn durable_reducer_restart_reconstructs_exact_live_and_tombstoned_lifecycle() {
    let live_owner = owner("durable-live", 1);
    let deleted_owner = owner("durable-deleted", 1);
    let live_ref = entity(&live_owner, CatalogEntityKind::Session, "live");
    let deleted_ref = entity(&deleted_owner, CatalogEntityKind::Session, "deleted");
    let prior_ref = entity(&live_owner, CatalogEntityKind::Session, "prior");
    let replacement_ref = entity(&live_owner, CatalogEntityKind::Session, "replacement");
    let mut reducer = CatalogReducer::default();
    reducer
        .upsert_session_assertion(
            session_assertion(
                live_owner.clone(),
                "live",
                live_ref,
                "Live",
                QualifiedValueQuality::Exact,
                authority("transcript-title", 80),
                None,
            ),
            10,
        )
        .unwrap();
    for (native_key, session_ref) in [("prior", prior_ref), ("replacement", replacement_ref)] {
        reducer
            .upsert_session_assertion(
                session_assertion(
                    live_owner.clone(),
                    native_key,
                    session_ref,
                    native_key,
                    QualifiedValueQuality::Exact,
                    authority("transcript-title", 80),
                    None,
                ),
                10,
            )
            .unwrap();
    }
    reducer
        .upsert_identity_relation(
            relation(
                live_owner,
                "replacement-relation",
                IdentityRelationKind::ReplacedBy,
                prior_ref,
                replacement_ref,
                QualifiedValueQuality::Exact,
                ContractCompleteness::Complete,
                None,
                None,
            )
            .unwrap(),
            11,
        )
        .unwrap();
    reducer
        .upsert_session_assertion(
            session_assertion(
                deleted_owner.clone(),
                "deleted",
                deleted_ref,
                "Deleted",
                QualifiedValueQuality::Exact,
                authority("transcript-title", 80),
                None,
            ),
            11,
        )
        .unwrap();
    let absence = retraction_evidence(&deleted_owner, "deleted-absence");
    reducer.retract_owner(&absence, 12).unwrap();
    reducer.confirm_absent(deleted_ref, &absence, 13).unwrap();

    let frozen = reducer
        .freeze_for_initial_publication(CatalogReducerPublicationLimits::default())
        .unwrap();
    let payload = frozen.durable_state_json(1024 * 1024).unwrap();
    let tombstone_payload = serde_json::to_vec(&frozen.tombstones()[0]).unwrap();
    let tombstone = decode_durable_tombstone(
        &tombstone_payload,
        deleted_ref.external_ref.entity_key.as_bytes(),
        1024 * 1024,
    )
    .unwrap();
    let restored = decode_durable_reducer_state(&payload, 1024 * 1024)
        .unwrap()
        .finish(
            vec![tombstone],
            frozen.revision(),
            CatalogReducerPublicationLimits::default(),
        )
        .unwrap();
    assert_eq!(restored, frozen);
    assert!(restored
        .validate_durable_row_commitments(
            1024 * 1024,
            restored.project_row_count(),
            restored.session_row_count() + 1,
            |_, _, _, _| true,
        )
        .is_err());
    assert!(matches!(
        restored
            .resolution_index()
            .unwrap()
            .resolve(live_ref.external_ref),
        CatalogResolvedLifecycle::Live { entity_ref } if entity_ref == live_ref
    ));
    assert!(matches!(
        restored
            .resolution_index()
            .unwrap()
            .resolve(deleted_ref.external_ref),
        CatalogResolvedLifecycle::Tombstoned { entity_ref, .. } if entity_ref == deleted_ref
    ));
    assert!(matches!(
        restored
            .resolution_index()
            .unwrap()
            .resolve(prior_ref.external_ref),
        CatalogResolvedLifecycle::Superseded {
            prior_ref: resolved_prior,
            replacement_refs,
            ..
        } if resolved_prior == prior_ref && replacement_refs == vec![replacement_ref]
    ));
}

#[test]
fn durable_reducer_restart_rejects_canonical_semantic_drift_and_unknown_fields() {
    let source = owner("durable-drift", 1);
    let session_ref = entity(&source, CatalogEntityKind::Session, "session");
    let mut reducer = CatalogReducer::default();
    reducer
        .upsert_session_assertion(
            session_assertion(
                source,
                "session",
                session_ref,
                "Session",
                QualifiedValueQuality::Exact,
                authority("transcript-title", 80),
                None,
            ),
            10,
        )
        .unwrap();
    let frozen = reducer
        .freeze_for_initial_publication(CatalogReducerPublicationLimits::default())
        .unwrap();
    let payload = frozen.durable_state_json(1024 * 1024).unwrap();
    let mut drifted: DurableReducerStateWire = serde_json::from_slice(&payload).unwrap();
    drifted.sessions[0].observation_commit = 11;
    let drifted =
        serialize_private_json_bounded(&drifted, 1024 * 1024, "drifted durable reducer fixture")
            .unwrap();
    let error = decode_durable_reducer_state(&drifted, 1024 * 1024)
        .unwrap()
        .finish(
            Vec::new(),
            frozen.revision(),
            CatalogReducerPublicationLimits::default(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("declared revision"));

    let mut unknown: Value = serde_json::from_slice(&payload).unwrap();
    unknown["sessions"][0]["unexpected"] = json!(true);
    assert!(
        decode_durable_reducer_state(&serde_json::to_vec(&unknown).unwrap(), 1024 * 1024,).is_err()
    );
}

#[test]
fn replacement_graph_cannot_publish_an_unbounded_resolution_provenance_set() {
    let source = owner("bounded-replacement", 1);
    let prior_ref = entity(&source, CatalogEntityKind::Session, "prior");
    let mut reducer = CatalogReducer::default();
    for index in 0..MAX_PROVENANCE_REVISIONS {
        let label = format!("replacement-{index}");
        let replacement_ref = entity(&source, CatalogEntityKind::Session, &label);
        reducer
            .upsert_identity_relation(
                relation(
                    source.clone(),
                    &label,
                    IdentityRelationKind::ReplacedBy,
                    prior_ref,
                    replacement_ref,
                    QualifiedValueQuality::Exact,
                    ContractCompleteness::Complete,
                    None,
                    None,
                )
                .unwrap(),
                10 + index as u64,
            )
            .unwrap();
    }
    let overflow = entity(&source, CatalogEntityKind::Session, "replacement-overflow");
    let error = reducer
        .upsert_identity_relation(
            relation(
                source,
                "replacement-overflow",
                IdentityRelationKind::ReplacedBy,
                prior_ref,
                overflow,
                QualifiedValueQuality::Exact,
                ContractCompleteness::Complete,
                None,
                None,
            )
            .unwrap(),
            10 + MAX_PROVENANCE_REVISIONS as u64,
        )
        .unwrap_err();
    assert!(error.to_string().contains("provenance revisions"));
}

#[test]
fn tombstone_construction_cannot_exceed_the_portable_resolution_provenance_bound() {
    let first_owner = owner("bounded-tombstone-0", 1);
    let session_ref = entity(&first_owner, CatalogEntityKind::Session, "deleted");
    let mut reducer = CatalogReducer::default();
    let mut retractions = Vec::new();
    for index in 0..=MAX_PROVENANCE_REVISIONS {
        let label = format!("bounded-tombstone-{index}");
        let evidence_owner = owner(&label, 1);
        reducer
            .upsert_session_assertion(
                session_assertion(
                    evidence_owner.clone(),
                    &label,
                    session_ref,
                    "Deleted",
                    QualifiedValueQuality::Exact,
                    authority("transcript-title", 80),
                    None,
                ),
                10 + index as u64,
            )
            .unwrap();
        let evidence = retraction_evidence(&evidence_owner, &format!("{label}-absence"));
        reducer
            .retract_owner(&evidence, 100 + index as u64)
            .unwrap();
        retractions.push(evidence);
    }

    let error = reducer
        .confirm_absent(session_ref, &retractions[0], 1_000)
        .unwrap_err();
    assert!(error.to_string().contains("semantic revisions"));
    assert!(matches!(
        reducer.resolve_external_ref(session_ref.external_ref),
        CatalogResolvedEntity::Unknown {
            reason: CatalogUnknownReferenceReason::RetractedPendingPublication,
            ..
        }
    ));
}

fn frozen_fixture() -> Value {
    let source = owner("frozen", 7);
    let project_ref = entity(&source, CatalogEntityKind::Project, "project");
    let session_ref = entity(&source, CatalogEntityKind::Session, "session");
    let project = project_assertion(source.clone(), "project", project_ref, "Fixture Project");
    let session = session_assertion(
        source.clone(),
        "session",
        session_ref,
        "Fixture Session",
        QualifiedValueQuality::Exact,
        authority("transcript-title", 80),
        Some(1_700_000_000),
    );
    let association = association(
        source.clone(),
        "session-project",
        session_ref,
        project_ref,
        ProjectAssociationBasis::NativeProjectIndex,
        QualifiedValueQuality::NativeClaimed,
        Some(1_700_000_000),
    )
    .unwrap();
    let locator = locator(
        source.clone(),
        "session-locator",
        session_ref,
        "/private/fixture/session.jsonl",
    );
    let alias = relation(
        source.clone(),
        "session-alias",
        IdentityRelationKind::Alias,
        session_ref,
        entity(
            &owner("frozen", 7),
            CatalogEntityKind::Session,
            "session-alias",
        ),
        QualifiedValueQuality::NativeClaimed,
        ContractCompleteness::Partial,
        None,
        None,
    )
    .unwrap();
    let mut reducer = CatalogReducer::default();
    reducer
        .upsert_project_assertion(project.clone(), 40)
        .unwrap();
    reducer
        .upsert_session_assertion(session.clone(), 41)
        .unwrap();
    reducer.upsert_association(association.clone(), 42).unwrap();
    reducer.upsert_locator_claim(locator.clone(), 43).unwrap();
    reducer.upsert_identity_relation(alias.clone(), 44).unwrap();

    let project_row = reducer.project_row(project_ref).unwrap();
    let session_row = reducer.session_row(session_ref).unwrap();
    let CatalogAssociationCoverage::Available {
        selection: association_selection,
    } = reducer.association_for_session(session_ref)
    else {
        unreachable!("the frozen association was inserted")
    };
    let local_locator = locator.for_view(CatalogPolicyView::LOCAL);
    let withheld_locator = locator.for_view(CatalogPolicyView::WITHHELD);

    json!({
        "fixture_version": 1,
        "owner": source,
        "entity_refs": {
            "project": project_ref,
            "session": session_ref,
        },
        "evidence_keys": {
            "project_assertion": project.assertion_key,
            "session_assertion": session.assertion_key,
            "association": association.association_key,
            "locator": locator.locator_claim_key,
            "identity_relation": alias.relation_key,
        },
        "selected_project_display_name": project_row.display_name,
        "selected_session_title": session_row.title,
        "selected_association": association_selection,
        "identity_relation": alias,
        "local_locator": local_locator,
        "withheld_locator": withheld_locator,
        "live_resolution": reducer.resolve_external_ref(session_ref.external_ref),
    })
}

#[test]
fn frozen_catalog_evidence_contract_matches_fixture() {
    let actual = frozen_fixture();
    let expected: Value = serde_json::from_str(include_str!(
        "../../../fixtures/contracts/rfc012b-catalog-evidence-v1.json"
    ))
    .unwrap();
    assert_eq!(
        actual,
        expected,
        "actual fixture:\n{}",
        serde_json::to_string_pretty(&actual).unwrap()
    );
}
