/**
 * TypeScript interfaces for all data structures found in:
 *   ~/.claude/projects/
 */

// ═══════════════════════════════════════════════════════════════════════════════
// SESSIONS INDEX
// ═══════════════════════════════════════════════════════════════════════════════

export interface SessionsIndex {
  version: number;
  originalPath?: string;
  entries: SessionIndexEntry[];
}

export interface SessionIndexEntry {
  sessionId: string;
  fullPath: string;
  fileMtime: number;
  firstPrompt: string;
  summary: string;
  messageCount: number;
  created: string;
  modified: string;
  gitBranch: string;
  projectPath: string;
  isSidechain: boolean;
}

// ═══════════════════════════════════════════════════════════════════════════════
// SESSION JSONL — BASE MESSAGE
// ═══════════════════════════════════════════════════════════════════════════════

export interface BaseMessageFields {
  type: SessionMessageType;
  uuid: string;
  parentUuid: string | null;
  timestamp: string;
  sessionId: string;
  cwd: string;
  version: string;
  gitBranch: string;
  isSidechain: boolean;
  userType: 'external';
  slug?: string;
  permissionMode?: string;
  entrypoint?: string;
  /**
   * Which surface produced the turn — `'bg'` marks a background agent.
   * Present on every envelope type, not just one.
   */
  sessionKind?: string;
  /**
   * Snake-case duplicate of {@link sessionId}. Claude Code emits both;
   * modelled so the field is not reported as unmodelled, but prefer
   * `sessionId`.
   */
  session_id?: string;
}

export type SessionMessageType =
  | 'agent-name'
  | 'ai-title'
  | 'attachment'
  | 'bridge-session'
  | 'custom-title'
  | 'file-history-delta'
  | 'file-history-snapshot'
  | 'frame-link'
  | 'mode'
  | 'pr-link'
  | 'progress'
  | 'permission-mode'
  | 'saved_hook_context'
  | 'user'
  | 'assistant'
  | 'system'
  | 'summary'
  | 'queue-operation'
  | 'last-prompt'
  | 'atis-latch';

export type SessionMessage =
  | AgentNameMessage
  | AiTitleMessage
  | AttachmentMessage
  | BridgeSessionMessage
  | CustomTitleMessage
  | FileHistoryDeltaMessage
  | FileHistorySnapshotMessage
  | FrameLinkMessage
  | ModeMessage
  | PrLinkMessage
  | ProgressMessage
  | PermissionModeMessage
  | SavedHookContextMessage
  | UserMessage
  | AssistantMessage
  | SystemMessage
  | SummaryMessage
  | QueueOperationMessage
  | LastPromptMessage
  | AtisLatchMessage;

export interface AgentNameMessage {
  type: 'agent-name';
  agentName: string;
  sessionId: string;
}

export interface CustomTitleMessage {
  type: 'custom-title';
  customTitle: string;
  sessionId: string;
}

/**
 * Model-generated session title. Additive to (not a rename of)
 * `custom-title` — both occur; `ai-title` is the auto-summarised title
 * shown in the session list and is indexed into FTS.
 */
export interface AiTitleMessage {
  type: 'ai-title';
  aiTitle: string;
  sessionId: string;
}

/** Session-mode marker (e.g. `mode: 'normal'`). */
export interface ModeMessage {
  type: 'mode';
  mode: string;
  sessionId: string;
}

/** Opaque anti-tamper latch token Claude Code stamps into the transcript
 * (first observed 2026-08-24). Carried, never interpreted. */
export interface AtisLatchMessage {
  type: 'atis-latch';
  atis: string;
  sessionId: string;
}

/** Emitted when a session is bridged to a remote (mobile/web) peer. */
export interface BridgeSessionMessage {
  type: 'bridge-session';
  sessionId: string;
  bridgeSessionId: string;
  lastSequenceNum: number;
  /**
   * Sensitive native bridge ownership metadata. These identifiers are exposed
   * only as part of the agent-native message shape; they are not canonical
   * Spaghetti identities and must not enter FTS, logs, telemetry, or runtime
   * semantic events.
   */
  ownerAccountUuid?: string;
  ownerOrganizationUuid?: string;
}

export interface PermissionModeMessage {
  type: 'permission-mode';
  permissionMode: string;
  sessionId: string;
}

export interface PrLinkMessage {
  type: 'pr-link';
  sessionId: string;
  prNumber: number;
  prUrl: string;
  prRepository: string;
  timestamp: string;
}

