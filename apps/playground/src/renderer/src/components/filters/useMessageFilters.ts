/**
 * useMessageFilters — solo/mute message type + tool filters.
 *
 * Ported from p008-claude-on-the-go desktop ProjectPage filters.
 *
 * Solo: when any solo is active, ONLY soloed types/tools show (stackable).
 * Mute: hide this type/tool (only when no solos are active).
 */

import { useState, useCallback, useMemo, useEffect } from 'react';

export interface FilterState {
  solo: boolean;
  mute: boolean;
}

export type FilterStates = Record<string, FilterState>;

export interface TypeFilterConfig {
  key: string;
  label: string;
  /** CSS color for the pill accent */
  color: string;
}

/** Type pills shown in the filter bar (tool_use appears as per-tool pills). */
export const DEFAULT_TYPE_FILTERS: TypeFilterConfig[] = [
  { key: 'user', label: 'User', color: '#ea580c' },
  { key: 'assistant', label: 'Assistant', color: '#34d399' },
  { key: 'thinking', label: 'Thinking', color: '#a855f7' },
  { key: 'tool_result', label: 'Result', color: '#f59e0b' },
  { key: 'compact_summary', label: 'Summary', color: '#a1a1aa' },
  { key: 'checkpoint', label: 'Checkpoint', color: '#6366f1' },
  { key: 'system', label: 'System', color: '#64748b' },
  { key: 'summary', label: 'Chat Summary', color: '#8b5cf6' },
  { key: 'queue-operation', label: 'Tasks', color: '#22c55e' },
];

export interface UseMessageFiltersProps {
  /** Tool names discovered in the loaded timeline */
  toolNames: string[];
}

export interface UseMessageFiltersReturn {
  typeFilters: FilterStates;
  toolFilters: FilterStates;
  searchQuery: string;
  visibleTypes: Set<string>;
  visibleTools: Set<string>;
  anySoloActive: boolean;
  toggleTypeSolo: (type: string) => void;
  toggleTypeMute: (type: string) => void;
  toggleToolSolo: (toolName: string) => void;
  toggleToolMute: (toolName: string) => void;
  clearAllSolos: () => void;
  setSearchQuery: (query: string) => void;
  resetFilters: () => void;
}

function createInitialTypeFilters(): FilterStates {
  return DEFAULT_TYPE_FILTERS.reduce<FilterStates>((acc, filter) => {
    acc[filter.key] = { solo: false, mute: false };
    return acc;
  }, {});
}

export function useMessageFilters({ toolNames }: UseMessageFiltersProps): UseMessageFiltersReturn {
  const [typeFilters, setTypeFilters] = useState<FilterStates>(createInitialTypeFilters);
  const [toolFilters, setToolFilters] = useState<FilterStates>({});
  const [searchQuery, setSearchQuery] = useState('');

  // Seed tool filter rows as tools appear in the loaded window
  useEffect(() => {
    if (toolNames.length === 0) return;
    setToolFilters((prev) => {
      const updated = { ...prev };
      let hasNew = false;
      for (const name of toolNames) {
        if (!(name in updated)) {
          updated[name] = { solo: false, mute: false };
          hasNew = true;
        }
      }
      return hasNew ? updated : prev;
    });
  }, [toolNames]);

  const toggleTypeSolo = useCallback((type: string) => {
    setTypeFilters((prev) => ({
      ...prev,
      [type]: { solo: !prev[type]?.solo, mute: prev[type]?.mute ?? false },
    }));
  }, []);

  const toggleTypeMute = useCallback((type: string) => {
    setTypeFilters((prev) => ({
      ...prev,
      [type]: { solo: prev[type]?.solo ?? false, mute: !prev[type]?.mute },
    }));
  }, []);

  const toggleToolSolo = useCallback((toolName: string) => {
    setToolFilters((prev) => ({
      ...prev,
      [toolName]: { solo: !prev[toolName]?.solo, mute: prev[toolName]?.mute ?? false },
    }));
  }, []);

  const toggleToolMute = useCallback((toolName: string) => {
    setToolFilters((prev) => ({
      ...prev,
      [toolName]: { solo: prev[toolName]?.solo ?? false, mute: !prev[toolName]?.mute },
    }));
  }, []);

  const clearAllSolos = useCallback(() => {
    setTypeFilters((prev) => {
      const updated: FilterStates = {};
      for (const key of Object.keys(prev)) {
        updated[key] = { ...prev[key], solo: false };
      }
      return updated;
    });
    setToolFilters((prev) => {
      const updated: FilterStates = {};
      for (const key of Object.keys(prev)) {
        updated[key] = { ...prev[key], solo: false };
      }
      return updated;
    });
  }, []);

  const resetFilters = useCallback(() => {
    setTypeFilters(createInitialTypeFilters());
    setToolFilters({});
    setSearchQuery('');
  }, []);

  const anySoloActive = useMemo(() => {
    return Object.values(typeFilters).some((f) => f.solo) || Object.values(toolFilters).some((f) => f.solo);
  }, [typeFilters, toolFilters]);

  const visibleTypes = useMemo(() => {
    const visible = new Set<string>();
    for (const [type, state] of Object.entries(typeFilters)) {
      if (anySoloActive) {
        if (state.solo) visible.add(type);
      } else if (!state.mute) {
        visible.add(type);
      }
    }
    return visible;
  }, [typeFilters, anySoloActive]);

  const visibleTools = useMemo(() => {
    const visible = new Set<string>();
    for (const [tool, state] of Object.entries(toolFilters)) {
      if (anySoloActive) {
        if (state.solo) visible.add(tool);
      } else if (!state.mute) {
        visible.add(tool);
      }
    }
    return visible;
  }, [toolFilters, anySoloActive]);

  return {
    typeFilters,
    toolFilters,
    searchQuery,
    visibleTypes,
    visibleTools,
    anySoloActive,
    toggleTypeSolo,
    toggleTypeMute,
    toggleToolSolo,
    toggleToolMute,
    clearAllSolos,
    setSearchQuery,
    resetFilters,
  };
}
