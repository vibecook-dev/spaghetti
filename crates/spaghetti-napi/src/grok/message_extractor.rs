//! Grok MessageExtractor — rich projection of one chat_history.jsonl line.
//!
//! All known records are retained. Display-time normalization hides internal
//! context, expands embedded assistant calls and pairs tool results by id.

use serde_json::Value;

use crate::core::text::truncate_utf16;

/// FTS/preview text cap in UTF-16 code units — matches the other extractors.
const MAX_TEXT_LENGTH: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageProjection {
    pub msg_type: String,
    pub uuid: Option<String>,
    /// Flattened, truncated FTS/preview text. Empty string when no prose.
    pub fts_text: String,
}

fn truncate(text: &str) -> &str {
    truncate_utf16(text, MAX_TEXT_LENGTH)
}

/// Collect readable text from a bare string or an array of `{ text }` blocks
/// (`type: 'text' | 'summary_text'`, etc.).
fn collect_text(value: &Value) -> String {
    if let Some(s) = value.as_str() {
        return s.to_owned();
    }
    let Some(arr) = value.as_array() else {
        return String::new();
    };
    let mut parts: Vec<&str> = Vec::new();
    for block in arr {
        if let Some(t) = block.get("text").and_then(Value::as_str) {
            parts.push(t);
        }
    }
    parts.join("\n")
}

fn human_user_text(obj: &serde_json::Map<String, Value>) -> Option<String> {
    if obj
        .get("synthetic_reason")
        .and_then(Value::as_str)
        .is_some_and(|reason| !reason.is_empty())
    {
        return None;
    }
    let text = obj.get("content").map(collect_text).unwrap_or_default();
    if let Some(start) = text.find("<user_query>") {
        let body_start = start + "<user_query>".len();
        if let Some(relative_end) = text[body_start..].find("</user_query>") {
            let query = text[body_start..body_start + relative_end].trim();
            return (!query.is_empty()).then(|| query.to_owned());
        }
    }
    let image_count = obj
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("image"))
                .count()
        })
        .unwrap_or(0);
    if image_count > 0 || text.contains("<image_files") {
        return Some(if image_count == 1 {
            "Image attachment".to_owned()
        } else {
            format!("{} image attachments", image_count.max(1))
        });
    }
    if text.contains("<user_info") || text.contains("<system-reminder") {
        return None;
    }
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn assistant_search_text(obj: &serde_json::Map<String, Value>) -> String {
    let mut parts = Vec::new();
    let prose = obj.get("content").map(collect_text).unwrap_or_default();
    if !prose.is_empty() {
        parts.push(prose);
    }
    if let Some(calls) = obj.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            let name = call
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .unwrap_or("Unknown Tool");
            let arguments = call.get("arguments").map(|value| match value.as_str() {
                Some(raw) => serde_json::from_str::<Value>(raw)
                    .map(|parsed| parsed.to_string())
                    .unwrap_or_else(|_| raw.to_owned()),
                None => value.to_string(),
            });
            parts.push(match arguments {
                Some(arguments) if !arguments.is_empty() => format!("{name} {arguments}"),
                _ => name.to_owned(),
            });
        }
    }
    parts.join("\n")
}

