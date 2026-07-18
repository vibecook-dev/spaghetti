import type { SessionMessage as TimelineMessage } from '../react/chat/types.js';

/** Session-wide normalized message counts. Never limited to the loaded page. */
export interface TimelineFacets {
  total: number;
  messageCounts: Record<string, number>;
  toolCounts: Record<string, number>;
}

/**
 * Database filter for normalized timeline rows.
 *
 * When either include array is present, rows matching an included message type
 * OR included tool name are returned (stackable solo behavior). Exclusions are
 * used only when no solo is active by the playground.
 */
export interface TimelineFilter {
  includeTypes?: string[];
  includeTools?: string[];
  excludeTypes?: string[];
  excludeTools?: string[];
  search?: string;
}

export interface TimelinePageRequest extends TimelineFilter {
  sourceId?: string;
  limit?: number;
  /** Return rows older than this stable normalized timeline index. */
  before?: number;
}

export interface TimelinePage {
  messages: TimelineMessage[];
  total: number;
  /** Cursor for the next (older) page. Undefined at the beginning. */
  nextCursor?: number;
  hasMore: boolean;
}
