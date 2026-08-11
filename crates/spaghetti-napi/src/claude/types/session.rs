//! SessionMessage enum and its variants.
//!
//! Mirrors the TS `SessionMessage` discriminated union from
//! `packages/sdk/src/types/projects.ts`. The outer discriminator is the
//! `type` field. Some variants (e.g. `user`, `assistant`, attachment, system,
//! progress) also carry the `BaseMessageFields` — we flatten those via a
//! shared struct referenced from each variant.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::content::{AssistantMessagePayload, UserMessagePayload};

// ─────────────────────────────────────────────────────────────────────────
// SessionMessage (outer discriminated union)
// ─────────────────────────────────────────────────────────────────────────

/// One line of a session JSONL file. TS `SessionMessage`.
///
/// Uses `#[serde(tag = "type")]` to dispatch on the `type` field. Unknown
/// types would fail to deserialize — callers in the ingest pipeline wrap
/// per-line parsing in error handlers, so that's acceptable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum SessionMessage {
    AgentName(AgentNameMessage),
    Attachment(AttachmentMessage),
    CustomTitle(CustomTitleMessage),
    FileHistorySnapshot(FileHistorySnapshotMessage),
    PrLink(PrLinkMessage),
    Progress(ProgressMessage),
    PermissionMode(PermissionModeMessage),
    #[serde(rename = "saved_hook_context")]
    SavedHookContext(SavedHookContextMessage),
    User(UserMessage),
    Assistant(AssistantMessage),
    #[serde(rename = "system")]
    System(SystemMessage),
    #[serde(rename = "summary")]
    Summary(SummaryMessage),
    QueueOperation(QueueOperationMessage),
    LastPrompt(LastPromptMessage),
    AiTitle(AiTitleMessage),
    Mode(ModeMessage),
    BridgeSession(BridgeSessionMessage),
    /// Any `type` value this build doesn't model. Without this backstop
    /// a message type newly introduced by Claude Code fails the typed
    /// parse — which nulls the line's FTS text (the raw JSONL line is
    /// still stored verbatim; only the typed projection is a no-op).
    #[serde(other)]
    Unknown,
}

// ─────────────────────────────────────────────────────────────────────────
// Shared BaseMessageFields — flattened into variants that include it
// ─────────────────────────────────────────────────────────────────────────

/// Fields shared by JSONL lines that live inside a threaded conversation.
/// TS `BaseMessageFields`. Included via `#[serde(flatten)]` in the variants
/// that need it (user, assistant, attachment, progress, system, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BaseMessageFields {
    pub uuid: String,
    #[serde(default)]
    pub parent_uuid: Option<String>,
    pub timestamp: String,
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub git_branch: String,
    #[serde(default)]
    pub is_sidechain: bool,
    /// TS literal `'external'`; kept as `String` for forward compat.
    #[serde(default)]
    pub user_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────
// Variant: agent-name
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentNameMessage {
    pub agent_name: String,
    pub session_id: String,
}

// ─────────────────────────────────────────────────────────────────────────
// Variant: custom-title
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomTitleMessage {
    pub custom_title: String,
    pub session_id: String,
}

/// `ai-title` — model-generated session title (additive to custom-title).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiTitleMessage {
    #[serde(default)]
    pub ai_title: String,
    #[serde(default)]
    pub session_id: String,
}

/// `mode` — session-mode marker (e.g. `mode: "normal"`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeMessage {
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub session_id: String,
}

/// `bridge-session` — remote (mobile/web) peer bridge marker.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeSessionMessage {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub bridge_session_id: String,
    #[serde(default)]
    pub last_sequence_num: f64,
}

// ─────────────────────────────────────────────────────────────────────────
// Variant: permission-mode
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionModeMessage {
    pub permission_mode: String,
    pub session_id: String,
}

// ─────────────────────────────────────────────────────────────────────────
// Variant: pr-link
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrLinkMessage {
    pub session_id: String,
    pub pr_number: u64,
    pub pr_url: String,
    pub pr_repository: String,
    pub timestamp: String,
}

// ─────────────────────────────────────────────────────────────────────────
// Variant: attachment
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentMessage {
    #[serde(flatten)]
    pub base: BaseMessageFields,
    pub attachment: AttachmentPayload,
}

