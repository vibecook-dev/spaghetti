/**
 * Message filters — solo/mute type + tool pills + in-transcript search.
 * Ported from p008-claude-on-the-go desktop ProjectPage.
 */

export { MessageFilterBar } from './MessageFilterBar.js';
export type { MessageFilterBarProps } from './MessageFilterBar.js';
export {
  useMessageFilters,
  DEFAULT_TYPE_FILTERS,
  type FilterState,
  type FilterStates,
  type TypeFilterConfig,
  type UseMessageFiltersProps,
  type UseMessageFiltersReturn,
} from './useMessageFilters.js';
export {
  countTimelineMessages,
  filterTimelineMessages,
  getMessageSearchText,
  type MessageCounts,
  type FilterTimelineOptions,
} from './filterMessages.js';
