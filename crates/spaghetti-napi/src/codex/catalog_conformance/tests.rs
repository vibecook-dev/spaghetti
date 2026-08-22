use std::fs;

use tempfile::TempDir;

use super::super::adapter::{normalize_session_meta, CodexSessionMetaError};
use super::*;
use crate::adapter::{
    verify_support_release_bundle, AdapterSupportRegistration, AuthorizedCatalogAccess,
    CompatibilityClass, ContractVersionSelection, Sha256Digest, SupportBundleDocument,
    SupportCatalog, SupportOperation, VerifiedSupportRelease, CONTRACT_VERSION_SELECTION_VERSION,
};
use crate::catalog_contract::CatalogAccessPolicyDigest;
use crate::codex::catalog_runtime::{
    codex_catalog_source_instance, codex_conformance_promoted_composition,
    codex_conformance_source_declaration_bytes, codex_conformance_support_release_bytes,
    codex_conformance_support_release_id, codex_planned_catalog_composition,
    produce_codex_library_coverage, produce_codex_library_coverage_with_post_head_mutation,
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

fn catalog_contract_selection() -> ContractVersionSelection {
    ContractVersionSelection {
        selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
        model_major: 1,
        external_entity_reference_version: 1,
        semantic_revision_reference_version: 1,
        coverage_contract_version: 1,
        fact_family_versions: std::collections::BTreeMap::from([
            ("catalog.project".to_owned(), 1),
            ("catalog.session".to_owned(), 1),
        ]),
        query_pack_version: Some(1),
        observation_contract_version: None,
    }
}

fn synthetic_codex_catalog_access(
    selection: &ContractVersionSelection,
    compatibility: CompatibilityClass,
) -> AuthorizedCatalogAccess<'_> {
    AuthorizedCatalogAccess::fixture_with_compatibility(
        ADAPTER_ID,
        codex_conformance_support_release_id(),
        Sha256Digest::of(codex_conformance_support_release_bytes()),
        Sha256Digest::of(codex_conformance_source_declaration_bytes()),
        selection,
        compatibility,
    )
}

fn produce_catalog_from_root(
    root: &Path,
    discriminator: &[u8],
    compatibility: CompatibilityClass,
    policy: &[u8],
) -> Result<crate::codex::catalog_runtime::CodexCatalogProduction, String> {
    let composition = codex_conformance_promoted_composition().unwrap();
    let selection = catalog_contract_selection();
    let executable = composition
        .authorize_execution(synthetic_codex_catalog_access(&selection, compatibility))
        .map_err(|error| error.to_string())?;
    let instance =
        codex_catalog_source_instance(root, discriminator).map_err(|error| error.to_string())?;
    let bound = executable
        .bind_source_instance(&instance)
        .map_err(|error| error.to_string())?;
    produce_codex_library_coverage(
        &bound,
        CatalogAccessPolicyDigest::derive(1, policy).unwrap(),
    )
    .map_err(|error| error.to_string())
}

fn produce_catalog_fixture(
    compatibility: CompatibilityClass,
    policy: &[u8],
) -> crate::codex::catalog_runtime::CodexCatalogProduction {
    produce_catalog_from_root(
        &fixture_root(),
        FIXTURE_SOURCE_INSTANCE,
        compatibility,
        policy,
    )
    .unwrap()
}

