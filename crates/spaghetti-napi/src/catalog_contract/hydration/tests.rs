use serde_json::{json, Value};

use super::*;
use crate::adapter::{
    CanonicalEntityKey, CanonicalFactId, CanonicalSourceInstanceKey, ContractCompleteness,
    ContractVersionOffer, ContractVersionRequest, CoverageDeclarationDigest, CoverageObjectKey,
    CoverageStreamKey, FactRevisionId, NativeIdentity, QualifiedValue, QualifiedValueQuality,
    CONTRACT_VERSION_SELECTION_VERSION, EXTERNAL_ENTITY_REFERENCE_VERSION,
    SEMANTIC_REFERENCE_CONTRACT_VERSION, SOURCE_COVERAGE_CONTRACT_VERSION,
};

use super::super::evidence::{
    CatalogAvailability, CatalogDisclosureClass, CatalogEvidenceOwner, CatalogFieldAuthority,
    CatalogLocatorValue, CatalogMutation, CatalogQualifiedField, CatalogSessionAssertion,
    IdentityRelationFact, IdentityRelationKind, NativeLocatorClaim, ProjectAssociationBasis,
};
use super::super::query::{
    negotiate_catalog_query_contract, CatalogQueryContractOffer, CatalogQueryContractRequest,
    CatalogTypedUnknownCapability, CATALOG_BASE_MODEL_MAJOR,
};
use super::super::{CatalogCoverageScope, CATALOG_QUERY_PACK_CONTRACT_VERSION};

const ADAPTER_ID: &str = "fixture-agent";
const SOURCE_LABEL: &[u8] = b"fixture-device/catalog-root";
const SECRET_REQUEST_TOKEN: &[u8] = b"raw-request-token-must-not-leak";
const SECRET_NATIVE_ID: &str = "raw-native-session-id-must-not-leak";
const SECRET_LOCATOR: &str = "/private/raw/session-locator-must-not-leak.jsonl";

#[derive(Clone)]
struct FixtureConfig {
    request_token: Vec<u8>,
    adapter_id: String,
    source_label: Vec<u8>,
    support_release_id: String,
    declaration: Vec<u8>,
    policy: Vec<u8>,
    generation: u64,
    locator_kind: CatalogLocatorKind,
    locator_basis: ProjectAssociationBasis,
    locator_disclosure: CatalogDisclosureClass,
    locator_value: String,
    locator_revision_label: String,
    locator_claim_provenance_labels: Vec<String>,
    selected_label: String,
    presentation_is_selected: bool,
    selection_unknown_max_bytes: u32,
    snapshot_epoch: u64,
    snapshot_commit: u64,
    include_message_family: bool,
    max_source_objects_per_pass: u32,
    max_records_per_pass: u32,
    max_bytes_per_pass: u64,
}

impl Default for FixtureConfig {
    fn default() -> Self {
        Self {
            request_token: SECRET_REQUEST_TOKEN.to_vec(),
            adapter_id: ADAPTER_ID.to_owned(),
            source_label: SOURCE_LABEL.to_vec(),
            support_release_id: "fixture-agent@catalog-v1".to_owned(),
            declaration: b"fixture-catalog-declaration-v1".to_vec(),
            policy: b"fixture-local-policy-v1".to_vec(),
            generation: 1,
            locator_kind: CatalogLocatorKind::Filesystem,
            locator_basis: ProjectAssociationBasis::SessionDirectory,
            locator_disclosure: CatalogDisclosureClass::LocalSensitive,
            locator_value: SECRET_LOCATOR.to_owned(),
            locator_revision_label: "locator-evidence-v1".to_owned(),
            locator_claim_provenance_labels: vec![
                "locator-claim-a".to_owned(),
                "locator-claim-b".to_owned(),
            ],
            selected_label: "selected-session".to_owned(),
            presentation_is_selected: false,
            selection_unknown_max_bytes: 4_096,
            snapshot_epoch: 7,
            snapshot_commit: 42,
            include_message_family: false,
            max_source_objects_per_pass: 8,
            max_records_per_pass: 2_048,
            max_bytes_per_pass: 1_048_576,
        }
    }
}

struct Fixture {
    plan: CatalogCoveragePlan,
    source: CatalogCoveragePlanSource,
    selection: CatalogQueryContractSelection,
    snapshot: CatalogSnapshotId,
    reducer: CatalogReducer,
    handoff: CatalogSessionAttachHandoff,
    authorization: CatalogHydrationLocatorAuthorization,
    scope: CatalogHydrationRequestedScope,
    command: CatalogHydrationCommand,
}

fn source_key(label: &[u8]) -> CanonicalSourceInstanceKey {
    CanonicalSourceInstanceKey::derive(1, label).unwrap()
}

