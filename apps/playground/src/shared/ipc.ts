/**
 * IPC contract between main and renderer.
 *
 * Every method maps 1:1 to a channel of the form `spaghetti:<method>` that the
 * main process exposes via ipcMain.handle, and the preload forwards through
 * contextBridge.exposeInMainWorld('spaghetti', …).
 *
 * The subset of the SDK's SpaghettiAPI exposed here is everything a read-only
 * agent-data browser needs — list/read/search, plus the initialization
 * lifecycle. Mutations aren't surfaced (the playground is read-only).
 *
 * Progress and change events are exposed as one-way channels from main →
 * renderer (no invoke), wrapped by `onProgress` / `onReady` / `onChange` on
 * the window.spaghetti surface. Each returns an unsubscribe function.
 */

import type {
  MessagePage,
  TimelineFacets,
  TimelinePage,
  TimelinePageRequest,
  ProjectListItem,
  ProjectReference,
  SessionListItem,
  SourceFilter,
  SubagentFilter,
  SubagentListItem,
  SubagentMessagePage,
  SubagentTimelinePage,
  SubagentTimelinePageRequest,
  TokenActivityQuery,
  TokenActivityResult,
} from '@vibecook/spaghetti-sdk';
import type {
  InitProgress,
  SearchQuery,
  SearchResultSet,
  SegmentChangeBatch,
  StoreStats,
} from '@vibecook/spaghetti-sdk';

export interface ReadyInfo {
  durationMs: number;
}

export interface SessionStreamSnapshot {
  streamId: string;
  sourceId: string;
  projectSlug: string;
  sessionId: string;
  page: TimelinePage;
  facets: TimelineFacets;
  subagents: SubagentListItem[];
}

export interface ActiveSessionChange {
  streamId: string;
  sourceId: string;
  projectSlug: string;
  sessionId: string;
  revision: number;
  reason: 'append' | 'upsert' | 'subagent' | 'reset';
}

export interface SpaghettiIPC {
  // Lifecycle ---------------------------------------------------------------
  isReady(): Promise<boolean>;
  /** Force a full cold rebuild of the index. */
  rebuildIndex(): Promise<{ durationMs: number }>;
  /**
   * Wipe the SQLite cache and re-run initialize from disk.
   * Used after corrupt/malformed DB errors so the UI is not stuck.
   */
  retryInit(): Promise<{ ok: true }>;
  /** Resolved ingest engine: `'rs'` (native Rust) or `'ts'` (TypeScript). */
  getEngine(): Promise<'rs' | 'ts'>;

  // Projects ----------------------------------------------------------------
  getProjectList(): Promise<ProjectListItem[]>;
  getProjectTokenActivity(project: ProjectReference, query: TokenActivityQuery): Promise<TokenActivityResult>;
  getProjectMemory(project: ProjectReference, options?: SourceFilter): Promise<string | null>;

  // Sessions ----------------------------------------------------------------
  /** List all project sessions; optionally filter to one agent source. */
  getSessionList(project: ProjectReference, options?: SourceFilter): Promise<SessionListItem[]>;
  getSessionMessages(
    projectSlug: string,
    sessionId: string,
    limit?: number,
    offset?: number,
    options?: SourceFilter,
  ): Promise<MessagePage>;
  getSessionTimelineFacets(projectSlug: string, sessionId: string, options?: SourceFilter): Promise<TimelineFacets>;
  getSessionTimeline(projectSlug: string, sessionId: string, request?: TimelinePageRequest): Promise<TimelinePage>;
  /** Register the one visible transcript before returning its consistent initial snapshot. */
  openSessionStream(
    projectSlug: string,
    sessionId: string,
    request: TimelinePageRequest,
  ): Promise<SessionStreamSnapshot>;
  /** Release a transcript stream; stale close calls cannot close a newer stream. */
  closeSessionStream(streamId: string): Promise<{ ok: true }>;
  getSessionTodos(projectSlug: string, sessionId: string): Promise<unknown[]>;
  getSessionPlan(projectSlug: string, sessionId: string): Promise<unknown | null>;
  getSessionTask(projectSlug: string, sessionId: string): Promise<unknown | null>;
  getToolResult(projectSlug: string, sessionId: string, toolUseId: string): Promise<string | null>;

  // Subagents ---------------------------------------------------------------
  getSessionSubagents(projectSlug: string, sessionId: string, options?: SubagentFilter): Promise<SubagentListItem[]>;
  getSubagentMessages(
    projectSlug: string,
    sessionId: string,
    agentId: string,
    limit?: number,
    offset?: number,
    workflowId?: string,
    options?: SourceFilter,
  ): Promise<SubagentMessagePage>;
  getSubagentTimeline(
    projectSlug: string,
    sessionId: string,
    agentId: string,
    request: SubagentTimelinePageRequest,
  ): Promise<SubagentTimelinePage>;

  // Search / stats ----------------------------------------------------------
  search(query: SearchQuery): Promise<SearchResultSet>;
  getStats(): Promise<StoreStats>;
}

/**
 * Event listener registration API. Each `on*` returns an unsubscribe fn that
 * removes the listener when called.
 */
export interface SpaghettiEvents {
  onProgress(cb: (progress: InitProgress) => void): () => void;
  onReady(cb: (info: ReadyInfo) => void): () => void;
  onChange(cb: (batch: SegmentChangeBatch) => void): () => void;
  onActiveSessionChange(cb: (change: ActiveSessionChange) => void): () => void;
  /** Fired when main-process SDK initialize() rejects. */
  onInitError(cb: (message: string) => void): () => void;
}

export type SpaghettiBridge = SpaghettiIPC & SpaghettiEvents;

// Channel names — single source of truth, shared between preload and main.
export const IPC_CHANNELS = {
  isReady: 'spaghetti:isReady',
  rebuildIndex: 'spaghetti:rebuildIndex',
  retryInit: 'spaghetti:retryInit',
  getEngine: 'spaghetti:getEngine',
  getProjectList: 'spaghetti:getProjectList',
  getProjectTokenActivity: 'spaghetti:getProjectTokenActivity',
  getProjectMemory: 'spaghetti:getProjectMemory',
  getSessionList: 'spaghetti:getSessionList',
  getSessionMessages: 'spaghetti:getSessionMessages',
  getSessionTimelineFacets: 'spaghetti:getSessionTimelineFacets',
  getSessionTimeline: 'spaghetti:getSessionTimeline',
  openSessionStream: 'spaghetti:openSessionStream',
  closeSessionStream: 'spaghetti:closeSessionStream',
  getSessionTodos: 'spaghetti:getSessionTodos',
  getSessionPlan: 'spaghetti:getSessionPlan',
  getSessionTask: 'spaghetti:getSessionTask',
  getToolResult: 'spaghetti:getToolResult',
  getSessionSubagents: 'spaghetti:getSessionSubagents',
  getSubagentMessages: 'spaghetti:getSubagentMessages',
  getSubagentTimeline: 'spaghetti:getSubagentTimeline',
  search: 'spaghetti:search',
  getStats: 'spaghetti:getStats',
} as const;

export const EVENT_CHANNELS = {
  progress: 'spaghetti:event:progress',
  ready: 'spaghetti:event:ready',
  change: 'spaghetti:event:change',
  activeSessionChange: 'spaghetti:event:active-session-change',
  initError: 'spaghetti:event:init-error',
} as const;
