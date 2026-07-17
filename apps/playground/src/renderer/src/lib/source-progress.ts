/**
 * Helpers for mapping SDK InitProgress onto a multi-source loading UI.
 * Kept local so the renderer never imports the Node SDK runtime.
 */

export type SourceId = 'claude-code' | 'codex' | 'grok' | string;

export type SourceStage = 'pending' | 'active' | 'done';

export interface ProgressSnapshot {
  phase: string;
  message: string;
  current?: number;
  total?: number;
}

export interface SourceProgressState {
  id: SourceId;
  label: string;
  stage: SourceStage;
  /** Last progress fraction 0–1 seen for this source (undefined = indeterminate). */
  fraction?: number;
  detail?: string;
}

export const KNOWN_SOURCES: { id: SourceId; label: string }[] = [
  { id: 'claude-code', label: 'Claude Code' },
  { id: 'codex', label: 'Codex' },
  { id: 'grok', label: 'Grok' },
];

export function sourceLabel(sourceId: string): string {
  switch (sourceId) {
    case 'claude-code':
      return 'Claude Code';
    case 'codex':
      return 'Codex';
    case 'grok':
      return 'Grok';
    default:
      return sourceId;
  }
}

/** Infer agent source from SDK progress message text. */
export function inferSourceFromMessage(message: string): SourceId | null {
  const m = message.toLowerCase();
  if (/\bgrok\b|\.grok\b/.test(m)) return 'grok';
  if (/\bcodex\b|\.codex\b/.test(m)) return 'codex';
  if (/\bclaude\b|\.claude\b/.test(m)) return 'claude-code';
  return null;
}

export function phaseLabel(phase: string): string {
  switch (phase) {
    case 'parsing':
      return 'Parsing';
    case 'storing':
      return 'Storing';
    case 'indexing':
      return 'Indexing';
    case 'reconciling':
      return 'Reconciling';
    default:
      return phase ? phase.charAt(0).toUpperCase() + phase.slice(1) : 'Working';
  }
}

/**
 * Apply a progress event onto ordered source rows.
 * Multi-source ingest runs serially; once a later source appears, earlier ones
 * are marked done.
 *
 * Claude progress messages often omit the word "Claude" ("Parsing 12 projects…"),
 * so unattributed events attach to the current active row, or promote the first
 * pending row (Claude runs first in the default owner order).
 */
export function applyProgressEvent(sources: SourceProgressState[], event: ProgressSnapshot): SourceProgressState[] {
  let inferred = inferSourceFromMessage(event.message);
  const fraction =
    event.total != null && event.total > 0 && event.current != null
      ? Math.min(1, Math.max(0, event.current / event.total))
      : undefined;

  // If we cannot attribute to a source, keep current active row's detail —
  // or start the first pending source (Claude Code is first in serial ingest).
  if (!inferred) {
    const activeIdx = sources.findIndex((s) => s.stage === 'active');
    if (activeIdx >= 0) {
      return sources.map((s, i) =>
        i === activeIdx ? { ...s, detail: event.message, fraction: fraction ?? s.fraction } : s,
      );
    }
    const firstPending = sources.findIndex((s) => s.stage === 'pending');
    if (firstPending >= 0) {
      inferred = sources[firstPending]!.id;
    } else {
      return sources;
    }
  }

  const order = sources.map((s) => s.id);
  const idx = order.indexOf(inferred);
  // Unknown source — append as active.
  if (idx === -1) {
    return [
      ...sources.map((s) => (s.stage === 'active' || s.stage === 'pending' ? { ...s, stage: 'done' as const } : s)),
      {
        id: inferred,
        label: sourceLabel(inferred),
        stage: 'active',
        fraction,
        detail: event.message,
      },
    ];
  }

  return sources.map((s, i) => {
    if (i < idx) {
      return { ...s, stage: 'done' as const, fraction: s.fraction ?? 1, detail: s.detail };
    }
    if (i === idx) {
      return {
        ...s,
        stage: 'active' as const,
        fraction: fraction ?? s.fraction,
        detail: event.message,
      };
    }
    // Later sources stay pending (or keep done if already finished on rebuild edge cases).
    return s.stage === 'done' ? s : { ...s, stage: 'pending' as const };
  });
}

export function initialSourceStates(detected?: SourceId[]): SourceProgressState[] {
  const ids = detected && detected.length > 0 ? detected : KNOWN_SOURCES.map((s) => s.id);
  // Always show the three known agents when we don't know yet — pending until
  // messages prove which ones are present. Prefer known order.
  const ordered = KNOWN_SOURCES.filter((s) => ids.includes(s.id));
  const extras = ids
    .filter((id) => !KNOWN_SOURCES.some((k) => k.id === id))
    .map((id) => ({ id, label: sourceLabel(id) }));
  return [...ordered, ...extras].map((s, i) => ({
    id: s.id,
    label: s.label,
    stage: i === 0 ? ('pending' as const) : ('pending' as const),
  }));
}

/** Overall 0–1 progress across sources (equal weight). */
export function overallFraction(sources: SourceProgressState[]): number | undefined {
  if (sources.length === 0) return undefined;
  let sum = 0;
  let known = 0;
  for (const s of sources) {
    if (s.stage === 'done') {
      sum += 1;
      known += 1;
    } else if (s.stage === 'active') {
      sum += s.fraction ?? 0.05;
      known += 1;
    } else {
      known += 1;
    }
  }
  if (known === 0) return undefined;
  return sum / sources.length;
}

export function formatElapsed(ms: number): string {
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  const rem = s % 60;
  return `${m}m ${rem.toString().padStart(2, '0')}s`;
}
