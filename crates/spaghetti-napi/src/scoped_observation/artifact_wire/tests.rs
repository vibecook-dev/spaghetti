use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::adapter::{
    AdapterId, ContractCompleteness, ContractVersionOffer, ContractVersionRequest,
    FactSemanticContext, NativeIdentity, NativeIdentityClaim, QualifiedValue,
    QualifiedValueQuality,
};
use crate::observation_contract::{
    negotiate_observation_contract, ObservationContractOffer, ObservationContractRequest,
};

use super::super::*;
use super::*;

const FROZEN_FIXTURE: &str =
    include_str!("../../../fixtures/contracts/rfc012d-scoped-artifact-v1.json");

fn contract_selection() -> ObservationContractSelection {
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

fn root_identity() -> ScopedObservationRootIdentity {
    let adapter_id = AdapterId::new("fixture").unwrap();
    let context = FactSemanticContext::new(
        &adapter_id,
        1,
        b"stable-source-instance",
        b"transcript",
        b"root-session.jsonl",
        1,
    )
    .unwrap();
    let session_key = CanonicalEntityKey::derive(
        adapter_id.as_str(),
        &context.source_instance_key(),
        "session",
        b"native-session",
    )
    .unwrap();
    let session_ref = ExternalEntityRef::new(session_key);
    let identity = QualifiedValue::from_parts(
        Some(NativeIdentity {
            native_namespace: "fixture.session".to_owned(),
            native_id: "native-session".to_owned(),
        }),
        QualifiedValueQuality::NativeClaimed,
        "fixture".to_owned(),
        ContractCompleteness::Complete,
        None,
        None,
        Vec::new(),
    )
    .unwrap();
    ScopedRootIdentityRequest::new(
        1,
        b"stable-source-instance".as_slice(),
        b"native-session".as_slice(),
        None,
        Some(session_key),
        Some(session_ref),
    )
    .with_native_session_claim(NativeIdentityClaim::new(session_ref, identity).unwrap())
    .resolve(&adapter_id, 1)
    .unwrap()
}

fn artifact_key() -> CanonicalEntityKey {
    let root = root_identity();
    CanonicalEntityKey::derive(
        root.adapter_id.as_str(),
        &root.source_instance_key,
        "artifact",
        b"artifact-17",
    )
    .unwrap()
}

fn command(
    token: u64,
    policy: ScopedArtifactContentPolicy,
    expected_generation: Option<u64>,
    max_bytes: u64,
) -> ScopedArtifactReadCommand {
    ScopedArtifactReadCommand::new(
        Arc::new(ScopedObservationAttachmentAuthority { token }),
        contract_selection(),
        root_identity(),
        ScopedArtifactReadParameters {
            artifact_key: artifact_key(),
            artifact_kind: "workflow_definition".to_owned(),
            expected_generation,
            max_bytes,
            content_policy: policy,
        },
    )
    .unwrap()
}

fn inline_observed(command: &ScopedArtifactReadCommand) -> ScopedObservedArtifactWire {
    let content = b"echo bounded artifact\n".to_vec();
    command
        .observed(ScopedArtifactReadOutcome::Available {
            generation: 7,
            provenance_ref: [23; 32],
            size_bytes: content.len() as u64,
            content_hash: Some(Sha256Digest::of(&content)),
            content: Some(content),
        })
        .unwrap()
}

fn fixture_value() -> Value {
    let command = command(51, ScopedArtifactContentPolicy::Inline, Some(7), 4096);
    json!({
        "context": command.context_wire(),
        "available": inline_observed(&command),
    })
}

fn mutate_available(
    mutator: impl FnOnce(&mut Value),
) -> Result<ScopedObservedArtifactWire, ScopedArtifactContractError> {
    let command = command(51, ScopedArtifactContentPolicy::Inline, Some(7), 4096);
    let mut value = serde_json::to_value(inline_observed(&command)).unwrap();
    mutator(&mut value);
    command.parse_observed(value)
}

#[test]
fn frozen_rust_artifact_fixture_is_stable() {
    let actual = format!(
        "{}\n",
        serde_json::to_string_pretty(&fixture_value()).unwrap()
    );
    assert_eq!(actual, FROZEN_FIXTURE);

    let fixture: Value = serde_json::from_str(FROZEN_FIXTURE).unwrap();
    let command = command(51, ScopedArtifactContentPolicy::Inline, Some(7), 4096);
    let parsed = command
        .parse_observed(fixture["available"].clone())
        .unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), fixture["available"]);
}