export interface AttachmentMessage extends BaseMessageFields {
  type: 'attachment';
  attachment: {
    type: string;
    hookName?: string;
    toolUseID?: string;
    hookEvent?: string;
    content?: string;
    stdout?: string;
    stderr?: string;
    exitCode?: number;
    command?: string;
    durationMs?: number;
    [key: string]: unknown;
  };
}

// ═══════════════════════════════════════════════════════════════════════════════
// TYPE: file-history-snapshot
// ═══════════════════════════════════════════════════════════════════════════════

export interface FileHistorySnapshotMessage {
  type: 'file-history-snapshot';
  messageId: string;
  isSnapshotUpdate: boolean;
  snapshot: FileHistorySnapshot;
}

/**
 * Incremental counterpart to {@link FileHistorySnapshotMessage}: one
 * tracked file changing, rather than a whole snapshot. Emitted between
 * snapshots, referring back to the snapshot it amends via
 * `snapshotMessageId`.
 */
export interface FileHistoryDeltaMessage {
  type: 'file-history-delta';
  messageId: string;
  /** The `file-history-snapshot` message this delta amends. */
  snapshotMessageId: string;
  /** Path relative to the project root, spelled with the host separator. */
  trackingPath: string;
  backup: FileBackupEntry;
}

/** Link from a transcript to an artifact/frame rendered by Claude Code. */
export interface FrameLinkMessage {
  type: 'frame-link';
  sessionId: string;
  path: string;
  frameUrl: string;
  title: string;
  timestamp: string;
}

export interface FileHistorySnapshot {
  messageId: string;
  timestamp: string;
  trackedFileBackups: Record<string, FileBackupEntry>;
}

export interface FileBackupEntry {
  backupFileName: string | null;
  version: number;
  backupTime: string;
  /**
   * Absolute directory the tracked file lives in. Observed on
   * `file-history-delta` backups; optional because the snapshot form has
   * not been seen carrying it.
   */
  realParentDir?: string;
}

// ═══════════════════════════════════════════════════════════════════════════════
// TYPE: user
// ═══════════════════════════════════════════════════════════════════════════════

export interface UserMessage extends BaseMessageFields {
  type: 'user';
  message: UserMessagePayload;
  origin?: {
    kind: string;
    [key: string]: unknown;
  };
  thinkingMetadata?: ThinkingMetadata;
  todos?: TodoItem[];
  permissionMode?: string;
  toolUseResult?: string | ToolUseResultObject;
  sourceToolAssistantUUID?: string;
  sourceToolUseID?: string;
  agentId?: string;
  isMeta?: boolean;
  isCompactSummary?: boolean;
  isVisibleInTranscriptOnly?: boolean;
  planContent?: string;
  promptId?: string;
  /** How the prompt was entered (e.g. 'typed', 'paste'). */
  promptSource?: string;
  /**
   * Observed as **numbers** (`[1]`, `[2]`) in real transcripts — this was
   * declared `string[]`, which TS never checks at runtime but the Rust port
   * mirrored into a strict `Vec<String>` that failed the whole record. Kept
   * as a union because older transcripts may carry either.
   */
  imagePasteIds?: Array<string | number>;
  teamName?: string;
  /**
   * Newline-delimited JSON the classifier prepends to the prompt (git
   * status and similar). A raw string, not parsed JSON — it may hold
   * several concatenated objects.
   */
  classifierMetaLines?: string;
  /** API message id of the assistant turn this prompt interrupted. */
  interruptedMessageId?: string;
  /** Why a tool call was refused (e.g. `'automode-blocked'`). */
  toolDenialKind?: string;
  /** Queue lane selected for this user turn. */
  queuePriority?: string;
  /** Free-form feedback attached to the turn by the client UI. */
  userFeedback?: string;
}

export interface UserMessagePayload {
  role: 'user';
  content: string | UserContentBlock[];
}

export interface ThinkingMetadata {
  level?: string;
  disabled?: boolean;
  triggers?: string[];
  maxThinkingTokens?: number;
}

export interface TodoItem {
  content: string;
  status: 'pending' | 'in_progress' | 'completed';
  activeForm?: string;
}

export type UserContentBlock = ToolResultBlock | UserTextBlock | DocumentBlock | ImageBlock;

export interface ToolResultBlock {
  type: 'tool_result';
  tool_use_id: string;
  content: string | Array<{ type: string; text?: string; [key: string]: unknown }>;
  is_error?: boolean;
}

export interface UserTextBlock {
  type: 'text';
  text: string;
}

export interface DocumentBlock {
  type: 'document';
  source: {
    type: 'base64';
    media_type: string;
    data: string;
  };
}

