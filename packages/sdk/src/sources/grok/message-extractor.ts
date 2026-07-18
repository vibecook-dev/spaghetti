/**
 * Grok CLI (xAI) MessageExtractor (RFC 006 third source).
 *
 * Grok stores one JSON object per line in
 * `~/.grok/sessions/<url-encoded-cwd>/<session-uuid>/chat_history.jsonl`. Unlike
 * Codex there is NO envelope: each line IS a typed record, discriminated by
 * `type`:
 *   - `system`    → `{ content: string }`
 *   - `user`      → `{ content: [{ type:'text', text }] }`      (block array)
 *   - `assistant` → `{ content: string, tool_calls?: [...] }`   (prose string)
 *   - `reasoning` → `{ summary: [{ type:'summary_text', text }], id, encrypted_content }`
 *   - `tool_result` / `backend_tool_call` → tool I/O
 *
 * Every known chat-history record is retained. The raw row stays lossless while
 * this projection supplies useful FTS text and stable source message types.
 * Display-time normalization filters model context, expands embedded calls and
 * pairs `tool_result` rows with their calls.
 *
 * Differences from Codex/Claude this normalizes away:
 *  - text lives in different fields per type (`content` string, `content[]`
 *    block array, or `summary[]`); one text collector handles all shapes.
 *  - chat_history lines carry NO per-message tokens (session-level only, in the
 *    sibling `signals.json`) and NO per-message timestamp (turn-level, in
 *    `events.jsonl`). So tokens are zero and timestamp is null by design —
 *    `sourceReportsPerMessageTokens('grok')` is false so the UI stays honest.
 */

import type { ExtractedMessage, MessageExtractor } from '../types.js';
import {
  grokAssistantSearchText,
  grokBackendToolCall,
  grokHumanUserText,
  grokReadableText,
  grokReasoningSummary,
  grokSearchInput,
  grokToolResultId,
  grokToolResultText,
} from './records.js';

/** FTS/preview text cap — matches the other extractors' convention. */
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

export const grokMessageExtractor: MessageExtractor = {
  extract(raw: unknown): ExtractedMessage | null {
    if (!raw || typeof raw !== 'object') return null;
    const rec = raw as Record<string, unknown>;
    const type = typeof rec.type === 'string' ? rec.type : '';

    let text: string;
    let msgType = type;
    let uuid: string | null = null;
    switch (type) {
      case 'system':
        // Keep the raw prompt, but do not pollute transcript FTS with model rules.
        text = '';
        break;
      case 'user': {
        const human = grokHumanUserText(rec);
        msgType = human == null ? 'context' : 'user';
        text = human ?? '';
        break;
      }
      case 'assistant':
        text = grokAssistantSearchText(rec);
        break;
      case 'reasoning':
        text = grokReasoningSummary(rec);
        uuid = typeof rec.id === 'string' ? rec.id : null;
        break;
      case 'tool_result':
        text = grokToolResultText(rec);
        uuid = grokToolResultId(rec) || null;
        break;
      case 'backend_tool_call': {
        const call = grokBackendToolCall(rec);
        if (!call) return null;
        msgType = 'tool_use';
        uuid = call.id || null;
        text = `${call.name} ${grokReadableText(call.input) || grokSearchInput(call.input)}`.trim();
        break;
      }
      default:
        return null;
    }

    return {
      msgType,
      text: truncate(text),
      uuid,
      timestamp: null, // per-message time is not in chat_history.jsonl (see events.jsonl)
      tokens: { ...ZERO_TOKENS },
    };
  },
};
