//! Registry behaviour: what a verified support release may authorize.
//!
//! Moved out of `registry.rs` under the landing plan's rule that an inline
//! `mod tests` past 500 lines becomes a sibling file.

use super::*;
use std::collections::BTreeMap;

use std::sync::Arc;

use crate::adapter::{
    verify_support_release_bundle, AdapterManifest, AdapterObjectContext, AdapterSupportBinding,
    DecodeContext, DecodeDisposition, DiscoveryContext, FactBatch, NativeArtifactProbe,
    Sha256Digest, SourceInstance, SourceInstanceSpec, SourceObjectDescriptor, StreamSpec,
    SupportBundleDocument,
};

use crate::source::{
    AccessOperation, AccessOutcome, AccessPhase, AuthorizedScopeAccessPlan, ScopeAccessReport,
    ScopeAccessRequest, ScopeIdentityInput, SourceRecord,
};

struct EmptyAdapter {
    manifest: AdapterManifest,
    streams: Vec<StreamSpec>,
}

impl EmptyAdapter {
    fn new(id: &str) -> Self {
        Self {
            manifest: AdapterManifest {
                id: AdapterId::new(id).unwrap(),
                display_name: id.to_string(),
                adapter_version: "1.0.0".to_string(),
                contract_version: 1,
                support_binding: None,
                scope_programs: None,
                source_schema_versions: Vec::new(),
                capabilities: Vec::new(),
            },
            streams: Vec::new(),
        }
    }

    fn with_support(
        mut self,
        binding: AdapterSupportBinding,
        scope_programs: crate::adapter::ScopeProgramManifest,
    ) -> Self {
        self.manifest.support_binding = Some(binding);
        self.manifest.scope_programs = Some(scope_programs);
        self
    }
}

const SINGLE_OBJECT_SCOPE_DOCUMENT: &[u8] = br#"{"schema_version":1,"declaration_id":"fixture-scope","adapter_id":"fixture","ads_id":"fixture-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","root_relation_id":"root-object","relations":[{"relation_id":"root-object","primitive":"KnownObject","access_root":"root","locator":"known-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":1024,"max_rows":0},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#;

const UNCOMPOSED_DYNAMIC_SCOPE_DOCUMENT: &[u8] = br#"{"schema_version":1,"declaration_id":"fixture-scope","adapter_id":"fixture","ads_id":"fixture-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","root_relation_id":"root-object","relations":[{"relation_id":"root-object","primitive":"KnownObject","access_root":"root","locator":"known-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":8388608,"max_rows":0},"observation_binding":{"stream_id":"root-stream","source_pattern":"sessions/*.jsonl"},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]},{"relation_id":"descendant-objects","primitive":"ChildDirectoryByNativeId","access_root":"root","locator":"sessions/{native-session-id}/children","identity_inputs":["native-session-id"],"directory_identity_authority":"configured_root","bounds":{"max_fan_out":8,"max_depth":2,"max_objects":8,"max_bytes":8192,"max_rows":0},"observation_binding":{"stream_id":"descendant-stream","source_pattern":"sessions/*/children/**","relative_selector":"**"},"unavailable_behavior":"skip_optional","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#;

const JOIN_DERIVED_DYNAMIC_SCOPE_DOCUMENT: &[u8] = br#"{"schema_version":1,"declaration_id":"fixture-scope","adapter_id":"fixture","ads_id":"fixture-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","root_relation_id":"root-object","relations":[{"relation_id":"root-object","primitive":"KnownObject","access_root":"root","locator":"known-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":8388608,"max_rows":0},"observation_binding":{"stream_id":"root-stream","source_pattern":"sessions/*.jsonl"},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]},{"relation_id":"descendant-objects","primitive":"ChildDirectoryByNativeId","access_root":"root","locator":"sessions/{child-id}/children","identity_inputs":["child-id"],"directory_identity_authority":"scope_join","bounds":{"max_fan_out":8,"max_depth":2,"max_objects":8,"max_bytes":8192,"max_rows":0},"observation_binding":{"stream_id":"descendant-stream","source_pattern":"sessions/*/children/**","relative_selector":"**"},"unavailable_behavior":"skip_optional","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#;