fn owner(config: &FixtureConfig) -> CatalogEvidenceOwner {
    CatalogEvidenceOwner::new(
        &config.adapter_id,
        source_key(&config.source_label),
        CoverageStreamKey::derive(&config.adapter_id, b"catalog-hydration").unwrap(),
        CoverageObjectKey::derive("fixture-hydration", b"catalog-object").unwrap(),
        config.generation,
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
        "catalog.hydration.fixture",
        label.as_bytes(),
    )
    .unwrap();
    SemanticRevisionRef::new(FactRevisionId::derive(&fact, 1, label.as_bytes()).unwrap())
}

fn authority(class_id: &str) -> CatalogFieldAuthority {
    CatalogFieldAuthority::new(class_id, 100, true).unwrap()
}

fn field<T>(
    owner: &CatalogEvidenceOwner,
    label: &str,
    value: T,
    quality: QualifiedValueQuality,
    disclosure: CatalogDisclosureClass,
) -> CatalogQualifiedField<T> {
    CatalogQualifiedField::new(
        QualifiedValue::from_parts(
            Some(value),
            quality,
            authority("hydration-fixture-authority"),
            ContractCompleteness::Complete,
            None,
            None,
            vec![revision(owner, label)],
        )
        .unwrap(),
        disclosure,
    )
    .unwrap()
}

fn selection(max_unknown_bytes: u32) -> CatalogQueryContractSelection {
    let fact_family_versions = [
        ("catalog.message".to_owned(), vec![1]),
        ("catalog.session".to_owned(), vec![1]),
    ]
    .into_iter()
    .collect();
    let request = CatalogQueryContractRequest::new(
        ContractVersionRequest {
            selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
            model_major: CATALOG_BASE_MODEL_MAJOR,
            external_entity_reference_version: EXTERNAL_ENTITY_REFERENCE_VERSION,
            semantic_revision_reference_version: SEMANTIC_REFERENCE_CONTRACT_VERSION,
            coverage_contract_versions: vec![SOURCE_COVERAGE_CONTRACT_VERSION],
            fact_family_versions,
            query_pack_versions: Some(vec![CATALOG_QUERY_PACK_CONTRACT_VERSION]),
            observation_contract_versions: None,
        },
        CatalogTypedUnknownCapability::preserving(max_unknown_bytes).unwrap(),
    )
    .unwrap();
    let offer = CatalogQueryContractOffer::new(
        ContractVersionOffer {
            selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
            model_major: CATALOG_BASE_MODEL_MAJOR,
            external_entity_reference_versions: vec![EXTERNAL_ENTITY_REFERENCE_VERSION],
            semantic_revision_reference_versions: vec![SEMANTIC_REFERENCE_CONTRACT_VERSION],
            coverage_contract_versions: vec![SOURCE_COVERAGE_CONTRACT_VERSION],
            fact_family_versions: [
                ("catalog.message".to_owned(), vec![1]),
                ("catalog.session".to_owned(), vec![1]),
            ]
            .into_iter()
            .collect(),
            query_pack_versions: vec![CATALOG_QUERY_PACK_CONTRACT_VERSION],
            observation_contract_versions: Vec::new(),
        },
        CatalogTypedUnknownCapability::preserving(8_192).unwrap(),
    )
    .unwrap();
    negotiate_catalog_query_contract(&request, &offer).unwrap()
}

