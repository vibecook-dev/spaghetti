use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{json, Value};

use crate::adapter::{
    AdapterId, CanonicalEntityKey, CanonicalSourceInstanceKey, ContractVersionOffer,
    ContractVersionRequest, CoverageObjectKey, CoverageStreamKey,
};
use crate::observation_contract::{
    negotiate_observation_contract, ObservationContractOffer, ObservationContractRequest,
    ObservationContractSelection, OBSERVATION_BASE_MODEL_MAJOR,
    OBSERVATION_ENVELOPE_CONTRACT_VERSION, OBSERVATION_EVENT_CONTRACT_VERSION,
    OBSERVATION_LIFECYCLE_CONTRACT_VERSION, OBSERVATION_PROFILE_CONTRACT_VERSION,
};
use crate::source::AccessObjectToken;

use super::super::artifact_availability::{
    ScopedArtifactAvailabilityObservation, ScopedArtifactAvailabilityReducer,
    ScopedArtifactAvailabilitySnapshot, ScopedArtifactAvailabilitySourceOccurrence,
    ScopedArtifactAvailabilityState,
};
use super::super::artifact_evidence::ScopedArtifactEvidenceSelection;
use super::super::ScopedSourceObjectIdentity;
use super::*;

const FROZEN_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/contracts/rfc012d-scoped-artifact-availability-v1.json"
));

fn contract_selection() -> ObservationContractSelection {
    let request = ObservationContractRequest::new(
        ContractVersionRequest {
            selection_contract_version: 1,
            model_major: OBSERVATION_BASE_MODEL_MAJOR,
            external_entity_reference_version: 1,
            semantic_revision_reference_version: 1,
            coverage_contract_versions: vec![1],
            fact_family_versions: BTreeMap::from([("runtime.usage-v2".to_owned(), vec![1])]),
            query_pack_versions: None,
            observation_contract_versions: Some(vec![OBSERVATION_PROFILE_CONTRACT_VERSION]),
        },
        vec![OBSERVATION_ENVELOPE_CONTRACT_VERSION],
        vec![OBSERVATION_EVENT_CONTRACT_VERSION],
        vec![OBSERVATION_LIFECYCLE_CONTRACT_VERSION],
    )
    .unwrap();
    let offer = ObservationContractOffer::new(
        ContractVersionOffer {
            selection_contract_version: 1,
            model_major: OBSERVATION_BASE_MODEL_MAJOR,
            external_entity_reference_versions: vec![1],
            semantic_revision_reference_versions: vec![1],
            coverage_contract_versions: vec![1],
            fact_family_versions: BTreeMap::from([("runtime.usage-v2".to_owned(), vec![1])]),
            query_pack_versions: Vec::new(),
            observation_contract_versions: vec![OBSERVATION_PROFILE_CONTRACT_VERSION],
        },
        vec![OBSERVATION_ENVELOPE_CONTRACT_VERSION],
        vec![OBSERVATION_EVENT_CONTRACT_VERSION],
        vec![OBSERVATION_LIFECYCLE_CONTRACT_VERSION],
    )
    .unwrap();
    negotiate_observation_contract(&request, &offer).unwrap()
}

fn identity(seed: &[u8], kind: &str) -> CanonicalEntityKey {
    let adapter = AdapterId::new("fixture").unwrap();
    let source = CanonicalSourceInstanceKey::derive(1, b"availability-wire-source").unwrap();
    CanonicalEntityKey::derive(adapter.as_str(), &source, kind, seed).unwrap()
}

fn root() -> CanonicalEntityKey {
    identity(b"root-session", "session")
}