#[test]
fn synthetic_producer_matches_frozen_identity_and_complete_coverage() {
    let report = head_projection(&fixture_root(), 7).unwrap().report();
    let oracle = report.independent_oracle;
    let durable = full_decoder_identities(&fixture_root(), 7).unwrap();
    let produced = produce_catalog_fixture(
        CompatibilityClass::ExactSupported,
        b"fixture-local-catalog-policy",
    );

    assert_eq!(produced.identity.adapter_id, ADAPTER_ID);
    assert_eq!(produced.identity.project_count, oracle.project_count);
    assert_eq!(produced.identity.session_count, oracle.session_count);
    assert_eq!(
        produced.identity.project_identity_digest,
        oracle.project_identity_digest
    );
    assert_eq!(
        produced.identity.session_identity_digest,
        oracle.session_identity_digest
    );
    assert_eq!(
        produced.identity.project_count,
        durable.projects.len() as u64
    );
    assert_eq!(
        produced.identity.session_count,
        durable.sessions.len() as u64
    );
    assert_eq!(
        produced.assembly.source_coverage().completeness,
        crate::adapter::CoverageSetCompleteness::Complete
    );
    assert_eq!(
        produced.assembly.source_coverage().points.len() as u64,
        report.rust_conformance.source_record_count
    );
    assert_ne!(
        produced.assembly.catalog_membership_revision().as_bytes(),
        produced
            .assembly
            .source_coverage()
            .membership_revision
            .as_bytes()
    );
    assert_ne!(
        produced.assembly.catalog_membership_revision().as_bytes(),
        produced.assembly.component_completion_revision().as_bytes()
    );

    let replayed = produce_catalog_fixture(
        CompatibilityClass::ExactSupported,
        b"fixture-local-catalog-policy",
    );
    assert_eq!(produced, replayed);
    let range = produce_catalog_fixture(
        CompatibilityClass::RangeSupported,
        b"fixture-local-catalog-policy",
    );
    assert_eq!(produced.identity, range.identity);
    assert_eq!(
        produced.assembly.catalog_membership_revision(),
        range.assembly.catalog_membership_revision()
    );

    let policy_drift = produce_catalog_fixture(
        CompatibilityClass::ExactSupported,
        b"fixture-other-catalog-policy",
    );
    assert_eq!(produced.identity, policy_drift.identity);
    assert_eq!(
        produced.assembly.catalog_membership_revision(),
        policy_drift.assembly.catalog_membership_revision()
    );
    assert_ne!(
        produced.assembly.component_completion_revision(),
        policy_drift.assembly.component_completion_revision()
    );

    let another_instance = produce_catalog_from_root(
        &fixture_root(),
        b"another-canonical-source-instance",
        CompatibilityClass::ExactSupported,
        b"fixture-local-catalog-policy",
    )
    .unwrap();
    assert_eq!(produced.identity, another_instance.identity);
    assert_ne!(
        produced.assembly.catalog_membership_revision(),
        another_instance.assembly.catalog_membership_revision()
    );

    let publication = produced.assembly.complete_publication_source().unwrap();
    assert_eq!(
        publication.member_count(),
        produced.identity.session_count as usize
    );
    assert_eq!(publication.plan_source(), produced.assembly.plan_source());
    assert_eq!(
        publication.source_coverage(),
        produced.assembly.source_coverage()
    );

    let debug = format!("{produced:?}");
    assert!(!debug.contains("/Users/"));
    assert!(!debug.contains("/Volumes/"));
    assert!(!debug.contains("small-codex"));
}

#[test]
fn planned_composition_cannot_authorize_synthetic_producer() {
    let selection = catalog_contract_selection();
    let planned = codex_planned_catalog_composition().unwrap();
    assert!(planned
        .authorize_execution(synthetic_codex_catalog_access(
            &selection,
            CompatibilityClass::ExactSupported,
        ))
        .is_err());

    let promoted = codex_conformance_promoted_composition().unwrap();
    let executable = promoted
        .authorize_execution(synthetic_codex_catalog_access(
            &selection,
            CompatibilityClass::ExactSupported,
        ))
        .unwrap();
    assert_ne!(planned.composition_id(), executable.composition_id());
    assert_ne!(
        planned.support_release_id(),
        executable.composition().support_release_id()
    );
    assert_ne!(
        executable.composition().support_release_id(),
        SUPPORT_RELEASE_ID
    );
}

#[test]
fn producer_rejects_composition_drift_before_source_access() {
    let reviewed = codex_conformance_promoted_composition().unwrap();
    let mut components = reviewed.components().to_vec();
    components[0].disposition_ownership = vec!["native-family:drifted".to_owned()];
    let drifted = crate::source::catalog_composition::CatalogSourceComposition::new_promoted(
        ADAPTER_ID,
        reviewed.support_release_id(),
        reviewed.source_declaration_id(),
        reviewed.promoted_binding().unwrap(),
        components,
    )
    .unwrap();
    let selection = catalog_contract_selection();
    let executable = drifted
        .authorize_execution(synthetic_codex_catalog_access(
            &selection,
            CompatibilityClass::ExactSupported,
        ))
        .unwrap();
    let absent = TempDir::new().unwrap();
    let instance = codex_catalog_source_instance(absent.path(), FIXTURE_SOURCE_INSTANCE).unwrap();
    let bound = executable.bind_source_instance(&instance).unwrap();
    let error = produce_codex_library_coverage(
        &bound,
        CatalogAccessPolicyDigest::derive(1, b"fixture-local-catalog-policy").unwrap(),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("exact synthetic conformance composition"));
    assert!(!error.contains("failed to read"));
    assert!(!error.contains(absent.path().to_string_lossy().as_ref()));
}

