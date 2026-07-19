/**
 * Formatting utilities — tokens, bytes, time, numbers, bars
 *
 * Ported and expanded from @spaghetti/ui formatters.
 */

import { sourceReportsPerMessageTokens } from '@vibecook/spaghetti-sdk';
import { theme } from './color.js';

export function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return String(n);
}

/**
 * Format token usage for display.
 * - `tokensEstimated`: prefix with "~" so local estimates aren't read as
 *   API-accurate counts (Codex tiktoken fallback).
 * - `sourceId` without per-message tokens still shows "—" when nothing
 *   was attributed (legacy sources).
 */
export function formatTokenUsage(
  usage: {
    inputTokens: number;
    outputTokens: number;
    cacheCreationTokens: number;
    cacheReadTokens: number;
    totalTokens?: number;
  },
  sourceId?: string,
  tokensEstimated?: boolean,
): string {
  const n = totalTokens(usage);
  if (tokensEstimated) {
    return n > 0 ? `~${formatTokens(n)}` : '—';
  }
  if (sourceId && !sourceReportsPerMessageTokens(sourceId)) {
    return '—';
  }
  return formatTokens(n);
}

export function formatBytes(n: number): string {
  if (n >= 1_073_741_824) return `${(n / 1_073_741_824).toFixed(1)} GB`;
  if (n >= 1_048_576) return `${(n / 1_048_576).toFixed(1)} MB`;
  if (n >= 1_024) return `${(n / 1_024).toFixed(1)} KB`;
  return `${n} B`;
}

export function formatRelativeTime(iso: string): string {
  const now = Date.now();
  const then = new Date(iso).getTime();
  const diffMs = now - then;
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
  if (ms < 0) ms = 0;
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

export function formatBar(value: number, max: number, width: number = 20): string {
  const ratio = max > 0 ? Math.min(value / max, 1) : 0;
  const filled = Math.round(ratio * width);
  const empty = width - filled;
  return theme.bar('\u2588'.repeat(filled)) + theme.barEmpty('\u2591'.repeat(empty));
}

/**
 * Compute total tokens from a TokenUsageSummary-like object.
 */
export function totalTokens(usage: {
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  totalTokens?: number;
}): number {
  return (
    usage.totalTokens ?? usage.inputTokens + usage.outputTokens + usage.cacheCreationTokens + usage.cacheReadTokens
  );
}
