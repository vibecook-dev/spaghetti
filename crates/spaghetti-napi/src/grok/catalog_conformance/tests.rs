use std::fs;

use tempfile::TempDir;

use super::*;
use crate::adapter::{
    verify_support_release_bundle, AdapterSupportRegistration, AuthorizedCatalogAccess,
    CompatibilityClass, ContractVersionOffer, ContractVersionRequest, ContractVersionSelection,
    NativeArtifactProbe, Sha256Digest, SupportBundleDocument, SupportCatalog, SupportOperation,
    VerifiedSupportRelease, CONTRACT_VERSION_SELECTION_VERSION,
};
use crate::catalog_contract::CatalogAccessPolicyDigest;
use crate::grok::catalog_runtime::{
    grok_catalog_source_instance, grok_conformance_promoted_composition,
    grok_conformance_source_declaration_bytes, grok_conformance_support_release_bytes,
    grok_conformance_support_release_id, grok_planned_catalog_composition,
    produce_grok_library_coverage, produce_grok_library_coverage_with_post_summary_mutation,
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
fn summary_catalog_oracle_uses_the_common_decode_boundary() {
    let source = include_str!("../catalog_conformance.rs");
    let direct_decode = [".de", "code("].concat();
    let direct_batch = ["FactBatch::", "new("].concat();
    assert!(source.contains("decode_record(DecodeRuntimeRequest"));
    assert!(!source.contains(&direct_decode));
    assert!(!source.contains(&direct_batch));
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
        serde_json::json!([
            "**/chat_history.jsonl",
            "**/events.jsonl",
            "**/signals.json",
            "**/summary.json"
        ])
    );
    assert_eq!(
        planned_membership["discovery_bounds"]["max_entries"],
        CANDIDATE_MEMBERSHIP_MAX_ENTRIES
    );
    assert_eq!(
        planned_membership["discovery_bounds"]["max_depth"],
        CANDIDATE_MEMBERSHIP_MAX_DEPTH
    );
    assert_eq!(
        planned_membership["overlap_strategy"]["kind"],
        "disjoint_catalog_family"
    );
    assert_eq!(
        planned_membership["overlap_strategy"]["ownership_contract_id"],
        "grok-session-membership-catalog-family-v1"
    );
    let planned_selectors = planned_membership["relative_selectors"].as_array().unwrap();
    let declared_patterns = declared_membership["relative_patterns"].as_array().unwrap();
    assert_eq!(planned_selectors.len(), declared_patterns.len());
    assert!(planned_selectors
        .iter()
        .all(|selector| declared_patterns.contains(selector)));
    let planned_summary = planned["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|component| component["component_id"] == "session-summary-metadata")
        .unwrap();
    assert_eq!(
        planned_summary["relative_selectors"],
        serde_json::json!(["**/summary.json"])
    );
    assert_eq!(
        planned_summary["discovery_bounds"],
        serde_json::json!({
            "max_entries": CANDIDATE_MEMBERSHIP_MAX_ENTRIES,
            "max_depth": CANDIDATE_MEMBERSHIP_MAX_DEPTH
        })
    );
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

fn catalog_contract_selection() -> ContractVersionSelection {
    ContractVersionSelection {
        selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
        model_major: 1,
        external_entity_reference_version: 1,
        semantic_revision_reference_version: 1,
        coverage_contract_version: 1,
        fact_family_versions: BTreeMap::from([
            ("catalog.project".to_owned(), 1),
            ("catalog.session".to_owned(), 1),
        ]),
        query_pack_version: Some(1),
        observation_contract_version: None,
    }
}

fn synthetic_grok_catalog_access(
    selection: &ContractVersionSelection,
    compatibility: CompatibilityClass,
) -> AuthorizedCatalogAccess<'_> {
    AuthorizedCatalogAccess::fixture_with_source_contracts(
        ADAPTER_ID,
        grok_conformance_support_release_id(),
        Sha256Digest::of(grok_conformance_support_release_bytes()),
        Sha256Digest::of(grok_conformance_source_declaration_bytes()),
        selection,
        compatibility,
        verified_candidate_release().source_contracts().clone(),
    )
}

fn produce_catalog_from_root(
    root: &Path,
    discriminator: &[u8],
    compatibility: CompatibilityClass,
    policy: &[u8],
) -> Result<crate::grok::catalog_runtime::GrokCatalogProduction, String> {
    let composition = grok_conformance_promoted_composition().unwrap();
    let selection = catalog_contract_selection();
    let executable = composition
        .authorize_execution(synthetic_grok_catalog_access(&selection, compatibility))
        .map_err(|error| error.to_string())?;
    let instance =
        grok_catalog_source_instance(root, discriminator).map_err(|error| error.to_string())?;
    let bound = executable
        .bind_source_instance(&instance)
        .map_err(|error| error.to_string())?;
    produce_grok_library_coverage(
        &bound,
        CatalogAccessPolicyDigest::derive(1, policy).unwrap(),
    )
    .map_err(|error| error.to_string())
}

fn produce_catalog_fixture(
    compatibility: CompatibilityClass,
    policy: &[u8],
) -> crate::grok::catalog_runtime::GrokCatalogProduction {
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
    let report = candidate_projection(&fixture_root(), 7).unwrap().report();
    let produced = produce_catalog_fixture(
        CompatibilityClass::ExactSupported,
        b"fixture-local-catalog-policy",
    );

    assert_eq!(produced.identity.adapter_id, ADAPTER_ID);
    assert_eq!(
        produced.identity.project_count,
        report.independent_oracle.project_count
    );
    assert_eq!(
        produced.identity.session_count,
        report.independent_oracle.session_count
    );
    assert_eq!(
        produced.identity.project_identity_digest,
        report.independent_oracle.project_identity_digest
    );
    assert_eq!(
        produced.identity.session_identity_digest,
        report.independent_oracle.session_identity_digest
    );
    assert_eq!(
        produced.assembly.source_coverage().completeness,
        crate::adapter::CoverageSetCompleteness::Complete
    );
    assert_eq!(
        produced.assembly.source_coverage().points.len() as u64,
        report.rust_conformance.admitted_membership_object_count
            + report.rust_conformance.summary_source_record_count
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
        report.rust_conformance.member_count as usize
    );
    assert_eq!(publication.plan_source(), produced.assembly.plan_source());
    assert_eq!(
        publication.source_coverage(),
        produced.assembly.source_coverage()
    );

    let debug = format!("{produced:?}");
    assert!(!debug.contains("/Users/"));
    assert!(!debug.contains("/Volumes/"));
    assert!(!debug.contains("small-grok"));
}

#[test]
fn planned_composition_cannot_authorize_synthetic_producer() {
    let selection = catalog_contract_selection();
    let planned = grok_planned_catalog_composition().unwrap();
    assert!(planned
        .authorize_execution(synthetic_grok_catalog_access(
            &selection,
            CompatibilityClass::ExactSupported,
        ))
        .is_err());

    let promoted = grok_conformance_promoted_composition().unwrap();
    let executable = promoted
        .authorize_execution(synthetic_grok_catalog_access(
            &selection,
            CompatibilityClass::ExactSupported,
        ))
        .unwrap();
    assert_ne!(planned.composition_id(), executable.composition_id());
    assert_ne!(
        executable.composition().support_release_id(),
        SUPPORT_RELEASE_ID
    );
}

#[test]
fn producer_rejects_composition_drift_before_source_access() {
    let reviewed = grok_conformance_promoted_composition().unwrap();
    let selection = catalog_contract_selection();

    let mut source_drift = reviewed.components().to_vec();
    source_drift[0].source_stream_id = "unverified-membership-stream".to_owned();
    let source_drift = crate::source::catalog_composition::CatalogSourceComposition::new_promoted(
        ADAPTER_ID,
        reviewed.support_release_id(),
        reviewed.source_declaration_id(),
        reviewed.promoted_binding().unwrap(),
        source_drift,
    )
    .unwrap();
    let error = match source_drift.authorize_execution(synthetic_grok_catalog_access(
        &selection,
        CompatibilityClass::ExactSupported,
    )) {
        Ok(_) => panic!("unverified catalog source stream authorized execution"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("digest-verified source stream"));

    let mut selector_drift = reviewed.components().to_vec();
    selector_drift[0].relative_selectors = vec!["**/unreviewed.json".to_owned()];
    let selector_drift =
        crate::source::catalog_composition::CatalogSourceComposition::new_promoted(
            ADAPTER_ID,
            reviewed.support_release_id(),
            reviewed.source_declaration_id(),
            reviewed.promoted_binding().unwrap(),
            selector_drift,
        )
        .unwrap();
    let error = match selector_drift.authorize_execution(synthetic_grok_catalog_access(
        &selection,
        CompatibilityClass::ExactSupported,
    )) {
        Ok(_) => panic!("unreviewed catalog selector authorized execution"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("digest-verified source stream"));

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
    let executable = drifted
        .authorize_execution(synthetic_grok_catalog_access(
            &selection,
            CompatibilityClass::ExactSupported,
        ))
        .unwrap();
    let absent = TempDir::new().unwrap();
    let instance = grok_catalog_source_instance(absent.path(), FIXTURE_SOURCE_INSTANCE).unwrap();
    let bound = executable.bind_source_instance(&instance).unwrap();
    let error = produce_grok_library_coverage(
        &bound,
        CatalogAccessPolicyDigest::derive(1, b"fixture-local-catalog-policy").unwrap(),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("exact synthetic conformance composition"));
    assert!(!error.contains("failed to read"));
    assert!(!error.contains(absent.path().to_string_lossy().as_ref()));
}

fn produce_with_post_summary_mutation(
    root: &Path,
    mutate: impl FnOnce(&Path),
) -> Result<crate::grok::catalog_runtime::GrokCatalogProduction, String> {
    let composition = grok_conformance_promoted_composition().unwrap();
    let selection = catalog_contract_selection();
    let executable = composition
        .authorize_execution(synthetic_grok_catalog_access(
            &selection,
            CompatibilityClass::ExactSupported,
        ))
        .unwrap();
    let instance = grok_catalog_source_instance(root, FIXTURE_SOURCE_INSTANCE)
        .map_err(|error| error.to_string())?;
    let bound = executable
        .bind_source_instance(&instance)
        .map_err(|error| error.to_string())?;
    produce_grok_library_coverage_with_post_summary_mutation(
        &bound,
        CatalogAccessPolicyDigest::derive(1, b"fixture-local-catalog-policy").unwrap(),
        mutate,
    )
    .map_err(|error| error.to_string())
}

fn summary(session_id: &str, cwd: &str, title: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "info": {"id": session_id, "cwd": cwd},
        "generated_title": title
    }))
    .unwrap()
}