#[test]
fn available_content_policy_matrix_is_exact() {
    let metadata = command(52, ScopedArtifactContentPolicy::MetadataOnly, None, 1024);
    assert!(metadata
        .observed(ScopedArtifactReadOutcome::Available {
            generation: 1,
            provenance_ref: [1; 32],
            size_bytes: 900,
            content_hash: None,
            content: None,
        })
        .is_ok());
    assert!(metadata
        .observed(ScopedArtifactReadOutcome::Available {
            generation: 1,
            provenance_ref: [1; 32],
            size_bytes: 900,
            content_hash: Some(Sha256Digest::of(b"secret")),
            content: None,
        })
        .is_err());

    let hash = command(53, ScopedArtifactContentPolicy::HashOnly, Some(3), 1024);
    assert!(hash
        .observed(ScopedArtifactReadOutcome::Available {
            generation: 3,
            provenance_ref: [2; 32],
            size_bytes: 900,
            content_hash: Some(Sha256Digest::of(b"secret")),
            content: None,
        })
        .is_ok());
    assert!(hash
        .observed(ScopedArtifactReadOutcome::Available {
            generation: 3,
            provenance_ref: [2; 32],
            size_bytes: 6,
            content_hash: Some(Sha256Digest::of(b"secret")),
            content: Some(b"secret".to_vec()),
        })
        .is_err());

    let inline = command(54, ScopedArtifactContentPolicy::Inline, Some(4), 1024);
    assert!(inline
        .observed(ScopedArtifactReadOutcome::Available {
            generation: 4,
            provenance_ref: [3; 32],
            size_bytes: 6,
            content_hash: Some(Sha256Digest::of(b"secret")),
            content: Some(b"secret".to_vec()),
        })
        .is_ok());
    assert!(inline
        .observed(ScopedArtifactReadOutcome::Available {
            generation: 4,
            provenance_ref: [3; 32],
            size_bytes: 6,
            content_hash: Some(Sha256Digest::of(b"wrong")),
            content: Some(b"secret".to_vec()),
        })
        .is_err());
}

#[test]
fn unavailable_reasons_require_exact_evidence_shapes() {
    let expected = command(55, ScopedArtifactContentPolicy::HashOnly, Some(7), 100);
    assert!(expected
        .observed(ScopedArtifactReadOutcome::Unavailable {
            reason: ScopedArtifactUnavailableReason::ChangedGeneration,
            observed_generation: Some(8),
            observed_bytes: None,
            provenance_ref: Some([4; 32]),
        })
        .is_ok());
    assert!(expected
        .observed(ScopedArtifactReadOutcome::Unavailable {
            reason: ScopedArtifactUnavailableReason::ChangedGeneration,
            observed_generation: Some(7),
            observed_bytes: None,
            provenance_ref: Some([4; 32]),
        })
        .is_err());
    assert!(expected
        .observed(ScopedArtifactReadOutcome::Unavailable {
            reason: ScopedArtifactUnavailableReason::OverLimit,
            observed_generation: Some(7),
            observed_bytes: Some(101),
            provenance_ref: Some([4; 32]),
        })
        .is_ok());
    assert!(expected
        .observed(ScopedArtifactReadOutcome::Unavailable {
            reason: ScopedArtifactUnavailableReason::Missing,
            observed_generation: None,
            observed_bytes: Some(101),
            provenance_ref: None,
        })
        .is_err());
    for reason in [
        ScopedArtifactUnavailableReason::OutOfScope,
        ScopedArtifactUnavailableReason::Denied,
        ScopedArtifactUnavailableReason::Missing,
        ScopedArtifactUnavailableReason::Unsupported,
        ScopedArtifactUnavailableReason::Malformed,
        ScopedArtifactUnavailableReason::Unstable,
    ] {
        assert!(expected
            .observed(ScopedArtifactReadOutcome::Unavailable {
                reason,
                observed_generation: None,
                observed_bytes: None,
                provenance_ref: None,
            })
            .is_ok());
    }
}

