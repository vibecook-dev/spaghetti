/**
 * AppService — Frontend-ready API wrapping the data service
 *
 * Adapted from ClaudeCodeAppService. Implements SpaghettiAPI.
 */

import { EventEmitter } from 'events';
import type {
  SpaghettiAPI,
  ProjectListItem,
  SessionListItem,
  MessagePage,
  SubagentListItem,
  WorkflowListItem,
  SubagentMessagePage,
  SourceFilter,
} from './api.js';
import type { TimelineFacets, TimelinePage, TimelinePageRequest } from './data/timeline-query.js';
import type { AgentDataService } from './data/agent-data-service.js';
import type { LifecycleInternal } from './data/lifecycle-owner.js';
import type { AgentDataStore } from './data/agent-data-store.js';
import type { LiveWatch } from './live/live-watch.js';
import type {
  SearchQuery,
  SearchResultSet,
  StoreStats,
  InitProgress,
  SegmentChangeBatch,
} from './data/segment-types.js';
import type { SessionSummaryData, ProjectSummaryData } from './data/summary-types.js';
import type { TeamDirectory } from './types/index.js';
import type { SpaghettiLive } from './live/spaghetti-live.js';
import { createSpaghettiLive } from './live/spaghetti-live.js';
import type { SpaghettiRuntime } from './runtime/spaghetti-runtime.js';
import { createSpaghettiRuntime } from './runtime/spaghetti-runtime.js';
import type { RuntimeBridge } from './planes/runtime-bridge.js';
import type { ErrorSink } from './io/error-sink.js';

class SpaghettiAppService extends EventEmitter implements SpaghettiAPI {
  private dataService: AgentDataService;
  private runtimeBridge: RuntimeBridge | undefined;

  /**
   * Public `api.live` handle — present only when the lifecycle owner
   * was constructed with a live-updates orchestrator (i.e. the caller
   * opted into `createSpaghettiService({ live: true })`). Wired up
   * in the constructor so access is a simple field read.
   */
  readonly live?: SpaghettiLive;

  /**
   * Public `api.runtime` handle — Plane 3 (hooks + channels). Present
   * when the factory passed a RuntimeBridge (default create path).
   */
  readonly runtime?: SpaghettiRuntime;

  constructor(dataService: AgentDataService, errorSink?: ErrorSink, runtimeBridge?: RuntimeBridge) {
    super();
    this.dataService = dataService;
    this.runtimeBridge = runtimeBridge;

    this.dataService.on('progress', (data) => this.emit('progress', data));
    this.dataService.on('ready', (data) => this.emit('ready', data));
    this.dataService.on('change', (data) => this.emit('change', data));
    this.dataService.on('error', (data) => this.emit('error', data));

    // C3.4: build api.live if the underlying service implements the
    // internal `LifecycleInternal` shape (only `LifecycleOwner` does
    // today). Reaches the methods via structural typing so the public
    // `AgentDataService` interface stays free of internal
    // type leaks.
    const internal = dataService as Partial<LifecycleInternal>;
    const getStore = internal.getStore?.bind(internal) as (() => AgentDataStore) | undefined;
    const getLiveWatch = internal.getLiveWatch?.bind(internal) as (() => LiveWatch | undefined) | undefined;
    if (getStore && getLiveWatch) {
      const liveWatch = getLiveWatch();
      if (liveWatch) {
        this.live = createSpaghettiLive(getStore(), liveWatch, errorSink);
      }
    }

    if (runtimeBridge) {
      this.runtime = createSpaghettiRuntime(runtimeBridge);
    }
  }

  async initialize(): Promise<void> {
    await this.dataService.initialize();
  }

  shutdown(): void {
    // Delegate to the properly sequenced async teardown (fire-and-forget):
    // the live pipeline must fully stop before SQLite closes, or an
    // in-flight writeBatch races a closed handle. Callers that need to
    // observe completion use dispose().
    void this.dispose().catch(() => {});
  }

  /**
   * C3.4: awaitable teardown. Stops runtime watchers, then delegates to
   * `shutdownAsync` when the underlying data-service exposes it.
   */
  async dispose(): Promise<void> {
    try {
      this.runtimeBridge?.stop();
    } catch {
      /* ignore */
    }
    const async_ = this.dataService.shutdownAsync?.bind(this.dataService);
    if (async_) {
      await async_();
    } else {
      this.dataService.shutdown();
    }
  }

  async rebuildIndex(): Promise<{ durationMs: number }> {
    return await this.dataService.rebuildIndex();
  }

  isReady(): boolean {
    return this.dataService.isReady();
  }

  getSourceIds(): string[] {
    return this.dataService.getSourceIds();
  }

  getProjectList(options?: SourceFilter): ProjectListItem[] {
    const summaries = this.dataService.getProjectSummaries(options);
    summaries.sort((a, b) => (a.lastActiveAt > b.lastActiveAt ? -1 : 1));
    return summaries.map(toProjectListItem);
  }