#[test]
fn producer_fails_closed_when_membership_or_summary_changes_during_production() {
    let membership = TempDir::new().unwrap();
    let membership_root = membership.path().join(".grok");
    write_sidecar(
        &membership_root,
        "%2Ftmp%2Fproject",
        "first",
        "summary.json",
        &summary("first", "/tmp/project", "first"),
    );
    let membership_error = produce_with_post_summary_mutation(&membership_root, |sessions_root| {
        let directory = sessions_root.join("%2Ftmp%2Fproject/late");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("chat_history.jsonl"), b"").unwrap();
    })
    .unwrap_err();
    assert!(membership_error.contains("membership authority changed"));
    assert!(!membership_error.contains("chat_history"));
    assert!(!membership_error.contains("/tmp/project"));

    let summary_change = TempDir::new().unwrap();
    let summary_root = summary_change.path().join(".grok");
    write_sidecar(
        &summary_root,
        "%2Ftmp%2Fproject",
        "first",
        "summary.json",
        &summary("first", "/tmp/project", "first"),
    );
    let summary_error = produce_with_post_summary_mutation(&summary_root, |sessions_root| {
        fs::write(
            sessions_root.join("%2Ftmp%2Fproject/first/summary.json"),
            summary("first", "/tmp/project", "changed"),
        )
        .unwrap();
    })
    .unwrap_err();
    assert!(summary_error.contains("summary driver revision changed"));
    assert!(!summary_error.contains("summary.json"));
    assert!(!summary_error.contains("/tmp/project"));
}

