//! FTS text extraction — ported from the `extractTextContent` and
//! `truncate` helpers in `packages/sdk/src/data/ingest-service.ts`.
//!
//! Produces the blob stored in `messages.text_content` and fed to the FTS5
//! virtual table. Behavioural parity with TS is preserved — only
//! `user`, `assistant`, and `summary` variants contribute text; all other
//! variants yield an empty string.
//!
//! Populated in RFC 003 commit 1.6.

use crate::claude::types::content::{
    AssistantContentBlock, ToolResultContent, UserContentBlock, UserMessageContent,
};
use crate::claude::types::SessionMessage;
use crate::core::text::truncate_utf16;

/// Maximum length of extracted text stored per message, measured in UTF-16
/// code units.
///
/// Matches the TS constant `MAX_TEXT_LENGTH = 2_000`, where the TS extractor
/// truncates via `String.prototype.substring` — i.e. UTF-16 code units. The
/// Rust engine counts the same unit (see [`truncate`]) so both engines store
/// the identical FTS/preview blob for text containing multi-byte characters.
pub const MAX_TEXT_LENGTH: usize = 2_000;

/// Truncate `text` to at most `MAX_TEXT_LENGTH` UTF-16 code units, never
/// splitting a `char`. Returns a borrowed slice.
///
/// Mirrors the TS `truncate` helper (`substring(0, MAX_TEXT_LENGTH)`).
pub fn truncate(text: &str) -> &str {
    truncate_utf16(text, MAX_TEXT_LENGTH)
}

