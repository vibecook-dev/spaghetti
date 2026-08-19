use super::*;

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::adapter::{
    AdapterId, CanonicalEntityKey, ContractVersionOffer, ContractVersionRequest,
    FactSemanticContext,
};
use crate::observation_contract::{
    negotiate_observation_contract, ObservationContractOffer, ObservationContractRequest,
};

fn selection() -> ObservationContractSelection {
    let families = BTreeMap::from([("runtime.usage-v2".to_owned(), vec![1])]);
    let request = ObservationContractRequest::new(
        ContractVersionRequest {
            selection_contract_version: 1,
            model_major: 1,
            external_entity_reference_version: 1,
            semantic_revision_reference_version: 1,
            coverage_contract_versions: vec![1],
            fact_family_versions: families.clone(),
            query_pack_versions: None,
            observation_contract_versions: Some(vec![1]),
        },
        vec![1],
        vec![1],
        vec![1],
    )
    .unwrap();
    let offer = ObservationContractOffer::new(
        ContractVersionOffer {
            selection_contract_version: 1,
            model_major: 1,
            external_entity_reference_versions: vec![1],
            semantic_revision_reference_versions: vec![1],
            coverage_contract_versions: vec![1],
            fact_family_versions: families,
            query_pack_versions: Vec::new(),
            observation_contract_versions: vec![1],
        },
        vec![1],
        vec![1],
        vec![1],
    )
    .unwrap();
    negotiate_observation_contract(&request, &offer).unwrap()
}

fn fixture_context() -> ScopedArtifactAvailabilityEnvelopeConsumerContext {
    let adapter = AdapterId::new("fixture").unwrap();
    let semantic = FactSemanticContext::new(
        &adapter,
        1,
        b"portable-source",
        b"artifact-stream",
        b"artifact-object",
        1,
    )
    .unwrap();
    let source = ScopedSourceObjectIdentity::from_semantic_context(&semantic).unwrap();
    let root_session = CanonicalEntityKey::derive(
        adapter.as_str(),
        &semantic.source_instance_key(),
        "session",
        b"root-session",
    )
    .unwrap();
    let root = ScopedObservationRootIdentity {
        adapter_id: adapter.clone(),
        source_instance_key: semantic.source_instance_key(),
        session_key: root_session,
        session_ref: crate::adapter::ExternalEntityRef::new(root_session),
        root_actor_run_key: CanonicalEntityKey::derive(
            adapter.as_str(),
            &semantic.source_instance_key(),
            "actor_run",
            b"root-actor",
        )
        .unwrap(),
        native_session_claim: None,
    };
    let expected_entry = ScopedArtifactAvailabilityEntryWire {
        artifact_key: CanonicalEntityKey::derive(
            adapter.as_str(),
            &semantic.source_instance_key(),
            "artifact",
            b"portable-artifact",
        )
        .unwrap(),
        artifact_kind: "workflow_definition".to_owned(),
        revision: encode_opaque(&[7; DIGEST_BYTES]),
        state: super::super::artifact_availability_wire::ScopedArtifactAvailabilityStateWire::Available {
            generation: 3,
            provenance_ref: encode_opaque(&[8; DIGEST_BYTES]),
            size_bytes: 17,
        },
    };
    let source_declaration_digest = [9; DIGEST_BYTES];
    let revision = [7; DIGEST_BYTES];
    let event_id = artifact_availability_event_id_for_components(
        &source,
        root.session_key,
        3,
        &source_declaration_digest,
        expected_entry.artifact_key,
        &expected_entry.artifact_kind,
        &revision,
    );
    ScopedArtifactAvailabilityEnvelopeConsumerContext {
        contract_selection: selection(),
        root,
        source,
        source_declaration_digest,
        source_generation: 3,
        observer_sequence: 4,
        scope_epoch: 2,
        event_id,
        observed_at: 1_234,
        phase: ArtifactAvailabilityDeliveryPhaseWire::Live,
        expected_entry,
    }
}

fn valid_value(context: &ScopedArtifactAvailabilityEnvelopeConsumerContext) -> Value {
    let context_wire = serde_json::to_value(context.wire()).unwrap();
    let root = context_wire["root"].clone();
    let source = &context_wire["expected_source"];
    json!({
        "scoped_artifact_availability_envelope_contract_version": 1,
        "contract_version": context.contract_selection.envelope_contract_version,
        "contract_selection": context.contract_selection,
        "observer_sequence": context.observer_sequence,
        "scope_epoch": context.scope_epoch,
        "event_id": encode_opaque(context.event_id.as_bytes()),
        "semantic_revision_ref": null,
        "root": root,
        "actor": {
            "root_session_key": context.root.session_key,
            "run_key": context.root.root_actor_run_key,
            "role": "root",
            "parent_run_key": null,
            "native_session_id": null,
            "native_actor_id": null,
            "native_actor_type": null
        },
        "actor_attribution": {
            "kind": "scope_fallback",
            "reason": "source_lifecycle_control"
        },
        "affiliations": {
            "actor_run_key": context.root.root_actor_run_key,
            "team_key": null,
            "native_team_id": null,
            "team_name": null,
            "member_key": null,
            "workflow_key": null,
            "native_workflow_id": null,
            "completeness": "unknown",
            "derived_from_revision_refs": []
        },
        "source": {
            "instance_key": source["instance_key"],
            "stream_key": source["stream_key"],
            "object_key": source["object_key"],
            "locator_id": null,
            "generation": source["generation"],
            "source_record_id": null,
            "record_index": null,
            "cursor_start": null,
            "cursor_end": null,
            "byte_range": null
        },
        "native_time": null,
        "observed_at": context.observed_at,
        "phase": match context.phase {
            ArtifactAvailabilityDeliveryPhaseWire::Bootstrap => "bootstrap",
            ArtifactAvailabilityDeliveryPhaseWire::Live => "live",
        },
        "evidence": {
            "authority": "common_reducer",
            "quality": "derived",
            "effective_at": null,
            "completeness": "complete"
        },
        "event": {
            "kind": "artifact_availability",
            "entry": context_wire["expected_entry"]
        },
        "native_evidence": {"kind": "engine_control"}
    })
}

