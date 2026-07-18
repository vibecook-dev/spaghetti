import { sourceArchiveLabel, sourceInk } from '../lib/archive-theme.js';

/**
 * Archive-style agent badge: `[ CLAUDE ]` with source-specific ink.
 */
export function SourceBadge({
  sourceId,
  isDark = true,
  className = '',
}: {
  sourceId: string;
  isDark?: boolean;
  className?: string;
}) {
  const color = sourceInk(sourceId, isDark);
  return (
    <span
      className={`shrink-0 font-mono text-[8px] font-bold tracking-[0.12em] ${className}`}
      style={{ color }}
      title={sourceId}
    >
      [ {sourceArchiveLabel(sourceId)} ]
    </span>
  );
}
