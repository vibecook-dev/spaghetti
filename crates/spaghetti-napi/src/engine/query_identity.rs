use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;

use super::EngineError;

const MAX_OPAQUE_ID_BYTES: usize = 32 * 1024;
pub(super) const PROJECT_ID_PREFIX: &str = "project_v1_";
pub(super) const SESSION_ID_PREFIX: &str = "session_v1_";

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
