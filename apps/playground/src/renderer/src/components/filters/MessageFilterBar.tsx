/**
 * MessageFilterBar — archive-styled type + tool filter tray.
 * Solo (pin) / mute (eye) + text filter — ProjectPage logic, design-mock chrome.
 */

import { memo } from 'react';
import { Eye, EyeOff, Pin, Search, X } from 'lucide-react';
import { DEFAULT_TYPE_FILTERS, type FilterStates } from './useMessageFilters.js';

export interface MessageFilterBarProps {
  typeFilters: FilterStates;
  toolFilters: FilterStates;
  searchQuery: string;
  visibleTypes: Set<string>;
  visibleTools: Set<string>;
  anySoloActive: boolean;
  messageCounts: Record<string, number>;
  toolCounts: Record<string, number>;
  filteredCount: number;
  totalCount: number;
  toggleTypeSolo: (type: string) => void;
  toggleTypeMute: (type: string) => void;
  toggleToolSolo: (toolName: string) => void;
  toggleToolMute: (toolName: string) => void;
  clearAllSolos: () => void;
  setSearchQuery: (query: string) => void;
  isDark?: boolean;
}

interface FilterPillProps {
  label: string;
  color: string;
  count: number;
  state: { solo: boolean; mute: boolean };
  isVisible: boolean;
  onToggleSolo: () => void;
  onToggleMute: () => void;
}

const FilterPill = memo(function FilterPill({
  label,
  color,
  count,
  state,
  isVisible,
  onToggleSolo,
  onToggleMute,
}: FilterPillProps) {
  const active = isVisible && !state.mute;
  return (
    <button
      type="button"
      onClick={onToggleMute}
      aria-pressed={active}
      className="flex h-8 min-w-0 items-center justify-between gap-2 border-0 border-b border-ink/25 px-1 bg-transparent font-mono text-[11px] tracking-[0.04em] transition-colors cursor-pointer"
      style={{
        color: active ? color : undefined,
        borderBottomColor: active ? `${color}aa` : undefined,
        opacity: active ? 1 : 0.38,
        textDecoration: state.mute ? 'line-through' : undefined,
      }}
      title={state.mute ? 'Show this type' : 'Hide this type'}
    >
      <span className="flex min-w-0 items-center gap-2">
        <span className="truncate">{label}</span>
        <span className="text-[10px] leading-none opacity-65">{count}</span>
      </span>
      <span className="flex shrink-0 items-center gap-1.5 opacity-45">
        <span
          role="button"
          tabIndex={0}
          onClick={(e) => {
            e.stopPropagation();
            onToggleSolo();
          }}
          onKeyDown={(e) => {
            if (e.key === 'Enter' || e.key === ' ') {
              e.preventDefault();
              e.stopPropagation();
              onToggleSolo();
            }
          }}
          className="p-0.5"
          style={{ color: state.solo ? color : undefined, opacity: state.solo ? 1 : 0.55 }}
          title={state.solo ? 'Unpin' : 'Pin (solo)'}
        >
          <Pin size={10} strokeWidth={1.5} className={state.solo ? 'fill-current' : ''} />
        </span>
        <span className="p-0.5" style={{ color: state.mute ? '#ef4444' : undefined }}>
          {state.mute ? <EyeOff size={11} strokeWidth={1.5} /> : <Eye size={11} strokeWidth={1.5} />}
        </span>
      </span>
    </button>
  );
});

export const MessageFilterBar = memo(function MessageFilterBar({
  typeFilters,
  toolFilters,
  searchQuery,
  visibleTypes,
  visibleTools,
  anySoloActive,
  messageCounts,
  toolCounts,
  filteredCount,
  totalCount,
  toggleTypeSolo,
  toggleTypeMute,
  toggleToolSolo,
  toggleToolMute,
  clearAllSolos,
  setSearchQuery,
  isDark = true,
}: MessageFilterBarProps) {
  void isDark;

  return (
    <div
      className="shrink-0 border-b border-ink/15 px-2 py-2 md:px-4 lg:px-6 bg-transparent"
      role="toolbar"
      aria-label="Message filters"
    >
      <div className="mx-auto flex max-w-3xl flex-wrap items-center gap-x-2 gap-y-1.5">
        <label className="flex h-8 w-36 items-center gap-2 border-b border-ink/25 px-1 font-mono text-[11px]">
          <Search size={14} className="shrink-0 opacity-45" />
          <input
            type="search"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            placeholder="Filter…"
            className="min-w-0 flex-1 bg-transparent font-mono text-[11px] outline-none placeholder:opacity-45 text-ink"
            aria-label="Filter transcript messages"
            spellCheck={false}
            autoComplete="off"
          />
          {searchQuery ? (
            <button
              type="button"
              onClick={() => setSearchQuery('')}
              className="p-0.5 opacity-50 hover:opacity-100 bg-transparent border-0 text-ink cursor-pointer"
              title="Clear"
            >
              <X size={10} />
            </button>
          ) : null}
        </label>

        {DEFAULT_TYPE_FILTERS.map(({ key, label, color }) => {
          const count = messageCounts[key] || 0;
          if (count === 0) return null;
          const state = typeFilters[key] || { solo: false, mute: false };
          return (
            <FilterPill
              key={key}
              label={label}
              color={color}
              count={count}
              state={state}
              isVisible={visibleTypes.has(key)}
              onToggleSolo={() => toggleTypeSolo(key)}
              onToggleMute={() => toggleTypeMute(key)}
            />
          );
        })}

        {Object.entries(toolCounts)
          .sort((a, b) => b[1] - a[1])
          .slice(0, 8)
          .map(([toolName, count]) => {
            const state = toolFilters[toolName] || { solo: false, mute: false };
            return (
              <FilterPill
                key={toolName}
                label={toolName}
                color="var(--archive-ink)"
                count={count}
                state={state}
                isVisible={visibleTools.has(toolName)}
                onToggleSolo={() => toggleToolSolo(toolName)}
                onToggleMute={() => toggleToolMute(toolName)}
              />
            );
          })}

        {anySoloActive ? (
          <button
            type="button"
            onClick={clearAllSolos}
            className="font-mono text-[9px] tracking-widest uppercase text-sanguine hover:opacity-80 bg-transparent border-0 cursor-pointer px-1"
          >
            Clear solos
          </button>
        ) : null}

        <div className="ml-auto flex h-8 items-center justify-end px-1 font-mono text-[10px] tracking-[0.12em] opacity-50">
          {filteredCount}/{totalCount}
        </div>
      </div>
    </div>
  );
});