fn build_fixture(config: FixtureConfig) -> Fixture {
    let source = CatalogCoveragePlanSource::new(
        &config.adapter_id,
        source_key(&config.source_label),
        &config.support_release_id,
        CoverageDeclarationDigest::derive(&config.declaration).unwrap(),
        CatalogAccessPolicyDigest::derive(1, &config.policy).unwrap(),
    )
    .unwrap();
    let plan = CatalogCoveragePlan::new(
        CatalogCoverageScope::Library,
        vec![source.clone()],
        Vec::new(),
    )
    .unwrap();
    let selection = selection(config.selection_unknown_max_bytes);
    let snapshot = CatalogSnapshotId::new(
        CATALOG_QUERY_PACK_CONTRACT_VERSION,
        plan.coverage_plan_id,
        config.snapshot_epoch,
        config.snapshot_commit,
    )
    .unwrap();

    let evidence_owner = owner(&config);
    let selected_ref = entity(
        &evidence_owner,
        CatalogEntityKind::Session,
        &config.selected_label,
    );
    let presentation_ref = if config.presentation_is_selected {
        selected_ref
    } else {
        entity(
            &evidence_owner,
            CatalogEntityKind::Session,
            "presentation-session",
        )
    };
    let related_member_ref = if config.presentation_is_selected {
        entity(
            &evidence_owner,
            CatalogEntityKind::Session,
            "related-base-session",
        )
    } else {
        selected_ref
    };
    let locator_claim = NativeLocatorClaim::new(
        evidence_owner.clone(),
        b"selected-transcript-locator",
        selected_ref,
        config.locator_kind,
        field(
            &evidence_owner,
            &config.locator_revision_label,
            CatalogLocatorValue {
                native_value: Some(config.locator_value.clone()),
                canonical_local_path: Some(config.locator_value.clone()),
            },
            QualifiedValueQuality::Exact,
            config.locator_disclosure,
        ),
        config.locator_basis,
        config
            .locator_claim_provenance_labels
            .iter()
            .map(|label| revision(&evidence_owner, label))
            .collect(),
    )
    .unwrap();
    let locator_claim_key = locator_claim.locator_claim_key;
    let assertion = CatalogSessionAssertion::new(
        evidence_owner.clone(),
        b"selected-session-assertion",
        selected_ref,
        Some(field(
            &evidence_owner,
            "native-session-identity",
            NativeIdentity {
                native_namespace: "fixture.session".to_owned(),
                native_id: SECRET_NATIVE_ID.to_owned(),
            },
            QualifiedValueQuality::NativeClaimed,
            CatalogDisclosureClass::PolicyShareable,
        )),
        None,
        None,
        None,
        None,
        None,
        Some(locator_claim_key),
        field(
            &evidence_owner,
            "session-availability",
            CatalogAvailability::TranscriptDiscovered,
            QualifiedValueQuality::Exact,
            CatalogDisclosureClass::Public,
        ),
        vec![revision(&evidence_owner, "session-assertion")],
    )
    .unwrap();
    let relation = IdentityRelationFact::new(
        evidence_owner.clone(),
        b"presentation-membership",
        IdentityRelationKind::SameEntity,
        presentation_ref,
        related_member_ref,
        authority("hydration-same-entity"),
        QualifiedValueQuality::Exact,
        ContractCompleteness::Complete,
        Some(presentation_ref),
        Some("fixture-presentation-winner-v1".to_owned()),
        vec![revision(&evidence_owner, "presentation-membership")],
    )
    .unwrap();
    let relation_key = relation.relation_key;
    let mut reducer = CatalogReducer::default();
    assert_eq!(
        reducer.upsert_session_assertion(assertion, 10).unwrap(),
        CatalogMutation::Inserted
    );
    assert_eq!(
        reducer.upsert_locator_claim(locator_claim, 11).unwrap(),
        CatalogMutation::Inserted
    );
    assert_eq!(
        reducer.upsert_identity_relation(relation, 12).unwrap(),
        CatalogMutation::Inserted
    );
    let handoff = CatalogSessionAttachHandoff::new(
        presentation_ref,
        vec![related_member_ref, presentation_ref],
        vec![relation_key],
        selected_ref,
        locator_claim_key,
    )
    .unwrap();
    let authorization = CatalogHydrationLocatorAuthorization::authorize(
        &reducer,
        handoff.clone(),
        CatalogPolicyView::LOCAL,
        &source,
    )
    .unwrap();

    let mut fact_family_versions = BTreeMap::from([("catalog.session".to_owned(), 1)]);
    if config.include_message_family {
        fact_family_versions.insert("catalog.message".to_owned(), 1);
    }
    let scope = CatalogHydrationRequestedScope::new(
        fact_family_versions,
        config.max_source_objects_per_pass,
        config.max_records_per_pass,
        config.max_bytes_per_pass,
        &selection,
    )
    .unwrap();
    let command = CatalogHydrationCommand::new(
        CatalogHydrationRequestKey::derive(&config.request_token).unwrap(),
        selection.clone(),
        snapshot,
        &plan,
        source.clone(),
        authorization.clone(),
        scope.clone(),
        CatalogHydrationReason::SelectedSession,
    )
    .unwrap();

    Fixture {
        plan,
        source,
        selection,
        snapshot,
        reducer,
        handoff,
        authorization,
        scope,
        command,
    }
}

fn assert_distinct_work(label: &str, base: &Fixture, changed: &Fixture) {
    assert_ne!(
        base.command.coalescing_key, changed.command.coalescing_key,
        "{label} must change the exact-work coalescing key"
    );
    assert_ne!(
        base.command.command_id, changed.command.command_id,
        "{label} must change immutable command identity"
    );
    assert!(!base.command.coalesces_with(&changed.command));
    assert!(!changed.command.coalesces_with(&base.command));
}

