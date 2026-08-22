use serde_json::{json, Value};

use crate::adapter::{BoundedNativeEvidence, CanonicalSourceInstanceKey, SourceRecordId};
use crate::decode_runtime::diagnostic_excerpt;
use crate::unknown_evidence_reducer::{UnknownEvidenceOccurrence, UnknownEvidenceReducer};

use super::*;

const FROZEN_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/contracts/rfc012c-unknown-evidence-snapshot-v1.json"
));

fn source_record_id(index: u64) -> SourceRecordId {
    SourceRecordId::derive(
        "unknown-wire-fixture",
        &CanonicalSourceInstanceKey::derive(1, b"unknown-wire-source").unwrap(),
        b"events",
        b"session.jsonl",
        1,
        &index.to_be_bytes(),
        1,
    )
    .unwrap()
}

fn occurrence(index: u64, family_hint: Option<&str>, payload: &[u8]) -> UnknownEvidenceOccurrence {
    UnknownEvidenceOccurrence::new(
        family_hint.map(str::to_owned),
        BoundedNativeEvidence {
            source_record_id: source_record_id(index),
            observed_bytes: payload.len() as u64,
            payload_digest: *blake3::hash(payload).as_bytes(),
            sanitized_excerpt: diagnostic_excerpt(payload),
        },
    )
    .unwrap()
}

fn snapshot(
    values: impl IntoIterator<Item = UnknownEvidenceOccurrence>,
) -> UnknownEvidenceAggregateSnapshot {
    let mut reducer = UnknownEvidenceReducer::new(
        MAX_UNKNOWN_EVIDENCE_OCCURRENCES,
        MAX_UNKNOWN_EVIDENCE_SAMPLES,
    )
    .unwrap();
    for value in values {
        reducer.apply(value).unwrap();
    }
    reducer.snapshot().unwrap()
}

