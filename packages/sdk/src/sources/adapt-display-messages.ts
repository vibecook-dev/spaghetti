/**
 * Adapt raw stored message records into a Claude-shaped envelope for UI
 * rendering.
 *
 * Codex stores RolloutLine JSON and Grok stores its own chat_history record in
 * `messages.data`. Transcript renderers (playground timeline, CLI TUI) expect
 * Anthropic-style `{ type, message: { content } }`, so each non-Claude source
 * maps through a small adapter here.
 *
 * This is a query-time concern (RFC 006): the index keeps raw `data` lossless
 * and only normalizes list/search columns at ingest.
 */

import type { SessionMessage } from '../types/index.js';
import { isCodexInjectedUserText } from './codex/first-prompt.js';
import {
  codexCallId,
  codexReasoningSummary,
  codexResponseItemPayload,
  codexToolInput,
  codexToolName,
  codexToolOutput,
  codexToolOutputIsError,
  isCodexToolCallType,
  isCodexToolResultType,
} from './codex/response-items.js';
import {
  grokAssistantToolCalls,
  grokBackendToolCall,
  grokHumanUserText,
  grokReadableText,
  grokReasoningSummary,
  grokToolResultId,
  grokToolResultText,
} from './grok/records.js';

/** Collect readable text from a string, or a `[{ type, text }]` block array. */
function collectContentText(content: unknown): string {
  if (typeof content === 'string') return content;
  if (!Array.isArray(content)) return '';
  const parts: string[] = [];
  for (const block of content) {
    if (!block || typeof block !== 'object') continue;
    const b = block as Record<string, unknown>;
    if (
      (b.type === 'input_text' || b.type === 'output_text' || b.type === 'text' || b.type === 'summary_text') &&
      typeof b.text === 'string'
    ) {
      parts.push(b.text);
    }
  }
  return parts.join('\n');
}

/**
 * Adapt one raw Grok chat_history record into a Claude-shaped SessionMessage.
 * Internal model context returns null; assistant records retain prose and expand
 * embedded tool calls; standalone results pair through the shared transformer.
 */
function adaptGrokMessage(line: Record<string, unknown>): SessionMessage | null {
  const type = typeof line.type === 'string' ? line.type : '';
  const uuid = typeof line.id === 'string' ? line.id : '';
  // Prefer sidecar-joined timestamp (query layer injects messages.timestamp).
  const timestamp = typeof line.timestamp === 'string' ? line.timestamp : '';
  const base = {
    uuid,
    parentUuid: null,
    timestamp,
    sessionId: '',
    cwd: '',
    version: '',
    gitBranch: '',
    isSidechain: false,
    userType: 'external' as const,
  };
  if (type === 'user') {
    const text = grokHumanUserText(line);
    if (!text) return null;
    return {
      ...base,
      type: 'user',
      message: { role: 'user', content: text },
    } as unknown as SessionMessage;
  }
  if (type === 'assistant') {
    const content: Record<string, unknown>[] = [];
    const text = grokReadableText(line.content);
    if (text) content.push({ type: 'text', text });
    for (const call of grokAssistantToolCalls(line)) {
      content.push({ type: 'tool_use', id: call.id, name: call.name, input: call.input });
    }
    if (content.length === 0) return null;
    return {
      ...base,
      type: 'assistant',
      message: { role: 'assistant', content },
    } as unknown as SessionMessage;
  }
  if (type === 'reasoning') {
    const summary = grokReasoningSummary(line);
    if (!summary) return null;
    return {
      ...base,
      type: 'assistant',
      message: { role: 'assistant', content: [{ type: 'thinking', thinking: summary }] },
    } as unknown as SessionMessage;
  }
  if (type === 'tool_result') {
    const callId = grokToolResultId(line);
    return {
      ...base,
      type: 'user',
      uuid: callId ? `result-${callId}` : '',
      message: {
        role: 'user',
        content: [{ type: 'tool_result', tool_use_id: callId, content: grokToolResultText(line) }],
      },
    } as unknown as SessionMessage;
  }
  if (type === 'backend_tool_call') {
    const call = grokBackendToolCall(line);
    if (!call) return null;
    return {
      ...base,
      type: 'assistant',
      uuid: call.id,
      message: {
        role: 'assistant',
        content: [{ type: 'tool_use', id: call.id, name: call.name, input: call.input }],
      },
    } as unknown as SessionMessage;
  }
  // Grok system prompts and synthetic context remain available in raw storage,
  // but are not product transcript events.
  return null;
}

