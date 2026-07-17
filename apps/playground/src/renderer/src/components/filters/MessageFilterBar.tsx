/**
 * MessageFilterBar — type + tool filter pills with solo (pin) / mute (eye).
 * Ported from p008-claude-on-the-go; styled for playground dark chrome.
 * Icons are inline SVG (no lucide dependency).
 */

import { memo } from 'react';
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
}

// ── Icons ───────────────────────────────────────────────────────────────────

function IconSearch({ size = 10 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden>
      <circle cx="11" cy="11" r="7" />
      <path d="M20 20l-3.5-3.5" strokeLinecap="round" />
    </svg>
  );
}

function IconX({ size = 8 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" aria-hidden>
      <path d="M6 6l12 12M18 6L6 18" strokeLinecap="round" />
    </svg>
  );
}

function IconPin({ size = 10, filled }: { size?: number; filled?: boolean }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill={filled ? 'currentColor' : 'none'}
      stroke="currentColor"
      strokeWidth="2"
      aria-hidden
    >
      <path d="M12 17v5M9 3h6l-1 7h3l-5 5-5-5h3L9 3z" strokeLinejoin="round" strokeLinecap="round" />
    </svg>
  );
}

function IconEye({ size = 10 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden>
      <path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z" />
      <circle cx="12" cy="12" r="3" />
    </svg>
  );
}

function IconEyeOff({ size = 10 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden>
      <path
        d="M3 3l18 18M10.6 10.6a3 3 0 004.2 4.2M9.5 5.2A10.5 10.5 0 0112 5c6.5 0 10 7 10 7a18.4 18.4 0 01-4.2 4.7M6.1 6.1A18 18 0 002 12s3.5 7 10 7c1.4 0 2.7-.3 3.9-.8"
        strokeLinecap="round"
      />
    </svg>
  );
}

// ── Pills ───────────────────────────────────────────────────────────────────

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
  return (
    <div
      className="flex items-center gap-1.5 px-2 py-1 rounded text-[10px] font-mono transition-all shrink-0 border"
      style={{
        backgroundColor: state.solo ? withAlpha(color, 0.14) : 'transparent',
        borderColor: state.solo ? color : 'rgba(255,255,255,0.1)',
        color: state.mute ? 'rgba(255,255,255,0.35)' : isVisible ? color : 'rgba(255,255,255,0.4)',
        opacity: state.mute ? 0.45 : isVisible ? 1 : 0.55,
      }}
    >
      <span className={state.mute ? 'line-through' : ''}>{label}</span>
      <span
        className="px-1 py-0.5 rounded text-[9px] tabular-nums"
        style={{
          backgroundColor: isVisible ? withAlpha(color, 0.18) : 'rgba(255,255,255,0.06)',
        }}
      >
        {count}
      </span>
      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          onToggleSolo();
        }}
        className="p-0.5 rounded hover:bg-white/10 border-0 bg-transparent cursor-pointer"
        style={{ color: state.solo ? color : 'rgba(255,255,255,0.4)', opacity: state.solo ? 1 : 0.55 }}
        title={state.solo ? 'Unpin (show all)' : 'Pin (solo this type)'}
        aria-pressed={state.solo}
      >
        <IconPin filled={state.solo} />
      </button>
      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          onToggleMute();
        }}
        className="p-0.5 rounded hover:bg-white/10 border-0 bg-transparent cursor-pointer"
        style={{ color: state.mute ? '#ef4444' : 'rgba(255,255,255,0.4)', opacity: state.mute ? 1 : 0.55 }}
        title={state.mute ? 'Show this type' : 'Hide this type'}
        aria-pressed={state.mute}
      >
        {state.mute ? <IconEyeOff /> : <IconEye />}
      </button>
    </div>
  );
});

interface ToolFilterPillProps {
  toolName: string;
  count: number;
  state: { solo: boolean; mute: boolean };
  isVisible: boolean;
  onToggleSolo: () => void;
  onToggleMute: () => void;
}