const COMPOSED_ROOT_SCOPE_DOCUMENT: &[u8] = br#"{"schema_version":1,"declaration_id":"fixture-scope","adapter_id":"fixture","ads_id":"fixture-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","root_relation_id":"root-object","relations":[{"relation_id":"root-object","primitive":"KnownObject","access_root":"root","locator":"known-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":8388608,"max_rows":0},"observation_binding":{"stream_id":"root-stream","source_pattern":"sessions/*.jsonl"},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#;

const COMPOSED_RELATED_SCOPE_DOCUMENT: &[u8] = br#"{"schema_version":1,"declaration_id":"fixture-scope","adapter_id":"fixture","ads_id":"fixture-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","root_relation_id":"root-object","relations":[{"relation_id":"root-object","primitive":"KnownObject","access_root":"root","locator":"known-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":8388608,"max_rows":0},"observation_binding":{"stream_id":"root-stream","source_pattern":"sessions/*.jsonl"},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]},{"relation_id":"team-config-from-evidence","primitive":"ReferencedObjectFromField","access_root":"root","locator":"teams/{team-name}/config.json","identity_inputs":["team-name"],"bounds":{"max_fan_out":4,"max_depth":1,"max_objects":4,"max_bytes":4096,"max_rows":0},"observation_binding":{"stream_id":"team-config-stream","source_pattern":"teams/*/config.json"},"unavailable_behavior":"skip_optional","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#;

const COMPOSED_TWO_APPEND_SCOPE_DOCUMENT: &[u8] = br#"{"schema_version":1,"declaration_id":"fixture-scope","adapter_id":"fixture","ads_id":"fixture-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","root_relation_id":"root-object","relations":[{"relation_id":"root-object","primitive":"KnownObject","access_root":"root","locator":"known-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":8388608,"max_rows":0},"observation_binding":{"stream_id":"root-stream","source_pattern":"sessions/*.jsonl"},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]},{"relation_id":"sibling-object","primitive":"KnownObject","access_root":"root","locator":"sibling-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":8388608,"max_rows":0},"observation_binding":{"stream_id":"sibling-stream","source_pattern":"siblings/*.jsonl"},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#;

struct PromotedTestSupportDocuments<'document> {
    support_release_id: &'document str,
    adapter_id: &'document str,
    ads_id: &'document str,
    artifact_family: &'document str,
    artifact_version: &'document str,
    required_marker: &'document str,
    adapter_package_version: &'document str,
    decoder_contract_version: u32,
    capabilities: &'document [(&'document str, &'document str)],
    source_document: &'document [u8],
    scope_document: &'document [u8],
}

fn promoted_test_catalog(
    config: PromotedTestSupportDocuments<'_>,
) -> (
    Arc<SupportCatalog>,
    AdapterSupportBinding,
    crate::adapter::ScopeProgramManifest,
) {
    let ads_document = serde_json::to_vec(&serde_json::json!({
        "adapter_id": config.adapter_id,
        "ads_id": config.ads_id,
    }))
    .unwrap();
    let evidence_document = ads_document.clone();
    let conformance_document = serde_json::to_vec(&serde_json::json!({
        "adapter_id": config.adapter_id,
        "support_release_id": config.support_release_id,
    }))
    .unwrap();
    let documents = [
        ("ads", "support/ads.json", ads_document),
        (
            "source_declaration",
            "support/source.json",
            config.source_document.to_vec(),
        ),
        (
            "scope_program",
            "support/scope.json",
            config.scope_document.to_vec(),
        ),
        ("evidence", "support/evidence.json", evidence_document),
        (
            "conformance",
            "support/conformance.json",
            conformance_document,
        ),
    ];
    let references = documents
        .iter()
        .map(|(kind, path, bytes)| {
            (
                (*kind).to_string(),
                serde_json::json!({"path": path, "sha256": Sha256Digest::of(bytes).to_string()}),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let capabilities = config
        .capabilities
        .iter()
        .map(|(capability_id, topology)| {
            serde_json::json!({
                "capability_id": capability_id,
                "topology": topology,
                "level": "supported",
                "notes": null
            })
        })
        .collect::<Vec<_>>();
    let release_json = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "support_release_id": config.support_release_id,
        "adapter_id": config.adapter_id,
        "version": "2026-08-21",
        "artifact_compatibility": {
            "family": config.artifact_family,
            "platforms": ["test"],
            "exact_versions": [config.artifact_version],
            "ranges": [],
            "required_markers": [config.required_marker],
            "forward_catalog_only": false
        },
        "references": references,
        "versions": {
            "adapter_package": config.adapter_package_version,
            "decoder_contract": config.decoder_contract_version
        },
        "capabilities": capabilities
    }))
    .unwrap();
    let bundle_documents = documents
        .iter()
        .map(|(_, path, bytes)| SupportBundleDocument::new(path, bytes))
        .collect::<Vec<_>>();
    let release = verify_support_release_bundle(&release_json, &bundle_documents).unwrap();
    let binding = release.adapter_binding().clone();
    let scope_programs = release.scope_programs().clone();
    (
        Arc::new(SupportCatalog::new([release]).unwrap()),
        binding,
        scope_programs,
    )
}

