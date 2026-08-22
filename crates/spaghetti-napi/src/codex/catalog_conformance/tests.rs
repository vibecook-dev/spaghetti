use std::fs;

use tempfile::TempDir;

use super::super::adapter::{normalize_session_meta, CodexSessionMetaError};
use super::*;
use crate::adapter::{
    verify_support_release_bundle, AdapterSupportRegistration, CompatibilityClass,
    SupportBundleDocument, SupportCatalog, SupportOperation, VerifiedSupportRelease,
};

const FROZEN_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/contracts/rfc012b-codex-candidate-conformance-v1.json"
));

#[test]
fn normalization_seam_rejects_malformed_and_classifies_internal_metadata() {
    let root = serde_json::json!({"type": "session_meta", "payload": null});
    assert!(matches!(
        normalize_session_meta(root.as_object().unwrap()),
        Err(CodexSessionMetaError::PayloadNotObject)
    ));

    let missing = serde_json::json!({
        "type": "session_meta",
        "payload": {"id": "session-without-cwd"}
    });
    assert!(matches!(
        normalize_session_meta(missing.as_object().unwrap()),
        Err(CodexSessionMetaError::MissingIdentity)
    ));

    let internal = serde_json::json!({
        "type": "session_meta",
        "payload": {
            "id": "child",
            "cwd": "/sanitized/project",
            "thread_source": "subagent"
        }
    });
    let normalized = normalize_session_meta(internal.as_object().unwrap()).unwrap();
    assert!(normalized.internal);
}

#[test]
fn bounded_head_matches_independent_census_and_full_decoder_identity() {
    let head = head_projection(&fixture_root(), 7).unwrap();
    let full = full_decoder_identities(&fixture_root(), 7).unwrap();
    assert_eq!(head.projects, full.projects);
    assert_eq!(head.sessions, full.sessions);

    let expected: CodexCandidateConformanceFixture = serde_json::from_str(FROZEN_FIXTURE).unwrap();
    let actual = head.report();
    assert_eq!(
        actual,
        expected,
        "repin with this Rust report:\n{}",
        serde_json::to_string_pretty(&actual).unwrap()
    );
}

#[test]
fn full_catalog_oracle_uses_the_common_decode_boundary() {
    let source = include_str!("../catalog_conformance.rs");
    let direct_decode = [".de", "code("].concat();
    let direct_batch = ["FactBatch::", "new("].concat();
    assert!(source.contains("decode_record(DecodeRuntimeRequest"));
    assert!(!source.contains(&direct_decode));
    assert!(!source.contains(&direct_batch));
}

#[test]
fn head_source_entity_and_fact_identities_ignore_registration_ids() {
    let first = head_projection(&fixture_root(), 7).unwrap().report();
    let reordered = head_projection(&fixture_root(), 70_007).unwrap().report();
    assert_eq!(first, reordered);

    let first_full = full_decoder_identities(&fixture_root(), 7).unwrap();
    let reordered_full = full_decoder_identities(&fixture_root(), 70_007).unwrap();
    assert_eq!(first_full, reordered_full);
}

#[test]
fn candidate_head_excludes_internal_and_rejects_invalid_or_oversized_first_records() {
    let internal = TempDir::new().unwrap();
    write_rollout(
        internal.path(),
        "rollout-internal.jsonl",
        &serde_json::json!({
            "type": "session_meta",
            "payload": {
                "id": "child",
                "cwd": "/sanitized/project",
                "thread_source": "subagent"
            }
        })
        .to_string(),
    );
    let projection = head_projection(internal.path(), 1).unwrap();
    assert!(projection.projects.is_empty());
    assert!(projection.sessions.is_empty());
    assert!(projection.fact_ids.is_empty());

    let invalid = TempDir::new().unwrap();
    write_rollout(
        invalid.path(),
        "rollout-invalid.jsonl",
        r#"{"type":"session_meta","payload":{"id":"missing-cwd"}}"#,
    );
    assert!(head_projection(invalid.path(), 1).is_err());

    let exact_bound = TempDir::new().unwrap();
    write_rollout(
        exact_bound.path(),
        "rollout-exact-bound.jsonl",
        &sized_session_meta(CANDIDATE_HEAD_RECORD_PAYLOAD_BYTES),
    );
    let exact_projection = head_projection(exact_bound.path(), 1).unwrap();
    assert_eq!(exact_projection.sessions.len(), 1);
    assert_eq!(
        exact_projection.total_physical_bytes_read,
        CANDIDATE_HEAD_PHYSICAL_BYTES
    );

    let oversized = TempDir::new().unwrap();
    write_rollout(
        oversized.path(),
        "rollout-oversized.jsonl",
        &sized_session_meta(CANDIDATE_HEAD_RECORD_PAYLOAD_BYTES + 1),
    );
    assert!(head_projection(oversized.path(), 1).is_err());

    let long_object = TempDir::new().unwrap();
    let directory = long_object.path().join("sessions/2026/01/01");
    fs::create_dir_all(&directory).unwrap();
    let first_record = sized_session_meta(256);
    fs::write(
        directory.join("rollout-long-tail.jsonl"),
        format!("{first_record}\n{}\n", "x".repeat(64 * 1024)),
    )
    .unwrap();
    let long_projection = head_projection(long_object.path(), 1).unwrap();
    assert_eq!(long_projection.sessions.len(), 1);
    assert!(long_projection.total_physical_bytes_read > CANDIDATE_HEAD_PREFIX_BYTES);
    assert!(long_projection.total_physical_bytes_read <= CANDIDATE_HEAD_PHYSICAL_BYTES);
}

