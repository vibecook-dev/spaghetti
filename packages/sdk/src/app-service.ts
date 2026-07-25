/**
 * AppService — Frontend-ready API wrapping the data service
 *
 * Adapted from ClaudeCodeAppService. Implements SpaghettiAPI.
 */

import { EventEmitter } from 'events';
import type {
  SpaghettiAPI,
  ProjectListItem,
  ProjectMember,
  ProjectReference,
  SessionListItem,
  MessagePage,
  SubagentListItem,
  WorkflowListItem,
  SubagentMessagePage,
  SourceFilter,
  SubagentFilter,
  TokenActivityDay,
  TokenActivityQuery,
  TokenActivityResult,
} from './api.js';
import type {
  SubagentTimelinePage,
  SubagentTimelinePageRequest,
  TimelineFacets,
  TimelinePage,
  TimelinePageRequest,
} from './data/timeline-query.js';
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
    return aggregateProjectSummaries(summaries).sort((a, b) =>
      a.lastActiveAt === b.lastActiveAt ? 0 : a.lastActiveAt > b.lastActiveAt ? -1 : 1,
    );
  }

  getSessionList(project: ProjectReference, options?: SourceFilter): SessionListItem[] {
    const summaries =
      typeof project === 'string'
        ? this.dataService.getSessionSummaries(project, options)
        : project.members
            .filter((member) => !options?.sourceId || member.sourceId === options.sourceId)
            .flatMap((member) => this.dataService.getSessionSummaries(member.slug, { sourceId: member.sourceId }));
    const uniqueSummaries = new Map<string, SessionSummaryData>();
    for (const summary of summaries) {
      uniqueSummaries.set(sessionKey(summary), summary);
    }
    return [...uniqueSummaries.values()]
      .sort((a, b) => (a.lastUpdate === b.lastUpdate ? 0 : a.lastUpdate > b.lastUpdate ? -1 : 1))
      .map(toSessionListItem);
  }

  getProjectTokenActivity(project: ProjectReference, query: TokenActivityQuery): TokenActivityResult {
    const buckets =
      typeof project === 'string'
        ? this.dataService.getProjectTokenActivity(project, query)
        : project.members
            .filter((member) => !query.sourceId || member.sourceId === query.sourceId)
            .flatMap((member) =>
              this.dataService.getProjectTokenActivity(member.slug, {
                sourceId: member.sourceId,
                from: query.from,
                to: query.to,
              }),
            );
    const days = new Map<string, TokenActivityDay>();
    for (const bucket of buckets) {
      const current = days.get(bucket.date);
      if (!current) {
        days.set(bucket.date, {
          date: bucket.date,
          tokenUsage: { ...bucket.tokenUsage },
          quality: tokenActivityQuality(bucket.exactTokens, bucket.estimatedTokens),
          exactTokens: bucket.exactTokens,
          estimatedTokens: bucket.estimatedTokens,
          messageCount: bucket.messageCount,
          sessionCount: bucket.sessionCount,
          sourceIds: [bucket.sourceId],
        });
        continue;
      }
      current.tokenUsage.inputTokens += bucket.tokenUsage.inputTokens;
      current.tokenUsage.outputTokens += bucket.tokenUsage.outputTokens;
      current.tokenUsage.cacheCreationTokens += bucket.tokenUsage.cacheCreationTokens;
      current.tokenUsage.cacheReadTokens += bucket.tokenUsage.cacheReadTokens;
      current.tokenUsage.totalTokens += bucket.tokenUsage.totalTokens;
      current.exactTokens += bucket.exactTokens;
      current.estimatedTokens += bucket.estimatedTokens;
      current.messageCount += bucket.messageCount;
      current.sessionCount += bucket.sessionCount;
      current.sourceIds = [...new Set([...current.sourceIds, bucket.sourceId])].sort();
      current.quality = tokenActivityQuality(current.exactTokens, current.estimatedTokens);
    }
    return { from: query.from, to: query.to, days: [...days.values()].sort((a, b) => a.date.localeCompare(b.date)) };
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

  getProjectMemory(project: ProjectReference, options?: SourceFilter): string | null {
    if (typeof project === 'string') return this.dataService.getProjectMemory(project, options);

    const sourceId = options?.sourceId ?? 'claude-code';
    const member = project.members.find((candidate) => candidate.sourceId === sourceId);
    return member ? this.dataService.getProjectMemory(member.slug, { sourceId: member.sourceId }) : null;
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

  getSessionSubagents(projectSlug: string, sessionId: string, options?: SubagentFilter): SubagentListItem[] {
    return this.dataService.getSessionSubagents(projectSlug, sessionId, options);
  }

  getSessionWorkflows(projectSlug: string, sessionId: string): WorkflowListItem[] {
    return this.dataService.getSessionWorkflows(projectSlug, sessionId);
  }

  getWorkflowSubagents(
    projectSlug: string,
    sessionId: string,
    workflowId: string,
    options?: SourceFilter,
  ): SubagentListItem[] {
    return this.dataService.getWorkflowSubagents(projectSlug, sessionId, workflowId, options);
  }

  getSubagentMessages(
    projectSlug: string,
    sessionId: string,
    agentId: string,
    limit = 30,
    offset = 0,
    workflowId?: string,
    options?: SourceFilter,
  ): SubagentMessagePage {
    const result = this.dataService.getSubagentMessages(
      projectSlug,
      sessionId,
      agentId,
      limit,
      offset,
      workflowId,
      options,
    );
    return {
      messages: result.segments.map((s) => s.data),
      total: result.total,
      offset: result.offset,
      hasMore: result.hasMore,
    };
  }

  getSubagentTimeline(
    projectSlug: string,
    sessionId: string,
    agentId: string,
    request: SubagentTimelinePageRequest,
  ): SubagentTimelinePage {
    return this.dataService.getSubagentTimeline(projectSlug, sessionId, agentId, request);
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

function canonicalProjectPath(data: ProjectSummaryData): string {
  const normalizedPath = data.absolutePath.trim().replace(/\\/g, '/');
  return normalizedPath.length > 1 ? normalizedPath.replace(/\/+$/, '') : normalizedPath;
}

function memberKey(member: ProjectMember): string {
  return JSON.stringify([member.sourceId, member.slug]);
}

function sessionKey(session: Pick<SessionSummaryData, 'sourceId' | 'sessionId'>): string {
  return JSON.stringify([session.sourceId, session.sessionId]);
}

function tokenActivityQuality(exactTokens: number, estimatedTokens: number): TokenActivityDay['quality'] {
  if (exactTokens > 0 && estimatedTokens > 0) return 'mixed';
  if (estimatedTokens > 0) return 'estimated';
  if (exactTokens > 0) return 'exact';
  return 'unavailable';
}

function projectGroupKey(data: ProjectSummaryData): string {
  const normalizedPath = canonicalProjectPath(data);
  return normalizedPath ? `path:${normalizedPath}` : `member:${memberKey(data)}`;
}

function isVisibleProject(data: ProjectSummaryData): boolean {
  return data.sessionCount > 0 || data.messageCount > 0 || data.hasMemory;
}

function aggregateProjectSummaries(summaries: ProjectSummaryData[]): ProjectListItem[] {
  const projects = new Map<string, ProjectListItem>();
  for (const data of summaries) {
    // Claude can leave project directories containing only stale subagent
    // folders. They have no user-visible project content and their path is a
    // lossy slug reconstruction, so exposing them creates phantom collisions.
    if (!isVisibleProject(data)) continue;

    const key = projectGroupKey(data);
    const current = projects.get(key);
    if (!current) {
      projects.set(key, {
        projectId: key,
        members: [{ sourceId: data.sourceId, slug: data.slug }],
        slug: data.slug,
        sourceIds: [data.sourceId],
        folderName: data.folderName,
        absolutePath: data.absolutePath,
        sessionCount: data.sessionCount,
        messageCount: data.messageCount,
        tokenUsage: { ...data.tokenUsage },
        tokensEstimated: data.tokensEstimated,
        lastActiveAt: data.lastActiveAt,
        firstActiveAt: data.firstActiveAt,
        latestGitBranch: data.latestGitBranch,
        latestPrompt: data.latestPrompt,
        hasMemory: data.hasMemory,
      });
      continue;
    }

    const nextMember = { sourceId: data.sourceId, slug: data.slug };
    if (!current.members.some((member) => memberKey(member) === memberKey(nextMember))) {
      current.members = [...current.members, nextMember].sort((a, b) => memberKey(a).localeCompare(memberKey(b)));
    }
    current.sourceIds = [...new Set(current.members.map((member) => member.sourceId))].sort();
    current.sessionCount += data.sessionCount;
    current.messageCount += data.messageCount;
    current.tokenUsage.inputTokens += data.tokenUsage.inputTokens;
    current.tokenUsage.outputTokens += data.tokenUsage.outputTokens;
    current.tokenUsage.cacheCreationTokens += data.tokenUsage.cacheCreationTokens;
    current.tokenUsage.cacheReadTokens += data.tokenUsage.cacheReadTokens;
    current.tokenUsage.totalTokens += data.tokenUsage.totalTokens;
    current.tokensEstimated ||= data.tokensEstimated;
    current.hasMemory ||= data.hasMemory;
    if (data.firstActiveAt < current.firstActiveAt) current.firstActiveAt = data.firstActiveAt;
    if (data.lastActiveAt > current.lastActiveAt) {
      current.lastActiveAt = data.lastActiveAt;
      current.latestGitBranch = data.latestGitBranch;
      current.latestPrompt = data.latestPrompt;
      current.slug = data.slug;
    }
  }
  return [...projects.values()];
}

function toSessionListItem(data: SessionSummaryData): SessionListItem {
  return {
    sessionId: data.sessionId,
    sourceId: data.sourceId,
    projectSlug: data.projectSlug,
    startTime: data.startTime,
    lastUpdate: data.lastUpdate,
    lifespanMs: data.lifespanMs,
    tokenUsage: data.tokenUsage,
    tokensEstimated: data.tokensEstimated,
    messageCount: data.messageCount,
    fullPath: data.fullPath,
    title: data.title,
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