/// Project one JSONL line. `Ok(None)` = unknown source record.
pub fn project_jsonl_line(line: &str) -> Result<Option<MessageProjection>, serde_json::Error> {
    let value: Value = serde_json::from_str(line)?;
    let obj = match value.as_object() {
        Some(o) => o,
        None => return Ok(None),
    };
    let ty = obj.get("type").and_then(Value::as_str).unwrap_or("");

    let (msg_type, text_src, uuid) = match ty {
        "system" => ("system", String::new(), None),
        "user" => match human_user_text(obj) {
            Some(text) => ("user", text, None),
            None => ("context", String::new(), None),
        },
        "assistant" => ("assistant", assistant_search_text(obj), None),
        "tool_result" => {
            let text = obj.get("content").map(collect_text).unwrap_or_default();
            let uuid = obj
                .get("tool_call_id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            ("tool_result", text, uuid)
        }
        "reasoning" => {
            let text = obj.get("summary").map(collect_text).unwrap_or_default();
            let uuid = obj.get("id").and_then(Value::as_str).map(str::to_owned);
            ("reasoning", text.trim().to_owned(), uuid)
        }
        "backend_tool_call" => {
            let Some(kind) = obj.get("kind").and_then(Value::as_object) else {
                return Ok(None);
            };
            let name = kind
                .get("tool_type")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .unwrap_or("Backend Tool");
            let action = kind.get("action").map(Value::to_string).unwrap_or_default();
            let uuid = kind.get("id").and_then(Value::as_str).map(str::to_owned);
            (
                "tool_use",
                format!("{name} {action}").trim().to_owned(),
                uuid,
            )
        }
        _ => return Ok(None),
    };

    Ok(Some(MessageProjection {
        msg_type: msg_type.to_owned(),
        uuid,
        fts_text: truncate(&text_src).to_owned(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_system_string_content() {
        let line = r#"{"type":"system","content":"You are Grok."}"#;
        let p = project_jsonl_line(line).unwrap().expect("system");
        assert_eq!(p.msg_type, "system");
        assert_eq!(p.fts_text, "");
        assert!(p.uuid.is_none());
    }

    #[test]
    fn extracts_user_block_array() {
        let line = r#"{"type":"user","content":[{"type":"text","text":"hello grok"}]}"#;
        let p = project_jsonl_line(line).unwrap().expect("user");
        assert_eq!(p.msg_type, "user");
        assert_eq!(p.fts_text, "hello grok");
    }

    #[test]
    fn extracts_assistant_string() {
        let line = r#"{"type":"assistant","content":"I'll look.","tool_calls":[]}"#;
        let p = project_jsonl_line(line).unwrap().expect("assistant");
        assert_eq!(p.msg_type, "assistant");
        assert_eq!(p.fts_text, "I'll look.");
    }

    #[test]
    fn extracts_reasoning_summary() {
        let line = r#"{
          "type":"reasoning",
          "id":"rs_1",
          "summary":[{"type":"summary_text","text":"thinking aloud"}],
          "encrypted_content":"xxx"
        }"#;
        let p = project_jsonl_line(line).unwrap().expect("reasoning");
        assert_eq!(p.msg_type, "reasoning");
        assert_eq!(p.fts_text, "thinking aloud");
        assert_eq!(p.uuid.as_deref(), Some("rs_1"));
    }

    #[test]
    fn extracts_tool_result() {
        let line = r#"{"type":"tool_result","tool_call_id":"c1","content":"a/\nb/"}"#;
        let p = project_jsonl_line(line).unwrap().expect("tool result");
        assert_eq!(p.msg_type, "tool_result");
        assert_eq!(p.uuid.as_deref(), Some("c1"));
        assert_eq!(p.fts_text, "a/\nb/");
    }

    #[test]
    fn extracts_backend_tool_call() {
        let line = r#"{"type":"backend_tool_call","kind":{"tool_type":"web_search","id":"ws1","action":{"type":"search","query":"tokens"}}}"#;
        let p = project_jsonl_line(line)
            .unwrap()
            .expect("backend tool call");
        assert_eq!(p.msg_type, "tool_use");
        assert_eq!(p.uuid.as_deref(), Some("ws1"));
        assert_eq!(
            p.fts_text,
            r#"web_search {"query":"tokens","type":"search"}"#
        );
    }

    #[test]
    fn classifies_synthetic_users_as_context_and_unwraps_queries() {
        let context = project_jsonl_line(
            r#"{"type":"user","synthetic_reason":"system_reminder","content":[{"type":"text","text":"<system-reminder>internal</system-reminder>"}]}"#,
        )
        .unwrap()
        .expect("context");
        assert_eq!(context.msg_type, "context");
        assert_eq!(context.fts_text, "");

        let query = project_jsonl_line(
            r#"{"type":"user","content":[{"type":"text","text":"<user_query>real prompt</user_query>\n<system-reminder>later</system-reminder>"}]}"#,
        )
        .unwrap()
        .expect("user");
        assert_eq!(query.msg_type, "user");
        assert_eq!(query.fts_text, "real prompt");
    }

    #[test]
    fn truncates_long_text() {
        let long = "x".repeat(3_000);
        let line = format!(r#"{{"type":"assistant","content":"{long}"}}"#);
        let p = project_jsonl_line(&line).unwrap().expect("assistant");
        assert_eq!(p.fts_text.len(), MAX_TEXT_LENGTH);
    }
}