export interface ImageBlock {
  type: 'image';
  source: {
    type: 'base64';
    media_type: string;
    data: string;
  };
}

export interface ToolUseResultObject {
  type: 'text';
  file: {
    filePath: string;
    content: string;
    numLines: number;
    startLine: number;
    totalLines: number;
  };
}

// ═══════════════════════════════════════════════════════════════════════════════
// TYPE: assistant
// ═══════════════════════════════════════════════════════════════════════════════

export interface AssistantMessage extends BaseMessageFields {
  type: 'assistant';
  requestId: string;
  message: AssistantMessagePayload;
  agentId?: string;
  error?: string;
  isApiErrorMessage?: boolean;
  apiError?: string;
  /** HTTP status of the API error that produced this line (e.g. 429, 529). */
  apiErrorStatus?: number;
  /**
   * Raw error body for the failed request, as `"<status> <json>"` — e.g.
   * `429 {"type":"error","error":{"type":"rate_limit_error",...}}`. Stored
   * verbatim, so it is a string rather than a parsed object.
   */
  errorDetails?: string;
  /**
   * True when the stream was cut off partway through, so `message.content`
   * holds a partial turn.
   */
  isAbortedMidStream?: boolean;
  teamName?: string;
  /**
   * Provenance for a turn produced on behalf of an MCP tool or a skill,
   * rather than directly by the model. Set together as applicable:
   * `attributionMcpServer` + `attributionMcpTool` name the MCP call
   * (e.g. `'claude-in-chrome'` / `'tabs_context_mcp'`);
   * `attributionSkill` names the invoked skill (e.g. `'deep-research'`).
   */
  attributionMcpServer?: string;
  attributionMcpTool?: string;
  attributionSkill?: string;
  attributionPlugin?: string;
  /** Reasoning-effort tier the turn ran at (e.g. `'low'`, `'xhigh'`). */
  effort?: string;
}

export interface AssistantMessagePayload {
  model: string;
  id: string;
  type: 'message';
  role: 'assistant';
  content: AssistantContentBlock[];
  stop_reason: 'end_turn' | 'tool_use' | 'stop_sequence' | 'max_tokens' | null;
  stop_sequence: string | null;
  usage: TokenUsage;
  context_management?: ContextManagement | null;
  container?: unknown;
}

export type AssistantContentBlock =
  | ThinkingBlock
  | RedactedThinkingBlock
  | AssistantTextBlock
  | ToolUseBlock
  | FallbackBlock;

export interface ThinkingBlock {
  type: 'thinking';
  thinking: string;
  signature?: string;
}

export interface RedactedThinkingBlock {
  type: 'redacted_thinking';
  data: string;
}

export interface AssistantTextBlock {
  type: 'text';
  text: string;
}

export interface ToolUseBlock {
  type: 'tool_use';
  id: string;
  name: ToolName;
  input: Record<string, unknown>;
}

/** Records an automatic model transition inside an assistant response. */
export interface FallbackBlock {
  type: 'fallback';
  from: { model: string; [key: string]: unknown };
  to: { model: string; [key: string]: unknown };
}

export type ToolName =
  | 'Read'
  | 'Write'
  | 'Edit'
  | 'Glob'
  | 'Grep'
  | 'Bash'
  | 'Task'
  | 'TodoWrite'
  | 'TaskCreate'
  | 'TaskUpdate'
  | 'TaskList'
  | 'TaskOutput'
  | 'TaskStop'
  | 'WebSearch'
  | 'WebFetch'
  | 'NotebookEdit'
  | 'AskUserQuestion'
  | 'EnterPlanMode'
  | 'ExitPlanMode'
  | 'Skill'
  | 'KillShell'
  | 'Agent'
  | 'Artifact'
  | 'ToolSearch'
  | 'EnterWorktree'
  | 'ExitWorktree'
  | 'SendMessage'
  | 'CronCreate'
  | 'CronDelete'
  | 'CronList'
  | 'LSP'
  | 'ListAgents'
  | 'TeamCreate'
  | 'TeamDelete'
  | 'TaskGet'
  | 'Monitor'
  | 'PowerShell'
  | 'PushNotification'
  | 'ReportFindings'
  | 'ScheduleWakeup'
  | 'SendUserFile'
  | 'Workflow'
  | `mcp__${string}`;

