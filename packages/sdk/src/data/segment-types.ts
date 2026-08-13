/**
 * Segment Types — Type definitions for segment-based data management
 */

export type SegmentType =
  | 'project'
  | 'project_memory'
  | 'session'
  | 'message'
  | 'subagent'
  | 'tool_result'
  | 'file_history'
  | 'todo'
  | 'task'
  | 'plan'
  | 'project_summary'
  | 'session_summary'
  | 'config_settings'
  | 'config_plugins'
  | 'config_statsig'
  | 'config_ide'
  | 'config_shell_snapshots'
  | 'config_cache'
  | 'config_statusline'
  | 'analytics_stats_cache'
  | 'analytics_history'
  | 'analytics_telemetry'
  | 'analytics_debug'
  | 'analytics_paste_cache'
  | 'analytics_session_env';

export type SegmentKey = string;

export function segmentKey(type: SegmentType, ...qualifiers: string[]): SegmentKey {
  return qualifiers.length > 0 ? `${type}:${qualifiers.join('/')}` : `${type}:`;
}

export function parseSegmentKey(key: SegmentKey): { type: SegmentType; qualifiers: string[] } {
  const colonIdx = key.indexOf(':');
  if (colonIdx === -1) return { type: key as SegmentType, qualifiers: [] };
  const type = key.substring(0, colonIdx) as SegmentType;
  const rest = key.substring(colonIdx + 1);
  const qualifiers = rest ? rest.split('/') : [];
  return { type, qualifiers };
}

export interface Segment<T = unknown> {
  key: SegmentKey;
  type: SegmentType;
  data: T;
  version: number;
  updatedAt: number;
}

export interface SourceFingerprint {
  path: string;
  mtimeMs: number;
  size: number;
  bytePosition?: number;
}

export type SegmentChangeAction = 'upsert' | 'delete';

export interface SegmentChange {
  key: SegmentKey;
  type: SegmentType;
  action: SegmentChangeAction;
  /** Agent source owning this session/project change. */
  sourceId?: string;
  projectSlug?: string;
  sessionId?: string;
  /** Monotonic process-local live revision (`Change.seq`). */
  revision?: number;
}

export interface SegmentChangeBatch {
  changes: SegmentChange[];
  timestamp: number;
}

export type FileCategory =
  | 'session_jsonl'
  | 'sessions_index'
  | 'project_memory'
  | 'subagent_jsonl'
  | 'tool_result'
  | 'file_history'
  | 'todo'
  | 'task'
  | 'plan'
  | 'config_settings'
  | 'config_plugins'
  | 'config_statsig'
  | 'config_ide'
  | 'config_shell_snapshots'
  | 'config_cache'
  | 'config_statusline'
  | 'analytics_stats_cache'
  | 'analytics_history'
  | 'analytics_telemetry'
  | 'analytics_debug'
  | 'analytics_paste_cache'
  | 'analytics_session_env'
  | 'unknown';

export interface FileClassification {
  category: FileCategory;
  projectSlug?: string;
  sessionId?: string;
  qualifier?: string;
}

export interface InitProgress {
  phase: 'parsing' | 'storing' | 'indexing' | 'reconciling';
  message: string;
  current?: number;
  total?: number;
  /** Explicit native adapter identity; consumers must not parse `message`. */
  sourceId?: string;
  sourceStage?: 'active' | 'done';
  /** One-based configured source position. */
  sourceIndex?: number;
  sourceCount?: number;
  elapsedMs?: number;
  commitSeq?: number;
}

export interface PaginatedSegmentQuery {
  type: SegmentType;
  keyPrefix: string;
  limit: number;
  offset: number;
}

export interface PaginatedSegmentResult<T = unknown> {
  segments: Segment<T>[];
  total: number;
  offset: number;
  hasMore: boolean;
}

export interface SearchQuery {
  text: string;
  type?: SegmentType;
  /** Exact source/slug rows for an aggregated project scope. */
  projectMembers?: Array<{ sourceId: string; slug: string }>;
  /** Legacy single-slug scope. Prefer `projectMembers`. */
  projectSlug?: string;
  sessionId?: string;
  /** When set, only messages from this agent source (`claude-code` | `codex` | `grok`). */
  sourceId?: string;
  tags?: string[];
  limit?: number;
  offset?: number;
}

export interface SearchResult {
  key: SegmentKey;
  type: SegmentType;
  snippet: string;
  rank: number;
  projectSlug?: string;
  sessionId?: string;
  /** Agent product that owns the hit (multi-source index). */
  sourceId?: string;
  /** Present when the hit belongs to an inline subagent branch. */
  agentId?: string;
  workflowId?: string;
  spawnToolId?: string;
  agentTimelineIndex?: number;
}

export interface SearchResultSet {
  results: SearchResult[];
  total: number;
  hasMore: boolean;
}

export interface StoreStats {
  totalSegments: number;
  segmentsByType: Record<string, number>;
  totalFingerprints: number;
  dbSizeBytes: number;
  searchIndexed: number;
}