fn fixture_value() -> Value {
    let empty = UnknownEvidenceSnapshotConsumerContext::from_expected(&snapshot([])).unwrap();
    let populated = UnknownEvidenceSnapshotConsumerContext::from_expected(&snapshot([
        occurrence(
            1,
            Some("future.object"),
            br#"{"private":"secret","count":1}"#,
        ),
        occurrence(2, None, b"opaque private bytes /Users/alice"),
        occurrence(3, Some("future.array"), br#"["secret",2,true]"#),
    ]))
    .unwrap();
    json!({
        "fixture_contract_version": 1,
        "empty": {
            "context": empty.wire(),
            "snapshot": UnknownEvidenceSnapshotWire::from_context(&empty).unwrap(),
        },
        "populated": {
            "context": populated.wire(),
            "snapshot": UnknownEvidenceSnapshotWire::from_context(&populated).unwrap(),
        },
        "expected": {
            "complete_count": 3,
            "sample_count": 3,
            "raw_native_values_disclosed": false,
            "source_locator_disclosed": false,
            "source_access_authority": false,
            "durable_query": false,
            "ordered_observer_event": false,
            "replacement_barrier": false,
            "aggregate_digest_algorithm": "blake3-256",
            "sampling_rank_basis": "source_record_id",
        },
    })
}

fn parse(
    value: Value,
    context: &UnknownEvidenceSnapshotConsumerContext,
) -> Result<UnknownEvidenceSnapshotWire, UnknownEvidenceSnapshotContractError> {
    UnknownEvidenceSnapshotWire::from_wire_value_for_context(value, context)
}

#[test]
fn frozen_unknown_evidence_snapshots_are_stable_bounded_and_value_free() {
    let actual = format!(
        "{}\n",
        serde_json::to_string_pretty(&fixture_value()).unwrap()
    );
    if FROZEN_FIXTURE == "{}\n" {
        println!("{actual}");
    }
    assert_eq!(actual, FROZEN_FIXTURE);

    let fixture: Value = serde_json::from_str(FROZEN_FIXTURE).unwrap();
    for (name, expected) in [
        ("empty", snapshot([])),
        (
            "populated",
            snapshot([
                occurrence(
                    1,
                    Some("future.object"),
                    br#"{"private":"secret","count":1}"#,
                ),
                occurrence(2, None, b"opaque private bytes /Users/alice"),
                occurrence(3, Some("future.array"), br#"["secret",2,true]"#),
            ]),
        ),
    ] {
        let context = UnknownEvidenceSnapshotConsumerContext::from_expected(&expected).unwrap();
        assert!(parse(fixture[name]["snapshot"].clone(), &context).is_ok());
    }

    let encoded = serde_json::to_string(&fixture["populated"]).unwrap();
    for prohibited in ["secret", "/Users/", "alice", "private bytes"] {
        assert!(!encoded.contains(prohibited));
    }
}

#[test]
fn caller_held_totals_digest_and_samples_cannot_be_rewritten_together() {
    let first = snapshot([occurrence(1, Some("future.object"), b"first")]);
    let second = snapshot([occurrence(1, Some("future.object"), b"second")]);
    let first_context = UnknownEvidenceSnapshotConsumerContext::from_expected(&first).unwrap();
    let second_context = UnknownEvidenceSnapshotConsumerContext::from_expected(&second).unwrap();
    let first_wire =
        serde_json::to_value(UnknownEvidenceSnapshotWire::from_context(&first_context).unwrap())
            .unwrap();
    assert_eq!(
        parse(first_wire, &second_context),
        Err(UnknownEvidenceSnapshotContractError::ContextMismatch)
    );
}

#[test]
fn strict_shape_versions_digests_and_sanitized_excerpt_fail_closed() {
    let expected = snapshot([occurrence(
        1,
        Some("future.object"),
        br#"{"private":"secret"}"#,
    )]);
    let context = UnknownEvidenceSnapshotConsumerContext::from_expected(&expected).unwrap();
    let value =
        serde_json::to_value(UnknownEvidenceSnapshotWire::from_context(&context).unwrap()).unwrap();

    let mut unknown = value.clone();
    unknown["future_meaning"] = json!(true);
    assert!(parse(unknown, &context).is_err());

    let mut sample_unknown = value.clone();
    sample_unknown["samples"][0]["native_path"] = json!("/Users/alice/private");
    assert!(parse(sample_unknown, &context).is_err());

    let mut omitted_nullable = value.clone();
    omitted_nullable["samples"][0]
        .as_object_mut()
        .unwrap()
        .remove("family_hint");
    assert!(parse(omitted_nullable, &context).is_err());

    let mut version = value.clone();
    version["unknown_evidence_aggregate_contract_version"] = json!(2);
    assert!(parse(version, &context).is_err());

    let mut zero = value.clone();
    zero["aggregate_digest"] = json!(encode_opaque(&[0; DIGEST_BYTES]));
    assert!(parse(zero, &context).is_err());

    let mut raw_value = value.clone();
    raw_value["samples"][0]["sanitized_excerpt"]["private"] = json!("secret");
    assert!(parse(raw_value, &context).is_err());

    let mut wrong_hash = value.clone();
    wrong_hash["samples"][0]["sanitized_excerpt"]["hash"] = json!("0".repeat(64));
    assert!(parse(wrong_hash, &context).is_err());

    let mut impossible_members = value;
    impossible_members["samples"][0]["sanitized_excerpt"]["members"] = json!(10_000);
    assert!(parse(impossible_members, &context).is_err());

    let value =
        serde_json::to_value(UnknownEvidenceSnapshotWire::from_context(&context).unwrap()).unwrap();
    let mut oversized_excerpt = value;
    oversized_excerpt["samples"][0]["sanitized_excerpt"]["hash"] = json!("a".repeat(1_000_000));
    assert!(parse(oversized_excerpt, &context).is_err());
}

#[test]
fn complete_totals_and_sample_policy_are_coherent() {
    let expected = snapshot([
        occurrence(1, Some("future.one"), b"one"),
        occurrence(2, Some("future.two"), b"two"),
    ]);
    let context = UnknownEvidenceSnapshotConsumerContext::from_expected(&expected).unwrap();
    let value =
        serde_json::to_value(UnknownEvidenceSnapshotWire::from_context(&context).unwrap()).unwrap();

    let mut missing_sample = value.clone();
    missing_sample["samples"].as_array_mut().unwrap().pop();
    assert!(parse(missing_sample, &context).is_err());

    let mut duplicate = value.clone();
    duplicate["samples"][1] = duplicate["samples"][0].clone();
    assert!(parse(duplicate, &context).is_err());

    let mut wrong_bytes = value;
    wrong_bytes["complete_observed_bytes"] = json!(1);
    assert!(parse(wrong_bytes, &context).is_err());
}

#[test]
fn context_debug_withholds_sample_identity_and_content() {
    let expected = snapshot([occurrence(
        1,
        Some("future.private"),
        b"/Users/alice/private",
    )]);
    let context = UnknownEvidenceSnapshotConsumerContext::from_expected(&expected).unwrap();
    let debug = format!("{context:?}");
    assert!(debug.contains("expected_complete_count"));
    assert!(debug.contains("expected_sample_count"));
    assert!(!debug.contains("future.private"));
    assert!(!debug.contains("v1:"));
    assert!(!debug.contains("Users"));
}
