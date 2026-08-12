use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;

use super::EngineError;

const MAX_OPAQUE_ID_BYTES: usize = 32 * 1024;
pub(super) const PROJECT_ID_PREFIX: &str = "project_v1_";
pub(super) const SESSION_ID_PREFIX: &str = "session_v1_";
pub(super) const MESSAGE_ID_PREFIX: &str = "message_v1_";
pub(super) const SOURCE_ID_PREFIX: &str = "source_v1_";
pub(super) const MEMORY_DOCUMENT_ID_PREFIX: &str = "memory_document_v1_";
pub(super) const TASK_COLLECTION_ID_PREFIX: &str = "task_collection_v1_";
pub(super) const TASK_ID_PREFIX: &str = "task_v1_";
pub(super) const PLAN_ID_PREFIX: &str = "plan_v1_";
pub(super) const TOOL_RESULT_ID_PREFIX: &str = "tool_result_v1_";
pub(super) const ARTIFACT_ID_PREFIX: &str = "artifact_v1_";
pub(super) const RUN_ID_PREFIX: &str = "run_v1_";
pub(super) const PRESENCE_ID_PREFIX: &str = "presence_v1_";
pub(super) const TEAM_ID_PREFIX: &str = "team_v1_";
pub(super) const TEAM_MEMBER_ID_PREFIX: &str = "team_member_v1_";
pub(super) const TEAM_INBOX_ID_PREFIX: &str = "team_inbox_v1_";
pub(super) const TEAM_INBOX_MESSAGE_ID_PREFIX: &str = "team_inbox_message_v1_";
pub(super) const WORKFLOW_ID_PREFIX: &str = "workflow_v1_";
pub(super) const WORKFLOW_MEMBER_ID_PREFIX: &str = "workflow_member_v1_";
pub(super) const FACT_ID_PREFIX: &str = "fact_v1_";

pub(super) fn encode_entity_id(prefix: &str, key: &[u8]) -> String {
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(key))
}

pub(super) fn decode_entity_id(
    value: &str,
    prefix: &'static str,
    label: &'static str,
) -> Result<Vec<u8>, EngineError> {
    if value.len() > MAX_OPAQUE_ID_BYTES || !value.starts_with(prefix) {
        return Err(EngineError::InvalidQuery(format!(
            "{label} is not a supported opaque identifier"
        )));
    }
    let encoded = &value[prefix.len()..];
    let key = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        EngineError::InvalidQuery(format!("{label} is not a supported opaque identifier"))
    })?;
    if key.is_empty() || key.len() > MAX_OPAQUE_ID_BYTES {
        return Err(EngineError::InvalidQuery(format!(
            "{label} is not a supported opaque identifier"
        )));
    }
    Ok(key)
}
