/**
 * IPC contract between main and renderer.
 *
 * Every method maps 1:1 to a channel of the form `spaghetti:<method>` that the
 * main process exposes via ipcMain.handle, and the preload forwards through
 * contextBridge.exposeInMainWorld('spaghetti', …).
 *
 * The subset of the SDK's asynchronous observation service exposed here is
 * everything a read-only agent-data browser needs — list/read/search, plus
 * the initialization lifecycle. Mutations aren't surfaced (the playground is
 * read-only).
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
import type { SpaghettiClientResponseMap } from '@vibecook/spaghetti-sdk/client';
import type {
  ObservationHostSnapshot,
  SpaghettiCatalogPageOptions,
  SpaghettiCatalogProjectPage,
  SpaghettiCatalogSessionPage,
  SpaghettiCatalogSessionPageOptions,
  SpaghettiReadiness,
} from '@vibecook/spaghetti-sdk/observation';
import type {
  InitProgress,
  SearchQuery,
  SearchResultSet,
  SegmentChangeBatch,
  StoreStats,
} from '@vibecook/spaghetti-sdk';

export interface ObservationOwnerStatus {
  enabled: boolean;
  state: 'disabled' | 'starting' | 'running' | 'failed' | 'stopped';
  error?: string;
  progress?: InitProgress;
}

export interface ObservationHostReport {
  enabled: boolean;
  state: ObservationOwnerStatus['state'] | 'degraded';
  error?: string;
  databasePath?: string;
  snapshot?: ObservationHostSnapshot;
}

/**
 * One entry from `git worktree list --porcelain`.
 *
 * Defined here rather than beside the enumerator in `src/main/worktrees.ts`
 * because it crosses the bridge: the renderer's tsconfig includes only
 * `src/renderer` and `src/shared`.
 */
export interface WorktreeInfo {
  /** Absolute path to the worktree's root directory, exactly as git reported it. */
  path: string;
  /**
   * `path` with symlinks resolved, or `null` when it could not be resolved
   * (a prunable worktree's directory is typically gone).
   *
   * Carried so consumers can match this worktree against a path recorded by
   * something else — an agent's `worktreePath`, say — without knowing which
   * spelling either side used. macOS `/var` vs `/private/var` is the usual
   * culprit, and comparing only the raw strings silently finds nothing.
   */
  realPath: string | null;
  /** Commit the worktree is checked out at. `null` for a bare repository. */
  head: string | null;
  /**
   * Short branch name (`feature/x`). `null` when detached or bare — read
   * `detached` to tell those two apart.
   */
  branch: string | null;
  /** Full ref as git reported it (`refs/heads/feature/x`), for exact matching. */
  branchRef: string | null;
  /**
   * The repository's primary worktree. Git always lists it first, which is the
   * only signal available — the porcelain format has no explicit marker.
   */
  isMain: boolean;
  detached: boolean;
  bare: boolean;
  locked: boolean;
  /** Text after `locked`, when git supplied a reason. Empty reasons stay `null`. */
  lockReason: string | null;
  prunable: boolean;
  prunableReason: string | null;
}

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
  /** The sole RFC 011 production engine. */
  getEngine(): Promise<'rs'>;
  /** Detailed production host health and native engine snapshot. */
  getObservationHostStatus(): Promise<ObservationHostReport>;
  /** Lightweight owner availability; unlike the full report, runs no queries. */
  getObservationOwnerStatus(): Promise<ObservationOwnerStatus>;
  /** Canonical catalog statistics read through the framed utility client. */
  getCanonicalStats(): Promise<SpaghettiClientResponseMap['getStats']>;
  /**
   * The readiness vector. The library renders from the catalog, which is
   * ready long before history, usage, and search converge, so the UI needs
   * this to say what is still arriving.
   */
  getReadiness(): Promise<SpaghettiReadiness>;

  // Projects ----------------------------------------------------------------
  /**
   * Discovered projects, answerable during background ingestion. The library
   * renders from these and swaps in `getProjectList` rows as they decode.
   */
  listCatalogProjects(options?: SpaghettiCatalogPageOptions): Promise<SpaghettiCatalogProjectPage>;
  /** Decoded projects. Waits for every supervisor to finish its scan. */
  getProjectList(): Promise<ProjectListItem[]>;
  getProjectTokenActivity(project: ProjectReference, query: TokenActivityQuery): Promise<TokenActivityResult>;
  getProjectMemory(project: ProjectReference, options?: SourceFilter): Promise<string | null>;
  /**
   * Linked worktrees of the repository containing `projectPath`.
   *
   * Unlike every other method here this is answered by the main process
   * directly rather than forwarded to the SDK — it is a live `git` query about
   * the workspace, not data derived from Claude Code's session files. Resolves
   * to `[]` for an unversioned project rather than rejecting.
   */
  getProjectWorktrees(projectPath: string): Promise<WorktreeInfo[]>;

  // Sessions ----------------------------------------------------------------
  /** List all project sessions; optionally filter to one agent source. */
  /** Discovered sessions of one catalog project, answerable during ingestion. */
  listCatalogSessions(options?: SpaghettiCatalogSessionPageOptions): Promise<SpaghettiCatalogSessionPage>;
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
  getObservationHostStatus: 'spaghetti:getObservationHostStatus',
  getObservationOwnerStatus: 'spaghetti:getObservationOwnerStatus',
  getCanonicalStats: 'spaghetti:getCanonicalStats',
  getReadiness: 'spaghetti:getReadiness',
  listCatalogProjects: 'spaghetti:listCatalogProjects',
  getProjectList: 'spaghetti:getProjectList',
  getProjectTokenActivity: 'spaghetti:getProjectTokenActivity',
  getProjectMemory: 'spaghetti:getProjectMemory',
  getProjectWorktrees: 'spaghetti:getProjectWorktrees',
  listCatalogSessions: 'spaghetti:listCatalogSessions',
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