  getSessionList(projectSlug: string, options?: SourceFilter): SessionListItem[] {
    const summaries = this.dataService.getSessionSummaries(projectSlug, options);
    summaries.sort((a, b) => (a.lastUpdate > b.lastUpdate ? -1 : 1));
    return summaries.map(toSessionListItem);
  }

  getSessionMessages(
    projectSlug: string,
    sessionId: string,
    limit = 30,
    offset = 0,
    options?: SourceFilter,
  ): MessagePage {
    const result = this.dataService.getSessionMessages(projectSlug, sessionId, limit, offset, options);
    return {
      messages: result.segments.map((s) => s.data),
      total: result.total,
      offset: result.offset,
      hasMore: result.hasMore,
    };
  }

  getSessionTimelineFacets(projectSlug: string, sessionId: string, options?: SourceFilter): TimelineFacets {
    return this.dataService.getSessionTimelineFacets(projectSlug, sessionId, options);
  }

  getSessionTimeline(projectSlug: string, sessionId: string, request?: TimelinePageRequest): TimelinePage {
    return this.dataService.getSessionTimeline(projectSlug, sessionId, request);
  }

  getProjectMemory(projectSlug: string, options?: SourceFilter): string | null {
    return this.dataService.getProjectMemory(projectSlug, options);
  }

  getSessionTodos(projectSlug: string, sessionId: string): unknown[] {
    return this.dataService.getSessionTodos(projectSlug, sessionId);
  }

  getSessionPlan(projectSlug: string, sessionId: string): unknown | null {
    return this.dataService.getSessionPlan(projectSlug, sessionId);
  }

  getSessionTask(projectSlug: string, sessionId: string): unknown | null {
    return this.dataService.getSessionTask(projectSlug, sessionId);
  }

  getToolResult(projectSlug: string, sessionId: string, toolUseId: string): string | null {
    return this.dataService.getPersistedToolResult(projectSlug, sessionId, toolUseId);
  }

  getSessionSubagents(projectSlug: string, sessionId: string): SubagentListItem[] {
    return this.dataService.getSessionSubagents(projectSlug, sessionId);
  }

  getSessionWorkflows(projectSlug: string, sessionId: string): WorkflowListItem[] {
    return this.dataService.getSessionWorkflows(projectSlug, sessionId);
  }

  getWorkflowSubagents(projectSlug: string, sessionId: string, workflowId: string): SubagentListItem[] {
    return this.dataService.getWorkflowSubagents(projectSlug, sessionId, workflowId);
  }

  getSubagentMessages(
    projectSlug: string,
    sessionId: string,
    agentId: string,
    limit = 30,
    offset = 0,
    workflowId?: string,
  ): SubagentMessagePage {
    const result = this.dataService.getSubagentMessages(projectSlug, sessionId, agentId, limit, offset, workflowId);
    return {
      messages: result.segments.map((s) => s.data),
      total: result.total,
      offset: result.offset,
      hasMore: result.hasMore,
    };
  }

  search(query: SearchQuery): SearchResultSet {
    return this.dataService.search(query);
  }

  getStats(): StoreStats {
    return this.dataService.getStoreStats();
  }

  getTeams(): TeamDirectory[] {
    return this.dataService.getConfig().teams;
  }

  onProgress(cb: (progress: InitProgress) => void): () => void {
    this.on('progress', cb);
    return () => this.removeListener('progress', cb);
  }

  onReady(cb: (info: { durationMs: number }) => void): () => void {
    this.on('ready', cb);
    return () => this.removeListener('ready', cb);
  }

  onChange(cb: (batch: SegmentChangeBatch) => void): () => void {
    this.on('change', cb);
    return () => this.removeListener('change', cb);
  }
}

function toProjectListItem(data: ProjectSummaryData): ProjectListItem {
  return {
    slug: data.slug,
    sourceId: data.sourceId,
    folderName: data.folderName,
    absolutePath: data.absolutePath,
    sessionCount: data.sessionCount,
    messageCount: data.messageCount,
    tokenUsage: data.tokenUsage,
    tokensEstimated: data.tokensEstimated,
    lastActiveAt: data.lastActiveAt,
    firstActiveAt: data.firstActiveAt,
    latestGitBranch: data.latestGitBranch,
    hasMemory: data.hasMemory,
  };
}

function toSessionListItem(data: SessionSummaryData): SessionListItem {
  return {
    sessionId: data.sessionId,
    sourceId: data.sourceId,
    startTime: data.startTime,
    lastUpdate: data.lastUpdate,
    lifespanMs: data.lifespanMs,
    tokenUsage: data.tokenUsage,
    tokensEstimated: data.tokensEstimated,
    messageCount: data.messageCount,
    fullPath: data.fullPath,
    summary: data.summary,
    firstPrompt: data.firstPrompt,
    gitBranch: data.gitBranch,
    todoCount: data.todoCount,
    planSlug: data.planSlug,
    hasTask: data.hasTask,
    isSidechain: data.isSidechain,
  };
}

export function createSpaghettiAppService(
  dataService: AgentDataService,
  errorSink?: ErrorSink,
  runtimeBridge?: RuntimeBridge,
): SpaghettiAPI {
  return new SpaghettiAppService(dataService, errorSink, runtimeBridge);
}
