/** Helpers for Codex's canonical `response_item` records. */

export function codexResponseItemPayload(line: Record<string, unknown>): Record<string, unknown> | null {
  if (line.type !== 'response_item') return null;
  const payload = line.payload;
  return payload && typeof payload === 'object' ? (payload as Record<string, unknown>) : null;
}

export function isCodexToolResultType(type: string): boolean {
  // Most Codex results use `*_call_output`; tool catalog search is the one
  // known exception (`tool_search_output`) even though it still pairs by
  // `call_id` with `tool_search_call`.
  return type.endsWith('_call_output') || type === 'tool_search_output';
}

export function isCodexToolCallType(type: string): boolean {
  return type.endsWith('_call') && !isCodexToolResultType(type);
}

export function codexCallId(payload: Record<string, unknown>): string {
  if (typeof payload.call_id === 'string' && payload.call_id) return payload.call_id;
  if (typeof payload.id === 'string' && payload.id) return payload.id;
  return '';
}

export function codexToolName(payload: Record<string, unknown>): string {
  if (typeof payload.name === 'string' && payload.name) return payload.name;
  const type = typeof payload.type === 'string' ? payload.type : '';
  return type.endsWith('_call') ? type.slice(0, -'_call'.length) : type || 'Unknown Tool';
}

function objectInput(value: unknown): Record<string, unknown> {
  if (value && typeof value === 'object' && !Array.isArray(value)) return value as Record<string, unknown>;
  if (Array.isArray(value)) return { items: value };
  if (value === undefined) return {};
  return { input: value };
}

export function codexToolInput(payload: Record<string, unknown>): Record<string, unknown> {
  const raw = payload.arguments ?? payload.input ?? payload.action;
  if (typeof raw !== 'string') return objectInput(raw);
  try {
    return objectInput(JSON.parse(raw) as unknown);
  } catch {
    // custom_tool_call input can be JavaScript rather than JSON.
    return { input: raw };
  }
}

function readableText(value: unknown): string {
  if (typeof value === 'string') return value;
  if (Array.isArray(value)) {
    const parts: string[] = [];
    for (const item of value) {
      if (!item || typeof item !== 'object') continue;
      const record = item as Record<string, unknown>;
      if (typeof record.text === 'string') parts.push(record.text);
    }
    if (parts.length > 0) return parts.join('');
    if (value.length === 0) return '';
    try {
      return JSON.stringify(value, null, 2);
    } catch {
      return String(value);
    }
  }
  if (value == null) return '';
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

export function codexToolOutput(payload: Record<string, unknown>): string {
  return readableText(payload.output ?? payload.tools);
}

export function codexToolOutputIsError(payload: Record<string, unknown>): boolean {
  const status = typeof payload.status === 'string' ? payload.status.toLowerCase() : '';
  return payload.is_error === true || payload.success === false || status === 'error' || status === 'failed';
}

export function codexReasoningSummary(payload: Record<string, unknown>): string {
  return readableText(payload.summary);
}