#[test]
fn commands_are_idempotent_exactly_coalesced_and_secret_free() {
    let base = build_fixture(FixtureConfig::default());
    let execution = CatalogHydrationExecutionAuthorization::authorize(
        &base.reducer,
        base.handoff.clone(),
        CatalogPolicyView::LOCAL,
        &base.source,
    )
    .unwrap();
    assert_eq!(execution.portable(), &base.authorization);
    assert_eq!(
        execution.attach_target().locator_claim_key,
        base.handoff.locator_claim_key
    );
    assert!(!format!("{execution:?}").contains(SECRET_LOCATOR));
    let same = build_fixture(FixtureConfig::default());
    assert_eq!(base.command, same.command);

    let second_request_config = FixtureConfig {
        request_token: b"independent-request-token".to_vec(),
        ..FixtureConfig::default()
    };
    let second_request = build_fixture(second_request_config);
    assert_ne!(base.command.command_id, second_request.command.command_id);
    assert_eq!(
        base.command.coalescing_key,
        second_request.command.coalescing_key
    );
    assert!(base.command.coalesces_with(&second_request.command));
    assert!(second_request.command.coalesces_with(&base.command));

    let changed_scope_config = FixtureConfig {
        include_message_family: true,
        ..FixtureConfig::default()
    };
    let changed_scope = build_fixture(changed_scope_config);
    assert_distinct_work("fact-family scope", &base, &changed_scope);

    let mut registry = CatalogHydrationReplayRegistry::default();
    assert_eq!(
        registry.observe(base.command.clone()).unwrap(),
        CatalogHydrationReplayObservation::New
    );
    assert_eq!(
        registry.observe(same.command).unwrap(),
        CatalogHydrationReplayObservation::Replay
    );
    let mut retargeted_request = changed_scope.command;
    retargeted_request.request_key = base.command.request_key;
    retargeted_request.command_id = retargeted_request.derive_command_id().unwrap();
    assert!(registry.observe(retargeted_request).is_err());

    let wire = serde_json::to_string(&base.command).unwrap();
    let debug = format!("{:?}", base.command);
    for secret in [
        std::str::from_utf8(SECRET_REQUEST_TOKEN).unwrap(),
        SECRET_NATIVE_ID,
        SECRET_LOCATOR,
    ] {
        assert!(!wire.contains(secret), "wire leaked {secret}");
        assert!(!debug.contains(secret), "Debug leaked {secret}");
    }
}

#[test]
fn locator_provenance_is_order_invariant_and_wire_canonical() {
    let forward = build_fixture(FixtureConfig {
        locator_claim_provenance_labels: vec![
            "locator-claim-a".to_owned(),
            "locator-claim-b".to_owned(),
        ],
        ..FixtureConfig::default()
    });
    let reversed = build_fixture(FixtureConfig {
        locator_claim_provenance_labels: vec![
            "locator-claim-b".to_owned(),
            "locator-claim-a".to_owned(),
        ],
        ..FixtureConfig::default()
    });
    assert_eq!(forward.authorization, reversed.authorization);
    assert_eq!(forward.command, reversed.command);
    assert!(forward
        .authorization
        .locator_provenance
        .windows(2)
        .all(|pair| pair[0].fact_revision_id < pair[1].fact_revision_id));

    let mut noncanonical = serde_json::to_value(&forward.command).unwrap();
    noncanonical["authorization"]["locator_provenance"]
        .as_array_mut()
        .unwrap()
        .reverse();
    assert!(CatalogHydrationCommand::from_wire_value(
        noncanonical,
        &forward.plan,
        &forward.selection,
        forward.snapshot,
        &forward.authorization,
    )
    .is_err());
}

