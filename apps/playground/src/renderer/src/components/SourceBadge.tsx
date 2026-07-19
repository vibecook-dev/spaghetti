import { Bot } from 'lucide-react';
import { sourceArchiveLabel, sourceInk } from '../lib/archive-theme.js';

/** Monochrome product marks from LobeHub Icons (MIT), sized to match our badges. */
function SourceProductIcon({ sourceId, size }: { sourceId: string; size: number }) {
  const commonProps = {
    'aria-hidden': true,
    fill: 'currentColor',
    height: size,
    viewBox: '0 0 24 24',
    width: size,
  } as const;

  if (sourceId === 'claude-code' || sourceId === 'claude') {
    return (
      <svg {...commonProps} fillRule="evenodd">
        <path
          clipRule="evenodd"
          d="M20.998 10.949H24v3.102h-3v3.028h-1.487V20H18v-2.921h-1.487V20H15v-2.921H9V20H7.488v-2.921H6V20H4.487v-2.921H3V14.05H0V10.95h3V5h17.998v5.949zM6 10.949h1.488V8.102H6v2.847zm10.51 0H18V8.102h-1.49v2.847z"
        />
      </svg>
    );
  }

  if (sourceId === 'codex') {
    return (
      <svg {...commonProps} fillRule="evenodd">
        <path
          clipRule="evenodd"
          d="M8.086.457a6.105 6.105 0 013.046-.415c1.333.153 2.521.72 3.564 1.7a.117.117 0 00.107.029c1.408-.346 2.762-.224 4.061.366l.063.03.154.076c1.357.703 2.33 1.77 2.918 3.198.278.679.418 1.388.421 2.126a5.655 5.655 0 01-.18 1.631.167.167 0 00.04.155 5.982 5.982 0 011.578 2.891c.385 1.901-.01 3.615-1.183 5.14l-.182.22a6.063 6.063 0 01-2.934 1.851.162.162 0 00-.108.102c-.255.736-.511 1.364-.987 1.992-1.199 1.582-2.962 2.462-4.948 2.451-1.583-.008-2.986-.587-4.21-1.736a.145.145 0 00-.14-.032c-.518.167-1.04.191-1.604.185a5.924 5.924 0 01-2.595-.622 6.058 6.058 0 01-2.146-1.781c-.203-.269-.404-.522-.551-.821a7.74 7.74 0 01-.495-1.283 6.11 6.11 0 01-.017-3.064.166.166 0 00.008-.074.115.115 0 00-.037-.064 5.958 5.958 0 01-1.38-2.202 5.196 5.196 0 01-.333-1.589 6.915 6.915 0 01.188-2.132c.45-1.484 1.309-2.648 2.577-3.493.282-.188.55-.334.802-.438.286-.12.573-.22.861-.304a.129.129 0 00.087-.087A6.016 6.016 0 015.635 2.31C6.315 1.464 7.132.846 8.086.457zm-.804 7.85a.848.848 0 00-1.473.842l1.694 2.965-1.688 2.848a.849.849 0 001.46.864l1.94-3.272a.849.849 0 00.007-.854l-1.94-3.393zm5.446 6.24a.849.849 0 000 1.695h4.848a.849.849 0 000-1.696h-4.848z"
        />
      </svg>
    );
  }

  if (sourceId === 'grok') {
    return (
      <svg {...commonProps}>
        <path d="M9.27 15.29l7.978-5.897c.391-.29.95-.177 1.137.272.98 2.369.542 5.215-1.41 7.169-1.951 1.954-4.667 2.382-7.149 1.406l-2.711 1.257c3.889 2.661 8.611 2.003 11.562-.953 2.341-2.344 3.066-5.539 2.388-8.42l.006.007c-.983-4.232.242-5.924 2.75-9.383.06-.082.12-.164.179-.248l-3.301 3.305v-.01L9.267 15.292M7.623 16.723c-2.792-2.67-2.31-6.801.071-9.184 1.761-1.763 4.647-2.483 7.166-1.425l2.705-1.25a7.808 7.808 0 00-1.829-1A8.975 8.975 0 005.984 5.83c-2.533 2.536-3.33 6.436-1.962 9.764 1.022 2.487-.653 4.246-2.34 6.022-.599.63-1.199 1.259-1.682 1.925l7.62-6.815" />
      </svg>
    );
  }

  return <Bot aria-hidden="true" size={size} strokeWidth={1.7} />;
}

/**
 * Compact archive-style agent badge. The icon keeps multi-agent project and
 * session rows readable without repeating long source labels.
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
  const iconSize = size === 'md' ? 12 : 10;
  const label = sourceArchiveLabel(sourceId);
  return (
    <span
      className={`inline-flex shrink-0 items-center justify-center bg-transparent ${
        size === 'md' ? 'size-[20px]' : 'size-4'
      } ${className}`}
      style={{ color }}
      title={label}
      aria-label={label}
    >
      <SourceProductIcon sourceId={sourceId} size={iconSize} />
    </span>
  );
}

export function SourceBadges({
  sourceIds,
  isDark = true,
  size = 'sm',
  className = '',
}: {
  sourceIds: string[];
  isDark?: boolean;
  size?: 'sm' | 'md';
  className?: string;
}) {
  return (
    <span className={`inline-flex shrink-0 items-center gap-1 ${className}`}>
      {sourceIds.map((sourceId) => (
        <SourceBadge key={sourceId} sourceId={sourceId} isDark={isDark} size={size} />
      ))}
    </span>
  );
}
