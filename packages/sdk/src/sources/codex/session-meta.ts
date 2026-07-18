/**
 * Codex writes a rollout for the human thread and additional rollouts for
 * internal workers such as the approval guardian. Those child rollouts share
 * the human thread's logical `session_id`, but have their own `id` and must not
 * be promoted to top-level Spaghetti sessions.
 */
export function isCodexInternalSessionPayload(payload: Record<string, unknown>): boolean {
  if (payload.thread_source === 'subagent') return true;

  const source = payload.source;
  if (source && typeof source === 'object' && 'subagent' in source) return true;

  const id = typeof payload.id === 'string' ? payload.id : '';
  const logicalSessionId = typeof payload.session_id === 'string' ? payload.session_id : '';
  const parentThreadId = typeof payload.parent_thread_id === 'string' ? payload.parent_thread_id : '';
  return Boolean(parentThreadId && id && logicalSessionId && logicalSessionId !== id);
}