#[test]
fn command_identity_covers_every_authority_scope_bound_and_snapshot_axis() {
    let base = build_fixture(FixtureConfig::default());

    let mut variants: Vec<(&str, FixtureConfig)> = Vec::new();
    variants.push((
        "adapter identity",
        FixtureConfig {
            adapter_id: "fixture-agent-next".to_owned(),
            ..FixtureConfig::default()
        },
    ));
    variants.push((
        "source identity",
        FixtureConfig {
            source_label: b"fixture-device/other-catalog-root".to_vec(),
            ..FixtureConfig::default()
        },
    ));
    variants.push((
        "support release",
        FixtureConfig {
            support_release_id: "fixture-agent@catalog-v2".to_owned(),
            ..FixtureConfig::default()
        },
    ));
    variants.push((
        "coverage declaration",
        FixtureConfig {
            declaration: b"fixture-catalog-declaration-v2".to_vec(),
            ..FixtureConfig::default()
        },
    ));
    variants.push((
        "access policy",
        FixtureConfig {
            policy: b"fixture-local-policy-v2".to_vec(),
            ..FixtureConfig::default()
        },
    ));
    variants.push((
        "locator owner generation",
        FixtureConfig {
            generation: 2,
            ..FixtureConfig::default()
        },
    ));
    variants.push((
        "locator kind",
        FixtureConfig {
            locator_kind: CatalogLocatorKind::OpaqueNative,
            ..FixtureConfig::default()
        },
    ));
    variants.push((
        "locator basis",
        FixtureConfig {
            locator_basis: ProjectAssociationBasis::NativeProjectIndex,
            ..FixtureConfig::default()
        },
    ));
    variants.push((
        "locator disclosure",
        FixtureConfig {
            locator_disclosure: CatalogDisclosureClass::PolicyShareable,
            ..FixtureConfig::default()
        },
    ));
    variants.push((
        "authorized locator value",
        FixtureConfig {
            locator_value: "/private/raw/other-session.jsonl".to_owned(),
            ..FixtureConfig::default()
        },
    ));
    variants.push((
        "locator provenance",
        FixtureConfig {
            locator_revision_label: "locator-evidence-v2".to_owned(),
            ..FixtureConfig::default()
        },
    ));
    variants.push((
        "selected base session",
        FixtureConfig {
            selected_label: "other-selected-session".to_owned(),
            ..FixtureConfig::default()
        },
    ));
    variants.push((
        "negotiated contract selection",
        FixtureConfig {
            selection_unknown_max_bytes: 2_048,
            ..FixtureConfig::default()
        },
    ));
    variants.push((
        "snapshot epoch",
        FixtureConfig {
            snapshot_epoch: 8,
            ..FixtureConfig::default()
        },
    ));
    variants.push((
        "snapshot commit",
        FixtureConfig {
            snapshot_commit: 43,
            ..FixtureConfig::default()
        },
    ));
    variants.push((
        "fact-family scope",
        FixtureConfig {
            include_message_family: true,
            ..FixtureConfig::default()
        },
    ));
    variants.push((
        "object bound",
        FixtureConfig {
            max_source_objects_per_pass: 9,
            ..FixtureConfig::default()
        },
    ));
    variants.push((
        "record bound",
        FixtureConfig {
            max_records_per_pass: 2_049,
            ..FixtureConfig::default()
        },
    ));
    variants.push((
        "byte bound",
        FixtureConfig {
            max_bytes_per_pass: 1_048_577,
            ..FixtureConfig::default()
        },
    ));

    for (label, config) in variants {
        assert_distinct_work(label, &base, &build_fixture(config));
    }
}

