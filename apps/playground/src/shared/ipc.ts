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
  SessionListItem,
  SourceFilter,
  SubagentListItem,
  SubagentMessagePage,
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
  getProjectMemory(projectSlug: string, options?: SourceFilter): Promise<string | null>;

  // Sessions ----------------------------------------------------------------
  /**
   * List sessions for a project. Pass `{ sourceId }` when the project came
   * from a multi-source index so agents sharing the same slug stay distinct.
   */
  getSessionList(projectSlug: string, options?: SourceFilter): Promise<SessionListItem[]>;
  getSessionMessages(
    projectSlug: string,
    sessionId: string,
    limit?: number,
    offset?: number,
    options?: SourceFilter,
  ): Promise<MessagePage>;
  getSessionTimelineFacets(projectSlug: string, sessionId: string, options?: SourceFilter): Promise<TimelineFacets>;
  getSessionTimeline(projectSlug: string, sessionId: string, request?: TimelinePageRequest): Promise<TimelinePage>;
  getSessionTodos(projectSlug: string, sessionId: string): Promise<unknown[]>;
  getSessionPlan(projectSlug: string, sessionId: string): Promise<unknown | null>;
  getSessionTask(projectSlug: string, sessionId: string): Promise<unknown | null>;
  getToolResult(projectSlug: string, sessionId: string, toolUseId: string): Promise<string | null>;

  // Subagents ---------------------------------------------------------------
  getSessionSubagents(projectSlug: string, sessionId: string): Promise<SubagentListItem[]>;
  getSubagentMessages(
    projectSlug: string,
    sessionId: string,
    agentId: string,
    limit?: number,
    offset?: number,
  ): Promise<SubagentMessagePage>;

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
  getProjectMemory: 'spaghetti:getProjectMemory',
  getSessionList: 'spaghetti:getSessionList',
  getSessionMessages: 'spaghetti:getSessionMessages',
  getSessionTimelineFacets: 'spaghetti:getSessionTimelineFacets',
  getSessionTimeline: 'spaghetti:getSessionTimeline',
  getSessionTodos: 'spaghetti:getSessionTodos',
  getSessionPlan: 'spaghetti:getSessionPlan',
  getSessionTask: 'spaghetti:getSessionTask',
  getToolResult: 'spaghetti:getToolResult',
  getSessionSubagents: 'spaghetti:getSessionSubagents',
  getSubagentMessages: 'spaghetti:getSubagentMessages',
  search: 'spaghetti:search',
  getStats: 'spaghetti:getStats',
} as const;

export const EVENT_CHANNELS = {
  progress: 'spaghetti:event:progress',
  ready: 'spaghetti:event:ready',
  change: 'spaghetti:event:change',
  initError: 'spaghetti:event:init-error',
} as const;