export interface TokenUsage {
  input_tokens: number;
  output_tokens: number;
  cache_creation_input_tokens: number;
  cache_read_input_tokens: number;
  cache_creation: {
    ephemeral_5m_input_tokens: number;
    ephemeral_1h_input_tokens: number;
  };
  service_tier: string | null;
  inference_geo?: string;
  server_tool_use?: {
    web_search_requests: number;
    web_fetch_requests: number;
  };
}

export interface ContextManagement {
  applied_edits: unknown[];
}

// ═══════════════════════════════════════════════════════════════════════════════
// TYPE: system
// ═══════════════════════════════════════════════════════════════════════════════

export type SystemMessage =
  | StopHookSummarySystemMessage
  | TurnDurationSystemMessage
  | ApiErrorSystemMessage
  | CompactBoundarySystemMessage
  | MicrocompactBoundarySystemMessage
  | LocalCommandSystemMessage
  | BridgeStatusSystemMessage
  | AwaySummarySystemMessage
  | InformationalSystemMessage
  | ScheduledTaskFireSystemMessage
  | ModelConsentFallbackSystemMessage
  | ModelRefusalFallbackSystemMessage
  | AgentsKilledSystemMessage;

interface SystemMessageBase extends BaseMessageFields {
  type: 'system';
  level?: 'info' | 'error' | 'suggestion';
  isMeta?: boolean;
  /** Extra context hooks injected into the turn. Empty array when none. */
  hookAdditionalContext?: unknown[];
  /** Work still outstanding when the line was written. */
  pendingBackgroundAgentCount?: number;
  pendingWorkflowCount?: number;
}

export interface StopHookSummarySystemMessage extends SystemMessageBase {
  subtype: 'stop_hook_summary';
  hookCount: number;
  hookInfos: Array<{ command: string }>;
  hookErrors: unknown[];
  preventedContinuation: boolean;
  stopReason: string;
  hasOutput: boolean;
  toolUseID: string;
}

export interface TurnDurationSystemMessage extends SystemMessageBase {
  subtype: 'turn_duration';
  durationMs: number;
  messageCount?: number;
}

export interface ApiErrorSystemMessage extends SystemMessageBase {
  subtype: 'api_error';
  cause: Record<string, unknown>;
  error: { cause: Record<string, unknown> };
  retryInMs: number;
  retryAttempt: number;
  maxRetries: number;
}

export interface CompactBoundarySystemMessage extends SystemMessageBase {
  subtype: 'compact_boundary';
  content: string;
  logicalParentUuid: string;
  compactMetadata: {
    trigger: string;
    preTokens: number;
  };
}

export interface MicrocompactBoundarySystemMessage extends SystemMessageBase {
  subtype: 'microcompact_boundary';
  content: string;
  microcompactMetadata: {
    trigger: string;
    preTokens: number;
    tokensSaved: number;
    compactedToolIds: string[];
  };
}

export interface LocalCommandSystemMessage extends SystemMessageBase {
  subtype: 'local_command';
  content: string;
}

export interface BridgeStatusSystemMessage extends SystemMessageBase {
  subtype: 'bridge_status';
  url?: string;
  content?: string;
}

/**
 * Recap prose shown when returning to an idle session ("away" digest).
 * `content` carries searchable summary text, so it is indexed into FTS.
 */
export interface AwaySummarySystemMessage extends SystemMessageBase {
  subtype: 'away_summary';
  content: string;
}

/** Free-form informational system line. */
export interface InformationalSystemMessage extends SystemMessageBase {
  subtype: 'informational';
  content?: string;
}

/**
 * Marks a turn started by a scheduled task (a cron routine firing) rather
 * than by the user. `content` names the schedule, e.g. "Running scheduled
 * task (Apr 15 12:15pm)".
 */
export interface ScheduledTaskFireSystemMessage extends SystemMessageBase {
  subtype: 'scheduled_task_fire';
  content: string;
}

/**
 * Records the prompt shown when the requested model was unavailable (rate
 * limited, out of credits) and Claude Code offered a fallback. Written whether
 * or not the user accepted, so `choice` is the outcome, not a request.
 */
export interface ModelConsentFallbackSystemMessage extends SystemMessageBase {
  subtype: 'model_consent_fallback';
  /** Outcome of the prompt, e.g. `cancelled` when the user declined. */
  choice: string;
  /** Model offered instead, e.g. `claude-opus-5[1m]`. */
  fallbackModel: string;
  /** Model originally requested, e.g. `claude-fable-5`. */
  originalModel: string;
  /** Whether the fallback was written back as the session default. */
  persistedAsDefault: boolean;
}

