use std::fs;
use std::path::Path;

use tempfile::TempDir;

use super::*;
use crate::adapter::{
    verify_support_release_bundle, AdapterSupportRegistration, AuthorizedCatalogAccess,
    CompatibilityClass, ContractVersionOffer, ContractVersionRequest, ContractVersionSelection,
    NativeArtifactProbe, Sha256Digest, SupportBundleDocument, SupportCatalog, SupportContractError,
    SupportOperation, VerifiedSupportRelease, CONTRACT_VERSION_SELECTION_VERSION,
};
use crate::catalog_contract::CatalogAccessPolicyDigest;
use crate::claude::catalog_runtime::{
    claude_catalog_components, claude_catalog_source_instance,
    claude_conformance_promoted_composition, claude_conformance_source_declaration_bytes,
    claude_conformance_source_declaration_id, claude_conformance_support_release_bytes,
    claude_conformance_support_release_id, claude_planned_catalog_composition,
    produce_claude_library_coverage, produce_claude_library_coverage_with_post_head_mutation,
};
use crate::source::catalog_composition::{
    CatalogContribution, CatalogDecoderStateBoundary, CatalogDiscoveryBounds,
    CatalogOverlapStrategy, CatalogPromotedBinding, CatalogSourceComposition,
    CatalogSourcePrimitive,
};

const INDEX_ONLY: &str = "11111111-1111-1111-1111-111111111111";
const TOP_ONLY: &str = "22222222-2222-2222-2222-222222222222";
const NESTED_ONLY: &str = "33333333-3333-3333-3333-333333333333";
const OVERLAP: &str = "44444444-4444-4444-4444-444444444444";
const FROZEN_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/contracts/rfc012b-claude-candidate-conformance-v1.json"
));

#[test]
fn complete_membership_and_bounded_heads_match_census_and_durable_identity() {
    let projection = candidate_projection(&fixture_root(), 7).unwrap();
    let durable = durable_decoder_identities(&fixture_root(), 7).unwrap();
    assert_eq!(projection.projects, durable.projects);
    assert_eq!(projection.sessions, durable.sessions);

    let actual = projection.report();
    let expected: ClaudeCandidateConformanceFixture = serde_json::from_str(FROZEN_FIXTURE)
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

    assert!(!FROZEN_FIXTURE.contains("/Users/"));
    assert!(!FROZEN_FIXTURE.contains("/home/"));
    assert!(!FROZEN_FIXTURE.contains("03ddf851"));
}

#[test]
fn candidate_identities_ignore_runtime_registration_ids() {
    let first = candidate_projection(&fixture_root(), 7).unwrap().report();
    let reordered = candidate_projection(&fixture_root(), 70_007)
        .unwrap()
        .report();
    assert_eq!(first, reordered);

    assert_eq!(
        durable_decoder_identities(&fixture_root(), 7).unwrap(),
        durable_decoder_identities(&fixture_root(), 70_007).unwrap()
    );
}

#[test]
fn union_admits_index_top_and_nested_only_members_without_fabrication() {
    let fixture = TempDir::new().unwrap();
    let root = fixture.path().join(".claude");
    let project = root.join("projects/-tmp-project");
    fs::create_dir_all(&project).unwrap();
    write_index(
        &project,
        &[(INDEX_ONLY, "/tmp/project"), (OVERLAP, "/tmp/project")],
    );
    write_parent(
        &project,
        TOP_ONLY,
        &user_record(TOP_ONLY, "/tmp/project", "top"),
    );
    write_parent(
        &project,
        OVERLAP,
        &user_record(OVERLAP, "/tmp/project", "overlap"),
    );
    write_nested(
        &project,
        NESTED_ONLY,
        &user_record(NESTED_ONLY, "/tmp/project", "nested"),
    );

    let projection = candidate_projection(&root, 1).unwrap();
    assert_eq!(projection.sessions.len(), 4);
    assert_eq!(
        projection.member_evidence[&(String::from("-tmp-project"), String::from(INDEX_ONLY))],
        INDEX_EVIDENCE
    );
    assert_eq!(
        projection.member_evidence[&(String::from("-tmp-project"), String::from(TOP_ONLY))],
        TOP_LEVEL_EVIDENCE
    );
    assert_eq!(
        projection.member_evidence[&(String::from("-tmp-project"), String::from(NESTED_ONLY))],
        NESTED_EVIDENCE
    );
    assert_eq!(
        projection.member_evidence[&(String::from("-tmp-project"), String::from(OVERLAP))],
        INDEX_EVIDENCE | TOP_LEVEL_EVIDENCE
    );
    assert_eq!(projection.transcript_head_objects, 1);
    assert_eq!(projection.member_metadata_unavailable_count(), 1);

    let durable = durable_decoder_identities(&root, 1).unwrap();
    assert_eq!(projection.projects, durable.projects);
    assert_eq!(projection.sessions, durable.sessions);
}