fn promoted_fixture_catalog_with_scope(
    scope_document: &[u8],
) -> (
    Arc<SupportCatalog>,
    AdapterSupportBinding,
    crate::adapter::ScopeProgramManifest,
) {
    let source_document = if scope_document == UNCOMPOSED_DYNAMIC_SCOPE_DOCUMENT
        || scope_document == JOIN_DERIVED_DYNAMIC_SCOPE_DOCUMENT
    {
        br#"{"adapter_id":"fixture","ads_id":"fixture-ads","streams":[{"stream_id":"root-stream","root_id":"root","relative_patterns":["sessions/*.jsonl"],"decoder_id":"fixture-root","authority":"canonical","primitive":"AppendDelimited","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_record_bytes":4194304,"max_batch_bytes":8388608,"max_records_per_batch":1024},"lifecycle":["append","partial_write","truncate","identity_change","delete","recreate"],"safe_decoder_state_boundary":"object_generation_cursor"},{"stream_id":"artifact-blobs","root_id":"artifact","relative_patterns":["artifacts/*"],"primitive":"ReplaceDocument","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_object_bytes":1024},"lifecycle":["replace","delete","recreate"],"safe_decoder_state_boundary":"object_generation_revision"},{"stream_id":"descendant-stream","root_id":"root","relative_patterns":["sessions/*/children/**"],"decoder_id":"fixture-descendant","authority":"canonical","primitive":"ReplaceDocument","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_object_bytes":1024},"lifecycle":["replace","delete","recreate"],"safe_decoder_state_boundary":"object_generation_revision"}]}"#.as_slice()
    } else if scope_document == COMPOSED_RELATED_SCOPE_DOCUMENT {
        br#"{"adapter_id":"fixture","ads_id":"fixture-ads","streams":[{"stream_id":"root-stream","root_id":"root","relative_patterns":["sessions/*.jsonl"],"decoder_id":"fixture-root","authority":"canonical","primitive":"AppendDelimited","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_record_bytes":4194304,"max_batch_bytes":8388608,"max_records_per_batch":1024},"lifecycle":["append","partial_write","truncate","identity_change","delete","recreate"],"safe_decoder_state_boundary":"object_generation_cursor"},{"stream_id":"team-config-stream","root_id":"root","relative_patterns":["teams/*/config.json"],"decoder_id":"fixture-related","authority":"canonical","primitive":"ReplaceDocument","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_object_bytes":4096},"lifecycle":["replace","delete","recreate"],"safe_decoder_state_boundary":"object_generation_revision"}]}"#.as_slice()
    } else if scope_document == COMPOSED_TWO_APPEND_SCOPE_DOCUMENT {
        br#"{"adapter_id":"fixture","ads_id":"fixture-ads","streams":[{"stream_id":"root-stream","root_id":"root","relative_patterns":["sessions/*.jsonl"],"decoder_id":"fixture-root","authority":"canonical","primitive":"AppendDelimited","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_record_bytes":4194304,"max_batch_bytes":8388608,"max_records_per_batch":1024},"lifecycle":["append","partial_write","truncate","identity_change","delete","recreate"],"safe_decoder_state_boundary":"object_generation_cursor"},{"stream_id":"sibling-stream","root_id":"root","relative_patterns":["siblings/*.jsonl"],"decoder_id":"fixture-sibling","authority":"canonical","primitive":"AppendDelimited","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_record_bytes":4194304,"max_batch_bytes":8388608,"max_records_per_batch":1024},"lifecycle":["append","partial_write","truncate","identity_change","delete","recreate"],"safe_decoder_state_boundary":"object_generation_cursor"}]}"#.as_slice()
    } else if scope_document == COMPOSED_ROOT_SCOPE_DOCUMENT {
        br#"{"adapter_id":"fixture","ads_id":"fixture-ads","streams":[{"stream_id":"root-stream","root_id":"root","relative_patterns":["sessions/*.jsonl"],"decoder_id":"fixture-root","authority":"canonical","primitive":"AppendDelimited","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_record_bytes":4194304,"max_batch_bytes":8388608,"max_records_per_batch":1024},"lifecycle":["append","partial_write","truncate","identity_change","delete","recreate"],"safe_decoder_state_boundary":"object_generation_cursor"}]}"#.as_slice()
    } else {
        br#"{"adapter_id":"fixture","ads_id":"fixture-ads","streams":[{"stream_id":"artifact-blobs","root_id":"artifact","primitive":"ReplaceDocument","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_object_bytes":1024},"lifecycle":["replace","delete","recreate"],"safe_decoder_state_boundary":"object_generation_revision"}]}"#.as_slice()
    };
    promoted_test_catalog(PromotedTestSupportDocuments {
        support_release_id: "fixture-release",
        adapter_id: "fixture",
        ads_id: "fixture-ads",
        artifact_family: "fixture",
        artifact_version: "1.0.0",
        required_marker: "fixture.marker",
        adapter_package_version: "1.0.0",
        decoder_contract_version: 1,
        capabilities: &[
            ("fixture-catalog", "catalog"),
            ("fixture-history", "durable"),
            ("fixture-observation", "scoped"),
        ],
        source_document,
        scope_document,
    })
}

