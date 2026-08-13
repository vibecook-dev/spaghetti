/**
 * Transitional validation for the canonical compatibility DTO.
 *
 * Source-format adaptation now happens in Rust. The CLI accepts the same
 * normalized message shape for every adapter and never interprets native
 * Codex/Grok/Claude records.
 */

import type { SessionMessage } from '@vibecook/spaghetti-sdk';

export function adaptMessageForDisplay(raw: unknown, _sourceId: string): SessionMessage | null {
  if (!raw || typeof raw !== 'object') return null;
  const type = (raw as { type?: unknown }).type;
  return typeof type === 'string' && type.length > 0 ? (raw as SessionMessage) : null;
}

export function adaptMessagesForDisplay(messages: unknown[], sourceId: string): SessionMessage[] {
  return messages
    .map((message) => adaptMessageForDisplay(message, sourceId))
    .filter((message): message is SessionMessage => message !== null);
}
