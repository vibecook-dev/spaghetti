//! Codex MessageExtractor — thin projection of one RolloutLine.
//!
//! Behaviour-aligned with `packages/sdk/src/sources/codex/message-extractor.ts`:
//! canonical `response_item` chat, tool-call/result, and readable reasoning
//! records produce projections. Telemetry/event records remain skipped.

use serde_json::Value;

use crate::core::text::truncate_utf16;

/// FTS/preview text cap in UTF-16 code units — matches Claude extractor + TS.
const MAX_TEXT_LENGTH: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageProjection {
    pub msg_type: String,
    pub uuid: Option<String>,
    pub timestamp: Option<String>,
    pub fts_text: Option<String>,
}

/// One parsed `token_count` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenCount {
    pub input: u64,
    pub output: u64,
    pub cache_creation: u64,
    pub cache_read: u64,
    /// True when these came from `last_token_usage` (a per-turn delta); false
    /// when they fell back to cumulative `total_token_usage`. The reader clears
    /// its last-assistant pointer after a total-only count so a subsequent
    /// total-only count isn't re-attributed to the same assistant.
    pub from_last: bool,
}

fn truncate(text: &str) -> &str {
    truncate_utf16(text, MAX_TEXT_LENGTH)
}

fn extract_text(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_owned();
    }
    let Some(arr) = content.as_array() else {
        return String::new();
    };
    let mut parts: Vec<&str> = Vec::new();
    for block in arr {
        let Some(obj) = block.as_object() else {
            continue;
        };
        let ty = obj.get("type").and_then(Value::as_str).unwrap_or("");
        if matches!(ty, "input_text" | "output_text" | "text" | "summary_text") {
            if let Some(t) = obj.get("text").and_then(Value::as_str) {
                parts.push(t);
            }
        }
    }
    parts.join("\n")
}

/// Project one JSONL line. `Ok(None)` = not a chat message (skip).
pub fn project_jsonl_line(line: &str) -> Result<Option<MessageProjection>, serde_json::Error> {
    let value: Value = serde_json::from_str(line)?;
    let obj = match value.as_object() {
        Some(o) => o,
        None => return Ok(None),
    };
    if obj.get("type").and_then(Value::as_str) != Some("response_item") {
        return Ok(None);
    }
    let payload = match obj.get("payload").and_then(Value::as_object) {
        Some(p) => p,
        None => return Ok(None),
    };
    let payload_type = payload.get("type").and_then(Value::as_str).unwrap_or("");
    let (msg_type, text) = if payload_type == "message" {
        (
            payload
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
            payload.get("content").map(extract_text).unwrap_or_default(),
        )
    } else if payload_type == "reasoning" {
        (
            "reasoning".to_owned(),
            payload.get("summary").map(extract_text).unwrap_or_default(),
        )
    } else if payload_type.ends_with("_call_output") || payload_type == "tool_search_output" {
        ("tool_result".to_owned(), String::new())
    } else if payload_type.ends_with("_call") {
        let name = payload
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| payload_type.strip_suffix("_call"))
            .unwrap_or("Unknown Tool")
            .to_owned();
        ("tool_use".to_owned(), name)
    } else {
        return Ok(None);
    };
    let fts = {
        let t = truncate(&text);
        if t.is_empty() {
            None
        } else {
            Some(t.to_owned())
        }
    };
    Ok(Some(MessageProjection {
        msg_type,
        uuid: payload
            .get("id")
            .and_then(Value::as_str)
            .or_else(|| payload.get("call_id").and_then(Value::as_str))
            .map(str::to_owned),
        timestamp: obj
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_owned),
        fts_text: fts,
    }))
}