#[test]
fn exact_context_round_trips_and_withholds_private_declaration_authority() {
    let context = fixture_context();
    let value = valid_value(&context);
    let parsed = ScopedArtifactAvailabilityEnvelopeWire::from_wire_value_for_context(
        value.clone(),
        &context,
    )
    .unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), value);

    let context_value = serde_json::to_value(context.wire()).unwrap();
    let encoded = serde_json::to_string(&context_value).unwrap();
    assert!(!encoded.contains("source_declaration"));
    assert!(!format!("{context:?}").contains(&encode_opaque(&[9; DIGEST_BYTES])));
}

#[test]
fn caller_order_event_and_entry_context_are_exact() {
    let context = fixture_context();
    for mutation in ["observer_sequence", "scope_epoch", "event_id"] {
        let mut value = valid_value(&context);
        value[mutation] = match mutation {
            "observer_sequence" => json!(5),
            "scope_epoch" => json!(3),
            "event_id" => json!(encode_opaque(&[6; DIGEST_BYTES])),
            _ => unreachable!(),
        };
        assert!(
            ScopedArtifactAvailabilityEnvelopeWire::from_wire_value_for_context(value, &context)
                .is_err()
        );
    }

    let mut state = valid_value(&context);
    state["event"]["entry"]["state"]["size_bytes"] = json!(18);
    assert!(
        ScopedArtifactAvailabilityEnvelopeWire::from_wire_value_for_context(state, &context)
            .is_err()
    );

    let mut revision = valid_value(&context);
    revision["event"]["entry"]["revision"] = json!(encode_opaque(&[5; DIGEST_BYTES]));
    assert!(
        ScopedArtifactAvailabilityEnvelopeWire::from_wire_value_for_context(revision, &context)
            .is_err()
    );

    let mut observed_at = valid_value(&context);
    observed_at["observed_at"] = json!(1_235);
    assert!(
        ScopedArtifactAvailabilityEnvelopeWire::from_wire_value_for_context(observed_at, &context)
            .is_err()
    );

    let mut phase = valid_value(&context);
    phase["phase"] = json!("bootstrap");
    assert!(
        ScopedArtifactAvailabilityEnvelopeWire::from_wire_value_for_context(phase, &context)
            .is_err()
    );

    let mut declaration = context.clone();
    declaration.source_declaration_digest[0] ^= 1;
    assert!(
        ScopedArtifactAvailabilityEnvelopeWire::from_wire_value_for_context(
            valid_value(&context),
            &declaration,
        )
        .is_err()
    );
}

#[test]
fn availability_event_rejects_source_privacy_and_evidence_drift() {
    let context = fixture_context();
    let mutations = [
        ("locator_id", json!("native/path")),
        ("source_record_id", json!(encode_opaque(&[4; DIGEST_BYTES]))),
        ("record_index", json!(0)),
        ("cursor_start", json!(encode_opaque(&[3; DIGEST_BYTES]))),
        ("byte_range", json!({"start": 0, "end": 1})),
    ];
    for (field, replacement) in mutations {
        let mut value = valid_value(&context);
        value["source"][field] = replacement;
        assert!(
            ScopedArtifactAvailabilityEnvelopeWire::from_wire_value_for_context(value, &context)
                .is_err()
        );
    }

    let mut correction = valid_value(&context);
    correction["phase"] = json!("correction");
    assert!(
        ScopedArtifactAvailabilityEnvelopeWire::from_wire_value_for_context(correction, &context,)
            .is_err()
    );

    let mut authority = valid_value(&context);
    authority["evidence"]["authority"] = json!("engine_control");
    assert!(
        ScopedArtifactAvailabilityEnvelopeWire::from_wire_value_for_context(authority, &context)
            .is_err()
    );
}

#[test]
fn availability_event_preflights_unknown_and_oversized_values() {
    let context = fixture_context();
    let mut unknown = valid_value(&context);
    unknown["extra"] = json!(true);
    assert!(
        ScopedArtifactAvailabilityEnvelopeWire::from_wire_value_for_context(unknown, &context)
            .is_err()
    );

    let mut oversized = valid_value(&context);
    oversized["event"]["entry"]["artifact_kind"] = json!("a".repeat(129));
    assert!(
        ScopedArtifactAvailabilityEnvelopeWire::from_wire_value_for_context(oversized, &context)
            .is_err()
    );

    let mut zero_generation = valid_value(&context);
    zero_generation["source"]["generation"] = json!(0);
    assert!(
        ScopedArtifactAvailabilityEnvelopeWire::from_wire_value_for_context(
            zero_generation,
            &context,
        )
        .is_err()
    );
}