#[test]
fn authoritative_index_failure_aborts_complete_projection() {
    let malformed = TempDir::new().unwrap();
    let malformed_root = malformed.path().join(".claude");
    let malformed_project = malformed_root.join("projects/-tmp-project");
    fs::create_dir_all(&malformed_project).unwrap();
    fs::write(malformed_project.join("sessions-index.json"), b"{").unwrap();
    let error = match candidate_projection(&malformed_root, 1) {
        Ok(_) => panic!("malformed authoritative index must fail closed"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("did not decode completely"));

    let oversized = TempDir::new().unwrap();
    let oversized_root = oversized.path().join(".claude");
    let oversized_project = oversized_root.join("projects/-tmp-project");
    fs::create_dir_all(&oversized_project).unwrap();
    fs::write(
        oversized_project.join("sessions-index.json"),
        vec![b'x'; INDEX_MAX_DOCUMENT_BYTES + 1],
    )
    .unwrap();
    let error = match candidate_projection(&oversized_root, 1) {
        Ok(_) => panic!("oversized authoritative index must fail closed"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("exceeds its declared bound"));
}

#[test]
fn malformed_and_oversized_heads_retain_members_as_metadata_unavailable() {
    let fixture = TempDir::new().unwrap();
    let root = fixture.path().join(".claude");
    let project = root.join("projects/-tmp-project");
    fs::create_dir_all(&project).unwrap();
    write_parent(&project, TOP_ONLY, "{");
    let oversized = "55555555-5555-5555-5555-555555555555";
    fs::write(
        project.join(format!("{oversized}.jsonl")),
        vec![b'x'; HEAD_FRAMING_READ_AHEAD_BYTES * 3],
    )
    .unwrap();

    let projection = candidate_projection(&root, 1).unwrap();
    assert_eq!(projection.sessions.len(), 2);
    assert_eq!(projection.transcript_head_objects, 2);
    assert_eq!(projection.member_metadata_unavailable_count(), 2);
    assert!(projection
        .member_evidence
        .contains_key(&("-tmp-project".to_string(), oversized.to_string())));
}

#[test]
fn exact_64k_payload_stops_within_explicit_132k_candidate_ceiling() {
    let fixture = TempDir::new().unwrap();
    let root = fixture.path().join(".claude");
    let project = root.join("projects/-tmp-project");
    fs::create_dir_all(&project).unwrap();
    let mut bytes =
        sized_user_record(TOP_ONLY, "/tmp/project", HEAD_RECORD_PAYLOAD_BYTES).into_bytes();
    assert_eq!(bytes.len(), 65_536);
    bytes.push(b'\n');
    bytes.extend(std::iter::repeat_n(b'x', HEAD_FRAMING_READ_AHEAD_BYTES * 2));
    fs::write(project.join(format!("{TOP_ONLY}.jsonl")), bytes).unwrap();

    let projection = candidate_projection(&root, 1).unwrap();
    assert_eq!(projection.sessions.len(), 1);
    assert_eq!(projection.transcript_head_records, 1);
    assert_eq!(
        projection.transcript_head_payload_bytes,
        HEAD_RECORD_PAYLOAD_BYTES as u64
    );
    assert_eq!(
        projection.transcript_head_physical_bytes,
        HEAD_PHYSICAL_READ_CEILING
    );
    assert_eq!(projection.member_metadata_unavailable_count(), 0);
}

#[test]
fn blank_index_display_metadata_requires_the_bounded_head_fallback() {
    // The current native decoder already fails closed on blank required index
    // fields. Keep the projection guard executable as well so a future decoder
    // that preserves a blank optional/display field cannot suppress fallback.
    assert!(!CandidateMetadata {
        cwd: Some("/tmp/project".to_string()),
        first_prompt: Some("   ".to_string()),
        title: None,
        created_at: Some("2026-01-01T00:00:00Z".to_string()),
    }
    .complete());

    let fixture = TempDir::new().unwrap();
    let root = fixture.path().join(".claude");
    let project = root.join("projects/-tmp-project");
    fs::create_dir_all(&project).unwrap();
    write_parent(
        &project,
        TOP_ONLY,
        &user_record(TOP_ONLY, "/tmp/project", "head prompt"),
    );

    let projection = candidate_projection(&root, 1).unwrap();
    assert_eq!(projection.transcript_head_objects, 1);
    assert_eq!(projection.member_metadata_unavailable_count(), 0);
    assert_eq!(
        projection
            .metadata
            .get(&("-tmp-project".to_string(), TOP_ONLY.to_string()))
            .unwrap()
            .first_prompt
            .as_deref(),
        Some("head prompt")
    );
}

#[test]
fn association_digest_binds_evidence_occurrence_but_not_registration_ids() {
    let fixture = TempDir::new().unwrap();
    let root = fixture.path().join(".claude");
    let project = root.join("projects/-tmp-project");
    fs::create_dir_all(&project).unwrap();
    write_index(&project, &[(INDEX_ONLY, "/tmp/project")]);

    let first = candidate_projection(&root, 1).unwrap().report();
    let reordered = candidate_projection(&root, 10_001).unwrap().report();
    assert_eq!(
        first.rust_conformance.association_evidence_digest,
        reordered.rust_conformance.association_evidence_digest
    );

    rewrite_index_first_prompt(&project, "other prompt with the same member identity");
    let changed = candidate_projection(&root, 1).unwrap().report();
    assert_eq!(first.independent_oracle, changed.independent_oracle);
    assert_eq!(
        first.rust_conformance.association_evidence_count,
        changed.rust_conformance.association_evidence_count
    );
    assert_ne!(
        first.rust_conformance.association_evidence_digest,
        changed.rust_conformance.association_evidence_digest
    );
}

#[test]
fn association_drift_is_retained_as_conflict_without_retargeting_membership() {
    let fixture = TempDir::new().unwrap();
    let root = fixture.path().join(".claude");
    let project = root.join("projects/-tmp-project");
    fs::create_dir_all(&project).unwrap();
    write_index(&project, &[(INDEX_ONLY, "/different/index-project")]);
    write_parent(
        &project,
        TOP_ONLY,
        &user_record(TOP_ONLY, "/different/transcript-project", "prompt"),
    );

    let projection = candidate_projection(&root, 1).unwrap();
    assert_eq!(projection.projects.len(), 1);
    assert!(projection
        .projects
        .contains(&(ADAPTER_ID.to_string(), "-tmp-project".to_string())));
    assert_eq!(projection.sessions.len(), 2);
    assert_eq!(projection.association_conflicts.len(), 2);
    assert!(projection
        .sessions
        .iter()
        .all(|(_, project, _)| project == "-tmp-project"));
}

#[test]
fn current_candidate_and_planned_composition_remain_non_authorizing_and_distinct() {
    let release_wire: serde_json::Value = serde_json::from_slice(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../agent-support/claude-code/candidate-2026-08-15/support-release.json"
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
    assert_eq!(catalog_capability["level"], "unsupported");

    let declaration_bytes = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../agent-support/claude-code/candidate-2026-08-15/source-declarations.json"
    ));
    let declaration: serde_json::Value = serde_json::from_slice(declaration_bytes).unwrap();
    let declaration_digest = Sha256Digest::of(declaration_bytes).to_string();
    assert_eq!(
        release_wire["references"]["source_declaration"]["sha256"],
        declaration_digest
    );
    assert_eq!(declaration["declaration_id"], SOURCE_DECLARATION_ID);
    assert_eq!(declaration["status"], "candidate");
    let declared_parent = exact_declared_stream(&declaration, PARENT_STREAM_ID);
    let declared_subagent = exact_declared_stream(&declaration, SUBAGENT_STREAM_ID);
    let declared_index = exact_declared_stream(&declaration, INDEX_STREAM_ID);
    for (stream, decoder) in [
        (declared_parent, "claude-session-record"),
        (declared_subagent, "claude-subagent-record"),
    ] {
        assert_eq!(stream["primitive"], "AppendDelimited");
        assert_eq!(stream["decoder_id"], decoder);
        assert_eq!(
            stream["bounds"],
            serde_json::json!({
                "max_record_bytes": 4 * 1024 * 1024,
                "max_batch_bytes": 8 * 1024 * 1024,
                "max_records_per_batch": 1024,
                "max_entries": 250_000,
                "max_depth": 64
            })
        );
        assert_eq!(stream["overlap_strategy"], "full_only");
    }
    assert_eq!(declared_index["primitive"], "ReplaceDocument");
    assert_eq!(declared_index["decoder_id"], "claude-session-index");
    assert_eq!(
        declared_index["bounds"],
        serde_json::json!({
            "max_object_bytes": INDEX_MAX_DOCUMENT_BYTES,
            "max_entries": 250_000,
            "max_depth": 64
        })
    );
    assert_eq!(declared_index["overlap_strategy"], "full_only");

    let adapter = ClaudeCodeAdapter::new();
    let instance = fixture_instance(&fixture_root(), 7).unwrap();
    let streams = adapter.streams(&instance).unwrap();
    let runtime_parent = exact_stream(&streams, PARENT_STREAM_ID).unwrap();
    let runtime_subagent = exact_stream(&streams, SUBAGENT_STREAM_ID).unwrap();
    let runtime_index = exact_stream(&streams, INDEX_STREAM_ID).unwrap();
    assert_eq!(runtime_parent.decoder.as_str(), "claude-session-record");
    assert_eq!(runtime_subagent.decoder.as_str(), "claude-subagent-record");
    assert_eq!(runtime_index.decoder.as_str(), "claude-session-index");
    assert_eq!(
        declared_parent["decoder_id"].as_str().unwrap(),
        runtime_parent.decoder.as_str()
    );
    assert_eq!(
        declared_subagent["decoder_id"].as_str().unwrap(),
        runtime_subagent.decoder.as_str()
    );

    let composition: serde_json::Value = serde_json::from_slice(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fixtures/contracts/rfc012b-catalog-compositions-v1.json"
    )))
    .unwrap();
    let planned = composition["compositions"]["claude_code"]
        .as_object()
        .unwrap();
    assert_eq!(planned["binding"]["status"], "planned_unbound");
    assert_eq!(planned["support_release_id"], PLANNED_SUPPORT_RELEASE_ID);
    assert_eq!(
        planned["source_declaration_id"],
        PLANNED_SOURCE_DECLARATION_ID
    );
    assert_ne!(
        planned["support_release_id"],
        release_wire["support_release_id"]
    );
    assert_ne!(
        planned["source_declaration_id"],
        declaration["declaration_id"]
    );
    let planned_head = exact_planned_component(planned, "transcript-head-fallback");
    assert_eq!(
        planned_head["relative_selectors"],
        serde_json::json!(["*/*.jsonl"])
    );
    assert_eq!(
        planned_head["discovery_bounds"],
        serde_json::json!({"max_entries": 250_000, "max_depth": 64})
    );
    assert_eq!(planned_head["primitive"]["max_record_bytes"], 65_536);
    assert_eq!(planned_head["primitive"]["max_window_bytes"], 65_536);
    assert_eq!(planned_head["primitive"]["max_records"], 128);
    assert_eq!(
        planned_head["decoder_contract_id"].as_str().unwrap(),
        declared_parent["decoder_id"].as_str().unwrap()
    );
    assert_eq!(
        planned_head["decoder_contract_id"].as_str().unwrap(),
        runtime_parent.decoder.as_str()
    );
    assert_eq!(
        planned_head["overlap_strategy"]["kind"],
        "idempotent_overlap"
    );

    for (component_id, selector, primitive) in [
        (
            "nested-parent-membership",
            "*/*/subagents/**/agent-*.jsonl",
            serde_json::json!({"kind": "directory_membership"}),
        ),
        (
            "session-index-membership",
            "*/sessions-index.json",
            serde_json::json!({
                "kind": "replace_document",
                "max_object_bytes": INDEX_MAX_DOCUMENT_BYTES
            }),
        ),
        (
            "top-level-transcript-membership",
            "*/*.jsonl",
            serde_json::json!({"kind": "directory_membership"}),
        ),
    ] {
        let component = exact_planned_component(planned, component_id);
        assert_eq!(component["root_id"], "projects");
        assert_eq!(
            component["relative_selectors"],
            serde_json::json!([selector])
        );
        assert_eq!(
            component["discovery_bounds"],
            serde_json::json!({"max_entries": 250_000, "max_depth": 64})
        );
        assert_eq!(component["primitive"], primitive);
    }

    // The configured 250k candidate bound is carried consistently through
    // scan and checkpoint restore. It remains candidate evidence, not a
    // ratified global performance limit.
    assert_eq!(
        planned["components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|component| {
                component["discovery_bounds"]["max_entries"]
                    .as_u64()
                    .unwrap()
            })
            .collect::<Vec<_>>(),
        vec![250_000, 250_000, 250_000, 250_000]
    );
    let non_uuid = candidate_catalog_nested_parent_coordinates(Path::new(
        "project/not-a-uuid/subagents/agent-child.jsonl",
    ))
    .unwrap();
    assert_eq!(non_uuid.session_id, "not-a-uuid");

    let release = verified_candidate_release();
    assert_eq!(release.descriptor().support_release_id, SUPPORT_RELEASE_ID);
    assert_eq!(
        release.descriptor().status,
        crate::adapter::SupportReleaseStatus::Candidate
    );
    let manifest = adapter.manifest();
    assert_eq!(
        manifest
            .support_binding
            .as_ref()
            .unwrap()
            .support_release_id(),
        "claude-code-support-2026-08-21-promoted"
    );
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

