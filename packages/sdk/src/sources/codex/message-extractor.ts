/**
 * OpenAI Codex CLI MessageExtractor (RFC 006).
 *
 * Codex stores one JSON object (`RolloutLine`) per line in
 * `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid7>.jsonl`. A line is
 * `{ timestamp, type, payload }`. Canonical `response_item` records for prose,
 * tool calls/results, and reasoning are retained. Telemetry/UI projections
 * (`event_msg`, `turn_context`, etc.) are skipped; token_count is consumed by
 * the Codex ingest hook without becoming a transcript row.
 *
 * Differences from Claude Code that this extractor normalizes away:
 *  - the discriminator is `payload.role` (`developer`/`user`/`assistant`), an
 *    open string, mapped straight to `msgType` (a source-defined value, the
 *    same way Claude's `msgType` is its raw `type`).
 *  - text is a `content[]` of typed blocks (`input_text`/`output_text`/
 *    `input_image`); we concatenate prose, index reasoning summaries and tool
 *    names, and keep tool payloads only in the verbatim raw record.
 *  - chat lines themselves carry no tokens. Codex emits periodic
 *    `event_msg` / `token_count` events; IngestService attributes
 *    `last_token_usage` onto the preceding assistant row (ccusage style).
 *    Extractor returns zeros here by design.
 *  - `timestamp` is the top-level `RolloutLine.timestamp` (ISO-8601), the
 *    cleanest per-message time in the whole survey.
 */

import type { ExtractedMessage, MessageExtractor } from '../types.js';
import {
  codexCallId,
  codexReasoningSummary,
  codexResponseItemPayload,
  codexToolName,
  isCodexToolCallType,
  isCodexToolResultType,
} from './response-items.js';

/** FTS/preview text cap — matches the claude-code extractor's convention. */
const MAX_TEXT_LENGTH = 2_000;

function truncate(text: string): string {
  return text.length <= MAX_TEXT_LENGTH ? text : text.substring(0, MAX_TEXT_LENGTH);
}

const ZERO_TOKENS = {
  inputTokens: 0,
  outputTokens: 0,
  cacheCreationTokens: 0,
  cacheReadTokens: 0,
} as const;

/** Concatenate the readable text blocks of a Codex `content[]` array. */
function extractText(content: unknown): string {
  if (typeof content === 'string') return content;
  if (!Array.isArray(content)) return '';
  const parts: string[] = [];
  for (const block of content) {
    if (!block || typeof block !== 'object') continue;
    const b = block as Record<string, unknown>;
    // input_text (user/developer) and output_text (assistant) carry prose;
    // input_image and other block kinds have no text.
    if ((b.type === 'input_text' || b.type === 'output_text' || b.type === 'text') && typeof b.text === 'string') {
      parts.push(b.text);
    }
  }
  return parts.join('\n');
}

export const codexMessageExtractor: MessageExtractor = {
  extract(raw: unknown): ExtractedMessage | null {
    if (!raw || typeof raw !== 'object') return null;
    const line = raw as Record<string, unknown>;
    const payload = codexResponseItemPayload(line);
    if (!payload) return null;

    const type = typeof payload.type === 'string' ? payload.type : '';
    let msgType: string;
    let text = '';
    if (type === 'message') {
      msgType = typeof payload.role === 'string' ? payload.role : 'unknown';
      text = extractText(payload.content);
    } else if (type === 'reasoning') {
      msgType = 'reasoning';
      text = codexReasoningSummary(payload);
    } else if (isCodexToolResultType(type)) {
      msgType = 'tool_result';
    } else if (isCodexToolCallType(type)) {
      msgType = 'tool_use';
      text = codexToolName(payload);
    } else {
      return null;
    }

    return {
      msgType,
      text: truncate(text),
      uuid: typeof payload.id === 'string' ? payload.id : codexCallId(payload) || null,
      timestamp: typeof line.timestamp === 'string' ? line.timestamp : null,
      tokens: { ...ZERO_TOKENS },
    };
  },
};
