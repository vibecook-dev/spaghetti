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
  /**
   * Return rows older than this cursor.
   *
   * Rust-owned timelines return an opaque keyset cursor. Numeric indexes are
   * retained only for the repository's legacy compatibility oracle.
   */
  before?: string | number;
}

export interface TimelinePage {
  messages: TimelineMessage[];
  total: number;
  /** Facets returned by the same Rust snapshot, when available. */
  facets?: TimelineFacets;
  /** Cursor for the next (older) page. Undefined at the beginning. */
  nextCursor?: string | number;
  hasMore: boolean;
  /**
   * The requested Rust snapshot expired while observation was still
   * committing. This page is a fresh page-one snapshot and must replace,
   * rather than prepend to, rows held by the caller.
   */
  snapshotReset?: boolean;
}

/** Independently paginated normalized branch rows (oldest page first). */
export interface SubagentTimelinePageRequest extends TimelineFilter {
  sourceId: string;
  workflowId?: string;
  limit?: number;
  offset?: number;
}

export interface SubagentTimelinePage {
  messages: TimelineMessage[];
  total: number;
  offset: number;
  hasMore: boolean;
}