#[test]
fn candidate_support_release_rejects_source_declaration_digest_drift() {
    let mut drifted = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../agent-support/claude-code/candidate-2026-08-15/source-declarations.json"
    ))
    .to_vec();
    drifted.push(b'\n');

    let error = verify_candidate_release_with_source_declaration(&drifted)
        .expect_err("digest drift must fail before a release can be verified");
    assert!(error
        .to_string()
        .contains("source_declaration document digest does not match"));
}

fn write_index(project: &Path, entries: &[(&str, &str)]) {
    let entries = entries
        .iter()
        .map(|(session_id, project_path)| {
            serde_json::json!({
                "sessionId": session_id,
                "fullPath": format!("/sanitized/{session_id}.jsonl"),
                "fileMtime": 1_770_000_000_000_u64,
                "firstPrompt": format!("index prompt {session_id}"),
                "summary": "",
                "messageCount": 2,
                "created": "2026-01-01T00:00:00Z",
                "modified": "2026-01-01T00:01:00Z",
                "gitBranch": "main",
                "projectPath": project_path,
                "isSidechain": false
            })
        })
        .collect::<Vec<_>>();
    fs::write(
        project.join("sessions-index.json"),
        serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "originalPath": "/tmp/project",
            "entries": entries
        }))
        .unwrap(),
    )
    .unwrap();
}

