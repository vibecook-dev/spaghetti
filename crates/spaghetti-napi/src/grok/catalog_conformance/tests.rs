use std::fs;

use tempfile::TempDir;

use super::*;
use crate::adapter::{
    verify_support_release_bundle, AdapterSupportRegistration, CompatibilityClass,
    ContractVersionOffer, ContractVersionRequest, NativeArtifactProbe, SupportBundleDocument,
    SupportCatalog, SupportOperation, VerifiedSupportRelease,
};

const FROZEN_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/contracts/rfc012b-grok-candidate-conformance-v1.json"
));

#[test]
fn bounded_membership_and_summary_match_independent_census_and_full_decoder_identity() {
    let projection = candidate_projection(&fixture_root(), 7).unwrap();
    assert_eq!(projection.projects, projection.hydrated_projects);
    assert_eq!(projection.sessions, projection.hydrated_sessions);

    let actual = projection.report();
    let expected: GrokCandidateConformanceFixture = serde_json::from_str(FROZEN_FIXTURE)
        .unwrap_or_else(|error| {
            panic!(
                "frozen fixture is invalid ({error}); repin with this Rust report:\n{}",
                serde_json::to_string_pretty(&actual).unwrap()
            )
        });
    assert_eq!(
        actual,
        expected,
        "repin with this Rust report:\n{}",
        serde_json::to_string_pretty(&actual).unwrap()
    );

    // The frozen artifact is safe to review and ship: it contains only counts,
    // bounds, release/declaration IDs, and opaque digests.
    assert!(!FROZEN_FIXTURE.contains("/tmp/"));
    assert!(!FROZEN_FIXTURE.contains("%2F"));
    assert!(!FROZEN_FIXTURE.contains("019f"));
}

#[test]
fn source_entity_fact_and_revision_identities_ignore_registration_ids() {
    let first = candidate_projection(&fixture_root(), 7).unwrap().report();
    let reordered = candidate_projection(&fixture_root(), 70_007)
        .unwrap()
        .report();
    assert_eq!(first, reordered);
}

#[test]
fn membership_precedes_summary_metadata_and_updates_only_stays_non_admitting() {
    let chat_only = TempDir::new().unwrap();
    let chat_root = chat_only.path().join(".grok");
    write_sidecar(
        &chat_root,
        "%2Ftmp%2Fproject",
        "chat-only",
        "chat_history.jsonl",
        b"",
    );
    let projection = candidate_projection(&chat_root, 1).unwrap();
    assert_eq!(projection.member_coordinates.len(), 1);
    assert_eq!(projection.sessions.len(), 1);
    assert!(projection.hydrated_sessions.is_empty());
    assert_eq!(projection.summary_metadata_unavailable, 0);

    let updates_only = TempDir::new().unwrap();
    let updates_root = updates_only.path().join(".grok");
    write_sidecar(
        &updates_root,
        "%2Ftmp%2Fproject",
        "updates-only",
        "updates.jsonl",
        b"{}\n",
    );
    let projection = candidate_projection(&updates_root, 1).unwrap();
    assert!(projection.member_coordinates.is_empty());
    assert!(projection.sessions.is_empty());

    let unknown_only = TempDir::new().unwrap();
    let unknown_root = unknown_only.path().join(".grok");
    write_sidecar(
        &unknown_root,
        "%2Ftmp%2Fproject",
        "unknown-only",
        "future-sidecar.json",
        b"{}",
    );
    let projection = candidate_projection(&unknown_root, 1).unwrap();
    assert!(projection.member_coordinates.is_empty());

    let malformed = TempDir::new().unwrap();
    let malformed_root = malformed.path().join(".grok");
    write_sidecar(
        &malformed_root,
        "%2Ftmp%2Fproject",
        "malformed",
        "summary.json",
        b"{",
    );
    let projection = candidate_projection(&malformed_root, 1).unwrap();
    assert_eq!(projection.sessions.len(), 1);
    assert!(projection.hydrated_sessions.is_empty());
    assert_eq!(projection.summary_metadata_unavailable, 1);
    assert_eq!(projection.summary_bytes_read, 1);

    let exact = TempDir::new().unwrap();
    let exact_root = exact.path().join(".grok");
    let exact_summary = sized_summary("exact-bound", "/tmp/project", CANDIDATE_SUMMARY_MAX_BYTES);
    write_sidecar(
        &exact_root,
        "%2Ftmp%2Fproject",
        "exact-bound",
        "summary.json",
        &exact_summary,
    );
    let projection = candidate_projection(&exact_root, 1).unwrap();
    assert_eq!(projection.sessions.len(), 1);
    assert_eq!(projection.hydrated_sessions.len(), 1);
    assert_eq!(
        projection.summary_bytes_read,
        CANDIDATE_SUMMARY_MAX_BYTES as u64
    );

    let oversized = TempDir::new().unwrap();
    let oversized_root = oversized.path().join(".grok");
    let oversized_summary =
        sized_summary("oversized", "/tmp/project", CANDIDATE_SUMMARY_MAX_BYTES + 1);
    write_sidecar(
        &oversized_root,
        "%2Ftmp%2Fproject",
        "oversized",
        "summary.json",
        &oversized_summary,
    );
    let projection = candidate_projection(&oversized_root, 1).unwrap();
    assert_eq!(projection.sessions.len(), 1);
    assert!(projection.hydrated_sessions.is_empty());
    assert_eq!(projection.summary_metadata_unavailable, 1);
    assert_eq!(projection.summary_bytes_read, 0);
}