/**
 * Map one raw DB message into a shape the existing Claude message renderer
 * understands. Unknown / non-chat source lines become null (skip).
 */
export function adaptMessageForDisplay(raw: unknown, sourceId: string): SessionMessage | null {
  if (!raw || typeof raw !== 'object') return null;

  if (sourceId === 'grok') {
    return adaptGrokMessage(raw as Record<string, unknown>);
  }

  if (sourceId !== 'codex') {
    return raw as SessionMessage;
  }

  const line = raw as Record<string, unknown>;
  const payload = codexResponseItemPayload(line);
  if (!payload) return null;
  const payloadType = typeof payload.type === 'string' ? payload.type : '';
  const timestamp = typeof line.timestamp === 'string' ? line.timestamp : '';

  if (isCodexToolResultType(payloadType)) {
    const callId = codexCallId(payload);
    return {
      type: 'user',
      uuid: typeof payload.id === 'string' ? payload.id : `result-${callId}`,
      parentUuid: null,
      timestamp,
      sessionId: '',
      cwd: '',
      version: '',
      gitBranch: '',
      isSidechain: false,
      userType: 'external',
      message: {
        role: 'user',
        content: [
          {
            type: 'tool_result',
            tool_use_id: callId,
            content: codexToolOutput(payload),
            is_error: codexToolOutputIsError(payload),
          },
        ],
      },
    } as unknown as SessionMessage;
  }

  if (isCodexToolCallType(payloadType)) {
    const callId = codexCallId(payload);
    return {
      type: 'assistant',
      uuid: typeof payload.id === 'string' ? payload.id : callId,
      parentUuid: null,
      timestamp,
      sessionId: '',
      cwd: '',
      version: '',
      gitBranch: '',
      isSidechain: false,
      userType: 'external',
      message: {
        role: 'assistant',
        content: [
          {
            type: 'tool_use',
            id: callId,
            name: codexToolName(payload),
            input: codexToolInput(payload),
          },
        ],
      },
    } as unknown as SessionMessage;
  }

  if (payloadType === 'reasoning') {
    const summary = codexReasoningSummary(payload);
    if (!summary) return null;
    return {
      type: 'assistant',
      uuid: typeof payload.id === 'string' ? payload.id : '',
      parentUuid: null,
      timestamp,
      sessionId: '',
      cwd: '',
      version: '',
      gitBranch: '',
      isSidechain: false,
      userType: 'external',
      message: { role: 'assistant', content: [{ type: 'thinking', thinking: summary }] },
    } as unknown as SessionMessage;
  }

  if (payloadType !== 'message') return null;
  const role = typeof payload.role === 'string' ? payload.role : 'unknown';
  const text = collectContentText(payload.content);
  if (role === 'user') {
    // Codex records environment/permission/plugin scaffolding with role=user.
    // It is model input, but not a human transcript turn and must not affect
    // the UI's user rows or database-backed message facets.
    if (isCodexInjectedUserText(text)) return null;
    return {
      type: 'user',
      uuid: typeof payload.id === 'string' ? payload.id : '',
      parentUuid: null,
      timestamp,
      sessionId: '',
      cwd: '',
      version: '',
      gitBranch: '',
      isSidechain: false,
      userType: 'external',
      message: { role: 'user', content: text },
    } as SessionMessage;
  }
  if (role === 'assistant') {
    return {
      type: 'assistant',
      uuid: typeof payload.id === 'string' ? payload.id : '',
      parentUuid: null,
      timestamp,
      sessionId: '',
      cwd: '',
      version: '',
      gitBranch: '',
      isSidechain: false,
      userType: 'external',
      message: {
        role: 'assistant',
        content: [{ type: 'text', text }],
      },
    } as SessionMessage;
  }
  // Developer/system scaffolding is internal model context. Keep it in the
  // raw message store, but do not surface it as a human transcript row.
  return null;
}

export function adaptMessagesForDisplay(raw: unknown[], sourceId: string): SessionMessage[] {
  const out: SessionMessage[] = [];
  for (const m of raw) {
    const adapted = adaptMessageForDisplay(m, sourceId);
    if (adapted) out.push(adapted);
  }
  return out;
}