fn rewrite_index_first_prompt(project: &Path, first_prompt: &str) {
    let path = project.join("sessions-index.json");
    let mut document: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    document["entries"][0]["firstPrompt"] = serde_json::Value::String(first_prompt.to_string());
    fs::write(path, serde_json::to_vec(&document).unwrap()).unwrap();
}

fn write_parent(project: &Path, session_id: &str, record: &str) {
    fs::write(
        project.join(format!("{session_id}.jsonl")),
        format!("{record}\n"),
    )
    .unwrap();
}

fn write_nested(project: &Path, session_id: &str, record: &str) {
    let directory = project.join(session_id).join("subagents");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("agent-child.jsonl"), format!("{record}\n")).unwrap();
}

fn user_record(session_id: &str, cwd: &str, prompt: &str) -> String {
    serde_json::json!({
        "type": "user",
        "sessionId": session_id,
        "cwd": cwd,
        "timestamp": "2026-01-02T00:00:00Z",
        "message": {"content": prompt}
    })
    .to_string()
}

fn sized_user_record(session_id: &str, cwd: &str, payload_bytes: usize) -> String {
    let prefix = format!(
        r#"{{"type":"user","sessionId":"{session_id}","cwd":"{cwd}","timestamp":"2026-01-02T00:00:00Z","message":{{"content":"prompt"}},"padding":""#
    );
    let suffix = r#""}"#;
    assert!(payload_bytes >= prefix.len() + suffix.len());
    format!(
        "{prefix}{}{suffix}",
        "x".repeat(payload_bytes - prefix.len() - suffix.len())
    )
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

fn exact_planned_component<'a>(
    composition: &'a serde_json::Map<String, serde_json::Value>,
    component_id: &str,
) -> &'a serde_json::Value {
    composition["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|component| component["component_id"] == component_id)
        .unwrap()
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
        version: Some("2.1.223".to_string()),
        markers: vec![
            "active-session.version".to_string(),
            "settings.schema-shape".to_string(),
            "transcript.type".to_string(),
        ],
        contradictory_markers: false,
    }
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