fn populated_snapshot() -> ScopedArtifactAvailabilitySnapshot {
    let root = root();
    let observations = [
        (
            identity(b"available", "artifact"),
            "workflow_definition",
            "available-relation",
            1,
            ScopedArtifactAvailabilityState::Available {
                generation: 3,
                provenance_ref: [1; 32],
                size_bytes: 91,
            },
        ),
        (
            identity(b"missing", "artifact"),
            "workflow_journal",
            "missing-relation",
            2,
            ScopedArtifactAvailabilityState::Missing {
                observed_generation: Some(4),
                provenance_ref: Some([2; 32]),
            },
        ),
        (
            identity(b"over-limit", "artifact"),
            "native_run_record",
            "over-limit-relation",
            3,
            ScopedArtifactAvailabilityState::OverLimit {
                generation: 5,
                provenance_ref: [3; 32],
                observed_bytes: 1_025,
                request_max_bytes: 1_024,
            },
        ),
        (
            identity(b"unstable", "artifact"),
            "team_configuration",
            "unstable-relation",
            4,
            ScopedArtifactAvailabilityState::Unstable,
        ),
    ];
    let mut reducer = ScopedArtifactAvailabilityReducer::new();
    let mut current = BTreeSet::new();
    for (artifact_key, artifact_kind, relation_id, revision_byte, state) in observations {
        let evidence = ScopedArtifactEvidenceSelection::fixture(
            root,
            artifact_key,
            format!("native-{revision_byte}"),
            u64::from(revision_byte),
            [revision_byte; 32],
        );
        let object_token = AccessObjectToken::derive(
            relation_id,
            &[
                artifact_key.as_bytes(),
                artifact_kind.as_bytes(),
                &[revision_byte],
            ],
        )
        .unwrap();
        let source_generation = match state {
            ScopedArtifactAvailabilityState::Available { generation, .. }
            | ScopedArtifactAvailabilityState::OverLimit { generation, .. } => generation,
            ScopedArtifactAvailabilityState::Missing {
                observed_generation,
                ..
            } => observed_generation.unwrap_or(1),
            ScopedArtifactAvailabilityState::Unstable => 1,
        };
        let source_instance_key =
            CanonicalSourceInstanceKey::derive(1, b"availability-wire-source").unwrap();
        let stream_key = CoverageStreamKey::derive("fixture", relation_id.as_bytes()).unwrap();
        let object_key = CoverageObjectKey::derive(relation_id, object_token.as_bytes()).unwrap();
        current.insert((artifact_key, *evidence.revision().as_bytes()));
        reducer
            .observe(ScopedArtifactAvailabilityObservation::new(
                evidence,
                Arc::from(artifact_kind),
                Arc::from(relation_id),
                object_token,
                ScopedArtifactAvailabilitySourceOccurrence::new(
                    [9; 32],
                    ScopedSourceObjectIdentity {
                        adapter_id: AdapterId::new("fixture").unwrap(),
                        source_instance_key,
                        stream_key,
                        object_key,
                    },
                    source_generation,
                ),
                state,
            ))
            .unwrap();
    }
    reducer
        .snapshot_with_current(root, |selection| {
            Ok(current.contains(&(selection.artifact_key(), *selection.revision().as_bytes())))
        })
        .unwrap()
}

fn context(
    snapshot: &ScopedArtifactAvailabilitySnapshot,
) -> ScopedArtifactAvailabilityConsumerContext {
    ScopedArtifactAvailabilityConsumerContext::from_expected(
        &contract_selection(),
        root(),
        snapshot,
    )
    .unwrap()
}

fn fixture_value() -> Value {
    let empty_snapshot = ScopedArtifactAvailabilitySnapshot::empty_fixture(root());
    let populated_snapshot = populated_snapshot();
    let empty = context(&empty_snapshot);
    let populated = context(&populated_snapshot);
    json!({
        "fixture_contract_version": 1,
        "empty": {
            "context": empty.wire(),
            "snapshot": ScopedArtifactAvailabilitySnapshotWire::from_context(&empty).unwrap(),
        },
        "populated": {
            "context": populated.wire(),
            "snapshot": ScopedArtifactAvailabilitySnapshotWire::from_context(&populated).unwrap(),
        },
        "expected": {
            "states": ["available", "missing", "over_limit", "unstable"],
            "semantic_digest_algorithm": "blake3-256",
            "ordered_observer_event": false,
            "bootstrap_or_resync_barrier": false,
            "source_access_authority": false,
            "native_locator_disclosure": "none",
            "native_content_disclosure": "none",
            "portable_observer_transport": false,
        },
    })
}

fn parse(
    value: Value,
    context: &ScopedArtifactAvailabilityConsumerContext,
) -> Result<ScopedArtifactAvailabilitySnapshotWire, ScopedArtifactAvailabilityContractError> {
    ScopedArtifactAvailabilitySnapshotWire::from_wire_value_for_context(value, context)
}

#[test]
fn frozen_empty_and_populated_snapshots_are_stable_and_contextual() {
    let actual = format!(
        "{}\n",
        serde_json::to_string_pretty(&fixture_value()).unwrap()
    );
    if FROZEN_FIXTURE == "{}\n" {
        println!("{actual}");
    }
    assert_eq!(actual, FROZEN_FIXTURE);

    let fixture: Value = serde_json::from_str(FROZEN_FIXTURE).unwrap();
    for (name, snapshot) in [
        (
            "empty",
            ScopedArtifactAvailabilitySnapshot::empty_fixture(root()),
        ),
        ("populated", populated_snapshot()),
    ] {
        let context = context(&snapshot);
        let parsed = parse(fixture[name]["snapshot"].clone(), &context).unwrap();
        assert_eq!(
            serde_json::to_value(parsed).unwrap(),
            fixture[name]["snapshot"]
        );
    }
}

#[test]
fn selection_root_entries_and_digest_are_exact_caller_held_context() {
    let snapshot = populated_snapshot();
    let context = context(&snapshot);
    let value = serde_json::to_value(
        ScopedArtifactAvailabilitySnapshotWire::from_context(&context).unwrap(),
    )
    .unwrap();

    let mut selection = value.clone();
    selection["contract_selection"]["event_contract_version"] = json!(2);
    assert!(parse(selection, &context).is_err());

    let mut root_key = value.clone();
    root_key["root_session_key"] = json!(identity(b"foreign", "session"));
    assert_eq!(
        parse(root_key, &context),
        Err(ScopedArtifactAvailabilityContractError::ContextMismatch)
    );

    let mut digest = value.clone();
    digest["semantic_digest"] = json!(encode_opaque(&[55; 32]));
    assert_eq!(
        parse(digest, &context),
        Err(ScopedArtifactAvailabilityContractError::ContextMismatch)
    );

    let mut entry = value.clone();
    entry["entries"][0]["revision"] = json!(encode_opaque(&[56; 32]));
    assert_eq!(
        parse(entry, &context),
        Err(ScopedArtifactAvailabilityContractError::ContextMismatch)
    );

    let foreign = ScopedArtifactAvailabilityConsumerContext::from_expected(
        &contract_selection(),
        identity(b"foreign", "session"),
        &snapshot,
    );
    assert!(foreign.is_err());
}