/// Parse an `event_msg` / `token_count` record. Prefers the per-turn
/// `last_token_usage`; falls back to cumulative `total_token_usage`. The
/// returned [`TokenCount::from_last`] records which source was used so the
/// reader can avoid re-applying a cumulative total to the same assistant.
pub fn parse_token_count(line: &str) -> Option<TokenCount> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("event_msg") {
        return None;
    }
    let payload = value.get("payload")?.as_object()?;
    if payload.get("type").and_then(Value::as_str) != Some("token_count") {
        return None;
    }
    let info = payload.get("info")?.as_object()?;
    let map_usage = |raw: &Value, from_last: bool| -> Option<TokenCount> {
        let o = raw.as_object()?;
        let pick = |k: &str| o.get(k).and_then(Value::as_u64).unwrap_or(0);
        let input = pick("input_tokens");
        let cached = pick("cached_input_tokens");
        let output = pick("output_tokens");
        let reasoning = pick("reasoning_output_tokens");
        Some(TokenCount {
            input,
            output: output + reasoning,
            cache_creation: 0,
            cache_read: cached,
            from_last,
        })
    };
    if let Some(last) = info.get("last_token_usage") {
        if let Some(u) = map_usage(last, true) {
            return Some(u);
        }
    }
    if let Some(total) = info.get("total_token_usage") {
        return map_usage(total, false);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_assistant_message() {
        let line = r#"{
          "timestamp":"2026-01-01T00:00:00.000Z",
          "type":"response_item",
          "payload":{
            "type":"message",
            "role":"assistant",
            "id":"msg_1",
            "content":[{"type":"output_text","text":"hello world"}]
          }
        }"#;
        let p = project_jsonl_line(line).unwrap().expect("message");
        assert_eq!(p.msg_type, "assistant");
        assert_eq!(p.uuid.as_deref(), Some("msg_1"));
        assert_eq!(p.fts_text.as_deref(), Some("hello world"));
    }

    #[test]
    fn extracts_function_call_and_output() {
        let call = r#"{"type":"response_item","payload":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"shell","arguments":"{\"cmd\":\"pwd\"}"}}"#;
        let p = project_jsonl_line(call).unwrap().expect("tool call");
        assert_eq!(p.msg_type, "tool_use");
        assert_eq!(p.uuid.as_deref(), Some("fc_1"));
        assert_eq!(p.fts_text.as_deref(), Some("shell"));

        let output = r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"ok"}}"#;
        let p = project_jsonl_line(output).unwrap().expect("tool result");
        assert_eq!(p.msg_type, "tool_result");
        assert_eq!(p.uuid.as_deref(), Some("call_1"));
        assert_eq!(p.fts_text, None);
    }

    #[test]
    fn extracts_tool_search_output_exception() {
        let output = r#"{"type":"response_item","payload":{"type":"tool_search_output","call_id":"call_search","tools":[{"type":"function","name":"spawn_agent"}]}}"#;
        let p = project_jsonl_line(output)
            .unwrap()
            .expect("tool search result");
        assert_eq!(p.msg_type, "tool_result");
        assert_eq!(p.uuid.as_deref(), Some("call_search"));
        assert_eq!(p.fts_text, None);
    }

    #[test]
    fn extracts_readable_reasoning_summary() {
        let line = r#"{"type":"response_item","payload":{"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"I checked the files."}],"encrypted_content":"opaque"}}"#;
        let p = project_jsonl_line(line).unwrap().expect("reasoning");
        assert_eq!(p.msg_type, "reasoning");
        assert_eq!(p.fts_text.as_deref(), Some("I checked the files."));
    }

    #[test]
    fn parses_token_count() {
        let line = r#"{
          "type":"event_msg",
          "payload":{
            "type":"token_count",
            "info":{
              "last_token_usage":{
                "input_tokens":10,
                "cached_input_tokens":2,
                "output_tokens":5,
                "reasoning_output_tokens":3,
                "total_tokens":20
              }
            }
          }
        }"#;
        let tc = parse_token_count(line).unwrap();
        assert_eq!(tc.input, 10);
        assert_eq!(tc.output, 8); // 5+3
        assert_eq!(tc.cache_creation, 0);
        assert_eq!(tc.cache_read, 2);
        assert!(tc.from_last, "last_token_usage present -> from_last");
    }

    #[test]
    fn parses_total_only_token_count_marks_not_from_last() {
        // No last_token_usage — falls back to cumulative total; from_last=false
        // signals the reader to clear its last-assistant pointer.
        let line = r#"{
          "type":"event_msg",
          "payload":{
            "type":"token_count",
            "info":{
              "total_token_usage":{
                "input_tokens":100,
                "cached_input_tokens":9,
                "output_tokens":40,
                "reasoning_output_tokens":10
              }
            }
          }
        }"#;
        let tc = parse_token_count(line).unwrap();
        assert_eq!(tc.input, 100);
        assert_eq!(tc.output, 50); // 40+10
        assert_eq!(tc.cache_read, 9);
        assert!(!tc.from_last, "total-only fallback -> not from_last");
    }
}