/// Catch-all attachment payload. TS uses a loose shape with `[key: string]:
/// unknown`, so we only type the fields we care about and stash the rest in
/// `extra` via `serde(flatten)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentPayload {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_name: Option<String>,
    #[serde(default, rename = "toolUseID", skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_event: Option<String>,
    /// Never read. String on most attachments but an array of content blocks
    /// on others — `Option<String>` failed 3,754 of 89,559 real corpus lines
    /// (4.2%), each one silently. Same reasoning as `image_paste_ids`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
    #[serde(flatten, default)]
    pub extra: serde_json::Map<String, Value>,
}

// ─────────────────────────────────────────────────────────────────────────
// Variant: file-history-snapshot
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHistorySnapshotMessage {
    pub message_id: String,
    #[serde(default)]
    pub is_snapshot_update: bool,
    pub snapshot: FileHistorySnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHistorySnapshot {
    pub message_id: String,
    pub timestamp: String,
    #[serde(default)]
    pub tracked_file_backups: std::collections::HashMap<String, FileBackupEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileBackupEntry {
    pub backup_file_name: Option<String>,
    pub version: u64,
    pub backup_time: String,
}

// ─────────────────────────────────────────────────────────────────────────
// Variant: user
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMessage {
    #[serde(flatten)]
    pub base: BaseMessageFields,
    pub message: UserMessagePayload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_metadata: Option<ThinkingMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todos: Option<Vec<TodoItem>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_result: Option<ToolUseResult>,
    #[serde(
        default,
        rename = "sourceToolAssistantUUID",
        skip_serializing_if = "Option::is_none"
    )]
    pub source_tool_assistant_uuid: Option<String>,
    #[serde(
        default,
        rename = "sourceToolUseID",
        skip_serializing_if = "Option::is_none"
    )]
    pub source_tool_use_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_meta: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_compact_summary: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_visible_in_transcript_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_source: Option<String>,
    /// Never read — carried only to mirror the JSONL shape.
    ///
    /// Declared `string[]` in `packages/sdk/src/types/claude/projects.ts`, but
    /// Claude Code actually writes **numbers** (`"imagePasteIds": [1]`). TS
    /// erases its types at runtime so the wrong annotation costs it nothing;
    /// serde is strict, so `Vec<String>` failed the whole `user` record and
    /// silently emptied its FTS blob and dropped it from subagent transcripts.
    /// Untyped `Value` because an unread field must never fail a record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_paste_ids: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triggers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_thinking_tokens: Option<u64>,
}

/// Inline todo item — matches TS `TodoItem` inside a `UserMessage`.
/// The standalone `TodoFile` items have the same shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoItem {
    pub content: String,
    /// TS: `'pending' | 'in_progress' | 'completed'`. Kept as `String`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
}

/// TS `toolUseResult?: string | object`.
///
/// The object half varies per tool — Read emits `{type, file:{...}}`,
/// Bash emits `{stdout, stderr, interrupted, ...}`, Edit emits diff
/// shapes, and so on. Nothing downstream introspects it, so it stays
/// opaque. An earlier strict struct here (Read's shape only) made typed
/// deserialization fail for ANY session line whose tool result didn't
/// match it, which nulled that message's FTS text — search over tool
/// output silently missed on the Rust engine (2026-07 audit).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolUseResult {
    Text(String),
    Object(Value),
}

// ─────────────────────────────────────────────────────────────────────────
// Variant: assistant
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    #[serde(flatten)]
    pub base: BaseMessageFields,
    /// Absent on some API-error assistant lines; a missing `requestId` must
    /// not fail the typed parse (which would null FTS + drop the line from
    /// subagent transcripts). Defaults to an empty string.
    #[serde(default)]
    pub request_id: String,
    pub message: AssistantMessagePayload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_api_error_message: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_error: Option<String>,
    /// HTTP status of the API error that produced this line (e.g. 429, 529).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_error_status: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_name: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────
// Variant: system
// ─────────────────────────────────────────────────────────────────────────