#[test]
fn strict_shapes_order_bounds_and_state_laws_fail_closed() {
    let snapshot = populated_snapshot();
    let context = context(&snapshot);
    let value = serde_json::to_value(
        ScopedArtifactAvailabilitySnapshotWire::from_context(&context).unwrap(),
    )
    .unwrap();

    let mut top_unknown = value.clone();
    top_unknown["future_meaning"] = json!(true);
    assert!(parse(top_unknown, &context).is_err());

    let mut entry_unknown = value.clone();
    entry_unknown["entries"][0]["native_path"] = json!("/Users/alice/private");
    assert!(parse(entry_unknown, &context).is_err());

    let mut state_unknown = value.clone();
    state_unknown["entries"][0]["state"]["native_id"] = json!("secret");
    assert!(parse(state_unknown, &context).is_err());

    let mut version = value.clone();
    version["scoped_artifact_availability_contract_version"] = json!(2);
    assert!(parse(version, &context).is_err());

    let mut count = value.clone();
    count["entry_count"] = json!(3);
    assert!(parse(count, &context).is_err());

    let mut reordered = value.clone();
    reordered["entries"].as_array_mut().unwrap().reverse();
    assert!(parse(reordered, &context).is_err());

    let mut duplicate = value.clone();
    duplicate["entries"][1] = duplicate["entries"][0].clone();
    assert!(parse(duplicate, &context).is_err());

    let available_index = value["entries"]
        .as_array()
        .unwrap()
        .iter()
        .position(|entry| entry["state"]["kind"] == "available")
        .unwrap();
    let mut zero_generation = value.clone();
    zero_generation["entries"][available_index]["state"]["generation"] = json!(0);
    assert!(parse(zero_generation, &context).is_err());

    let missing_index = value["entries"]
        .as_array()
        .unwrap()
        .iter()
        .position(|entry| entry["state"]["kind"] == "missing")
        .unwrap();
    let mut missing_pair = value.clone();
    missing_pair["entries"][missing_index]["state"]["provenance_ref"] = Value::Null;
    assert!(parse(missing_pair, &context).is_err());

    let over_limit_index = value["entries"]
        .as_array()
        .unwrap()
        .iter()
        .position(|entry| entry["state"]["kind"] == "over_limit")
        .unwrap();
    let mut not_over_limit = value.clone();
    not_over_limit["entries"][over_limit_index]["state"]["observed_bytes"] = json!(1_024);
    assert!(parse(not_over_limit, &context).is_err());

    let mut unsafe_integer = value.clone();
    unsafe_integer["entries"][available_index]["state"]["generation"] =
        json!(JS_SAFE_INTEGER_MAX + 1);
    assert!(parse(unsafe_integer, &context).is_err());

    let mut invalid_kind = value.clone();
    invalid_kind["entries"][0]["artifact_kind"] = json!("Workflow Definition");
    assert!(parse(invalid_kind, &context).is_err());

    let mut zero_revision = value.clone();
    zero_revision["entries"][0]["revision"] = json!(encode_opaque(&[0; 32]));
    assert!(parse(zero_revision, &context).is_err());

    let mut oversized = value;
    let entry = oversized["entries"][0].clone();
    oversized["entries"] = json!(vec![entry; MAX_SCOPED_ARTIFACT_EVIDENCE_ASSERTIONS + 1]);
    oversized["entry_count"] = json!(MAX_SCOPED_ARTIFACT_EVIDENCE_ASSERTIONS + 1);
    assert!(parse(oversized, &context).is_err());
}

#[test]
fn snapshot_and_context_remain_path_free_non_authorizing_and_redacted() {
    let snapshot = populated_snapshot();
    let context = context(&snapshot);
    let value = serde_json::to_value(
        ScopedArtifactAvailabilitySnapshotWire::from_context(&context).unwrap(),
    )
    .unwrap();
    for field in [
        "observer_sequence",
        "scope_epoch",
        "phase",
        "source",
        "relation_id",
        "object_token",
        "locator",
        "content",
        "barrier_sequence",
        "source_access",
    ] {
        assert!(value.get(field).is_none());
    }
    let encoded = serde_json::to_string(&value).unwrap();
    for secret in ["/Users/alice/private", "native-backup", "file-artifact"] {
        assert!(!encoded.contains(secret));
    }
    let debug = format!("{context:?}");
    assert!(debug.contains("expected_entry_count"));
    for secret in ["workflow_definition", "v1:", "root_session_key"] {
        assert!(!debug.contains(secret));
    }
}
