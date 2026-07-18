import { sourceArchiveLabel, sourceInk } from '../lib/archive-theme.js';

/**
 * Archive-style agent badge: `[ CLAUDE ]` with source-specific ink.
 *
 * Sizes match spaghetti-ui-design:
 *   sm — project list model chip (text-[8px] tracking-[0.12em])
 *   md — session reading header (text-[10px] tracking-[0.1em])
 */
export function SourceBadge({
  sourceId,
  isDark = true,
  size = 'sm',
  className = '',
}: {
  sourceId: string;
  isDark?: boolean;
  size?: 'sm' | 'md';
  className?: string;
}) {
  const color = sourceInk(sourceId, isDark);
  const sizeCls = size === 'md' ? 'text-[10px] font-bold tracking-[0.1em]' : 'text-[8px] font-bold tracking-[0.12em]';
  return (
    <span className={`shrink-0 font-mono ${sizeCls} ${className}`} style={{ color }} title={sourceId}>
      [ {sourceArchiveLabel(sourceId)} ]
    </span>
  );
}
