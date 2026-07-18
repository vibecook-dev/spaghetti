/**
 * Chat UI Types
 *
 * All type definitions for the chat components.
 * Ported from desktop's ambient types.d.ts into proper exported interfaces.
 */

// =============================================================================
// TOOL TYPES
// =============================================================================

export interface ToolResultInfo {
  toolId: string;
  isError: boolean;
  content: string;
  rawJson?: unknown;
}

export interface ToolUseInfo {
  toolName: string;
  toolId: string;
  input: Record<string, unknown>;
  result?: ToolResultInfo;
}

export interface ThinkingMetadata {
  level: 'high' | 'medium' | 'low' | string;
  disabled: boolean;
  triggers: string[];
}

// =============================================================================
// HOOK TYPES
// =============================================================================

export interface HookInfo {
  command: string;
}

// =============================================================================
// SESSION MESSAGE
// =============================================================================

/**
 * Parsed session message for display in the UI.
 *
 * This is a transformed/flattened version of the raw JSONL data.
 * The parser transforms raw JSONL entries into this format:
 * - Assistant messages are split into separate thinking, text, and tool_use messages
 * - Tool results from user messages are merged into their corresponding tool_use messages
 * - Token usage is normalized to camelCase
 */
export interface SessionMessage {
  /**
   * Stable identity of this normalized timeline row. Unlike source `uuid`,
   * this is guaranteed unique within a session and is safe for UI keys.
   */
  timelineId?: string;
  uuid: string;
  parentUuid: string | null;
  /** Message type: 'user' | 'assistant' | 'tool_use' | 'thinking' | 'compact_summary' | 'system' */
  type: string;
  timestamp: string;
  sessionId: string;
  /** Text content (for user, assistant, thinking, compact_summary messages) */
  content?: string;
  /** @deprecated Use separate thinking message type instead */
  thinking?: string;
  /** Role from the original message */
  role?: string;
  /** Model identifier (e.g., "claude-opus-4-5-20251101") */
  model?: string;
  /** Tool use info (for type='tool_use' messages) - includes merged result */
  toolUse?: ToolUseInfo;
  /** Tool result info (for type='tool_result' messages) */
  toolResult?: ToolResultInfo;
  /** Token usage (from assistant messages) - normalized to camelCase */
  usage?: {
    inputTokens: number;
    outputTokens: number;
    cacheCreationInputTokens: number;
    cacheReadInputTokens: number;
  };
  /** Original JSON object from the JSONL file for debugging */
  rawJson?: unknown;
  /** True for auto-generated conversation compaction messages */
  isCompactSummary?: boolean;
  /** Thinking level metadata (from user messages) */
  thinkingMetadata?: ThinkingMetadata;

  // --- Agent threading fields ---
  /** Agent ID for sub-agent messages */
  agentId?: string;
  /** Whether this message is part of a sidechain (sub-agent thread) */
  isSidechain?: boolean;

  // --- System event fields ---
  /** Subtype for system messages: 'compact_boundary' | 'local_command' | etc. */
  systemSubtype?: string;
  /** Compact metadata for context compaction boundaries */
  compactMetadata?: {
    trigger: string;
    preTokens: number;
  };
  /** Leaf UUID for summary messages */
  leafUuid?: string;
  /** Checkpoint data for file history snapshots */
  checkpointData?: {
    messageId: string;
    isUpdate: boolean;
    fileCount: number;
  };
  /** Queue operation type */
  queueOperation?: string;

  // --- Hook fields ---
  /** Number of hooks executed */
  hookCount?: number;
  /** Hook info details */
  hookInfos?: HookInfo[];
  /** Hook error messages */
  hookErrors?: string[];
  /** Whether hooks prevented continuation */
  preventedContinuation?: boolean;
  /** Stop reason */
  stopReason?: string;
  /** Whether message has output */
  hasOutput?: boolean;
  /** Message level (e.g., for system events) */
  level?: string;
}

// =============================================================================
// CONNECTOR INFO
// =============================================================================

/** Connector info passed to message components for SVG timeline rendering */
export interface ConnectorInfo {
  /** Current message indent (0 = main thread, 1 = sub-agent) */
  indent: 0 | 1;
  /** Next message indent (determines connector curve) */
  nextIndent: 0 | 1;
  /** Whether there's a next message to connect to */
  hasNext: boolean;
  /** Whether this is a sub-agent message */
  isAgent: boolean;
  /** Whether this is the first message in an agent group */
  isFirstInAgentGroup: boolean;
  /** Agent ID for header display */
  agentId?: string;
}
