/**
 * Claude Code MessageExtractor (RFC 006).
 *
 * The seam that turns a raw transcript record into the normalized projection
 * the ingest writer stores (`msg_type`, `text_content`, the token columns,
 * `uuid`, `timestamp`). This logic used to live inline in
 * `data/ingest-service.ts` and knew Anthropic's message envelope directly;
 * RFC 006 relocates it here so extraction becomes a property of the source,
 * the same way `classify` (step 2) is. A second `AgentSource` supplies its own
 * `MessageExtractor` without the ingest engines changing.
 *
 * This is a **behavior-identical** relocation: it produces exactly the columns
 * the previous inline functions did (verified by the parser tests +
 * `test:ingest-diff` staying zero-diff). In particular `msgType` is still the
 * **raw** Claude type string (`user`/`assistant`/`summary`/`ai-title`/`system`/
 * `unknown`) — tightening it to RFC 006 §3's normalized enum is a separate,
 * value-changing decision, deliberately NOT bundled here.
 */

import type { SessionMessage } from '../../types/index.js';
import type { ExtractedMessage, MessageExtractor } from '../types.js';
import { extractClaudeSessionMetadata } from './session-metadata.js';

/** FTS/preview text is capped; the raw line in `messages.data` is untouched. */
const MAX_TEXT_LENGTH = 2_000;

function truncate(text: string): string {
  if (text.length <= MAX_TEXT_LENGTH) return text;
  return text.substring(0, MAX_TEXT_LENGTH);
}

/**
 * Extract searchable text content from a SessionMessage for FTS indexing.
 * Handles user messages (text content), assistant messages (text blocks),
 * and tool_use blocks (tool name + input summary).
 */
function extractTextContent(message: SessionMessage): string {
  const textParts: string[] = [];
  const msg = message as unknown as Record<string, unknown>;
  const msgType = msg.type as string | undefined;

  if (msgType === 'user') {
    const payload = msg.message as Record<string, unknown> | undefined;
    if (payload) {
      const content = payload.content;
      if (typeof content === 'string') {
        textParts.push(content);
      } else if (Array.isArray(content)) {
        for (const block of content) {
          const b = block as Record<string, unknown>;
          if (b.type === 'text' && typeof b.text === 'string') {
            textParts.push(b.text);
          } else if (b.type === 'tool_result') {
            const rc = b.content;
            if (typeof rc === 'string') {
              textParts.push(rc);
            } else if (Array.isArray(rc)) {
              for (const r of rc) {
                const rb = r as Record<string, unknown>;
                if (rb.type === 'text' && typeof rb.text === 'string') {
                  textParts.push(rb.text);
                }
              }
            }
          }
        }
      }
    }
  } else if (msgType === 'assistant') {
    const payload = msg.message as Record<string, unknown> | undefined;
    if (payload) {
      const content = payload.content;
      if (Array.isArray(content)) {
        for (const block of content) {
          const b = block as Record<string, unknown>;
          if (b.type === 'text' && typeof b.text === 'string') {
            textParts.push(b.text);
          } else if (b.type === 'tool_use') {
            const toolName = b.name as string | undefined;
            if (toolName) textParts.push(`[tool:${toolName}]`);
          }
        }
      }
    }
  } else if (msgType === 'summary') {
    const summary = msg.summary as string | undefined;
    if (summary) textParts.push(summary);
  } else if (msgType === 'ai-title') {
    // The model-generated session title — searchable so a query can
    // find a session by its title.
    const aiTitle = msg.aiTitle as string | undefined;
    if (aiTitle) textParts.push(aiTitle);
  } else if (msgType === 'system') {
    // Several system subtypes carry prose (away_summary recap,
    // local_command, compact boundaries); index whatever `content` holds.
    const content = msg.content as string | undefined;
    if (content) textParts.push(content);
  }

  return truncate(textParts.join('\n'));
}

/**
 * Extract token usage from an assistant message.
 */
function extractTokens(message: SessionMessage): ExtractedMessage['tokens'] {
  const msg = message as unknown as Record<string, unknown>;
  const defaults = { inputTokens: 0, outputTokens: 0, cacheCreationTokens: 0, cacheReadTokens: 0 };

  if (msg.type !== 'assistant') return defaults;

  const payload = msg.message as Record<string, unknown> | undefined;
  if (!payload) return defaults;

  const usage = payload.usage as Record<string, unknown> | undefined;
  if (!usage) return defaults;

  return {
    inputTokens: typeof usage.input_tokens === 'number' ? usage.input_tokens : 0,
    outputTokens: typeof usage.output_tokens === 'number' ? usage.output_tokens : 0,
    cacheCreationTokens: typeof usage.cache_creation_input_tokens === 'number' ? usage.cache_creation_input_tokens : 0,
    cacheReadTokens: typeof usage.cache_read_input_tokens === 'number' ? usage.cache_read_input_tokens : 0,
  };
}

/** Extract the (raw) message type string from a SessionMessage. */
function extractMsgType(message: SessionMessage): string {
  const msg = message as unknown as Record<string, unknown>;
  return typeof msg.type === 'string' ? msg.type : 'unknown';
}

/** Extract uuid from a SessionMessage. */
function extractUuid(message: SessionMessage): string | null {
  const msg = message as unknown as Record<string, unknown>;
  return typeof msg.uuid === 'string' ? msg.uuid : null;
}

/** Extract timestamp from a SessionMessage. */
function extractTimestamp(message: SessionMessage): string | null {
  const msg = message as unknown as Record<string, unknown>;
  return typeof msg.timestamp === 'string' ? msg.timestamp : null;
}

/**
 * The Claude Code message extractor. `createClaudeCodeSource` exposes this as
 * `source.messages`; the ingest writer calls `extract(record)` per line.
 *
 * Claude Code emits a stored row for every transcript line, so `extract` never
 * returns `null` — the `| null` in the interface is for sources that interleave
 * non-message records (see RFC 006 §3).
 */
export const claudeCodeMessageExtractor: MessageExtractor = {
  extract(raw: unknown): ExtractedMessage {
    const message = raw as SessionMessage;
    return {
      msgType: extractMsgType(message),
      text: extractTextContent(message),
      uuid: extractUuid(message),
      timestamp: extractTimestamp(message),
      tokens: extractTokens(message),
      sessionMetadata: extractClaudeSessionMetadata(message),
    };
  },
};
