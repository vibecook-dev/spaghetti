/** Helpers for Grok's canonical `chat_history.jsonl` record shapes. */

export interface GrokToolCall {
  id: string;
  name: string;
  input: Record<string, unknown>;
}

/** Collect readable text without copying image data URLs into projections. */
export function grokReadableText(value: unknown): string {
  if (typeof value === 'string') return value;
  if (!Array.isArray(value)) return '';
  const parts: string[] = [];
  for (const block of value) {
    if (!block || typeof block !== 'object') continue;
    const record = block as Record<string, unknown>;
    if (typeof record.text === 'string') parts.push(record.text);
  }
  return parts.join('\n');
}

function objectInput(value: unknown): Record<string, unknown> {
  if (value && typeof value === 'object' && !Array.isArray(value)) return value as Record<string, unknown>;
  if (Array.isArray(value)) return { items: value };
  if (value === undefined) return {};
  return { input: value };
}

export function grokToolInput(value: unknown): Record<string, unknown> {
  if (typeof value !== 'string') return objectInput(value);
  try {
    return objectInput(JSON.parse(value) as unknown);
  } catch {
    return { input: value };
  }
}

function sortJson(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(sortJson);
  if (!value || typeof value !== 'object') return value;
  const out: Record<string, unknown> = {};
  for (const key of Object.keys(value as Record<string, unknown>).sort()) {
    out[key] = sortJson((value as Record<string, unknown>)[key]);
  }
  return out;
}

/** Deterministic TS/Rust search projection; raw JSON remains untouched. */
export function grokSearchInput(value: unknown): string {
  try {
    return JSON.stringify(sortJson(value));
  } catch {
    return String(value ?? '');
  }
}

/** Embedded local tool calls carried by an assistant record. */
export function grokAssistantToolCalls(line: Record<string, unknown>): GrokToolCall[] {
  if (!Array.isArray(line.tool_calls)) return [];
  const calls: GrokToolCall[] = [];
  for (const raw of line.tool_calls) {
    if (!raw || typeof raw !== 'object') continue;
    const call = raw as Record<string, unknown>;
    const id = typeof call.id === 'string' ? call.id : '';
    const name = typeof call.name === 'string' && call.name ? call.name : 'Unknown Tool';
    calls.push({ id, name, input: grokToolInput(call.arguments) });
  }
  return calls;
}

/** Grok's standalone backend call (currently observed for web search). */
export function grokBackendToolCall(line: Record<string, unknown>): GrokToolCall | null {
  if (line.type !== 'backend_tool_call' || !line.kind || typeof line.kind !== 'object') return null;
  const kind = line.kind as Record<string, unknown>;
  const name = typeof kind.tool_type === 'string' && kind.tool_type ? kind.tool_type : 'Backend Tool';
  const id = typeof kind.id === 'string' ? kind.id : '';
  return { id, name, input: grokToolInput(kind.action) };
}

export function grokReasoningSummary(line: Record<string, unknown>): string {
  return grokReadableText(line.summary).trim();
}

/**
 * Return genuine human text, or null for Grok-injected model context.
 *
 * Grok records project instructions, reminders, compaction messages and task
 * notifications with role `user`. `synthetic_reason` is authoritative when
 * present. Bootstrap `<user_info>` records are also model context. A genuine
 * prompt may be followed by a reminder, so `<user_query>` is extracted before
 * considering the surrounding wrapper text.
 */
export function grokHumanUserText(line: Record<string, unknown>): string | null {
  if (line.type !== 'user') return null;
  const text = grokReadableText(line.content);
  if (typeof line.synthetic_reason === 'string' && line.synthetic_reason) return null;

  const query = text.match(/<user_query>\s*([\s\S]*?)\s*<\/user_query>/i);
  if (query) return query[1]?.trim() || null;

  const imageCount = Array.isArray(line.content)
    ? line.content.filter(
        (block) => block && typeof block === 'object' && (block as Record<string, unknown>).type === 'image',
      ).length
    : 0;
  if (imageCount > 0 || /<image_files(?:\s[^>]*)?>/i.test(text)) {
    return imageCount === 1 ? 'Image attachment' : `${imageCount || 1} image attachments`;
  }

  if (/<user_info(?:\s[^>]*)?>[\s\S]*?<\/user_info>/i.test(text)) return null;
  if (/<system-reminder(?:\s[^>]*)?>/i.test(text)) return null;
  return text.trim() || null;
}

export function grokToolResultText(line: Record<string, unknown>): string {
  return grokReadableText(line.content);
}

export function grokToolResultId(line: Record<string, unknown>): string {
  return typeof line.tool_call_id === 'string' ? line.tool_call_id : '';
}

/** Compact searchable text for an assistant record, including its calls. */
export function grokAssistantSearchText(line: Record<string, unknown>): string {
  const parts: string[] = [];
  const prose = grokReadableText(line.content);
  if (prose) parts.push(prose);
  for (const call of grokAssistantToolCalls(line)) {
    const input = grokSearchInput(call.input);
    parts.push(input ? `${call.name} ${input}` : call.name);
  }
  return parts.join('\n');
}
