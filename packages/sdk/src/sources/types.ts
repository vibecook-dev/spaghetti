/**
 * AgentSource — adapter boundary for a local agent product (Plane 0).
 *
 * Claude Code is the only source today. Future agents plug in by
 * implementing this interface; ingest planes read roots from here
 * instead of hardcoding `~/.claude` / `~/.spaghetti`.
 *
 * See `docs/THREE-PLANE-INGEST-ARCHITECTURE.md` and
 * `docs/PR-PLAN-THREE-PLANE-SHAPE.md`.
 */

import type { RouteResult } from '../live/router.js';

/** Stable id for a supported agent product. Extend as sources land. */
export type AgentSourceId = 'claude-code' | 'codex' | 'grok';

/**
 * The normalized projection an ingest writer stores for one message, produced
 * by a source's {@link MessageExtractor} (RFC 006). The verbatim source record
 * is stored separately as `messages.data` — this is the thin, queryable core
 * (list / FTS / token stats), not a lossless model.
 *
 * `msgType` is the source's message-type string. It is currently the RAW type
 * (Claude Code: `user`/`assistant`/`summary`/`ai-title`/`system`/`unknown`);
 * tightening it to RFC 006 §3's normalized enum is a deferred, value-changing
 * decision, not part of the initial extractor relocation.
 */
export interface ExtractedMessage {
  msgType: string;
  /** Flattened, truncated FTS/preview text — excludes token + raw structure. */
  text: string;
  uuid: string | null;
  timestamp: string | null;
  tokens: {
    inputTokens: number;
    outputTokens: number;
    cacheCreationTokens: number;
    cacheReadTokens: number;
  };
  /** Optional session-list metadata carried by this source record. */
  sessionMetadata?: {
    humanPrompt?: string;
    aiTitle?: string;
    customTitle?: string;
  };
}

/**
 * Per-source extraction seam (RFC 006). Maps one raw source record into the
 * normalized {@link ExtractedMessage} the ingest writer binds to columns. This
 * is the counterpart to {@link AgentSource.classify}: it makes message-shape
 * knowledge a property of the source, so a second agent ships one small
 * `extract()` instead of the ingest engines learning its envelope.
 */
export interface MessageExtractor {
  /**
   * Extract the stored projection for one raw record. Return `null` to skip a
   * record that is not a message row (no source needs this yet — Claude Code
   * stores a row per line). The raw record's shape is source-specific; the
   * source's own extractor knows it, which is why the parameter is `unknown`.
   */
  extract(raw: unknown): ExtractedMessage | null;
}

/** Four-column token bag written to `messages.*_tokens`. */
export interface MessageTokenBag {
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
}

/** Thin row for session-complete token estimation (e.g. Codex tiktoken). */
export interface SessionMessageTextRow {
  msgIndex: number;
  msgType: string;
  text: string | null;
}

/**
 * Write API the ingest writer exposes to {@link IngestHooks} so product
 * code can stamp tokens without owning SQL.
 */
export interface SessionTokenApi {
  updateMessageTokens(sessionId: string, msgIndex: number, tokens: MessageTokenBag): void;
  setSessionTokensEstimated(sessionId: string, estimated: boolean): void;
  listSessionMessageTexts(sessionId: string): SessionMessageTextRow[];
  /**
   * Optional: stamp a message timestamp after write (Grok events.jsonl join).
   * Absent on older writers — callers should optional-chain.
   */
  updateMessageTimestamp?(sessionId: string, msgIndex: number, timestamp: string): void;
}

/**
 * Optional per-source ingest hooks (Phase D). Lets a product attribute
 * tokens or react to skipped records without baking product branches into
 * {@link IngestService}. Default is no-op (Claude Code, Grok).
 */
export interface IngestHooks {
  onSessionStart?(sessionId: string): void;
  /**
   * `extract()` returned null — non-message source record (e.g. Codex
   * `token_count` event). May use `api` to update prior message tokens.
   */
  onSkippedRecord?(raw: unknown, ctx: { slug: string; sessionId: string }, api: SessionTokenApi): void;
  /** After a message row was written. */
  onMessageWritten?(extracted: ExtractedMessage, ctx: { slug: string; sessionId: string; msgIndex: number }): void;
  /** End of session stream — session totals / estimates. */
  onSessionComplete?(sessionId: string, api: SessionTokenApi): void;
}

/**
 * Shared path bag for every agent source.
 *
 * Only fields that are either (a) used across agents, or (b) Spaghetti-owned
 * state. Claude-specific layout (projects/todos/plans/…) lives on
 * {@link ClaudeCodePaths} — do not invent dummy dirs for Codex/Grok.
 */
export interface AgentSourcePaths {
  /**
   * Agent session-related directory under the product root.
   * - Claude Code: active-session PID registry (`~/.claude/sessions`)
   * - Codex: rollout tree (`~/.codex/sessions`)
   * - Grok: per-cwd session dirs (`~/.grok/sessions`)
   */
  sessionsDir: string;
  /**
   * Primary settings/config file for the agent, if any.
   * Claude: `settings.json`; Codex/Grok: often `config.toml`.
   */
  settingsFile?: string;
  /** Hook events JSONL (Spaghetti state), e.g. `~/.spaghetti/hooks/events.jsonl` */
  hookEventsFile: string;
  /** Channel session discovery dir, e.g. `~/.spaghetti/channel/sessions` */
  channelSessionsDir: string;
  /** Channel message history dir, e.g. `~/.spaghetti/channel/messages` */
  channelMessagesDir: string;
}

/**
 * Claude Code path layout — extends the shared bag with product subtrees
 * under `~/.claude`.
 */
export interface ClaudeCodePaths extends AgentSourcePaths {
  /** `<rootDir>/projects` */
  projectsDir: string;
  /** `<rootDir>/todos` */
  todosDir: string;
  /** `<rootDir>/plans` */
  plansDir: string;
  /** `<rootDir>/tasks` */
  tasksDir: string;
  /** `<rootDir>/file-history` */
  fileHistoryDir: string;
  /** Required for Claude: `settings.json` at root. */
  settingsFile: string;
}

/**
 * Describes where an agent product stores data on disk and where
 * Spaghetti keeps auxiliary runtime state for that machine.
 */
export interface AgentSource {
  readonly id: AgentSourceId;
  /** Agent product data root, e.g. `~/.claude`. */
  readonly rootDir: string;
  /** Spaghetti-owned state root, e.g. `~/.spaghetti`. */
  readonly stateDir: string;
  /** Derived absolute paths (shared bag; Claude sources use {@link ClaudeCodePaths}). */
  readonly paths: AgentSourcePaths;
  /**
   * Classify an absolute path under this source's root into a normalized
   * `Category` (session / subagent / tool_result / …). This is the seam that
   * makes file-layout knowledge a property of the source: the live plane calls
   * `source.classify(path)` rather than assuming Claude Code's directory shape,
   * so a second AgentSource declares its own path→category mapping without the
   * ingest engines changing. Pure — no I/O; safe in hot watcher callbacks.
   */
  classify(absPath: string): RouteResult;
  /**
   * Extract a stored message projection from one raw transcript record (RFC
   * 006). The ingest writer calls `source.messages.extract(record)` instead of
   * knowing Claude Code's message envelope, so extraction — like `classify` —
   * is a property of the source.
   */
  readonly messages: MessageExtractor;
}