fn promoted_fixture_catalog() -> (
    Arc<SupportCatalog>,
    AdapterSupportBinding,
    crate::adapter::ScopeProgramManifest,
) {
    promoted_fixture_catalog_with_scope(SINGLE_OBJECT_SCOPE_DOCUMENT)
}

impl AgentAdapter for EmptyAdapter {
    /// The surviving registry tests resolve adapters and authorize scope
    /// programs; none of them discovers instances from configured roots.
    fn discover(
        &self,
        _context: &DiscoveryContext,
    ) -> Result<Vec<SourceInstanceSpec>, AdapterError> {
        Ok(Vec::new())
    }
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }

    fn streams(&self, _instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
        Ok(self.streams.clone())
    }

    fn bootstrap_object(
        &self,
        _instance: &SourceInstance,
        _object: &SourceObjectDescriptor,
    ) -> Result<AdapterObjectContext, AdapterError> {
        Ok(AdapterObjectContext::empty())
    }

    /// The surviving registry tests never decode; the fixture adapter only
    /// has to satisfy the trait.
    fn decode(
        &self,
        _context: DecodeContext<'_>,
        _record: &SourceRecord,
        _output: &mut FactBatch,
    ) -> Result<DecodeDisposition, AdapterError> {
        Ok(DecodeDisposition::IgnoredKnown)
    }
}

#[test]
fn registry_rejects_duplicate_open_ids() {
    let result = AdapterRegistryBuilder::new()
        .register(EmptyAdapter::new("same"))
        .register(EmptyAdapter::new("same"))
        .build();
    assert!(result.is_err());
}

