//! Session-list metadata projected from Claude Code transcript records.

use serde_json::Value;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SessionMetadataProjection {
    pub human_prompt: Option<String>,
    pub ai_title: Option<String>,
    pub custom_title: Option<String>,
}

const SYNTHETIC_USER_PREFIXES: &[&str] = &[
    "<local-command-caveat>",
    "<local-command-stdout>",
    "<command-name>",
    "<command-message>",
    "<task-notification>",
    "<system-reminder>",
    "<ide_opened_file>",
    "<ide_selection>",
];

fn non_empty_string(value: Option<&Value>) -> Option<String> {
    let trimmed = value?.as_str()?.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn first_text_content(record: &Value) -> Option<String> {
    let content = record.get("message")?.get("content")?;
    if content.is_string() {
        return non_empty_string(Some(content));
    }
    content.as_array()?.iter().find_map(|block| {
        (block.get("type").and_then(Value::as_str) == Some("text"))
            .then(|| non_empty_string(block.get("text")))
            .flatten()
    })
}

fn is_synthetic_prompt_text(value: &str) -> bool {
    let trimmed = value.trim();
    let normalized = trimmed.to_ascii_lowercase();
    trimmed.is_empty()
        || SYNTHETIC_USER_PREFIXES
            .iter()
            .any(|prefix| normalized.starts_with(prefix))
}

pub fn normalize_first_prompt(value: &str) -> String {
    if is_synthetic_prompt_text(value) {
        "No prompt".to_owned()
    } else {
        value.to_owned()
    }
}

pub fn extract_human_prompt(record: &Value) -> Option<String> {
    if record.get("type").and_then(Value::as_str) != Some("user")
        || record.get("isMeta").and_then(Value::as_bool) == Some(true)
        || record.get("isSidechain").and_then(Value::as_bool) == Some(true)
        || record.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
        || record
            .get("isVisibleInTranscriptOnly")
            .and_then(Value::as_bool)
            == Some(true)
    {
        return None;
    }

    let text = first_text_content(record)?;
    if is_synthetic_prompt_text(&text) {
        return None;
    }
    Some(crate::core::text::truncate_utf16(&text, 200).to_owned())
}

pub fn project_session_metadata(record: &Value) -> Option<SessionMetadataProjection> {
    match record.get("type").and_then(Value::as_str) {
        Some("ai-title") => {
            non_empty_string(record.get("aiTitle")).map(|ai_title| SessionMetadataProjection {
                ai_title: Some(ai_title),
                ..SessionMetadataProjection::default()
            })
        }
        Some("custom-title") => non_empty_string(record.get("customTitle")).map(|custom_title| {
            SessionMetadataProjection {
                custom_title: Some(custom_title),
                ..SessionMetadataProjection::default()
            }
        }),
        Some("user") => {
            extract_human_prompt(record).map(|human_prompt| SessionMetadataProjection {
                human_prompt: Some(human_prompt),
                ..SessionMetadataProjection::default()
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_local_commands_and_extracts_human_prompt() {
        let caveat = serde_json::json!({
            "type": "user",
            "isMeta": true,
            "message": { "role": "user", "content": "<local-command-caveat>ignore</local-command-caveat>" }
        });
        let command = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": "<command-name>/login</command-name>" }
        });
        let human = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": "Build the product" }
        });
        assert_eq!(extract_human_prompt(&caveat), None);
        assert_eq!(extract_human_prompt(&command), None);
        assert_eq!(
            extract_human_prompt(&human).as_deref(),
            Some("Build the product")
        );
    }

    #[test]
    fn projects_title_kinds_separately() {
        let ai = serde_json::json!({ "type": "ai-title", "aiTitle": "Generated" });
        let custom = serde_json::json!({ "type": "custom-title", "customTitle": "Pinned" });
        assert_eq!(
            project_session_metadata(&ai).unwrap().ai_title.as_deref(),
            Some("Generated")
        );
        assert_eq!(
            project_session_metadata(&custom)
                .unwrap()
                .custom_title
                .as_deref(),
            Some("Pinned")
        );
    }

    #[test]
    fn normalizes_synthetic_index_prompts() {
        assert_eq!(
            normalize_first_prompt("<task-notification>generated</task-notification>"),
            "No prompt"
        );
        assert_eq!(normalize_first_prompt("A real prompt"), "A real prompt");
    }
}
