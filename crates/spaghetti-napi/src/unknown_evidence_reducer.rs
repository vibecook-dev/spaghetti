//! Topology-neutral RFC 012C unknown-native-evidence validation.
//!
//! One retained unknown-native record is admissible only if its family hint,
//! observed byte count, and sanitized diagnostic excerpt are all within the
//! declared bounds and the excerpt's self-describing shape matches the
//! evidence it claims to summarize. The durable projection in
//! `engine/unknown_evidence_projection.rs` owns the bounded SQL state.

use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::adapter::{BoundedNativeEvidence, MAX_UNKNOWN_RAW_PAYLOAD_BYTES};
use crate::decode_runtime::{MAX_DIAGNOSTIC_EXCERPT_BYTES, MAX_DIAGNOSTIC_SHAPE_ITEMS};

pub(crate) const MAX_UNKNOWN_EVIDENCE_OCCURRENCES: usize = 65_536;
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum UnknownEvidenceReductionError {
    #[error("invalid bounded unknown evidence")]
    InvalidEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnknownEvidenceOccurrence {
    pub family_hint: Option<String>,
    pub evidence: BoundedNativeEvidence,
}

impl UnknownEvidenceOccurrence {
    pub(crate) fn new(
        family_hint: Option<String>,
        evidence: BoundedNativeEvidence,
    ) -> Result<Self, UnknownEvidenceReductionError> {
        let value = Self {
            family_hint,
            evidence,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), UnknownEvidenceReductionError> {
        if self
            .family_hint
            .as_deref()
            .is_some_and(|value| !is_safe_family_hint(value))
            || self.evidence.observed_bytes > MAX_UNKNOWN_RAW_PAYLOAD_BYTES as u64
            || self.evidence.sanitized_excerpt.is_empty()
            || self.evidence.sanitized_excerpt.len() > MAX_DIAGNOSTIC_EXCERPT_BYTES
            || !is_valid_sanitized_excerpt(&self.evidence)
        {
            return Err(UnknownEvidenceReductionError::InvalidEvidence);
        }
        Ok(())
    }
}

fn is_safe_family_hint(value: &str) -> bool {
    const MAX_FAMILY_HINT_BYTES: usize = 128;
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(first) if first.is_ascii_lowercase() || first.is_ascii_digit())
        && value.len() <= MAX_FAMILY_HINT_BYTES
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn is_valid_sanitized_excerpt(evidence: &BoundedNativeEvidence) -> bool {
    let Ok(JsonValue::Object(value)) =
        serde_json::from_slice::<JsonValue>(&evidence.sanitized_excerpt)
    else {
        return false;
    };
    let Some(kind) = value.get("kind").and_then(JsonValue::as_str) else {
        return false;
    };
    let expected_hash = blake3::Hash::from_bytes(evidence.payload_digest)
        .to_hex()
        .to_string();
    if value.get("bytes").and_then(JsonValue::as_u64) != Some(evidence.observed_bytes)
        || value.get("hash").and_then(JsonValue::as_str) != Some(expected_hash.as_str())
    {
        return false;
    }

    match kind {
        "json_object" => validate_object_shape(&value, evidence.observed_bytes),
        "json_array" => validate_array_shape(&value, evidence.observed_bytes),
        "null" | "boolean" | "number" | "string" | "opaque" => {
            exact_fields(&value, &["bytes", "hash", "kind"])
        }
        _ => false,
    }
}

fn validate_object_shape(value: &JsonMap<String, JsonValue>, observed_bytes: u64) -> bool {
    if !exact_fields(
        value,
        &["bytes", "hash", "kind", "members", "shape", "truncated"],
    ) {
        return false;
    }
    let Some(members) = value.get("members").and_then(JsonValue::as_u64) else {
        return false;
    };
    if members > observed_bytes {
        return false;
    }
    let Some(shape) = value.get("shape").and_then(JsonValue::as_array) else {
        return false;
    };
    let expected_len = usize::try_from(members)
        .unwrap_or(usize::MAX)
        .min(MAX_DIAGNOSTIC_SHAPE_ITEMS);
    if shape.len() != expected_len
        || value.get("truncated").and_then(JsonValue::as_bool)
            != Some(members > MAX_DIAGNOSTIC_SHAPE_ITEMS as u64)
    {
        return false;
    }
    shape.iter().all(|entry| {
        let JsonValue::Object(entry) = entry else {
            return false;
        };
        exact_fields(entry, &["key_hash", "value_kind"])
            && entry
                .get("key_hash")
                .and_then(JsonValue::as_str)
                .is_some_and(|value| {
                    value.len() == 12
                        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
                        && value.bytes().all(|byte| !byte.is_ascii_uppercase())
                })
            && entry
                .get("value_kind")
                .and_then(JsonValue::as_str)
                .is_some_and(is_json_value_kind)
    })
}

fn validate_array_shape(value: &JsonMap<String, JsonValue>, observed_bytes: u64) -> bool {
    if !exact_fields(
        value,
        &["bytes", "hash", "item_kinds", "items", "kind", "truncated"],
    ) {
        return false;
    }
    let Some(items) = value.get("items").and_then(JsonValue::as_u64) else {
        return false;
    };
    if items > observed_bytes {
        return false;
    }
    let Some(item_kinds) = value.get("item_kinds").and_then(JsonValue::as_array) else {
        return false;
    };
    let expected_len = usize::try_from(items)
        .unwrap_or(usize::MAX)
        .min(MAX_DIAGNOSTIC_SHAPE_ITEMS);
    item_kinds.len() == expected_len
        && value.get("truncated").and_then(JsonValue::as_bool)
            == Some(items > MAX_DIAGNOSTIC_SHAPE_ITEMS as u64)
        && item_kinds
            .iter()
            .all(|kind| kind.as_str().is_some_and(is_json_value_kind))
}

fn exact_fields(value: &JsonMap<String, JsonValue>, fields: &[&str]) -> bool {
    value.len() == fields.len()
        && fields.iter().all(|field| value.contains_key(*field))
        && value.keys().all(|field| fields.contains(&field.as_str()))
}

fn is_json_value_kind(value: &str) -> bool {
    matches!(
        value,
        "null" | "boolean" | "number" | "string" | "array" | "object"
    )
}