#[test]
fn command_consumption_requires_exact_retained_authority_and_concrete_base() {
    let base = build_fixture(FixtureConfig::default());
    let wire = serde_json::to_value(&base.command).unwrap();
    assert_eq!(
        CatalogHydrationCommand::from_wire_value(
            wire.clone(),
            &base.plan,
            &base.selection,
            base.snapshot,
            &base.authorization,
        )
        .unwrap(),
        base.command
    );

    let changed = FixtureConfig {
        selection_unknown_max_bytes: 2_048,
        ..FixtureConfig::default()
    };
    let changed_selection = build_fixture(changed);
    assert!(CatalogHydrationCommand::from_wire_value(
        wire.clone(),
        &base.plan,
        &changed_selection.selection,
        base.snapshot,
        &base.authorization,
    )
    .is_err());

    let changed = FixtureConfig {
        policy: b"other-authorized-policy".to_vec(),
        ..FixtureConfig::default()
    };
    let changed_authority = build_fixture(changed);
    assert!(CatalogHydrationCommand::from_wire_value(
        wire.clone(),
        &base.plan,
        &base.selection,
        base.snapshot,
        &changed_authority.authorization,
    )
    .is_err());
    assert!(CatalogHydrationCommand::from_wire_value(
        wire.clone(),
        &changed_authority.plan,
        &base.selection,
        changed_authority.snapshot,
        &base.authorization,
    )
    .is_err());

    let mut unknown = wire.clone();
    unknown["unnegotiated_scheduler_hint"] = json!(true);
    assert!(CatalogHydrationCommand::from_wire_value(
        unknown,
        &base.plan,
        &base.selection,
        base.snapshot,
        &base.authorization,
    )
    .is_err());

    let mut nonportable = wire.clone();
    nonportable["snapshot_id"]["readiness_epoch"] = json!(MAX_JAVASCRIPT_SAFE_INTEGER + 1);
    assert!(CatalogHydrationCommand::from_wire_value(
        nonportable,
        &base.plan,
        &base.selection,
        base.snapshot,
        &base.authorization,
    )
    .is_err());

    let representative = build_fixture(FixtureConfig {
        presentation_is_selected: true,
        ..FixtureConfig::default()
    });
    assert_eq!(
        representative
            .authorization
            .handoff
            .selected_base_session_ref,
        representative.authorization.handoff.presentation_ref
    );
    assert!(CatalogHydrationCommand::from_wire_value(
        serde_json::to_value(&representative.command).unwrap(),
        &representative.plan,
        &representative.selection,
        representative.snapshot,
        &representative.authorization,
    )
    .is_ok());

    let outsider = build_fixture(FixtureConfig {
        selected_label: "not-a-disclosed-member".to_owned(),
        ..FixtureConfig::default()
    });
    assert!(CatalogSessionAttachHandoff::new(
        base.handoff.presentation_ref,
        base.handoff.member_refs.clone(),
        base.handoff.relation_keys.clone(),
        outsider.handoff.selected_base_session_ref,
        base.handoff.locator_claim_key,
    )
    .is_err());
    assert!(CatalogSessionAttachHandoff::new(
        base.handoff.presentation_ref,
        base.handoff.member_refs.clone(),
        vec![base.handoff.relation_keys[0]; 4_097],
        base.handoff.selected_base_session_ref,
        base.handoff.locator_claim_key,
    )
    .is_err());
    assert!(CatalogHydrationLocatorAuthorization::authorize(
        &base.reducer,
        base.handoff,
        CatalogPolicyView::WITHHELD,
        &base.source,
    )
    .is_err());

    for nested_path in ["source", "snapshot_id"] {
        let mut nested_unknown = wire.clone();
        nested_unknown[nested_path]["future_authority"] = json!(true);
        assert!(CatalogHydrationCommand::from_wire_value(
            nested_unknown,
            &base.plan,
            &base.selection,
            base.snapshot,
            &base.authorization,
        )
        .is_err());
    }
    let mut handoff_unknown = wire.clone();
    handoff_unknown["authorization"]["handoff"]["future_relation"] = json!(true);
    assert!(CatalogHydrationCommand::from_wire_value(
        handoff_unknown,
        &base.plan,
        &base.selection,
        base.snapshot,
        &base.authorization,
    )
    .is_err());
    let mut entity_unknown = wire.clone();
    entity_unknown["authorization"]["handoff"]["selected_base_session_ref"]["future_identity"] =
        json!(true);
    assert!(CatalogHydrationCommand::from_wire_value(
        entity_unknown,
        &base.plan,
        &base.selection,
        base.snapshot,
        &base.authorization,
    )
    .is_err());
    let mut zero_generation = wire;
    zero_generation["authorization"]["locator_source_generation"] = json!(0);
    assert!(CatalogHydrationCommand::from_wire_value(
        zero_generation,
        &base.plan,
        &base.selection,
        base.snapshot,
        &base.authorization,
    )
    .is_err());
}