const ToolFilterPill = memo(function ToolFilterPill({
  toolName,
  count,
  state,
  isVisible,
  onToggleSolo,
  onToggleMute,
}: ToolFilterPillProps) {
  const color = 'rgba(242,242,242,0.85)';
  return (
    <div
      className="flex items-center gap-1.5 px-2 py-1 rounded text-[10px] font-mono transition-all shrink-0 border"
      style={{
        backgroundColor: state.solo ? 'rgba(255,255,255,0.08)' : 'transparent',
        borderColor: state.solo ? 'rgba(255,255,255,0.35)' : 'rgba(255,255,255,0.1)',
        color: state.mute ? 'rgba(255,255,255,0.35)' : isVisible ? color : 'rgba(255,255,255,0.4)',
        opacity: state.mute ? 0.45 : isVisible ? 1 : 0.55,
      }}
    >
      <span className={state.mute ? 'line-through' : ''}>{toolName}</span>
      <span
        className="px-1 py-0.5 rounded text-[9px] tabular-nums"
        style={{
          backgroundColor: isVisible ? 'rgba(255,255,255,0.1)' : 'rgba(255,255,255,0.05)',
        }}
      >
        {count}
      </span>
      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          onToggleSolo();
        }}
        className="p-0.5 rounded hover:bg-white/10 border-0 bg-transparent cursor-pointer"
        style={{ color: state.solo ? '#ea580c' : 'rgba(255,255,255,0.4)', opacity: state.solo ? 1 : 0.55 }}
        title={state.solo ? 'Unpin (show all)' : 'Pin (solo this tool)'}
        aria-pressed={state.solo}
      >
        <IconPin filled={state.solo} />
      </button>
      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          onToggleMute();
        }}
        className="p-0.5 rounded hover:bg-white/10 border-0 bg-transparent cursor-pointer"
        style={{ color: state.mute ? '#ef4444' : 'rgba(255,255,255,0.4)', opacity: state.mute ? 1 : 0.55 }}
        title={state.mute ? 'Show this tool' : 'Hide this tool'}
        aria-pressed={state.mute}
      >
        {state.mute ? <IconEyeOff /> : <IconEye />}
      </button>
    </div>
  );
});

// ── Bar ─────────────────────────────────────────────────────────────────────

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
}: MessageFilterBarProps) {
  return (
    <div
      className="shrink-0 flex flex-col border-b border-white/10 bg-white/[0.02]"
      role="toolbar"
      aria-label="Message filters"
    >
      <div className="flex items-center gap-1.5 px-3 py-1.5 flex-wrap">
        {/* In-transcript search */}
        <div className="flex items-center gap-1 px-1.5 py-0.5 rounded shrink-0 bg-black/40 border border-white/10">
          <span className="text-white/35">
            <IconSearch />
          </span>
          <input
            type="search"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            placeholder="Filter…"
            className="bg-transparent border-none outline-none text-[10px] font-mono w-24 text-white/85 placeholder:text-white/30"
            spellCheck={false}
            autoComplete="off"
          />
          {searchQuery ? (
            <button
              type="button"
              onClick={() => setSearchQuery('')}
              className="p-0.5 rounded hover:bg-white/10 border-0 bg-transparent cursor-pointer text-white/40"
              title="Clear filter text"
            >
              <IconX />
            </button>
          ) : null}
        </div>

        <div className="w-px h-3 shrink-0 bg-white/10" aria-hidden />

        {/* Type filters — only when present in the loaded window */}
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

        {/* Per-tool filters, highest count first */}
        {Object.entries(toolCounts)
          .sort((a, b) => b[1] - a[1])
          .map(([toolName, count]) => {
            const state = toolFilters[toolName] || { solo: false, mute: false };
            return (
              <ToolFilterPill
                key={toolName}
                toolName={toolName}
                count={count}
                state={state}
                isVisible={visibleTools.has(toolName)}
                onToggleSolo={() => toggleToolSolo(toolName)}
                onToggleMute={() => toggleToolMute(toolName)}
              />
            );
          })}

        {anySoloActive ? (
          <>
            <div className="w-px h-3 shrink-0 bg-white/10" aria-hidden />
            <button
              type="button"
              onClick={clearAllSolos}
              className="text-[9px] font-medium px-1.5 py-0.5 rounded shrink-0 text-orange-400/90 hover:text-orange-300 cursor-pointer bg-transparent border-0"
            >
              Clear solos
            </button>
          </>
        ) : null}

        <span className="text-[9px] font-mono ml-auto shrink-0 text-white/30 tabular-nums">
          {filteredCount}/{totalCount}
        </span>
      </div>
    </div>
  );
});

/** #rrggbb → rgba */
function withAlpha(hex: string, alpha: number): string {
  if (!hex.startsWith('#') || (hex.length !== 7 && hex.length !== 4)) {
    return hex;
  }
  let r: number;
  let g: number;
  let b: number;
  if (hex.length === 4) {
    r = parseInt(hex[1] + hex[1], 16);
    g = parseInt(hex[2] + hex[2], 16);
    b = parseInt(hex[3] + hex[3], 16);
  } else {
    r = parseInt(hex.slice(1, 3), 16);
    g = parseInt(hex.slice(3, 5), 16);
    b = parseInt(hex.slice(5, 7), 16);
  }
  return `rgba(${r},${g},${b},${alpha})`;
}
