/**
 * Archive / paper design tokens shared by the playground shell.
 * Matches spaghetti-ui-design (Figma “Start Design Implementation”).
 */

import type { CSSProperties } from 'react';

export type ArchiveSource = 'claude-code' | 'codex' | 'grok' | string;

/** Core palette — light paper / dark parchment ink. */
export const ARCHIVE = {
  ink: { light: '#2b2623', dark: '#d4cbbd' },
  paper: { light: '#f1ede4', dark: '#141312' },
  paperBright: { light: '#fcfaf6', dark: '#141312' },
  paperDeep: { light: '#e9e5da', dark: '#11100f' },
  chrome: { light: '#d1ccc0', dark: '#0a0908' },
  /** Gradients for paperStyle */
  gradientTop: { light: '#f8f6f0', dark: '#171615' },
  gradientBot: { light: '#e9e5da', dark: '#11100f' },
  /** Semantic accents (accentHex / modelInk) */
  sanguine: { light: '#9a3b28', dark: '#c9755f' },
  /** Claude model badge uses a slightly warmer dark sanguine */
  sanguineBadge: { light: '#9a3b28', dark: '#d98d78' },
  indigo: { light: '#3f5c6b', dark: '#7fa6b5' },
  indigoBadge: { light: '#3f5c6b', dark: '#87b3c3' },
  faded: { light: '#736958', dark: '#a89a86' },
  verdigris: { light: '#4f6b4a', dark: '#8ba888' },
  ochre: { light: '#786037', dark: '#c4a96a' },
  /** File-viewer code pane (design dialog) */
  codePane: { light: '#eee9de', dark: '#0f0e0d' },
} as const;

function pick(pair: { light: string; dark: string }, isDark: boolean): string {
  return isDark ? pair.dark : pair.light;
}

/** Source badge ink: [light, dark] — design modelInk */
const SOURCE_INK: Record<string, [string, string]> = {
  'claude-code': [ARCHIVE.sanguineBadge.light, ARCHIVE.sanguineBadge.dark],
  claude: [ARCHIVE.sanguineBadge.light, ARCHIVE.sanguineBadge.dark],
  codex: [ARCHIVE.indigoBadge.light, ARCHIVE.indigoBadge.dark],
  grok: [ARCHIVE.ochre.light, ARCHIVE.ochre.dark],
  native: [ARCHIVE.verdigris.light, ARCHIVE.verdigris.dark],
  rs: [ARCHIVE.verdigris.light, ARCHIVE.verdigris.dark],
};

export function sourceInk(sourceId: string, isDark: boolean): string {
  const pair = SOURCE_INK[sourceId] ?? SOURCE_INK['claude-code']!;
  return pair[isDark ? 1 : 0];
}

export function sourceArchiveLabel(sourceId: string): string {
  switch (sourceId) {
    case 'claude-code':
      return 'CLAUDE';
    case 'codex':
      return 'CODEX';
    case 'grok':
      return 'GROK';
    default:
      return sourceId.toUpperCase().slice(0, 8);
  }
}

/**
 * Per-message-type accent (design accentHex).
 * Used by the transcript rail and filter pills.
 */
export type ArchiveMsgKind =
  | 'user'
  | 'assistant'
  | 'thought'
  | 'thinking'
  | 'tool_use'
  | 'tool_result'
  | 'branch_start'
  | 'system'
  | 'summary'
  | 'compact_summary'
  | 'checkpoint'
  | 'queue-operation'
  | string;

export function accentHex(kind: ArchiveMsgKind, isDark: boolean): string {
  switch (kind) {
    case 'assistant':
      return pick(ARCHIVE.sanguine, isDark);
    case 'branch_start':
    case 'checkpoint':
      return pick(ARCHIVE.indigo, isDark);
    case 'thought':
    case 'thinking':
    case 'system':
    case 'compact_summary':
    case 'summary':
      return pick(ARCHIVE.faded, isDark);
    case 'tool_result':
    case 'queue-operation':
      return pick(ARCHIVE.verdigris, isDark);
    case 'tool_use':
    case 'user':
    default:
      return pick(ARCHIVE.ink, isDark);
  }
}

/** Outer page + paper panel background (noise + gradient). */
export function paperStyle(isDark: boolean): CSSProperties {
  const noiseOpacity = isDark ? 0.03 : 0.06;
  const gradient = isDark
    ? `${ARCHIVE.gradientTop.dark}, ${ARCHIVE.gradientBot.dark}`
    : `${ARCHIVE.gradientTop.light}, ${ARCHIVE.gradientBot.light}`;
  return {
    backgroundImage: `
      url("data:image/svg+xml,%3Csvg viewBox='0 0 200 200' xmlns='http://www.w3.org/2000/svg'%3E%3Cfilter id='noiseFilter'%3E%3CfeTurbulence type='fractalNoise' baseFrequency='1.2' numOctaves='3' stitchTiles='stitch'/%3E%3C/filter%3E%3Crect width='100%25' height='100%25' filter='url(%23noiseFilter)' opacity='${noiseOpacity}'/%3E%3C/svg%3E"),
      linear-gradient(to bottom, ${gradient})
    `,
    backgroundBlendMode: 'multiply, normal',
  };
}

export function paperFill(isDark: boolean): string {
  return pick(ARCHIVE.paper, isDark);
}

export function inkHex(isDark: boolean): string {
  return pick(ARCHIVE.ink, isDark);
}
