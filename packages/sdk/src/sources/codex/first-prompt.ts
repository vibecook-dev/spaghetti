/**
 * Codex session list titles (`first_prompt`) come from a rollout peek.
 *
 * Real rollouts almost always open with *injected* user turns before the human
 * prompt:
 *   - `<environment_context>…`
 *   - `# AGENTS.md instructions for …`
 *   - `<recommended_plugins>…`
 *   - developer/system scaffolding (role=developer — already ignored)
 *
 * Taking the first `role=user` response_item therefore stores cwd/shell noise
 * as the session summary. Prefer the clean `event_msg/user_message` payload,
 * or the first non-injected user response_item.
 */

/** Cap for session list previews — matches Claude/Grok convention. */
export const CODEX_FIRST_PROMPT_MAX = 200;

/**
 * True when a user-turn body is Codex-injected context, not the human prompt.
 * Conservative: only skip known scaffolding prefixes.
 */
export function isCodexInjectedUserText(text: string): boolean {
  const t = text.trimStart();
  if (!t) return true;
  if (t.startsWith('<environment_context>')) return true;
  if (t.startsWith('<recommended_plugins>')) return true;
  if (t.startsWith('<permissions instructions>')) return true;
  if (t.startsWith('<collaboration_mode>')) return true;
  if (t.startsWith('<INSTRUCTIONS>')) return true;
  if (t.startsWith('# AGENTS.md instructions')) return true;
  // environment_context without the exact open tag (rare / truncated peeks)
  if (t.startsWith('<') && (t.includes('</cwd>') || t.includes('<shell>')) && t.includes('<cwd>')) {
    return true;
  }
  return false;
}

/** Collect readable text from a Codex `content` string or block array. */
export function codexContentText(content: unknown): string {
  if (typeof content === 'string') return content;
  if (!Array.isArray(content)) return '';
  const parts: string[] = [];
  for (const block of content) {
    if (!block || typeof block !== 'object') continue;
    const b = block as Record<string, unknown>;
    if ((b.type === 'input_text' || b.type === 'output_text' || b.type === 'text') && typeof b.text === 'string') {
      parts.push(b.text);
    }
  }
  return parts.join('\n');
}

/**
 * Update `current` with a better first-prompt candidate from one rollout line.
 * Returns the (possibly updated) prompt string.
 */
export function considerCodexFirstPromptLine(current: string, line: Record<string, unknown>): string {
  if (current) return current;

  const type = line.type;
  const payload = line.payload as Record<string, unknown> | undefined;

  // Cleanest signal: event_msg / user_message carries the human text only.
  if (type === 'event_msg' && payload?.type === 'user_message') {
    const msg = payload.message;
    if (typeof msg === 'string' && msg.trim() && !isCodexInjectedUserText(msg)) {
      return msg.slice(0, CODEX_FIRST_PROMPT_MAX);
    }
  }

  if (type === 'response_item' && payload?.type === 'message' && payload.role === 'user') {
    const text = codexContentText(payload.content);
    if (text && !isCodexInjectedUserText(text)) {
      return text.slice(0, CODEX_FIRST_PROMPT_MAX);
    }
  }

  return current;
}
