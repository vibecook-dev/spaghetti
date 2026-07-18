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
 * Adapt one raw Grok chat_history record (system/user/assistant/reasoning; tool
 * I/O was already skipped at ingest so it never reaches here) into a Claude-shaped
 * SessionMessage.
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
    return {
      ...base,
      type: 'user',
      message: { role: 'user', content: collectContentText(line.content) },
    } as SessionMessage;
  }
  if (type === 'assistant') {
    return {
      ...base,
      type: 'assistant',
      message: { role: 'assistant', content: [{ type: 'text', text: collectContentText(line.content) }] },
    } as SessionMessage;
  }
  if (type === 'reasoning') {
    // Grok's plaintext reasoning summary → a thin system line (dim thinking).
    return { ...base, type: 'system', content: collectContentText(line.summary), level: 'info' } as SessionMessage;
  }
  if (type === 'system') {
    return { ...base, type: 'system', content: collectContentText(line.content), level: 'info' } as SessionMessage;
  }
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
  // Codex chat turns only. Non-message rollout lines (session_meta, event_msg,
  // function_call, …) are not displayable as transcript rows — skip them.
  if (line.type !== 'response_item') return null;

  const payload = line.payload as Record<string, unknown> | undefined;
  if (!payload || payload.type !== 'message') return null;
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
      timestamp: typeof line.timestamp === 'string' ? line.timestamp : '',
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
      timestamp: typeof line.timestamp === 'string' ? line.timestamp : '',
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