#[test]
fn exact_candidate_bundle_remains_non_authorizing_for_catalog_access() {
    let release_wire: serde_json::Value = serde_json::from_slice(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../agent-support/codex/candidate-2026-08-15/support-release.json"
    )))
    .unwrap();
    let catalog_capability = release_wire["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|capability| capability["capability_id"] == "project-session-catalog")
        .unwrap();
    assert_eq!(catalog_capability["topology"], "catalog");
    assert_eq!(catalog_capability["level"], "unsupported");

    let declaration: serde_json::Value = serde_json::from_slice(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../agent-support/codex/candidate-2026-08-15/source-declarations.json"
    )))
    .unwrap();
    let stream = declaration["streams"]
        .as_array()
        .unwrap()
        .iter()
        .find(|stream| stream["stream_id"] == STREAM_ID)
        .expect("candidate declaration must retain the exact conformance stream");
    assert_eq!(stream["stream_id"], STREAM_ID);
    assert_eq!(stream["topologies"], serde_json::json!(["durable"]));
    assert_eq!(stream["overlap_strategy"], "full_only");
    let declared_record_bytes = stream["bounds"]["max_record_bytes"].as_u64().unwrap();
    assert_eq!(declared_record_bytes, 4 * 1024 * 1024);
    assert_ne!(
        declared_record_bytes,
        CANDIDATE_HEAD_RECORD_PAYLOAD_BYTES as u64
    );

    let release = verified_candidate_release();
    assert_eq!(release.descriptor().support_release_id, SUPPORT_RELEASE_ID);
    assert_candidate_release(release.descriptor().status);

    let adapter = CodexAdapter::new();
    let manifest = adapter.manifest();
    release
        .verify_adapter_binding(
            manifest.id.as_str(),
            manifest.support_binding.as_ref().unwrap(),
        )
        .unwrap();
    release
        .verify_scope_programs(manifest.scope_programs.as_ref().unwrap())
        .unwrap();

    let catalog = SupportCatalog::new([release]).unwrap();
    let probe = candidate_probe();
    let decision = catalog.classify(&probe).unwrap();
    assert_eq!(
        decision.compatibility_class(),
        CompatibilityClass::RecognizedUnverified
    );

    let (request, offer) = compatible_contracts();
    let error = catalog
        .authorize_typed_access(
            AdapterSupportRegistration::new(
                manifest.id.as_str(),
                manifest.support_binding.as_ref().unwrap(),
                manifest.scope_programs.as_ref().unwrap(),
            ),
            &probe,
            SupportOperation::CatalogDiscovery,
            &request,
            &offer,
        )
        .unwrap_err();
    assert!(error.to_string().contains("forbidden"));
}

fn write_rollout(root: &Path, name: &str, first_record: &str) {
    let directory = root.join("sessions/2026/01/01");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join(name), format!("{first_record}\n")).unwrap();
}

fn sized_session_meta(payload_bytes: usize) -> String {
    let prefix =
        r#"{"type":"session_meta","payload":{"id":"edge","cwd":"/sanitized/project","padding":""#;
    let suffix = r#""}}"#;
    assert!(payload_bytes >= prefix.len() + suffix.len());
    format!(
        "{prefix}{}{suffix}",
        "x".repeat(payload_bytes - prefix.len() - suffix.len())
    )
}

fn verified_candidate_release() -> VerifiedSupportRelease {
    verify_support_release_bundle(
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../agent-support/codex/candidate-2026-08-15/support-release.json"
        )),
        &[
            SupportBundleDocument::new(
                "agent-support/codex/candidate-2026-08-15/ads.json",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../agent-support/codex/candidate-2026-08-15/ads.json"
                )),
            ),
            SupportBundleDocument::new(
                "agent-support/codex/candidate-2026-08-15/source-declarations.json",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../agent-support/codex/candidate-2026-08-15/source-declarations.json"
                )),
            ),
            SupportBundleDocument::new(
                "agent-support/codex/candidate-2026-08-15/scope-programs.json",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../agent-support/codex/candidate-2026-08-15/scope-programs.json"
                )),
            ),
            SupportBundleDocument::new(
                "agent-support/codex/candidate-2026-08-15/evidence.json",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../agent-support/codex/candidate-2026-08-15/evidence.json"
                )),
            ),
            SupportBundleDocument::new(
                "agent-support/codex/candidate-2026-08-15/conformance.json",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../agent-support/codex/candidate-2026-08-15/conformance.json"
                )),
            ),
        ],
    )
    .unwrap()
}