#[test]
fn registry_resolves_an_adapter_without_source_specific_dispatch() {
    let registry = AdapterRegistryBuilder::new()
        .register(EmptyAdapter::new("one"))
        .register(EmptyAdapter::new("two"))
        .build()
        .unwrap();
    assert_eq!(registry.len(), 2);
    assert!(registry.get(&AdapterId::new("two").unwrap()).is_some());
}

#[test]
fn native_support_probe_is_host_registered_exact_and_panic_contained() {
    let unregistered = AdapterRegistryBuilder::new()
        .register(EmptyAdapter::new("one"))
        .register_native_support_probe("missing", |_| unreachable!())
        .build();
    assert!(unregistered.is_err());

    let duplicate = AdapterRegistryBuilder::new()
        .register(EmptyAdapter::new("one"))
        .register_native_support_probe("one", |_| unreachable!())
        .register_native_support_probe("one", |_| unreachable!())
        .build();
    assert!(duplicate.is_err());

    let registry = AdapterRegistryBuilder::new()
        .register(EmptyAdapter::new("one"))
        .register(EmptyAdapter::new("two"))
        .register_native_support_probe("one", |_| {
            Ok(NativeArtifactProbe {
                family: "fixture".to_string(),
                platform: "test".to_string(),
                version: Some("1.0.0".to_string()),
                markers: vec!["fixture.marker".to_string()],
                contradictory_markers: false,
            })
        })
        .build()
        .unwrap();
    let probe = registry
        .probe_native_support(&AdapterId::new("one").unwrap(), &[])
        .unwrap()
        .unwrap();
    assert_eq!(probe.family, "fixture");
    assert!(registry
        .probe_native_support(&AdapterId::new("two").unwrap(), &[])
        .unwrap()
        .is_none());

    let panicking = AdapterRegistryBuilder::new()
        .register(EmptyAdapter::new("one"))
        .register_native_support_probe("one", |_| panic!("private probe detail"))
        .build()
        .unwrap();
    let error = panicking
        .probe_native_support(&AdapterId::new("one").unwrap(), &[])
        .unwrap_err();
    assert_eq!(error.code, "native_support_probe_panic");
    assert!(!error.to_string().contains("private probe detail"));
}