#[test]
fn scheduling_receipts_enforce_retry_terminal_and_coalescing_lineage() {
    let base = build_fixture(FixtureConfig::default());
    assert!(CatalogHydrationFailure::terminal("/private/native/error.txt").is_err());
    assert!(CatalogHydrationFailure::terminal("free form internal failure").is_err());
    assert!(CatalogHydrationFailure::terminal("UPPERCASE_FAILURE").is_err());
    let accepted = CatalogSchedulingReceipt::issue(
        &base.command,
        None,
        None,
        43,
        CatalogHydrationSchedulingOutcome::Accepted,
    )
    .unwrap();
    let satisfied = CatalogSchedulingReceipt::issue(
        &base.command,
        Some(&accepted),
        None,
        44,
        CatalogHydrationSchedulingOutcome::AlreadySatisfied,
    )
    .unwrap();
    assert!(CatalogSchedulingReceipt::issue(
        &base.command,
        Some(&satisfied),
        None,
        45,
        CatalogHydrationSchedulingOutcome::Accepted,
    )
    .is_err());

    let retryable = CatalogSchedulingReceipt::issue(
        &base.command,
        None,
        None,
        43,
        CatalogHydrationSchedulingOutcome::Rejected {
            failure: CatalogHydrationFailure::retryable("source_busy", 250).unwrap(),
        },
    )
    .unwrap();
    let retried = CatalogSchedulingReceipt::issue(
        &base.command,
        Some(&retryable),
        None,
        44,
        CatalogHydrationSchedulingOutcome::Accepted,
    )
    .unwrap();
    assert_eq!(retried.attempt, 2);
    assert_eq!(retried.prior_receipt_id, Some(retryable.receipt_id));

    let terminal = CatalogSchedulingReceipt::issue(
        &base.command,
        None,
        None,
        43,
        CatalogHydrationSchedulingOutcome::Rejected {
            failure: CatalogHydrationFailure::terminal("unsupported_locator").unwrap(),
        },
    )
    .unwrap();
    assert!(CatalogSchedulingReceipt::issue(
        &base.command,
        Some(&terminal),
        None,
        44,
        CatalogHydrationSchedulingOutcome::Accepted,
    )
    .is_err());

    let coalesced_config = FixtureConfig {
        request_token: b"coalesced-second-request".to_vec(),
        ..FixtureConfig::default()
    };
    let coalesced = build_fixture(coalesced_config);
    let in_progress = CatalogSchedulingReceipt::issue(
        &coalesced.command,
        None,
        Some(CatalogHydrationActiveSchedule::new(&base.command, &accepted).unwrap()),
        44,
        CatalogHydrationSchedulingOutcome::InProgress {
            active_command_id: accepted.command_id,
            active_receipt_id: accepted.receipt_id,
        },
    )
    .unwrap();
    assert_eq!(in_progress.attempt, 1);
    assert!(CatalogSchedulingReceipt::issue(
        &coalesced.command,
        None,
        Some(CatalogHydrationActiveSchedule::new(&base.command, &accepted).unwrap()),
        accepted.emitted_at_commit - 1,
        CatalogHydrationSchedulingOutcome::InProgress {
            active_command_id: accepted.command_id,
            active_receipt_id: accepted.receipt_id,
        },
    )
    .is_err());
    assert!(CatalogHydrationActiveSchedule::new(&coalesced.command, &accepted).is_err());
    let mut forged_active_command = base.command.clone();
    forged_active_command.command_id = coalesced.command.command_id;
    assert!(CatalogHydrationActiveSchedule::new(&forged_active_command, &accepted).is_err());

    let different_work_config = FixtureConfig {
        max_records_per_pass: 2_049,
        ..FixtureConfig::default()
    };
    let different_work = build_fixture(different_work_config);
    assert!(CatalogSchedulingReceipt::issue(
        &different_work.command,
        None,
        Some(CatalogHydrationActiveSchedule::new(&base.command, &accepted).unwrap()),
        44,
        CatalogHydrationSchedulingOutcome::InProgress {
            active_command_id: accepted.command_id,
            active_receipt_id: accepted.receipt_id,
        },
    )
    .is_err());
}

#[test]
fn receipt_wire_rejects_forged_prior_active_and_command_lineage() {
    let base = build_fixture(FixtureConfig::default());
    let retryable = CatalogSchedulingReceipt::issue(
        &base.command,
        None,
        None,
        43,
        CatalogHydrationSchedulingOutcome::Rejected {
            failure: CatalogHydrationFailure::retryable("source_busy", 250).unwrap(),
        },
    )
    .unwrap();
    let retried = CatalogSchedulingReceipt::issue(
        &base.command,
        Some(&retryable),
        None,
        44,
        CatalogHydrationSchedulingOutcome::Accepted,
    )
    .unwrap();
    let retried_wire = serde_json::to_value(&retried).unwrap();
    assert_eq!(
        CatalogSchedulingReceipt::from_wire_value(
            retried_wire.clone(),
            &base.command,
            Some(&retryable),
            None,
        )
        .unwrap(),
        retried
    );

    let mut forged_prior = retried_wire.clone();
    forged_prior["prior_receipt_id"] = json!(base.command.command_id);
    assert!(CatalogSchedulingReceipt::from_wire_value(
        forged_prior,
        &base.command,
        Some(&retryable),
        None,
    )
    .is_err());
    let unrelated_prior = CatalogSchedulingReceipt::issue(
        &base.command,
        None,
        None,
        43,
        CatalogHydrationSchedulingOutcome::Accepted,
    )
    .unwrap();
    assert!(CatalogSchedulingReceipt::from_wire_value(
        retried_wire,
        &base.command,
        Some(&unrelated_prior),
        None,
    )
    .is_err());

    let coalesced_config = FixtureConfig {
        request_token: b"coalesced-receipt-request".to_vec(),
        ..FixtureConfig::default()
    };
    let coalesced = build_fixture(coalesced_config);
    let active = CatalogSchedulingReceipt::issue(
        &base.command,
        None,
        None,
        43,
        CatalogHydrationSchedulingOutcome::Accepted,
    )
    .unwrap();
    let in_progress = CatalogSchedulingReceipt::issue(
        &coalesced.command,
        None,
        Some(CatalogHydrationActiveSchedule::new(&base.command, &active).unwrap()),
        44,
        CatalogHydrationSchedulingOutcome::InProgress {
            active_command_id: active.command_id,
            active_receipt_id: active.receipt_id,
        },
    )
    .unwrap();
    let in_progress_wire = serde_json::to_value(&in_progress).unwrap();
    assert_eq!(
        CatalogSchedulingReceipt::from_wire_value(
            in_progress_wire.clone(),
            &coalesced.command,
            None,
            Some(CatalogHydrationActiveSchedule::new(&base.command, &active).unwrap()),
        )
        .unwrap(),
        in_progress
    );
    let mut forged_active = in_progress_wire.clone();
    forged_active["outcome"]["active_receipt_id"] = json!(in_progress.receipt_id);
    assert!(CatalogSchedulingReceipt::from_wire_value(
        forged_active,
        &coalesced.command,
        None,
        Some(CatalogHydrationActiveSchedule::new(&base.command, &active).unwrap()),
    )
    .is_err());

    let mut wrong_command = in_progress_wire.clone();
    wrong_command["command_id"] = json!(base.command.command_id);
    assert!(CatalogSchedulingReceipt::from_wire_value(
        wrong_command,
        &coalesced.command,
        None,
        Some(CatalogHydrationActiveSchedule::new(&base.command, &active).unwrap()),
    )
    .is_err());
    let mut unknown = in_progress_wire;
    unknown["scheduler_private_state"] = json!("must-not-cross-boundary");
    assert!(CatalogSchedulingReceipt::from_wire_value(
        unknown,
        &coalesced.command,
        None,
        Some(CatalogHydrationActiveSchedule::new(&base.command, &active).unwrap()),
    )
    .is_err());
}