fn synthetic_claude_catalog_access(
    selection: &ContractVersionSelection,
    compatibility: CompatibilityClass,
) -> AuthorizedCatalogAccess<'_> {
    AuthorizedCatalogAccess::fixture_with_compatibility(
        ADAPTER_ID,
        claude_conformance_support_release_id(),
        Sha256Digest::of(claude_conformance_support_release_bytes()),
        Sha256Digest::of(claude_conformance_source_declaration_bytes()),
        selection,
        compatibility,
    )
}

fn produce_from_root(
    executable: &crate::source::catalog_composition::CatalogExecutableComposition<'_, '_>,
    catalog_root: &Path,
    discriminator: &[u8],
    policy: crate::catalog_contract::CatalogAccessPolicyDigest,
) -> Result<crate::claude::catalog_runtime::ClaudeCatalogProduction, String> {
    let instance = claude_catalog_source_instance(catalog_root, discriminator)
        .map_err(|error| error.to_string())?;
    let access = executable
        .bind_source_instance(&instance)
        .map_err(|error| error.to_string())?;
    produce_claude_library_coverage(&access, policy).map_err(|error| error.to_string())
}

fn produce_fixture_with(
    compatibility: CompatibilityClass,
    policy: &[u8],
) -> crate::claude::catalog_runtime::ClaudeCatalogProduction {
    let composition = claude_conformance_promoted_composition().unwrap();
    let selection = catalog_contract_selection();
    let access = synthetic_claude_catalog_access(&selection, compatibility);
    let executable = composition.authorize_execution(access).unwrap();
    produce_from_root(
        &executable,
        &fixture_root(),
        FIXTURE_SOURCE_INSTANCE,
        CatalogAccessPolicyDigest::derive(1, policy).unwrap(),
    )
    .unwrap()
}

