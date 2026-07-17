import { sourceLabel } from '../lib/source-progress.js';

/**
 * Per-source accent colors — distinct on near-black UI while staying muted.
 * Claude: warm amber · Codex: green · Grok: blue-violet
 */
const SOURCE_BADGE_STYLES: Record<string, { background: string; color: string; border: string }> = {
  'claude-code': {
    background: 'rgba(232, 168, 90, 0.14)',
    color: '#e8b86d',
    border: 'rgba(232, 168, 90, 0.35)',
  },
  codex: {
    background: 'rgba(110, 200, 160, 0.12)',
    color: '#7dcea0',
    border: 'rgba(110, 200, 160, 0.32)',
  },
  grok: {
    background: 'rgba(130, 160, 255, 0.14)',
    color: '#9bb0ff',
    border: 'rgba(130, 160, 255, 0.35)',
  },
};

export function SourceBadge({ sourceId }: { sourceId: string }) {
  const style = SOURCE_BADGE_STYLES[sourceId] ?? {
    background: 'rgba(255,255,255,0.08)',
    color: 'rgba(255,255,255,0.65)',
    border: 'rgba(255,255,255,0.14)',
  };
  return (
    <span
      style={{
        fontSize: 9,
        padding: '2px 6px',
        borderRadius: 2,
        fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
        letterSpacing: 0.4,
        flexShrink: 0,
        background: style.background,
        color: style.color,
        border: `1px solid ${style.border}`,
      }}
      title={sourceId}
    >
      {sourceLabel(sourceId)}
    </span>
  );
}