fn produce_with_post_head_mutation(
    root: &Path,
    mutate: impl FnOnce(&Path),
) -> Result<crate::codex::catalog_runtime::CodexCatalogProduction, String> {
    let composition = codex_conformance_promoted_composition().unwrap();
    let selection = catalog_contract_selection();
    let executable = composition
        .authorize_execution(synthetic_codex_catalog_access(
            &selection,
            CompatibilityClass::ExactSupported,
        ))
        .unwrap();
    let instance = codex_catalog_source_instance(root, FIXTURE_SOURCE_INSTANCE)
        .map_err(|error| error.to_string())?;
    let bound = executable
        .bind_source_instance(&instance)
        .map_err(|error| error.to_string())?;
    produce_codex_library_coverage_with_post_head_mutation(
        &bound,
        CatalogAccessPolicyDigest::derive(1, b"fixture-local-catalog-policy").unwrap(),
        mutate,
    )
    .map_err(|error| error.to_string())
}

fn session_meta(id: &str, cwd: &str) -> String {
    serde_json::json!({
        "type": "session_meta",
        "payload": {"id": id, "cwd": cwd}
    })
    .to_string()
}

#[test]
fn producer_fails_closed_when_membership_or_heads_change_during_production() {
    let membership = TempDir::new().unwrap();
    write_rollout(
        membership.path(),
        "rollout-first.jsonl",
        &session_meta("first", "/sanitized/project"),
    );
    let membership_error = produce_with_post_head_mutation(membership.path(), |sessions_root| {
        let directory = sessions_root.join("2026/01/01");
        fs::write(
            directory.join("rollout-late.jsonl"),
            format!("{}\n", session_meta("late", "/sanitized/project")),
        )
        .unwrap();
    })
    .unwrap_err();
    assert!(membership_error.contains("membership changed during head revalidation"));
    assert!(!membership_error.contains("rollout-late"));
    assert!(!membership_error.contains("/sanitized/"));

    let head = TempDir::new().unwrap();
    write_rollout(
        head.path(),
        "rollout-first.jsonl",
        &session_meta("first", "/sanitized/project"),
    );
    let head_error = produce_with_post_head_mutation(head.path(), |sessions_root| {
        fs::write(
            sessions_root.join("2026/01/01/rollout-first.jsonl"),
            format!("{}\n", session_meta("changed", "/sanitized/project")),
        )
        .unwrap();
    })
    .unwrap_err();
    assert!(head_error.contains("head changed during revalidation"));
    assert!(!head_error.contains("rollout-first"));
    assert!(!head_error.contains("/sanitized/"));
}

#[test]
fn producer_rejects_competing_identity_and_oversized_head_without_path_leakage() {
    let competing = TempDir::new().unwrap();
    write_rollout(
        competing.path(),
        "rollout-first.jsonl",
        &session_meta("shared", "/sanitized/one"),
    );
    write_rollout(
        competing.path(),
        "rollout-second.jsonl",
        &session_meta("shared", "/sanitized/two"),
    );
    let competing_error = produce_catalog_from_root(
        competing.path(),
        FIXTURE_SOURCE_INSTANCE,
        CompatibilityClass::ExactSupported,
        b"fixture-local-catalog-policy",
    )
    .unwrap_err();
    assert!(competing_error.contains("competing projects"));
    assert!(!competing_error.contains("/sanitized/"));
    assert!(!competing_error.contains("rollout-"));

    let oversized = TempDir::new().unwrap();
    write_rollout(
        oversized.path(),
        "rollout-secret.jsonl",
        &sized_session_meta(CANDIDATE_HEAD_RECORD_PAYLOAD_BYTES + 1),
    );
    let oversized_error = produce_catalog_from_root(
        oversized.path(),
        FIXTURE_SOURCE_INSTANCE,
        CompatibilityClass::ExactSupported,
        b"fixture-local-catalog-policy",
    )
    .unwrap_err();
    assert!(
        oversized_error.contains("declared source bound")
            || oversized_error.contains("declared record bound")
    );
    assert!(!oversized_error.contains("rollout-secret"));
    assert!(!oversized_error.contains("/Users/"));
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
