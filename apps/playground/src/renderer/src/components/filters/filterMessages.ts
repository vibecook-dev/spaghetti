/**
 * Pure helpers: count + filter timeline SessionMessages with solo/mute + search.
 * Logic ported from p008-claude-on-the-go ProjectPage.
 */

import type { ChatSessionMessage } from '@vibecook/spaghetti-sdk/react';
import type { FilterStates } from './useMessageFilters.js';

export interface MessageCounts {
  messageCounts: Record<string, number>;
  toolCounts: Record<string, number>;
}

/** Count displayable types / tools in a timeline window. */
export function countTimelineMessages(messages: ChatSessionMessage[]): MessageCounts {
  const messageCounts: Record<string, number> = {
    user: 0,
    assistant: 0,
    thinking: 0,
    tool_result: 0,
    compact_summary: 0,
    checkpoint: 0,
    system: 0,
    summary: 0,
    'queue-operation': 0,
  };
  const toolCounts: Record<string, number> = {};

  for (const m of messages) {
    if (m.type === 'user' && m.content) messageCounts.user++;
    else if (m.type === 'assistant' && m.content) messageCounts.assistant++;
    else if (m.type === 'thinking' && m.content) messageCounts.thinking++;
    else if (m.type === 'tool_use' && m.toolUse) {
      const toolName = m.toolUse.toolName;
      toolCounts[toolName] = (toolCounts[toolName] || 0) + 1;
    } else if (m.type === 'tool_result' && m.toolResult) messageCounts.tool_result++;
    else if (m.type === 'compact_summary' && m.content) messageCounts.compact_summary++;
    else if (m.type === 'checkpoint') messageCounts.checkpoint++;
    else if (m.type === 'system') messageCounts.system++;
    else if (m.type === 'summary') messageCounts.summary++;
    else if (m.type === 'queue-operation') messageCounts['queue-operation']++;
  }

  return { messageCounts, toolCounts };
}

export function getMessageSearchText(m: ChatSessionMessage): string {
  if (m.content) return m.content;
  if (m.toolUse) {
    try {
      return `${m.toolUse.toolName} ${JSON.stringify(m.toolUse.input)}`;
    } catch {
      return m.toolUse.toolName;
    }
  }
  if (m.toolResult) return m.toolResult.content;
  return '';
}

export interface FilterTimelineOptions {
  messages: ChatSessionMessage[];
  visibleTypes: Set<string>;
  visibleTools: Set<string>;
  typeFilters: FilterStates;
  toolFilters: FilterStates;
  anySoloActive: boolean;
  searchQuery: string;
}

/**
 * Apply type/tool solo-mute + text search to timeline messages.
 * Sidechain user prompts are always hidden (same as ProjectPage).
 */
export function filterTimelineMessages(opts: FilterTimelineOptions): ChatSessionMessage[] {
  const { messages, visibleTypes, visibleTools, typeFilters, toolFilters, anySoloActive, searchQuery } = opts;
  const query = searchQuery.toLowerCase().trim();

  return messages.filter((m) => {
    let isVisible = false;

    if (m.type === 'user' && m.content) {
      // Sidechain user rows are agent prompts, not human input
      if (m.isSidechain) {
        isVisible = false;
      } else {
        isVisible = visibleTypes.has('user');
      }
    } else if (m.type === 'assistant' && m.content) {
      isVisible = visibleTypes.has('assistant');
    } else if (m.type === 'tool_use' && m.toolUse) {
      const toolName = m.toolUse.toolName;
      if (anySoloActive) {
        const toolState = toolFilters[toolName];
        const anyToolSoloed = Object.values(toolFilters).some((f) => f.solo);
        if (anyToolSoloed) {
          isVisible = toolState?.solo || false;
        } else {
          // Type-level solo on tool_use isn't in DEFAULT_TYPE_FILTERS, but keep parity
          isVisible = visibleTypes.has('tool_use') || !!typeFilters['tool_use']?.solo;
        }
      } else {
        // No solos — show unless this tool is muted (or unknown tool → show)
        isVisible = visibleTools.has(toolName) || !toolFilters[toolName];
      }
    } else if (m.type === 'tool_result') {
      if (m.toolResult) {
        isVisible = visibleTypes.has('tool_result');
      }
    } else if (m.type === 'compact_summary' && m.content) {
      isVisible = visibleTypes.has('compact_summary');
    } else if (m.type === 'thinking' && m.content) {
      isVisible = visibleTypes.has('thinking');
    } else if (m.type === 'checkpoint') {
      isVisible = visibleTypes.has('checkpoint');
    } else if (m.type === 'system') {
      isVisible = visibleTypes.has('system');
    } else if (m.type === 'summary') {
      isVisible = visibleTypes.has('summary');
    } else if (m.type === 'queue-operation') {
      isVisible = visibleTypes.has('queue-operation');
    }

    if (!isVisible) return false;

    if (query) {
      return getMessageSearchText(m).toLowerCase().includes(query);
    }
    return true;
  });
}