#[test]
fn producer_rejects_invalid_summary_and_keeps_updates_only_non_admitting() {
    let updates = TempDir::new().unwrap();
    let updates_root = updates.path().join(".grok");
    write_sidecar(
        &updates_root,
        "%2Ftmp%2Fproject",
        "updates-only",
        "updates.jsonl",
        b"{}\n",
    );
    let empty = produce_catalog_from_root(
        &updates_root,
        FIXTURE_SOURCE_INSTANCE,
        CompatibilityClass::ExactSupported,
        b"fixture-local-catalog-policy",
    )
    .unwrap();
    assert_eq!(empty.identity.session_count, 0);
    assert_eq!(
        empty.assembly.source_coverage().completeness,
        crate::adapter::CoverageSetCompleteness::Complete
    );
    assert_eq!(
        empty
            .assembly
            .complete_publication_source()
            .unwrap()
            .member_count(),
        0
    );

    let malformed = TempDir::new().unwrap();
    let malformed_root = malformed.path().join(".grok");
    write_sidecar(
        &malformed_root,
        "%2Ftmp%2Fproject",
        "malformed",
        "summary.json",
        b"{",
    );
    let malformed_error = produce_catalog_from_root(
        &malformed_root,
        FIXTURE_SOURCE_INSTANCE,
        CompatibilityClass::ExactSupported,
        b"fixture-local-catalog-policy",
    )
    .unwrap_err();
    assert!(malformed_error.contains("summary JSON is invalid"));
    assert!(!malformed_error.contains("summary.json"));

    let oversized = TempDir::new().unwrap();
    let oversized_root = oversized.path().join(".grok");
    write_sidecar(
        &oversized_root,
        "%2Ftmp%2Fproject",
        "oversized",
        "summary.json",
        &sized_summary("oversized", "/tmp/project", CANDIDATE_SUMMARY_MAX_BYTES + 1),
    );
    let oversized_error = produce_catalog_from_root(
        &oversized_root,
        FIXTURE_SOURCE_INSTANCE,
        CompatibilityClass::ExactSupported,
        b"fixture-local-catalog-policy",
    )
    .unwrap_err();
    assert!(oversized_error.contains("not completely readable"));
    assert!(!oversized_error.contains("summary.json"));

    let retarget = TempDir::new().unwrap();
    let retarget_root = retarget.path().join(".grok");
    write_sidecar(
        &retarget_root,
        "%2Ftmp%2Fproject",
        "path-session",
        "summary.json",
        &summary("different-session", "/tmp/different", "retarget"),
    );
    let retarget_error = produce_catalog_from_root(
        &retarget_root,
        FIXTURE_SOURCE_INSTANCE,
        CompatibilityClass::ExactSupported,
        b"fixture-local-catalog-policy",
    )
    .unwrap_err();
    assert!(retarget_error.contains("identity disagrees"));
    assert!(!retarget_error.contains("different-session"));
    assert!(!retarget_error.contains("/tmp/"));
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