#[test]
fn summary_identity_drift_fails_closed_instead_of_retargeting() {
    let fixture = TempDir::new().unwrap();
    let root = fixture.path().join(".grok");
    let summary = serde_json::json!({
        "info": {
            "id": "different-session",
            "cwd": "/tmp/different-project"
        },
        "generated_title": "must not retarget"
    });
    write_sidecar(
        &root,
        "%2Ftmp%2Fproject",
        "path-session",
        "summary.json",
        summary.to_string().as_bytes(),
    );

    let error = match candidate_projection(&root, 1) {
        Ok(_) => panic!("drifted summary must not produce a catalog projection"),
        Err(error) => error,
    };
    assert!(error
        .to_string()
        .contains("explicit relation evidence is required"));
}

#[test]
fn exact_candidate_bundle_and_streams_remain_non_authorizing_and_planned_unbound() {
    let release_wire: serde_json::Value = serde_json::from_slice(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../agent-support/grok/candidate-2026-08-15/support-release.json"
    )))
    .unwrap();
    assert_eq!(release_wire["support_release_id"], SUPPORT_RELEASE_ID);
    assert_eq!(release_wire["status"], "candidate");
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
        "/../../agent-support/grok/candidate-2026-08-15/source-declarations.json"
    )))
    .unwrap();
    assert_eq!(declaration["declaration_id"], SOURCE_DECLARATION_ID);
    assert_eq!(declaration["status"], "candidate");
    let declared_membership = exact_declared_stream(&declaration, MEMBERSHIP_STREAM_ID);
    assert_eq!(declared_membership["primitive"], "DirectoryMembership");
    assert_eq!(
        declared_membership["relative_patterns"],
        serde_json::json!([
            "**/chat_history.jsonl",
            "**/summary.json",
            "**/events.jsonl",
            "**/signals.json"
        ])
    );
    assert_eq!(
        declared_membership["bounds"],
        serde_json::json!({"max_entries": 100_000, "max_depth": 8})
    );
    assert_eq!(
        declared_membership["topologies"],
        serde_json::json!(["durable"])
    );
    assert_eq!(declared_membership["overlap_strategy"], "full_only");
    assert_eq!(declared_membership["decoder_id"], "grok-session-membership");
    assert_eq!(
        declared_membership["disposition_ownership"],
        serde_json::json!(["native-family:session-membership"])
    );
    let declared_summary = exact_declared_stream(&declaration, SUMMARY_STREAM_ID);
    assert_eq!(declared_summary["primitive"], "ReplaceDocument");
    assert_eq!(declared_summary["bounds"]["max_object_bytes"], 1024 * 1024);
    assert_eq!(declared_summary["overlap_strategy"], "full_only");
    assert_eq!(declared_summary["decoder_id"], "grok-summary");
    assert_eq!(
        declared_summary["disposition_ownership"],
        serde_json::json!(["native-family:session-summary"])
    );

    let adapter = GrokAdapter::new();
    let instance = fixture_instance(&fixture_root(), 7).unwrap();
    let streams = adapter.streams(&instance).unwrap();
    let membership = exact_stream(&streams, MEMBERSHIP_STREAM_ID).unwrap();
    assert_eq!(
        membership.selector.include,
        ADMITTED_SIDECARS.map(|name| format!("**/{name}"))
    );
    assert!(matches!(
        membership.driver,
        DriverSpec::DirectorySnapshot(ref config)
            if config.max_entries == CANDIDATE_MEMBERSHIP_MAX_ENTRIES
                && config.max_depth == CANDIDATE_MEMBERSHIP_MAX_DEPTH
    ));
    let summary = exact_stream(&streams, SUMMARY_STREAM_ID).unwrap();
    assert_eq!(summary.selector.include, vec!["**/summary.json"]);
    assert!(matches!(
        summary.driver,
        DriverSpec::ReplaceDocument(ref config)
            if config.max_document_bytes == CANDIDATE_SUMMARY_MAX_BYTES
    ));

    let composition: serde_json::Value = serde_json::from_slice(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fixtures/contracts/rfc012b-catalog-compositions-v1.json"
    )))
    .unwrap();
    let planned = composition["compositions"]["grok"].as_object().unwrap();
    assert_eq!(planned["binding"]["status"], "planned_unbound");
    let planned_membership = planned["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|component| component["component_id"] == "session-directory-membership")
        .unwrap();
    assert_eq!(
        planned_membership["relative_selectors"],
        serde_json::json!(["*/*"])
    );
    assert_eq!(
        planned_membership["discovery_bounds"]["max_entries"],
        250_000
    );
    assert_eq!(planned_membership["discovery_bounds"]["max_depth"], 64);
    assert_eq!(
        planned_membership["overlap_strategy"]["kind"],
        "commit_catalog_facts"
    );
    assert_ne!(
        planned_membership["relative_selectors"],
        declared_membership["relative_patterns"]
    );
    let planned_summary = planned["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|component| component["component_id"] == "session-summary-metadata")
        .unwrap();
    assert_eq!(
        planned_summary["overlap_strategy"]["kind"],
        "idempotent_overlap"
    );
    assert_ne!(
        planned_summary["overlap_strategy"]["kind"],
        declared_summary["overlap_strategy"]
    );

    let release = verified_candidate_release();
    assert_eq!(release.descriptor().support_release_id, SUPPORT_RELEASE_ID);
    assert_eq!(
        release.descriptor().status,
        crate::adapter::SupportReleaseStatus::Candidate
    );
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

fn exact_declared_stream<'a>(
    declaration: &'a serde_json::Value,
    stream_id: &str,
) -> &'a serde_json::Value {
    declaration["streams"]
        .as_array()
        .unwrap()
        .iter()
        .find(|stream| stream["stream_id"] == stream_id)
        .unwrap()
}

