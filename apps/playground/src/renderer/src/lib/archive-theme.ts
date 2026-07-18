/**
 * Archive / paper design tokens shared by the playground shell.
 * Matches spaghetti-ui-design (Figma “Start Design Implementation”).
 */

import type { CSSProperties } from 'react';

export type ArchiveSource = 'claude-code' | 'codex' | 'grok' | string;

/** Source badge ink: [light, dark] */
const SOURCE_INK: Record<string, [string, string]> = {
  'claude-code': ['#9a3b28', '#d98d78'],
  claude: ['#9a3b28', '#d98d78'],
  codex: ['#3f5c6b', '#87b3c3'],
  grok: ['#786037', '#c4a96a'],
  native: ['#4f6b4a', '#8ba888'],
  rs: ['#4f6b4a', '#8ba888'],
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

/** Outer page + paper panel background (noise + gradient). */
export function paperStyle(isDark: boolean): CSSProperties {
  const noiseOpacity = isDark ? 0.03 : 0.06;
  const gradient = isDark ? '#171615, #11100f' : '#f8f6f0, #e9e5da';
  return {
    backgroundImage: `
      url("data:image/svg+xml,%3Csvg viewBox='0 0 200 200' xmlns='http://www.w3.org/2000/svg'%3E%3Cfilter id='noiseFilter'%3E%3CfeTurbulence type='fractalNoise' baseFrequency='1.2' numOctaves='3' stitchTiles='stitch'/%3E%3C/filter%3E%3Crect width='100%25' height='100%25' filter='url(%23noiseFilter)' opacity='${noiseOpacity}'/%3E%3C/svg%3E"),
      linear-gradient(to bottom, ${gradient})
    `,
    backgroundBlendMode: 'multiply, normal',
  };
}