/// Extract searchable text content from a [`SessionMessage`] for FTS
/// indexing.
///
/// Handles:
/// - `user` messages: plain string content, or `UserContentBlock[]` where
///   `text` and `tool_result` blocks contribute text (tool_result content
///   may itself be a string or an array of text sub-blocks).
/// - `assistant` messages: `text` blocks contribute their text, `tool_use`
///   blocks contribute a marker of the form `[tool:NAME]`.
/// - `summary` messages: the `summary` field contributes directly.
///
/// All other variants (attachment, progress, system, saved_hook_context,
/// queue_operation, last_prompt, agent_name, custom_title, permission_mode,
/// pr_link, file_history_snapshot) return an empty string — matching the
/// TS behaviour where those discriminator values don't match any branch.
///
/// Parts are joined with `\n` and the result is clamped to
/// `MAX_TEXT_LENGTH` bytes via [`truncate`].
pub fn extract_message_text(msg: &SessionMessage) -> String {
    let mut parts: Vec<&str> = Vec::new();

    match msg {
        SessionMessage::User(user) => match &user.message.content {
            UserMessageContent::Text(s) => parts.push(s.as_str()),
            UserMessageContent::Blocks(blocks) => {
                for block in blocks {
                    match block {
                        UserContentBlock::Text(t) => parts.push(t.text.as_str()),
                        UserContentBlock::ToolResult(tr) => match &tr.content {
                            ToolResultContent::Text(s) => parts.push(s.as_str()),
                            ToolResultContent::Blocks(sub_blocks) => {
                                for sub in sub_blocks {
                                    // TS checks `type === 'text'` && `typeof text === 'string'`.
                                    if sub.kind == "text" {
                                        if let Some(t) = sub.text.as_deref() {
                                            parts.push(t);
                                        }
                                    }
                                }
                            }
                        },
                        // Image / Document blocks contribute no searchable text.
                        UserContentBlock::Image(_) | UserContentBlock::Document(_) => {}
                    }
                }
            }
        },
        SessionMessage::Assistant(asst) => {
            // We need owned strings for the `[tool:NAME]` markers, so
            // switch to an owned-strings accumulator for this branch.
            let mut owned: Vec<String> = Vec::new();
            for block in &asst.message.content {
                match block {
                    AssistantContentBlock::Text(t) => owned.push(t.text.clone()),
                    AssistantContentBlock::ToolUse(tu) => {
                        // TS: `if (toolName) textParts.push(`[tool:${toolName}]`);`
                        // — only emit when the name is non-empty.
                        if !tu.name.is_empty() {
                            owned.push(format!("[tool:{}]", tu.name));
                        }
                    }
                    // Thinking / RedactedThinking are excluded in TS.
                    AssistantContentBlock::Thinking(_)
                    | AssistantContentBlock::RedactedThinking(_) => {}
                }
            }
            return truncate(&owned.join("\n")).to_owned();
        }
        SessionMessage::Summary(summary) => {
            // TS: `if (summary) textParts.push(summary);` — push even though
            // the field is always a `String` in Rust (empty string joins to
            // empty and is a no-op after truncation).
            parts.push(summary.summary.as_str());
        }
        SessionMessage::AiTitle(m) => {
            // TS extractTextContent 'ai-title' arm — index the session title.
            parts.push(m.ai_title.as_str());
        }
        SessionMessage::System(sys) => {
            // TS indexes the top-level `content` string of ANY system message
            // regardless of subtype (missing/unknown subtype included). We read
            // it straight off the message, matching extractTextContent exactly.
            if let Some(c) = sys.content_str() {
                parts.push(c);
            }
        }
        // All other variants contribute nothing to the FTS blob.
        SessionMessage::AgentName(_)
        | SessionMessage::Attachment(_)
        | SessionMessage::CustomTitle(_)
        | SessionMessage::FileHistorySnapshot(_)
        | SessionMessage::PrLink(_)
        | SessionMessage::Progress(_)
        | SessionMessage::PermissionMode(_)
        | SessionMessage::SavedHookContext(_)
        | SessionMessage::Mode(_)
        | SessionMessage::BridgeSession(_)
        | SessionMessage::QueueOperation(_)
        | SessionMessage::LastPrompt(_)
        | SessionMessage::Unknown => {}
    }

    truncate(&parts.join("\n")).to_owned()
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::types::SessionMessage;

    // ─── new message types + unknown fallbacks ─────────────────────────────

    #[test]
    fn unknown_top_level_type_deserializes_to_unknown_not_error() {
        // The whole point of #[serde(other)]: a type this build has never
        // seen must NOT fail the parse (which would null the line's FTS).
        let line = r#"{"type":"totally-new-future-type","sessionId":"s","x":1}"#;
        let msg: SessionMessage = serde_json::from_str(line).expect("unknown type must parse");
        assert!(matches!(msg, SessionMessage::Unknown));
        assert_eq!(extract_message_text(&msg), "");
    }

    #[test]
    fn unknown_system_subtype_deserializes_and_does_not_error() {
        let line = r#"{"type":"system","subtype":"some_new_subtype","sessionId":"s","uuid":"u","timestamp":"t"}"#;
        let msg: SessionMessage =
            serde_json::from_str(line).expect("unknown system subtype must parse");
        assert!(matches!(msg, SessionMessage::System(_)));
    }

    #[test]
    fn ai_title_is_indexed() {
        let line = r#"{"type":"ai-title","aiTitle":"My Session Title","sessionId":"s"}"#;
        let msg: SessionMessage = serde_json::from_str(line).unwrap();
        assert_eq!(extract_message_text(&msg), "My Session Title");
    }

    #[test]
    fn away_summary_content_is_indexed() {
        let line = r#"{"type":"system","subtype":"away_summary","content":"recap prose","sessionId":"s","uuid":"u","timestamp":"t"}"#;
        let msg: SessionMessage = serde_json::from_str(line).unwrap();
        assert_eq!(extract_message_text(&msg), "recap prose");
    }

    #[test]
    fn system_message_missing_subtype_still_indexes_content() {
        // No `subtype` field at all: the old internally-tagged payload failed
        // this parse, nulling FTS. It must now parse AND index `content`.
        let line =
            r#"{"type":"system","content":"heads up","sessionId":"s","uuid":"u","timestamp":"t"}"#;
        let msg: SessionMessage = serde_json::from_str(line).expect("missing subtype must parse");
        assert!(matches!(msg, SessionMessage::System(_)));
        assert_eq!(extract_message_text(&msg), "heads up");
    }

    #[test]
    fn system_message_unknown_subtype_indexes_content() {
        let line = r#"{"type":"system","subtype":"brand_new_kind","content":"future prose","sessionId":"s","uuid":"u","timestamp":"t"}"#;
        let msg: SessionMessage = serde_json::from_str(line).unwrap();
        assert_eq!(extract_message_text(&msg), "future prose");
    }

    #[test]
    fn system_message_non_string_content_is_skipped_not_errored() {
        // A non-string `content` must not fail the parse; it just yields no
        // FTS text (matching the tolerant extractor contract).
        let line = r#"{"type":"system","subtype":"x","content":{"nested":true},"sessionId":"s","uuid":"u","timestamp":"t"}"#;
        let msg: SessionMessage = serde_json::from_str(line).expect("must parse");
        assert_eq!(extract_message_text(&msg), "");
    }

    // ─── truncate ──────────────────────────────────────────────────────────

    #[test]
    fn truncate_short_text_is_unchanged() {
        let s = "hello world";
        assert_eq!(truncate(s), "hello world");
    }

    #[test]
    fn truncate_exactly_at_limit_is_unchanged() {
        let s = "a".repeat(MAX_TEXT_LENGTH);
        let out = truncate(&s);
        assert_eq!(out.len(), MAX_TEXT_LENGTH);
        assert_eq!(out, s.as_str());
    }

    #[test]
    fn truncate_oversized_ascii_cuts_at_limit() {
        let s = "b".repeat(MAX_TEXT_LENGTH + 100);
        let out = truncate(&s);
        assert_eq!(out.len(), MAX_TEXT_LENGTH);
    }

    #[test]
    fn truncate_counts_utf16_units_not_bytes() {
        // '€' is 3 UTF-8 bytes but a single UTF-16 code unit. With (MAX-1)
        // ASCII chars before it, the '€' lands at unit MAX and therefore FITS
        // — a byte-based cut would have dropped it. `tail` exceeds the cap.
        let prefix = "a".repeat(MAX_TEXT_LENGTH - 1);
        let s = format!("{prefix}€tail");
        let out = truncate(&s);
        assert_eq!(out, format!("{prefix}€"));
        assert!(!out.contains("tail"));
    }

    #[test]
    fn truncate_never_splits_astral_char() {
        // '😀' is 2 UTF-16 code units. With (MAX-1) ASCII chars before it,
        // including it would reach MAX+1 units, so it is dropped whole rather
        // than emitting a lone surrogate.
        let prefix = "a".repeat(MAX_TEXT_LENGTH - 1);
        let s = format!("{prefix}😀tail");
        let out = truncate(&s);
        assert_eq!(out, prefix.as_str());
    }

    #[test]
    fn truncate_empty_is_empty() {
        assert_eq!(truncate(""), "");
    }

    // ─── extract_message_text ──────────────────────────────────────────────

    // Shared BaseMessageFields JSON fragment to keep fixtures compact.
    const BASE: &str = r#"
        "uuid": "u1",
        "timestamp": "2024-01-01T00:00:00Z",
        "sessionId": "s1",
        "cwd": "/tmp",
        "version": "1",
        "gitBranch": "main",
        "isSidechain": false,
        "userType": "external"
    "#;

    #[test]
    fn extract_user_plain_string_content() {
        let json = format!(
            r#"{{
                "type": "user",
                {BASE},
                "message": {{
                    "role": "user",
                    "content": "hello there"
                }}
            }}"#
        );
        let msg: SessionMessage = serde_json::from_str(&json).expect("parse user plain");
        assert_eq!(extract_message_text(&msg), "hello there");
    }

    #[test]
    fn numeric_image_paste_ids_do_not_null_the_fts_blob() {
        // Regression: Claude Code writes `imagePasteIds` as NUMBERS, but the
        // TS type said `string[]` and the Rust port mirrored it as
        // `Vec<String>`. Serde failed the whole record, so a user message that
        // pasted an image lost its text from `messages.text_content` (hence
        // from search) and was dropped from subagent transcripts entirely.
        // 21 messages + 54 subagent transcripts on a 113-project corpus.
        let json = format!(
            r#"{{
                "type": "user",
                {BASE},
                "imagePasteIds": [1, 2],
                "message": {{
                    "role": "user",
                    "content": [
                        {{ "type": "text", "text": "look at this" }},
                        {{ "type": "image", "source": {{
                            "type": "base64", "media_type": "image/png", "data": "AA=="
                        }}}}
                    ]
                }}
            }}"#
        );
        let msg: SessionMessage =
            serde_json::from_str(&json).expect("numeric imagePasteIds must still parse as user");
        assert!(matches!(msg, SessionMessage::User(_)), "must be a User, not Unknown");
        assert_eq!(extract_message_text(&msg), "look at this");
    }

    #[test]
    fn string_image_paste_ids_still_parse() {
        // The union is deliberate — older transcripts may carry strings.
        let json = format!(
            r#"{{
                "type": "user",
                {BASE},
                "imagePasteIds": ["a", "b"],
                "message": {{ "role": "user", "content": "hi" }}
            }}"#
        );
        let msg: SessionMessage = serde_json::from_str(&json).expect("string ids must parse");
        assert_eq!(extract_message_text(&msg), "hi");
    }

    #[test]
    fn attachment_with_array_content_parses() {
        // Regression: `attachment.content` is a string on most attachments and
        // an array of content blocks on others. Typing it `Option<String>`
        // failed 3,754 of 89,559 real corpus lines — silently, because the
        // caller swallows the error. Attachments contribute no FTS text either
        // way, so the damage was confined to transcripts that drop unparseable
        // lines, but the failure mode is the same one as `imagePasteIds`.
        let json = format!(
            r#"{{
                "type": "attachment",
                {BASE},
                "attachment": {{
                    "type": "new_directory",
                    "content": [
                        {{ "type": "text", "text": "block one" }},
                        {{ "type": "text", "text": "block two" }}
                    ]
                }}
            }}"#
        );
        let msg: SessionMessage =
            serde_json::from_str(&json).expect("array attachment content must parse");
        assert!(
            matches!(msg, SessionMessage::Attachment(_)),
            "must be an Attachment, not Unknown"
        );
        // Attachments contribute nothing to FTS — the point is that the record
        // parses at all, so the line is not dropped by callers.
        assert_eq!(extract_message_text(&msg), "");
    }

    #[test]
    fn attachment_with_string_content_still_parses() {
        let json = format!(
            r#"{{
                "type": "attachment",
                {BASE},
                "attachment": {{ "type": "hook", "content": "plain string" }}
            }}"#
        );
        let msg: SessionMessage = serde_json::from_str(&json).expect("string content must parse");
        assert!(matches!(msg, SessionMessage::Attachment(_)));
    }

    #[test]
    fn extract_user_content_blocks() {
        // Mix text + tool_result (string) + tool_result (array of text blocks)
        // + image (ignored).
        let json = format!(
            r#"{{
                "type": "user",
                {BASE},
                "message": {{
                    "role": "user",
                    "content": [
                        {{ "type": "text", "text": "first" }},
                        {{ "type": "tool_result", "tool_use_id": "t1", "content": "second" }},
                        {{ "type": "tool_result", "tool_use_id": "t2", "content": [
                            {{ "type": "text", "text": "third" }},
                            {{ "type": "image", "source": {{}} }},
                            {{ "type": "text", "text": "fourth" }}
                        ]}},
                        {{ "type": "image", "source": {{
                            "type": "base64", "media_type": "image/png", "data": "AA=="
                        }}}}
                    ]
                }}
            }}"#
        );
        let msg: SessionMessage = serde_json::from_str(&json).expect("parse user blocks");
        assert_eq!(extract_message_text(&msg), "first\nsecond\nthird\nfourth");
    }

    #[test]
    fn extract_assistant_mixed_text_and_tool_use() {
        // Plus a thinking block that must be ignored.
        let json = format!(
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
                        {{ "type": "thinking", "thinking": "hidden" }},
                        {{ "type": "text", "text": "reasoning output" }},
                        {{ "type": "tool_use", "id": "tu1", "name": "Read", "input": {{"path": "/x"}} }},
                        {{ "type": "text", "text": "done" }}
                    ],
                    "stop_reason": "end_turn",
                    "usage": {{
                        "input_tokens": 10,
                        "output_tokens": 20,
                        "cache_creation_input_tokens": 0,
                        "cache_read_input_tokens": 0
                    }}
                }}
            }}"#
        );
        let msg: SessionMessage = serde_json::from_str(&json).expect("parse assistant");
        assert_eq!(
            extract_message_text(&msg),
            "reasoning output\n[tool:Read]\ndone"
        );
    }

    #[test]
    fn extract_summary_message() {
        let json = r#"{
            "type": "summary",
            "summary": "this is the summary text",
            "leafUuid": "leaf-1"
        }"#;
        let msg: SessionMessage = serde_json::from_str(json).expect("parse summary");
        assert_eq!(extract_message_text(&msg), "this is the summary text");
    }

    #[test]
    fn extract_truncates_long_output() {
        // Build a user message whose plain-string content exceeds the cap.
        let big = "x".repeat(MAX_TEXT_LENGTH + 500);
        let json = format!(
            r#"{{
                "type": "user",
                {BASE},
                "message": {{
                    "role": "user",
                    "content": {big:?}
                }}
            }}"#
        );
        let msg: SessionMessage = serde_json::from_str(&json).expect("parse big user");
        let out = extract_message_text(&msg);
        assert_eq!(out.len(), MAX_TEXT_LENGTH);
        assert!(out.chars().all(|c| c == 'x'));
    }
}