/// System messages carry a `subtype` that further discriminates them, but the
/// only field the ingest pipeline reads is the top-level `content` prose (for
/// FTS, regardless of subtype — matching the TS extractor). We therefore keep
/// `base` / `level` / `is_meta` typed and stash everything else (`subtype`,
/// `content`, and any subtype-specific payload) verbatim in `extra`.
///
/// This deliberately does NOT model `subtype` as an internally-tagged enum:
/// such an enum requires the `subtype` field to be present, so a system line
/// with a *missing* `subtype` failed the whole parse (and `#[serde(other)]`
/// only catches unknown *values*, never absence) — nulling the line's FTS
/// text. The flat shape parses a missing or unknown subtype without error and
/// round-trips the extra keys.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemMessage {
    #[serde(flatten)]
    pub base: BaseMessageFields,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_meta: Option<bool>,
    /// `subtype`, `content`, and any subtype-specific fields, kept verbatim so
    /// re-serialization (subagent transcripts) preserves them.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl SystemMessage {
    /// The top-level `content` prose string, when present and a string.
    ///
    /// TS indexes this for FTS on any system message regardless of subtype
    /// (`away_summary` recap, `local_command`, compact boundaries, …). A
    /// non-string `content` yields `None` rather than failing.
    pub fn content_str(&self) -> Option<&str> {
        self.extra.get("content").and_then(|v| v.as_str())
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Variant: progress
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressMessage {
    #[serde(flatten)]
    pub base: BaseMessageFields,
    pub data: ProgressData,
    #[serde(default, rename = "toolUseID")]
    pub tool_use_id: String,
    #[serde(default, rename = "parentToolUseID")]
    pub parent_tool_use_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProgressData {
    HookProgress(HookProgress),
    BashProgress(BashProgress),
    AgentProgress(AgentProgress),
    McpProgress(McpProgress),
    QueryUpdate(QueryUpdate),
    SearchResultsReceived(SearchResultsReceived),
    WaitingForTask(WaitingForTask),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookProgress {
    #[serde(default)]
    pub hook_event: String,
    #[serde(default)]
    pub hook_name: String,
    #[serde(default)]
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashProgress {
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub full_output: String,
    #[serde(default)]
    pub elapsed_time_seconds: f64,
    #[serde(default)]
    pub total_lines: u64,
}

/// AgentProgress.message is a nested user/assistant snapshot; we keep it as
/// raw JSON since ingest doesn't introspect it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProgress {
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub normalized_messages: Vec<Value>,
    #[serde(default)]
    pub message: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpProgress {
    #[serde(default)]
    pub server_name: String,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_time_ms: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryUpdate {
    #[serde(default)]
    pub query: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResultsReceived {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub result_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitingForTask {
    #[serde(default)]
    pub task_description: String,
    #[serde(default)]
    pub task_type: String,
}

// ─────────────────────────────────────────────────────────────────────────
// Variant: saved_hook_context
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedHookContextMessage {
    #[serde(flatten)]
    pub base: BaseMessageFields,
    #[serde(default)]
    pub content: Vec<String>,
    #[serde(default)]
    pub hook_name: String,
    #[serde(default)]
    pub hook_event: String,
    #[serde(default, rename = "toolUseID")]
    pub tool_use_id: String,
}

// ─────────────────────────────────────────────────────────────────────────
// Variant: summary
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryMessage {
    #[serde(default)]
    pub summary: String,
    /// Some summary lines omit `leafUuid`; a missing value must not fail the
    /// parse (which would null the summary's FTS text). Defaults to empty.
    #[serde(default)]
    pub leaf_uuid: String,
}

// ─────────────────────────────────────────────────────────────────────────
// Variant: queue-operation
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueOperationMessage {
    /// TS: `'enqueue' | 'dequeue' | 'popAll' | 'remove'`. Kept as `String`.
    pub operation: String,
    pub timestamp: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────
// Variant: last-prompt
// ─────────────────────────────────────────────────────────────────────────

/// Modelled from the real record rather than from `BaseMessageFields`.
///
/// This used to flatten the base (requiring `uuid`, `timestamp`, `sessionId`)
/// and require `lastPrompt`. Real records carry none of `uuid`/`timestamp`/`cwd`
/// and make both payload fields optional — the three observed shapes are
/// `{sessionId, type}` plus `lastPrompt` and/or `leafUuid`. So every one of the
/// 515 `last-prompt` lines in a 113-project corpus failed the typed parse,
/// silently. Nothing reads this variant, so nothing was lost, but it is the
/// same defect as the `attachment.content` and `imagePasteIds` mismatches: a
/// shape asserted from documentation instead of from data.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastPromptMessage {
    #[serde(default)]
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaf_uuid: Option<String>,
}
