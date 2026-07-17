/**
 * Display formatters — mirror packages/cli/src/lib/format.ts so playground
 * list rows show the same stats as the TUI (without importing the CLI package).
 */

export interface TokenUsageLike {
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
}

export function totalTokens(usage: TokenUsageLike): number {
  return usage.inputTokens + usage.outputTokens + usage.cacheCreationTokens + usage.cacheReadTokens;
}

export function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return String(n);
}

/** Local mirror of SDK sourceReportsPerMessageTokens (keep renderer free of Node SDK). */
function sourceReportsTokens(sourceId?: string): boolean {
  switch (sourceId) {
    case 'grok':
    case 'codex':
    case 'claude-code':
    case undefined:
      return true;
    default:
      return true;
  }
}

/**
 * Format token usage for display.
 * - estimated → "~1.2K"
 * - zero / n/a → "—"
 */
export function formatTokenUsage(usage: TokenUsageLike, sourceId?: string, tokensEstimated?: boolean): string {
  const n = totalTokens(usage);
  if (tokensEstimated) {
    return n > 0 ? `~${formatTokens(n)}` : '—';
  }
  if (sourceId && !sourceReportsTokens(sourceId)) {
    return '—';
  }
  if (n <= 0) return '—';
  return formatTokens(n);
}

export function formatRelativeTime(iso: string): string {
  if (!iso) return '—';
  const then = new Date(iso).getTime();
  if (Number.isNaN(then)) return '—';
  const diffMs = Date.now() - then;
  const diffMins = Math.floor(diffMs / 60_000);
  if (diffMins < 1) return 'just now';
  if (diffMins < 60) return `${diffMins}m ago`;
  const diffHours = Math.floor(diffMins / 60);
  if (diffHours < 24) return `${diffHours}h ago`;
  const diffDays = Math.floor(diffHours / 24);
  if (diffDays === 1) return 'yesterday';
  if (diffDays < 30) return `${diffDays}d ago`;
  const diffMonths = Math.floor(diffDays / 30);
  return `${diffMonths}mo ago`;
}

export function formatDuration(ms: number): string {
  if (ms < 0 || !Number.isFinite(ms)) ms = 0;
  const seconds = Math.floor(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ${seconds % 60}s`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ${minutes % 60}m`;
}

export function formatNumber(n: number): string {
  return n.toLocaleString('en-US');
}

export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n < 0) return '—';
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

/** Collapse whitespace and truncate for list previews. */
export function flattenPrompt(text: string | undefined | null, maxLen = 72): string {
  if (!text) return '';
  const flat = text.replace(/\n+/g, ' ').replace(/\s+/g, ' ').trim();
  if (!flat) return '';
  if (flat.length <= maxLen) return flat;
  return flat.slice(0, maxLen - 1) + '…';
}