/** API refusal that caused Claude Code to retract the turn and change model. */
export interface ModelRefusalFallbackSystemMessage extends SystemMessageBase {
  subtype: 'model_refusal_fallback';
  content: string;
  trigger: string;
  direction: string;
  originalModel: string;
  fallbackModel: string;
  requestId: string;
  apiRefusalCategory: string;
  apiRefusalExplanation: string;
  retractedMessageUuids: string[];
  refusedUserMessageUuid: string | null;
}

/** Marker emitted after Claude Code terminates outstanding background agents. */
export interface AgentsKilledSystemMessage extends SystemMessageBase {
  subtype: 'agents_killed';
}

// ═══════════════════════════════════════════════════════════════════════════════
// TYPE: progress
// ═══════════════════════════════════════════════════════════════════════════════

export interface ProgressMessage extends BaseMessageFields {
  type: 'progress';
  data: ProgressData;
  toolUseID: string;
  parentToolUseID: string;
  agentId?: string;
  teamName?: string;
}

export type ProgressData =
  | HookProgressData
  | BashProgressData
  | AgentProgressData
  | McpProgressData
  | QueryUpdateData
  | SearchResultsReceivedData
  | WaitingForTaskData;

export interface HookProgressData {
  type: 'hook_progress';
  hookEvent: string;
  hookName: string;
  command: string;
}

export interface BashProgressData {
  type: 'bash_progress';
  output: string;
  fullOutput: string;
  elapsedTimeSeconds: number;
  totalLines: number;
}

export interface AgentProgressData {
  type: 'agent_progress';
  agentId: string;
  prompt: string;
  normalizedMessages: unknown[];
  message: {
    type: 'user' | 'assistant';
    uuid: string;
    timestamp: string;
    message: UserMessagePayload | AssistantMessagePayload;
    toolUseResult?: string;
    requestId?: string;
  };
}

export interface McpProgressData {
  type: 'mcp_progress';
  serverName: string;
  toolName: string;
  status: 'started' | 'completed';
  elapsedTimeMs?: number;
}

export interface QueryUpdateData {
  type: 'query_update';
  query: string;
}

export interface SearchResultsReceivedData {
  type: 'search_results_received';
  query: string;
  resultCount: number;
}

export interface WaitingForTaskData {
  type: 'waiting_for_task';
  taskDescription: string;
  taskType: string;
}

// ═══════════════════════════════════════════════════════════════════════════════
// TYPE: saved_hook_context
// ═══════════════════════════════════════════════════════════════════════════════

export interface SavedHookContextMessage extends BaseMessageFields {
  type: 'saved_hook_context';
  content: string[];
  hookName: string;
  hookEvent: string;
  toolUseID: string;
}

// ═══════════════════════════════════════════════════════════════════════════════
// TYPE: summary
// ═══════════════════════════════════════════════════════════════════════════════

export interface SummaryMessage {
  type: 'summary';
  summary: string;
  leafUuid: string;
}

// ═══════════════════════════════════════════════════════════════════════════════
// TYPE: queue-operation
// ═══════════════════════════════════════════════════════════════════════════════

export interface QueueOperationMessage {
  type: 'queue-operation';
  operation: 'enqueue' | 'dequeue' | 'popAll' | 'remove';
  timestamp: string;
  sessionId: string;
  content?: string;
}

// ═══════════════════════════════════════════════════════════════════════════════
// SUBAGENT MESSAGES
// ═══════════════════════════════════════════════════════════════════════════════

export interface SubagentMessage extends BaseMessageFields {
  agentId: string;
  isSidechain: true;
}

export type SubagentType = 'task' | 'prompt_suggestion' | 'compact';

// ═══════════════════════════════════════════════════════════════════════════════
// THREADING MODEL
// ═══════════════════════════════════════════════════════════════════════════════

export interface MessageThread {
  messages: SessionMessage[];
  rootUuid: string | null;
}

// ═══════════════════════════════════════════════════════════════════════════════
// TOOL RESULTS (on-disk)
// ═══════════════════════════════════════════════════════════════════════════════

export interface PersistedToolResult {
  toolUseId: string;
  content: string;
}

// ═══════════════════════════════════════════════════════════════════════════════
// PROJECT MEMORY (on-disk)
// ═══════════════════════════════════════════════════════════════════════════════

export interface ProjectMemory {
  projectSlug: string;
  content: string;
}

// ═══════════════════════════════════════════════════════════════════════════════
// TYPE: last-prompt
// ═══════════════════════════════════════════════════════════════════════════════

export interface LastPromptMessage extends BaseMessageFields {
  type: 'last-prompt';
  lastPrompt: string;
}
