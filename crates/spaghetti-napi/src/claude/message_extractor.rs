//! Claude Code message projection (RFC 006 / Phase B).
//!
//! Maps one Anthropic-envelope JSONL line into the thin columns the core
//! writer stores (`msg_type`, tokens, FTS text, …). Behaviour-identical to
//! the previous inline `build_message_event` helpers in `project_parser`
//! (and to the TS `claudeCodeMessageExtractor` for the same fields).
//!
//! A second agent source would ship its own extractor module; the core
//! writer only ever sees projected columns + `raw_json`.

use serde_json::Value;

use crate::claude::fts_text;
use crate::claude::types::SessionMessage;

/// Thin, queryable projection of one Claude Code transcript line.
///
/// The verbatim JSONL line is kept separately as `IngestEvent::Message.raw_json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageProjection {
    pub msg_type: String,
    pub uuid: Option<String>,
    pub timestamp: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    /// `None` when empty / untyped parse failed — writer treats as `""`.
    pub fts_text: Option<String>,
}

/// Project one JSONL line into stored columns.
///
/// Always returns `Ok` for valid JSON (including unknown `type` values).
/// Returns `Err` only when the line is not JSON at all — matching the
/// parser behaviour of emitting `RecordSkip` for bad lines.
pub fn project_jsonl_line(line: &str) -> Result<MessageProjection, serde_json::Error> {
    // Loose Value first for top-level fields (matches TS
    // `msg as Record<string, unknown>`), so we don't reverse-engineer
    // serde tag renames for `msg_type`.
    let value: Value = serde_json::from_str(line)?;

    let msg_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let uuid = value.get("uuid").and_then(Value::as_str).map(str::to_owned);
    let timestamp = value
        .get("timestamp")
        .and_then(Value::as_str)
        .map(str::to_owned);

    let (input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens) =
        if msg_type == "assistant" {
            extract_tokens(&value)
        } else {
            (0, 0, 0, 0)
        };

    // Typed parse for FTS; failure → still emit the row with no fts blob.
    let fts_text = match serde_json::from_str::<SessionMessage>(line) {
        Ok(msg) => {
            let s = fts_text::extract_message_text(&msg);
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        }
        Err(_) => None,
    };

    Ok(MessageProjection {
        msg_type,
        uuid,
        timestamp,
        input_tokens,
        output_tokens,
        cache_creation_tokens,
        cache_read_tokens,
        fts_text,
    })
}

fn extract_tokens(value: &Value) -> (u64, u64, u64, u64) {
    let Some(usage) = value.get("message").and_then(|m| m.get("usage")) else {
        return (0, 0, 0, 0);
    };
    let pick = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
    (
        pick("input_tokens"),
        pick("output_tokens"),
        pick("cache_creation_input_tokens"),
        pick("cache_read_input_tokens"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // BaseMessageFields required by SessionMessage user/assistant variants.
    const BASE: &str = r#""uuid":"u1","parentUuid":null,"timestamp":"2026-01-01T00:00:00.000Z","sessionId":"s1","cwd":"/tmp","version":"1","gitBranch":"main","isSidechain":false,"userType":"external""#;

    #[test]
    fn projects_assistant_tokens_and_fts() {
        let line = format!(
            r#"{{
                "type": "assistant",
                {BASE},
                "requestId": "r1",
                "message": {{
                    "model": "claude-sonnet",
                    "id": "m1",
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {{ "type": "text", "text": "hello" }},
                        {{ "type": "tool_use", "id": "t1", "name": "Read", "input": {{}} }}
                    ],
                    "stop_reason": "end_turn",
                    "usage": {{
                        "input_tokens": 10,
                        "output_tokens": 5,
                        "cache_creation_input_tokens": 1,
                        "cache_read_input_tokens": 2
                    }}
                }}
            }}"#
        );
        let p = project_jsonl_line(&line).expect("project");
        assert_eq!(p.msg_type, "assistant");
        assert_eq!(p.uuid.as_deref(), Some("u1"));
        assert_eq!(p.input_tokens, 10);
        assert_eq!(p.output_tokens, 5);
        assert_eq!(p.cache_creation_tokens, 1);
        assert_eq!(p.cache_read_tokens, 2);
        let fts = p.fts_text.expect("fts");
        assert!(fts.contains("hello"));
        assert!(fts.contains("[tool:Read]"));
    }

    #[test]
    fn user_plain_text() {
        let line = format!(
            r#"{{
                "type": "user",
                {BASE},
                "message": {{ "role": "user", "content": "hi there" }}
            }}"#
        );
        let p = project_jsonl_line(&line).expect("project");
        assert_eq!(p.msg_type, "user");
        assert_eq!(p.input_tokens, 0);
        assert_eq!(p.fts_text.as_deref(), Some("hi there"));
    }

    #[test]
    fn invalid_json_errors() {
        assert!(project_jsonl_line("not-json").is_err());
    }

    #[test]
    fn assistant_without_usage_or_request_id_still_produces_fts() {
        // API-error assistant line: no `requestId`, no `message.usage`. It must
        // still project to msg_type=assistant with fts text (the typed parse no
        // longer fails on the missing fields), and zero tokens.
        let line = format!(
            r#"{{
                "type": "assistant",
                {BASE},
                "isApiErrorMessage": true,
                "message": {{
                    "model": "claude-opus",
                    "id": "m-err",
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {{ "type": "text", "text": "overloaded, retrying" }},
                        {{ "type": "tool_use", "id": "t1", "name": "Bash", "input": {{}} }}
                    ]
                }}
            }}"#
        );
        let p = project_jsonl_line(&line).expect("project");
        assert_eq!(p.msg_type, "assistant");
        assert_eq!(p.input_tokens, 0);
        assert_eq!(p.output_tokens, 0);
        let fts = p.fts_text.expect("api-error assistant must keep fts text");
        assert!(fts.contains("overloaded, retrying"));
        assert!(fts.contains("[tool:Bash]"));
    }
}