#[test]
fn authorized_producer_matches_frozen_identity_without_authorizing_candidate() {
    let projection = candidate_projection(&fixture_root(), 7).unwrap().report();
    let durable = durable_decoder_identities(&fixture_root(), 7).unwrap();
    let produced = produce_fixture_with(
        CompatibilityClass::ExactSupported,
        b"fixture-local-catalog-policy",
    );

    assert_eq!(
        produced.identity.project_count,
        projection.independent_oracle.project_count
    );
    assert_eq!(
        produced.identity.session_count,
        projection.independent_oracle.session_count
    );
    assert_eq!(
        produced.identity.project_identity_digest,
        projection.independent_oracle.project_identity_digest
    );
    assert_eq!(
        produced.identity.session_identity_digest,
        projection.independent_oracle.session_identity_digest
    );
    assert_eq!(
        produced.identity.project_count,
        durable.projects.len() as u64
    );
    assert_eq!(
        produced.identity.session_count,
        durable.sessions.len() as u64
    );
    assert!(!projection.catalog_execution_authorized);
    assert_eq!(projection.planned_composition_status, "planned_unbound");
    assert_eq!(
        produced.assembly.source_coverage().completeness,
        crate::adapter::CoverageSetCompleteness::Complete
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

    let replayed = produce_fixture_with(
        CompatibilityClass::ExactSupported,
        b"fixture-local-catalog-policy",
    );
    assert_eq!(produced.assembly, replayed.assembly);
    let range = produce_fixture_with(
        CompatibilityClass::RangeSupported,
        b"fixture-local-catalog-policy",
    );
    assert_eq!(produced.identity, range.identity);
    assert_eq!(
        produced.assembly.catalog_membership_revision(),
        range.assembly.catalog_membership_revision()
    );

    let drifted_policy = produce_fixture_with(
        CompatibilityClass::ExactSupported,
        b"fixture-other-catalog-policy",
    );
    assert_eq!(produced.identity, drifted_policy.identity);
    assert_ne!(
        produced.assembly.component_completion_revision(),
        drifted_policy.assembly.component_completion_revision()
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
    assert!(!debug.contains("/home/"));
    assert!(!debug.contains("03ddf851"));
}

#[test]
fn planned_and_builtin_candidate_cannot_authorize_producer_execution() {
    let selection = catalog_contract_selection();
    let planned = claude_planned_catalog_composition().unwrap();
    assert!(planned
        .authorize_execution(synthetic_claude_catalog_access(
            &selection,
            CompatibilityClass::ExactSupported,
        ))
        .is_err());

    let promoted = claude_conformance_promoted_composition().unwrap();
    assert_ne!(planned.composition_id(), promoted.composition_id());
    assert_ne!(planned.support_release_id(), promoted.support_release_id());
    assert_ne!(
        planned.source_declaration_id(),
        promoted.source_declaration_id()
    );
    assert_eq!(
        promoted.support_release_id(),
        claude_conformance_support_release_id()
    );
    assert_ne!(
        promoted.support_release_id(),
        "claude-code-support-2026-08-15-candidate"
    );
    assert_ne!(
        promoted.source_declaration_id(),
        "claude-code-sources-2026-08-15-candidate"
    );

    let release = verified_candidate_release();
    let adapter = ClaudeCodeAdapter::new();
    let manifest = adapter.manifest();
    let catalog = SupportCatalog::new([release]).unwrap();
    let error = catalog
        .authorize_typed_access(
            AdapterSupportRegistration::new(
                manifest.id.as_str(),
                manifest.support_binding.as_ref().unwrap(),
                manifest.scope_programs.as_ref().unwrap(),
            ),
            &candidate_probe(),
            SupportOperation::CatalogDiscovery,
            &compatible_contracts().0,
            &compatible_contracts().1,
        )
        .unwrap_err();
    assert!(error.to_string().contains("forbidden"));
}

const ABSENT_CATALOG_ROOT: &str = "/spaghetti-rfc012-b2-absent-catalog";
const HEAD_PHASE_MEMBER: &str = "55555555-5555-5555-5555-555555555555";

#[test]
fn producer_bind_rejects_missing_roots_before_source_access() {
    let composition = claude_conformance_promoted_composition().unwrap();
    let selection = catalog_contract_selection();
    let executable = composition
        .authorize_execution(synthetic_claude_catalog_access(
            &selection,
            CompatibilityClass::ExactSupported,
        ))
        .unwrap();
    let instance = SourceInstance {
        id: 1,
        spec: SourceInstanceSpec {
            identity_contract_version: 1,
            stable_key: SourceInstanceKey::new(FIXTURE_SOURCE_INSTANCE.to_vec()).unwrap(),
            display_name: "missing-root".to_string(),
            roots: vec![SourceRoot {
                name: "home".to_string(),
                path: Path::new(ABSENT_CATALOG_ROOT).to_path_buf(),
            }],
            discovery_reason: "missing composition root".to_string(),
        },
    };
    let error = executable.bind_source_instance(&instance).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("missing a required composition root"));
    assert!(!message.contains(ABSENT_CATALOG_ROOT));
    assert!(!message.contains("/Users/"));
}

fn conformance_promoted_binding() -> CatalogPromotedBinding {
    CatalogPromotedBinding::from_digests(
        *Sha256Digest::of(claude_conformance_source_declaration_bytes()).as_bytes(),
        *Sha256Digest::of(claude_conformance_support_release_bytes()).as_bytes(),
    )
    .unwrap()
}

fn reviewed_promoted_composition(
    support_release_id: &str,
    source_declaration_id: &str,
    components: Vec<crate::source::catalog_composition::CatalogSourceComponent>,
) -> CatalogSourceComposition {
    CatalogSourceComposition::new_promoted(
        ADAPTER_ID,
        support_release_id,
        source_declaration_id,
        conformance_promoted_binding(),
        components,
    )
    .unwrap()
}

fn produce_error_for_composition(
    composition: &CatalogSourceComposition,
    catalog_root: &Path,
) -> String {
    let selection = catalog_contract_selection();
    let executable = composition
        .authorize_execution(synthetic_claude_catalog_access(
            &selection,
            CompatibilityClass::ExactSupported,
        ))
        .unwrap();
    produce_from_root(
        &executable,
        catalog_root,
        FIXTURE_SOURCE_INSTANCE,
        CatalogAccessPolicyDigest::derive(1, b"fixture-local-catalog-policy").unwrap(),
    )
    .unwrap_err()
}

fn assert_rejected_before_source_access(composition: CatalogSourceComposition) {
    let message = produce_error_for_composition(&composition, Path::new(ABSENT_CATALOG_ROOT));
    assert!(
        message.contains("exact synthetic Claude catalog conformance composition"),
        "composition drift must fail closed before source access, got {message}"
    );
    assert!(!message.contains("failed to read"));
    assert!(!message.contains("not completely available"));
    assert!(!message.contains("/Users/"));
    assert!(!message.contains("projects/"));
    assert!(!message.contains(ABSENT_CATALOG_ROOT));
}

fn mutate_component(
    component_id: &str,
    mutate: impl FnOnce(&mut crate::source::catalog_composition::CatalogSourceComponent),
) -> CatalogSourceComposition {
    let mut components = claude_catalog_components();
    let component = components
        .iter_mut()
        .find(|component| component.component_id == component_id)
        .expect("reviewed Claude composition contains the drifted component");
    mutate(component);
    reviewed_promoted_composition(
        claude_conformance_support_release_id(),
        claude_conformance_source_declaration_id(),
        components,
    )
}

#[test]
fn producer_rejects_reviewed_composition_drift_before_source_access() {
    assert_rejected_before_source_access(mutate_component(
        "session-index-membership",
        |component| {
            component.contribution = CatalogContribution::Membership {
                member_identity_contract_id: "catalog-session-identity-v1".to_owned(),
                admission_contract_id: "session-index-entry-admission-v1".to_owned(),
                provides_metadata: false,
            };
        },
    ));
    assert_rejected_before_source_access(mutate_component(
        "transcript-head-fallback",
        |component| {
            component.overlap_strategy = CatalogOverlapStrategy::CommitCatalogFacts;
        },
    ));
    assert_rejected_before_source_access(mutate_component(
        "transcript-head-fallback",
        |component| {
            component.safe_decoder_state_boundary = CatalogDecoderStateBoundary::StatelessRecord;
        },
    ));
    assert_rejected_before_source_access(mutate_component(
        "top-level-transcript-membership",
        |component| {
            component.discovery_bounds = CatalogDiscoveryBounds::new(249_999, 64).unwrap();
        },
    ));
    assert_rejected_before_source_access(mutate_component(
        "transcript-head-fallback",
        |component| {
            let CatalogSourcePrimitive::DelimitedPrefix { max_records, .. } =
                &mut component.primitive
            else {
                panic!("reviewed head component is a delimited prefix");
            };
            *max_records = 127;
        },
    ));
    assert_rejected_before_source_access(mutate_component(
        "session-index-membership",
        |component| {
            component.disposition_ownership =
                vec!["native-family:session-index-drifted".to_owned()];
        },
    ));
    assert_rejected_before_source_access(reviewed_promoted_composition(
        claude_conformance_support_release_id(),
        "claude-code.catalog-conformance-sources-v1-x",
        claude_catalog_components(),
    ));

    let drifted_release = reviewed_promoted_composition(
        "claude-code.catalog-conformance-support-v1-x",
        claude_conformance_source_declaration_id(),
        claude_catalog_components(),
    );
    let selection = catalog_contract_selection();
    assert!(drifted_release
        .authorize_execution(synthetic_claude_catalog_access(
            &selection,
            CompatibilityClass::ExactSupported,
        ))
        .is_err());
}

fn produce_with_post_head_mutation(
    root: &Path,
    mutate: impl FnOnce(&Path),
) -> Result<crate::claude::catalog_runtime::ClaudeCatalogProduction, String> {
    let composition = claude_conformance_promoted_composition().unwrap();
    let selection = catalog_contract_selection();
    let access = synthetic_claude_catalog_access(&selection, CompatibilityClass::ExactSupported);
    let executable = composition.authorize_execution(access).unwrap();
    let policy = CatalogAccessPolicyDigest::derive(1, b"fixture-local-catalog-policy").unwrap();
    let instance = claude_catalog_source_instance(root, FIXTURE_SOURCE_INSTANCE)
        .map_err(|error| error.to_string())?;
    let bound = executable
        .bind_source_instance(&instance)
        .map_err(|error| error.to_string())?;
    produce_claude_library_coverage_with_post_head_mutation(&bound, policy, mutate)
        .map_err(|error| error.to_string())
}

fn privacy_safe_mutation_error(message: &str) {
    assert!(!message.contains("/Users/"));
    assert!(!message.contains("projects/"));
    assert!(!message.contains(HEAD_PHASE_MEMBER));
}

#[test]
fn membership_created_during_transcript_head_reads_cannot_publish_complete_coverage() {
    let fixture = TempDir::new().unwrap();
    let root = fixture.path().join(".claude");
    let project = root.join("projects/-tmp-project");
    fs::create_dir_all(&project).unwrap();
    write_index(&project, &[(TOP_ONLY, "/tmp/project")]);
    write_parent(
        &project,
        TOP_ONLY,
        &user_record(TOP_ONLY, "/tmp/project", "top"),
    );

    let composition = claude_conformance_promoted_composition().unwrap();
    let selection = catalog_contract_selection();
    let access = synthetic_claude_catalog_access(&selection, CompatibilityClass::ExactSupported);
    let executable = composition.authorize_execution(access).unwrap();
    let policy = CatalogAccessPolicyDigest::derive(1, b"fixture-local-catalog-policy").unwrap();
    let stable = produce_from_root(&executable, &root, FIXTURE_SOURCE_INSTANCE, policy)
        .expect("synthetic catalog must produce complete coverage before the mutation seam");
    assert_eq!(
        stable.assembly.source_coverage().completeness,
        crate::adapter::CoverageSetCompleteness::Complete
    );
    assert_eq!(stable.identity.session_count, 1);

    let new_member = produce_with_post_head_mutation(&root, |catalog_root| {
        write_parent(
            &catalog_root.join("projects/-tmp-project"),
            HEAD_PHASE_MEMBER,
            &user_record(HEAD_PHASE_MEMBER, "/tmp/project", "head-phase"),
        );
    })
    .expect_err("a member created during revalidation must not publish complete coverage");
    assert!(new_member.contains("membership authority changed during complete production"));
    privacy_safe_mutation_error(&new_member);

    let index_root = TempDir::new().unwrap();
    let index_catalog = index_root.path().join(".claude");
    let index_project = index_catalog.join("projects/-tmp-project");
    fs::create_dir_all(&index_project).unwrap();
    write_index(&index_project, &[(INDEX_ONLY, "/tmp/project")]);
    let index_mutation = produce_with_post_head_mutation(&index_catalog, |catalog_root| {
        rewrite_index_first_prompt(
            &catalog_root.join("projects/-tmp-project"),
            "mutated index prompt",
        );
    })
    .expect_err("same-path index mutation must fail closed as an index revision rejection");
    assert!(index_mutation.contains("index driver revision changed during production"));
    privacy_safe_mutation_error(&index_mutation);

    let head_root = TempDir::new().unwrap();
    let head_catalog = head_root.path().join(".claude");
    let head_project = head_catalog.join("projects/-tmp-project");
    fs::create_dir_all(&head_project).unwrap();
    write_parent(
        &head_project,
        TOP_ONLY,
        &user_record(TOP_ONLY, "/tmp/project", "original-head"),
    );
    let head_mutation = produce_with_post_head_mutation(&head_catalog, |catalog_root| {
        write_parent(
            &catalog_root.join("projects/-tmp-project"),
            TOP_ONLY,
            &user_record(TOP_ONLY, "/tmp/project", "mutated-head"),
        );
    })
    .expect_err("same-path transcript-head mutation must fail closed as a head revision rejection");
    assert!(head_mutation.contains("transcript-head driver revision changed during production"));
    privacy_safe_mutation_error(&head_mutation);
}

#[test]
fn producer_rejects_oversized_and_quarantined_heads_instead_of_complete_coverage() {
    let composition = claude_conformance_promoted_composition().unwrap();
    let oversized = TempDir::new().unwrap();
    let oversized_root = oversized.path().join(".claude");
    let oversized_project = oversized_root.join("projects/-tmp-project");
    fs::create_dir_all(&oversized_project).unwrap();
    fs::write(
        oversized_project.join(format!("{TOP_ONLY}.jsonl")),
        vec![b'x'; HEAD_FRAMING_READ_AHEAD_BYTES * 3],
    )
    .unwrap();
    let oversized_error = produce_error_for_composition(&composition, &oversized_root);
    assert!(
        oversized_error.contains("exceeded its declared bound"),
        "oversized heads must fail closed, got {oversized_error}"
    );
    assert!(!oversized_error.contains("/Users/"));
    assert!(!oversized_error.contains("projects/"));

    let quarantined = TempDir::new().unwrap();
    let quarantined_root = quarantined.path().join(".claude");
    let quarantined_project = quarantined_root.join("projects/-tmp-project");
    fs::create_dir_all(&quarantined_project).unwrap();
    let mut record =
        sized_user_record(TOP_ONLY, "/tmp/project", HEAD_RECORD_PAYLOAD_BYTES + 1).into_bytes();
    record.push(b'\n');
    fs::write(
        quarantined_project.join(format!("{TOP_ONLY}.jsonl")),
        record,
    )
    .unwrap();
    let quarantined_error = produce_error_for_composition(&composition, &quarantined_root);
    assert!(
        quarantined_error.contains("quarantined"),
        "quarantined heads must fail closed, got {quarantined_error}"
    );
    assert!(!quarantined_error.contains("/Users/"));
    assert!(!quarantined_error.contains("projects/"));
}

#[test]
fn producer_member_identity_separates_source_instances() {
    let first = produce_fixture_with(
        CompatibilityClass::ExactSupported,
        b"fixture-local-catalog-policy",
    );
    let composition = claude_conformance_promoted_composition().unwrap();
    let selection = catalog_contract_selection();
    let executable = composition
        .authorize_execution(synthetic_claude_catalog_access(
            &selection,
            CompatibilityClass::ExactSupported,
        ))
        .unwrap();
    let second = produce_from_root(
        &executable,
        &fixture_root(),
        b"claude-other-source-instance-v1",
        CatalogAccessPolicyDigest::derive(1, b"fixture-local-catalog-policy").unwrap(),
    )
    .unwrap();
    assert_eq!(first.identity, second.identity);
    assert_ne!(
        first.assembly.catalog_membership_revision(),
        second.assembly.catalog_membership_revision()
    );
    assert_ne!(
        first.assembly.plan_source().source_instance_key,
        second.assembly.plan_source().source_instance_key
    );
}

fn verified_candidate_release() -> VerifiedSupportRelease {
    verify_candidate_release_with_source_declaration(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../agent-support/claude-code/candidate-2026-08-15/source-declarations.json"
    )))
    .unwrap()
}

fn verify_candidate_release_with_source_declaration(
    source_declaration: &[u8],
) -> Result<VerifiedSupportRelease, SupportContractError> {
    verify_support_release_bundle(
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../agent-support/claude-code/candidate-2026-08-15/support-release.json"
        )),
        &[
            SupportBundleDocument::new(
                "agent-support/claude-code/candidate-2026-08-15/ads.json",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../agent-support/claude-code/candidate-2026-08-15/ads.json"
                )),
            ),
            SupportBundleDocument::new(
                "agent-support/claude-code/candidate-2026-08-15/source-declarations.json",
                source_declaration,
            ),
            SupportBundleDocument::new(
                "agent-support/claude-code/candidate-2026-08-15/scope-programs.json",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../agent-support/claude-code/candidate-2026-08-15/scope-programs.json"
                )),
            ),
            SupportBundleDocument::new(
                "agent-support/claude-code/candidate-2026-08-15/evidence.json",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../agent-support/claude-code/candidate-2026-08-15/evidence.json"
                )),
            ),
            SupportBundleDocument::new(
                "agent-support/claude-code/candidate-2026-08-15/conformance.json",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../agent-support/claude-code/candidate-2026-08-15/conformance.json"
                )),
            ),
        ],
    )
}