#[test]
fn supported_registry_requires_and_retains_a_promoted_digest_binding() {
    let (catalog, binding, scope_programs) = promoted_fixture_catalog();
    let missing = AdapterRegistryBuilder::new()
        .register(EmptyAdapter::new("fixture"))
        .build_supported(Arc::clone(&catalog));
    assert!(missing.err().unwrap().to_string().contains("digest-bound"));

    let mut mismatched_scope = scope_programs.clone();
    mismatched_scope.programs[0].relations[0].locator = "different-object".to_string();
    let mismatched = AdapterRegistryBuilder::new()
        .register(EmptyAdapter::new("fixture").with_support(binding.clone(), mismatched_scope))
        .build_supported(Arc::clone(&catalog));
    assert!(mismatched
        .err()
        .unwrap()
        .to_string()
        .contains("compiled scope declarations"));

    let registry = AdapterRegistryBuilder::new()
        .register(EmptyAdapter::new("fixture").with_support(binding, scope_programs))
        .build_supported(catalog)
        .unwrap();
    assert!(registry.enforces_promoted_support());

    let probe = NativeArtifactProbe {
        family: "fixture".to_string(),
        platform: "test".to_string(),
        version: Some("1.0.0".to_string()),
        markers: vec!["fixture.marker".to_string()],
        contradictory_markers: false,
    };
    let durable_authorization = registry
        .authorize_durable_if_supported(&AdapterId::new("fixture").unwrap(), &probe)
        .unwrap()
        .unwrap();
    assert_eq!(
        durable_authorization.operation().operation(),
        SupportOperation::DurableHistoryRuntime
    );

    let request = ContractVersionRequest {
        selection_contract_version: 1,
        model_major: 1,
        external_entity_reference_version: 1,
        semantic_revision_reference_version: 1,
        coverage_contract_versions: vec![1],
        fact_family_versions: BTreeMap::new(),
        query_pack_versions: Some(vec![1]),
        observation_contract_versions: None,
    };
    let offer = ContractVersionOffer {
        selection_contract_version: 1,
        model_major: 1,
        external_entity_reference_versions: vec![1],
        semantic_revision_reference_versions: vec![1],
        coverage_contract_versions: vec![1],
        fact_family_versions: BTreeMap::new(),
        query_pack_versions: vec![1],
        observation_contract_versions: Vec::new(),
    };
    let (decision, authorization) = registry
        .authorize_typed_access(
            &AdapterId::new("fixture").unwrap(),
            &probe,
            SupportOperation::DurableHistoryRuntime,
            &request,
            &offer,
        )
        .unwrap();
    assert_eq!(decision.support_release_id(), Some("fixture-release"));
    assert_eq!(
        authorization.operation().support_release_id(),
        Some("fixture-release")
    );
    assert!(authorization
        .select_scope_program("observe-session")
        .is_err());

    let missing_observation_contract = registry.authorize_typed_access(
        &AdapterId::new("fixture").unwrap(),
        &NativeArtifactProbe {
            family: "fixture".to_string(),
            platform: "test".to_string(),
            version: Some("1.0.0".to_string()),
            markers: vec!["fixture.marker".to_string()],
            contradictory_markers: false,
        },
        SupportOperation::ScopedTypedObservation,
        &request,
        &offer,
    );
    assert!(missing_observation_contract
        .unwrap_err()
        .to_string()
        .contains("observation contract"));

    let mut scoped_request = request;
    scoped_request.observation_contract_versions = Some(vec![1]);
    let mut scoped_offer = offer;
    scoped_offer.observation_contract_versions = vec![1];
    let (scoped_decision, scoped_authorization) = registry
        .authorize_typed_access(
            &AdapterId::new("fixture").unwrap(),
            &NativeArtifactProbe {
                family: "fixture".to_string(),
                platform: "test".to_string(),
                version: Some("1.0.0".to_string()),
                markers: vec!["fixture.marker".to_string()],
                contradictory_markers: false,
            },
            SupportOperation::ScopedTypedObservation,
            &scoped_request,
            &scoped_offer,
        )
        .unwrap();
    assert!(scoped_decision.permissions().scoped_observation);
    assert_eq!(
        scoped_authorization.operation().operation(),
        SupportOperation::ScopedTypedObservation
    );
    let program = scoped_authorization
        .select_scope_program("observe-session")
        .unwrap();
    let plan = AuthorizedScopeAccessPlan::from_authorized_program(program).unwrap();
    assert_eq!(plan.adapter_id(), "fixture");
    assert_eq!(plan.support_release_id(), "fixture-release");

    let unsupported_probe = NativeArtifactProbe {
        family: "fixture".to_string(),
        platform: "test".to_string(),
        version: Some("9.9.9".to_string()),
        markers: vec!["fixture.marker".to_string()],
        contradictory_markers: false,
    };
    assert!(registry
        .authorize_typed_access(
            &AdapterId::new("fixture").unwrap(),
            &unsupported_probe,
            SupportOperation::ScopedTypedObservation,
            &scoped_request,
            &scoped_offer,
        )
        .is_err());

    let empty_report = plan.report();
    assert!(empty_report.verify_digest());
    let empty_digest = empty_report.digest();
    let identity = [ScopeIdentityInput {
        name: "native-session-id",
        value: b"secret-session-id",
    }];
    plan.reserve(ScopeAccessRequest {
        relation_id: "root-object",
        operation: AccessOperation::ObjectRead,
        phase: AccessPhase::Initial,
        parent_token: None,
        identity_inputs: &identity,
        depth: 1,
        max_bytes: 128,
        max_rows: 0,
    })
    .unwrap()
    .complete(64, 0, AccessOutcome::Available)
    .unwrap();
    let report = plan.report();
    assert!(report.verify_digest());
    assert_eq!(
        report.digest().to_string(),
        "sha256:aaa6f494b3839f6de18f5e534e6f3fb41f988e4942d33dc52269703e7a46b531"
    );
    assert_ne!(report.digest(), empty_digest);
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(!encoded.contains("secret-session-id"));

    let report_value = serde_json::to_value(report).unwrap();
    let mut unknown_field = report_value.clone();
    unknown_field["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<ScopeAccessReport>(unknown_field).is_err());

    let mut tampered = report_value;
    tampered["relations"][0]["bytes_read"] = serde_json::json!(65);
    let tampered: ScopeAccessReport = serde_json::from_value(tampered).unwrap();
    assert!(!tampered.verify_digest());
}

#[test]
fn a_candidate_scope_declaration_verifies_but_cannot_authorize() {
    let source_document = br#"{"adapter_id":"fixture","ads_id":"fixture-ads","streams":[{"stream_id":"root-stream","root_id":"root","relative_patterns":["sessions/*.jsonl"],"decoder_id":"fixture-root","authority":"canonical","primitive":"AppendDelimited","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_record_bytes":4194304,"max_batch_bytes":8388608,"max_records_per_batch":1024},"lifecycle":["append","partial_write","truncate","identity_change","delete","recreate"],"safe_decoder_state_boundary":"object_generation_cursor"},{"stream_id":"artifact-blobs","root_id":"artifact","relative_patterns":["artifacts/*"],"primitive":"ReplaceDocument","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_object_bytes":1024},"lifecycle":["replace","delete","recreate"],"safe_decoder_state_boundary":"object_generation_revision"},{"stream_id":"descendant-stream","root_id":"root","relative_patterns":["sessions/*/children/**"],"decoder_id":"fixture-descendant","authority":"canonical","primitive":"ReplaceDocument","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_object_bytes":1024},"lifecycle":["replace","delete","recreate"],"safe_decoder_state_boundary":"object_generation_revision"}]}"#;
    let documents = [
        (
            "ads",
            "support/ads.json",
            br#"{"adapter_id":"fixture","ads_id":"fixture-ads"}"#.as_slice(),
        ),
        (
            "source_declaration",
            "support/source.json",
            source_document.as_slice(),
        ),
        (
            "scope_program",
            "support/scope.json",
            // The same declaration one tier down, where it may not authorize.
            &String::from_utf8_lossy(UNCOMPOSED_DYNAMIC_SCOPE_DOCUMENT)
                .replace(r#""status":"promoted""#, r#""status":"candidate""#)
                .replace(r#""blockers":[]"#, r#""blockers":["unproven"]"#)
                .into_bytes(),
        ),
        (
            "evidence",
            "support/evidence.json",
            br#"{"adapter_id":"fixture","ads_id":"fixture-ads"}"#.as_slice(),
        ),
        (
            "conformance",
            "support/conformance.json",
            br#"{"adapter_id":"fixture","support_release_id":"fixture-release"}"#.as_slice(),
        ),
    ];
    let references = documents
        .iter()
        .map(|(kind, path, bytes)| {
            (
                (*kind).to_string(),
                serde_json::json!({"path": path, "sha256": Sha256Digest::of(bytes).to_string()}),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let release_json = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "support_release_id": "fixture-release",
        "adapter_id": "fixture",
        "version": "2026-08-21",
        "artifact_compatibility": {
            "family": "fixture",
            "platforms": ["test"],
            "exact_versions": ["1.0.0"],
            "ranges": [],
            "required_markers": ["fixture.marker"],
            "forward_catalog_only": false
        },
        "references": references,
        "versions": {"adapter_package": "1.0.0", "decoder_contract": 1},
        "capabilities": [
            {
                "capability_id": "fixture-observation",
                "topology": "scoped",
                "level": "supported",
                "notes": null
            }
        ]
    }))
    .unwrap();
    let bundle_documents = documents
        .iter()
        .map(|(_, path, bytes)| SupportBundleDocument::new(path, bytes))
        .collect::<Vec<_>>();
    // The release claims no tier of its own, so the declaration it carries is
    // what decides that it may not be selected.
    let verified = verify_support_release_bundle(&release_json, &bundle_documents).unwrap();
    assert!(!verified.descriptor().runtime_selectable);
}
