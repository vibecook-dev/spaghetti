//! JSON-string N-API helpers for committed RFC 012A/012C semantic fixtures.
//!
//! These helpers take UTF-16 JSON source, reject unpaired surrogates, and
//! return UTF-8 JSON strings. They do not expose object-mirror bindings,
//! engine methods, or source/store/query authority.

use napi::bindgen_prelude::*;
use napi_derive::napi;

use crate::semantic_contract::MAX_SEMANTIC_FIXTURE_JSON_BYTES;

fn public_fixture_error(error: impl ToString) -> Error {
    Error::new(
        Status::InvalidArg,
        classify_public_fixture_error(&error.to_string()),
    )
}

fn classify_public_fixture_error(message: &str) -> &'static str {
    if message.contains("must not be empty") {
        "invalid semantic fixture: empty JSON"
    } else if message.contains("unpaired") || message.contains("invalid utf-16") {
        "invalid semantic fixture: unpaired UTF-16"
    } else if message.contains("semantic fixture JSON exceeds") && message.contains("bytes") {
        "invalid semantic fixture: oversized JSON"
    } else if message.contains("exceeds depth")
        || message.contains("exceeds") && message.contains("nodes")
    {
        "invalid semantic fixture: unbounded JSON graph"
    } else if message.contains("unknown field") {
        "invalid semantic fixture: unknown field"
    } else if message.contains("noncanonical integer") {
        "invalid semantic fixture: noncanonical integer"
    } else {
        "invalid semantic fixture"
    }
}

fn utf16_json_to_utf8(json: Utf16String) -> Result<String> {
    if json.len() > MAX_SEMANTIC_FIXTURE_JSON_BYTES {
        return Err(public_fixture_error(format!(
            "semantic fixture JSON exceeds {MAX_SEMANTIC_FIXTURE_JSON_BYTES} bytes"
        )));
    }
    String::from_utf16(&json).map_err(|_| public_fixture_error("invalid utf-16"))
}

/// Parse one committed RFC 012A v1 semantic fixture from a JSON string.
///
/// Returns the same fixture as canonical JSON. The helper does not open a
/// source, store, query, or delivery path.
#[napi(js_name = "parseRfc012aV1Json")]
pub fn parse_rfc012a_v1_json(json: Utf16String) -> Result<String> {
    let json = utf16_json_to_utf8(json)?;
    crate::semantic_contract::parse_rfc012a_v1_json(&json).map_err(public_fixture_error)
}

/// Parse one committed RFC 012C v1 runtime fixture from a JSON string.
///
/// Returns the same fixture as canonical JSON. The helper does not open a
/// source, store, query, or delivery path.
#[napi(js_name = "parseRfc012cRuntimeV1Json")]
pub fn parse_rfc012c_runtime_v1_json(json: Utf16String) -> Result<String> {
    let json = utf16_json_to_utf8(json)?;
    crate::semantic_contract::parse_rfc012c_runtime_v1_json(&json).map_err(public_fixture_error)
}

/// Parse one committed RFC 012C v1 effective-state fixture from a JSON string.
///
/// Returns the same fixture as canonical JSON. The helper does not open a
/// source, store, query, or delivery path.
#[napi(js_name = "parseRfc012cEffectiveStateV1Json")]
pub fn parse_rfc012c_effective_state_v1_json(json: Utf16String) -> Result<String> {
    let json = utf16_json_to_utf8(json)?;
    crate::semantic_contract::parse_rfc012c_effective_state_v1_json(&json)
        .map_err(public_fixture_error)
}

/// Parse one committed RFC 012C v1 user-input interaction fixture from a JSON string.
///
/// Returns the same fixture as canonical JSON. The helper does not open a
/// source, store, query, or delivery path.
#[napi(js_name = "parseRfc012cInteractionV1Json")]
pub fn parse_rfc012c_interaction_v1_json(json: Utf16String) -> Result<String> {
    let json = utf16_json_to_utf8(json)?;
    crate::semantic_contract::parse_rfc012c_interaction_v1_json(&json).map_err(public_fixture_error)
}

/// Parse one committed RFC 012C v1 message fixture from a JSON string.
#[napi(js_name = "parseRfc012cMessageV1Json")]
pub fn parse_rfc012c_message_v1_json(json: Utf16String) -> Result<String> {
    let json = utf16_json_to_utf8(json)?;
    crate::semantic_contract::parse_rfc012c_message_v1_json(&json).map_err(public_fixture_error)
}

/// Parse one committed RFC 012C v1 task fixture from a JSON string.
#[napi(js_name = "parseRfc012cTaskV1Json")]
pub fn parse_rfc012c_task_v1_json(json: Utf16String) -> Result<String> {
    let json = utf16_json_to_utf8(json)?;
    crate::semantic_contract::parse_rfc012c_task_v1_json(&json).map_err(public_fixture_error)
}