#[test]
fn command_and_result_bounds_fail_before_content_decode() {
    assert!(ScopedArtifactReadCommand::new(
        Arc::new(ScopedObservationAttachmentAuthority { token: 56 }),
        contract_selection(),
        root_identity(),
        ScopedArtifactReadParameters {
            artifact_key: artifact_key(),
            artifact_kind: "Artifact Path".to_owned(),
            expected_generation: Some(1),
            max_bytes: 1,
            content_policy: ScopedArtifactContentPolicy::MetadataOnly,
        },
    )
    .is_err());
    assert!(ScopedArtifactReadCommand::new(
        Arc::new(ScopedObservationAttachmentAuthority { token: 56 }),
        contract_selection(),
        root_identity(),
        ScopedArtifactReadParameters {
            artifact_key: artifact_key(),
            artifact_kind: "artifact".to_owned(),
            expected_generation: Some(0),
            max_bytes: 1,
            content_policy: ScopedArtifactContentPolicy::MetadataOnly,
        },
    )
    .is_err());
    assert!(ScopedArtifactReadCommand::new(
        Arc::new(ScopedObservationAttachmentAuthority { token: 56 }),
        contract_selection(),
        root_identity(),
        ScopedArtifactReadParameters {
            artifact_key: artifact_key(),
            artifact_kind: "artifact".to_owned(),
            expected_generation: Some(1),
            max_bytes: MAX_INLINE_ARTIFACT_BYTES + 1,
            content_policy: ScopedArtifactContentPolicy::Inline,
        },
    )
    .is_err());

    assert!(mutate_available(|value| {
        value["outcome"]["content_base64"] = Value::String("A".repeat(MAX_INLINE_BASE64_BYTES + 1));
    })
    .is_err());

    let bounded_inline = command(
        58,
        ScopedArtifactContentPolicy::Inline,
        Some(1),
        MAX_INLINE_ARTIFACT_BYTES,
    );
    let oversized_content = vec![0_u8; MAX_INLINE_ARTIFACT_BYTES as usize + 1];
    assert!(bounded_inline
        .observed(ScopedArtifactReadOutcome::Available {
            generation: 1,
            provenance_ref: [5; 32],
            size_bytes: MAX_INLINE_ARTIFACT_BYTES,
            content_hash: Some(Sha256Digest::of(&oversized_content)),
            content: Some(oversized_content),
        })
        .is_err());
}

#[test]
fn response_consumption_is_bound_to_exact_request_context() {
    for mutate in [
        |value: &mut Value| value["request"]["max_bytes"] = json!(4095),
        |value: &mut Value| value["request"]["expected_generation"] = json!(8),
        |value: &mut Value| value["request"]["artifact_kind"] = json!("workflow_journal"),
        |value: &mut Value| {
            value["request"]["artifact_key"] = value["request"]["attachment_ref"].clone()
        },
        |value: &mut Value| {
            value["request"]["attachment_ref"] = value["request"]["request_id"].clone()
        },
        |value: &mut Value| {
            value["request"]["contract_selection"]["event_contract_version"] = json!(2)
        },
        |value: &mut Value| {
            value["request"]["root"]["session_key"] = value["request"]["artifact_key"].clone()
        },
        |value: &mut Value| value["request"]["request_id"] = json!(encode_opaque(&[7; 32])),
        |value: &mut Value| value["request"]["unexpected"] = json!(true),
        |value: &mut Value| value["outcome"]["generation"] = json!(8),
        |value: &mut Value| value["locator_disclosure"] = json!("disclosed"),
    ] {
        assert!(mutate_available(mutate).is_err());
    }
    assert!(mutate_available(|value| {
        value["outcome"]
            .as_object_mut()
            .unwrap()
            .remove("content_hash");
    })
    .is_err());
    assert!(mutate_available(|value| {
        value["outcome"]["provenance_ref"] = json!(encode_opaque(&[0; 32]));
    })
    .is_err());
}

#[test]
fn artifact_wire_and_debug_never_expose_native_locators() {
    let command = command(57, ScopedArtifactContentPolicy::Inline, Some(7), 4096);
    let response = inline_observed(&command);
    let serialized = serde_json::to_string(&response).unwrap();
    let debug = format!("{command:?}");
    for secret in ["/Users/alice/private", "backup-name", "secret.txt"] {
        assert!(!serialized.contains(secret));
        assert!(!debug.contains(secret));
    }
    assert!(serialized.contains("\"locator_disclosure\":\"withheld\""));
    assert!(!serialized.contains("locator_path"));
}