fn write_sidecar(root: &Path, encoded_cwd: &str, session_id: &str, name: &str, bytes: &[u8]) {
    let directory = root.join("sessions").join(encoded_cwd).join(session_id);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join(name), bytes).unwrap();
}

fn sized_summary(session_id: &str, cwd: &str, bytes: usize) -> Vec<u8> {
    let prefix = format!(r#"{{"info":{{"id":"{session_id}","cwd":"{cwd}"}},"padding":""#);
    let suffix = r#""}"#;
    assert!(bytes >= prefix.len() + suffix.len());
    format!(
        "{prefix}{}{suffix}",
        "x".repeat(bytes - prefix.len() - suffix.len())
    )
    .into_bytes()
}

fn compatible_contracts() -> (ContractVersionRequest, ContractVersionOffer) {
    (
        ContractVersionRequest {
            selection_contract_version: 1,
            model_major: 1,
            external_entity_reference_version: 1,
            semantic_revision_reference_version: 1,
            coverage_contract_versions: vec![1],
            fact_family_versions: Default::default(),
            query_pack_versions: Some(vec![1]),
            observation_contract_versions: None,
        },
        ContractVersionOffer {
            selection_contract_version: 1,
            model_major: 1,
            external_entity_reference_versions: vec![1],
            semantic_revision_reference_versions: vec![1],
            coverage_contract_versions: vec![1],
            fact_family_versions: Default::default(),
            query_pack_versions: vec![1],
            observation_contract_versions: Vec::new(),
        },
    )
}

fn candidate_probe() -> NativeArtifactProbe {
    NativeArtifactProbe {
        family: ADAPTER_ID.to_string(),
        platform: "darwin".to_string(),
        version: Some("0.5.0".to_string()),
        markers: vec![
            "chat-history.sidecar".to_string(),
            "summary.sidecar".to_string(),
        ],
        contradictory_markers: false,
    }
}

fn verified_candidate_release() -> VerifiedSupportRelease {
    verify_support_release_bundle(
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../agent-support/grok/candidate-2026-08-15/support-release.json"
        )),
        &[
            SupportBundleDocument::new(
                "agent-support/grok/candidate-2026-08-15/ads.json",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../agent-support/grok/candidate-2026-08-15/ads.json"
                )),
            ),
            SupportBundleDocument::new(
                "agent-support/grok/candidate-2026-08-15/source-declarations.json",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../agent-support/grok/candidate-2026-08-15/source-declarations.json"
                )),
            ),
            SupportBundleDocument::new(
                "agent-support/grok/candidate-2026-08-15/scope-programs.json",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../agent-support/grok/candidate-2026-08-15/scope-programs.json"
                )),
            ),
            SupportBundleDocument::new(
                "agent-support/grok/candidate-2026-08-15/evidence.json",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../agent-support/grok/candidate-2026-08-15/evidence.json"
                )),
            ),
            SupportBundleDocument::new(
                "agent-support/grok/candidate-2026-08-15/conformance.json",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../agent-support/grok/candidate-2026-08-15/conformance.json"
                )),
            ),
        ],
    )
    .unwrap()
}