fn frozen_fixture() -> Value {
    let base = build_fixture(FixtureConfig::default());
    let accepted = CatalogSchedulingReceipt::issue(
        &base.command,
        None,
        None,
        43,
        CatalogHydrationSchedulingOutcome::Accepted,
    )
    .unwrap();
    let coalesced_config = FixtureConfig {
        request_token: b"portable-coalesced-request".to_vec(),
        ..FixtureConfig::default()
    };
    let coalesced = build_fixture(coalesced_config);
    let in_progress = CatalogSchedulingReceipt::issue(
        &coalesced.command,
        None,
        Some(CatalogHydrationActiveSchedule::new(&base.command, &accepted).unwrap()),
        44,
        CatalogHydrationSchedulingOutcome::InProgress {
            active_command_id: accepted.command_id,
            active_receipt_id: accepted.receipt_id,
        },
    )
    .unwrap();
    let retryable = CatalogSchedulingReceipt::issue(
        &base.command,
        None,
        None,
        45,
        CatalogHydrationSchedulingOutcome::Rejected {
            failure: CatalogHydrationFailure::retryable("source_busy", 250).unwrap(),
        },
    )
    .unwrap();
    let retry_accepted = CatalogSchedulingReceipt::issue(
        &base.command,
        Some(&retryable),
        None,
        46,
        CatalogHydrationSchedulingOutcome::Accepted,
    )
    .unwrap();
    let terminal = CatalogSchedulingReceipt::issue(
        &base.command,
        None,
        None,
        47,
        CatalogHydrationSchedulingOutcome::Rejected {
            failure: CatalogHydrationFailure::terminal("unsupported_locator").unwrap(),
        },
    )
    .unwrap();

    json!({
        "fixture_contract_version": 1,
        "command": base.command,
        "coalesced_command_binding": {
            "request_key": coalesced.command.request_key,
            "command_id": coalesced.command.command_id,
            "coalescing_key": coalesced.command.coalescing_key,
            "selected_base_session_ref": coalesced
                .command
                .authorization
                .handoff
                .selected_base_session_ref,
            "snapshot_id": coalesced.command.snapshot_id
        },
        "accepted_receipt": accepted,
        "in_progress_receipt": in_progress,
        "retryable_receipt": retryable,
        "retry_accepted_receipt": retry_accepted,
        "terminal_receipt": terminal,
        "expected": {
            "selected_base_is_presentation": false,
            "coalesces": true,
            "retry_attempt": 2,
            "raw_request_token_present": false,
            "raw_native_identity_present": false,
            "raw_locator_present": false,
            "cancellation_contract_present": false
        }
    })
}

#[test]
fn frozen_hydration_contract_matches_portable_fixture_and_contains_no_raw_authority() {
    let actual = frozen_fixture();
    let encoded = serde_json::to_string(&actual).unwrap();
    assert!(!encoded.contains(std::str::from_utf8(SECRET_REQUEST_TOKEN).unwrap()));
    assert!(!encoded.contains(SECRET_NATIVE_ID));
    assert!(!encoded.contains(SECRET_LOCATOR));

    let expected = serde_json::from_str::<Value>(include_str!(
        "../../../fixtures/contracts/rfc012b-catalog-hydration-v1.json"
    ))
    .unwrap();
    if actual != expected {
        eprintln!("{}", serde_json::to_string_pretty(&actual).unwrap());
    }
    assert_eq!(actual, expected);
}
