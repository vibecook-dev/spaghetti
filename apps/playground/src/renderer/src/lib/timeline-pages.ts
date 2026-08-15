import type { ChatSessionMessage } from '@vibecook/spaghetti-sdk/react';

const timelineFingerprintCache = new WeakMap<object, string>();

function timelineFingerprint(message: ChatSessionMessage): string {
  const cached = timelineFingerprintCache.get(message);
  if (cached) return cached;
  let fingerprint: string;
  try {
    fingerprint = JSON.stringify(message);
  } catch {
    fingerprint = `${message.timelineId}:${message.timestamp}:${message.type}:${message.content ?? ''}`;
  }
  timelineFingerprintCache.set(message, fingerprint);
  return fingerprint;
}

/** Preserve object identity for unchanged rows while appending a fresh tail. */
export function reconcileTimelineTail(
  current: readonly ChatSessionMessage[],
  incoming: readonly ChatSessionMessage[],
): ChatSessionMessage[] {
  const byId = new Map(
    incoming.filter((message) => message.timelineId).map((message) => [message.timelineId, message]),
  );
  const currentIds = new Set(current.map((message) => message.timelineId).filter(Boolean));
  const preserved = current.map((previous) => {
    const next = previous.timelineId ? byId.get(previous.timelineId) : undefined;
    return next && timelineFingerprint(previous) !== timelineFingerprint(next) ? next : previous;
  });
  const additions = incoming.filter((message) => !message.timelineId || !currentIds.has(message.timelineId));
  return [...preserved, ...additions];
}

/** Prepend an older keyset page without duplicating its boundary row. */
export function prependTimelinePage(
  current: readonly ChatSessionMessage[],
  older: readonly ChatSessionMessage[],
): ChatSessionMessage[] {
  const currentIds = new Set(current.map((message) => message.timelineId).filter(Boolean));
  return [...older.filter((message) => !message.timelineId || !currentIds.has(message.timelineId)), ...current];
}

/**
 * Tool results can land in the page immediately newer than their tool call.
 * Reconcile that boundary after pages are merged so the result decorates its
 * call exactly once instead of remaining as a misleading orphan row.
 */
export function attachCrossPageToolResults(messages: readonly ChatSessionMessage[]): ChatSessionMessage[] {
  const callIndexes = new Map<string, number[]>();
  const resultIndexes = new Map<string, number[]>();
  for (const [index, message] of messages.entries()) {
    const callId = message.toolUse?.toolId;
    if (callId) callIndexes.set(callId, [...(callIndexes.get(callId) ?? []), index]);
    const resultId = message.toolResult?.toolId;
    if (resultId) resultIndexes.set(resultId, [...(resultIndexes.get(resultId) ?? []), index]);
  }

  const replacements = new Map<number, ChatSessionMessage>();
  const attachedResults = new Set<number>();
  for (const [toolId, indexes] of resultIndexes) {
    const owners = callIndexes.get(toolId);
    const result = messages[indexes.at(-1)!]?.toolResult;
    if (!owners?.length || !result) continue;
    for (const ownerIndex of owners) {
      const owner = messages[ownerIndex]!;
      if (!owner.toolUse || owner.toolUse.result === result) continue;
      replacements.set(ownerIndex, {
        ...owner,
        toolUse: { ...owner.toolUse, result },
      });
    }
    for (const resultIndex of indexes) attachedResults.add(resultIndex);
  }

  if (replacements.size === 0 && attachedResults.size === 0) return [...messages];
  return messages.flatMap((message, index) => {
    if (attachedResults.has(index)) return [];
    return [replacements.get(index) ?? message];
  });
}
