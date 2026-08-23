use std::sync::Arc;

use super::{
    AdapterRegistry, AgentAdapter, NativeArtifactProbe, SupportCatalog, SupportContractError,
};
use crate::claude::ClaudeCodeAdapter;
use crate::codex::CodexAdapter;
use crate::grok::GrokAdapter;

pub(crate) fn verified_builtin_support_catalog() -> Result<SupportCatalog, SupportContractError> {
    SupportCatalog::new([
        crate::claude::verified_support_release()?,
        crate::codex::verified_support_release()?,
        crate::grok::verified_support_release()?,
    ])
}

pub(crate) fn verified_claude_candidate_for_test(
) -> Result<super::VerifiedSupportRelease, SupportContractError> {
    crate::claude::verified_support_release()
}

#[test]
fn compiled_catalog_verifies_all_candidates_without_authorizing_typed_access() {
    let catalog = verified_builtin_support_catalog().unwrap();
    assert_eq!(catalog.len(), 3);
    let registry = AdapterRegistry::builder()
        .register(ClaudeCodeAdapter::new())
        .register(CodexAdapter::new())
        .register(GrokAdapter::new())
        .build_verified(Arc::new(catalog.clone()))
        .unwrap();
    assert_eq!(registry.len(), 3);
    assert!(registry.has_verified_support_catalog());
    let probe = NativeArtifactProbe {
        family: "claude-code".to_string(),
        platform: "darwin".to_string(),
        version: Some("2.1.223".to_string()),
        markers: vec![
            "active-session.version".to_string(),
            "settings.schema-shape".to_string(),
            "transcript.type".to_string(),
        ],
        contradictory_markers: false,
    };
    let exact = catalog.classify(&probe).unwrap();
    assert!(!exact.permissions().durable);
    assert!(registry
        .authorize_durable_if_supported(&ClaudeCodeAdapter::new().manifest().id, &probe)
        .unwrap()
        .is_none());
    let candidate = catalog
        .classify(&NativeArtifactProbe {
            family: "codex".to_string(),
            platform: "darwin".to_string(),
            version: None,
            markers: Vec::new(),
            contradictory_markers: false,
        })
        .unwrap();
    assert!(!candidate.permissions().durable);
}
