/**
 * Product compatibility facade backed exclusively by the RFC 011 Rust owner.
 *
 * This module intentionally contains presentation/compatibility mapping only:
 * it does not read agent files, open SQLite, repair projections, or own a
 * process-local source watcher. Every durable read crosses `SpaghettiClient`.
 */

import { EventEmitter } from 'node:events';
import { basename } from 'node:path';

import type {
  MessagePage,
  ProjectListItem,
  ProjectMember,
  ProjectReference,
  SessionListItem,
  SourceFilter,
  SubagentFilter,
  SubagentListItem,
  SubagentMessagePage,
  TokenActivityDay,
  TokenActivityQuery,
  TokenActivityResult,
  WorkflowListItem,
  SpaghettiAPI,
} from './api.js';
import type {
  InitProgress,
  SearchQuery,
  SearchResultSet,
  SegmentChangeBatch,
  StoreStats,
} from './data/segment-types.js';
import type {
  SubagentTimelinePage,
  SubagentTimelinePageRequest,
  TimelineFacets,
  TimelinePage,
  TimelinePageRequest,
} from './data/timeline-query.js';
import type { TokenUsageSummary } from './data/summary-types.js';
import type { SessionMessage } from './react/chat/types.js';
import type { TeamDirectory } from './types/index.js';
import type {
  SpaghettiEngineDelegation,
  SpaghettiEngineHistoryProject,
  SpaghettiEngineHistorySession,
  SpaghettiEngineMessageDetail,
  SpaghettiEngineTask,
  SpaghettiEngineTimelineFacets,
  SpaghettiEngineTimelineMessage,
  SpaghettiEngineTimelinePage,
  SpaghettiEngineTimelinePageOptions,
  SpaghettiEngineUsageAggregate,
  SpaghettiEngineWorkflowMember,
} from './native.js';
import {
  openObservationHost,
  type ObservationHost,
  type ObservationHostOptions,
  type ObservationHostProgress,
  type ObservationHostSnapshot,
} from './observation-host.js';
import type { SpaghettiIpcChannel, SpaghettiIpcHost } from './client/index.js';

const PAGE_LIMIT = 200;
const MAX_CHANGE_ORDINAL = 0xffff_ffff;
const COMPATIBILITY_QUERY_CONCURRENCY = 8;
const CURSOR_SNAPSHOT_RESTARTS = 4;

export interface ObservationServiceOptions extends ObservationHostOptions {
  /** Subscribe to committed projection changes. Defaults to true. */
  live?: boolean;
}

/**
 * Async product surface used by production consumers after the Phase 10 cutover.
 *
 * Result DTOs deliberately remain compatible with the former `SpaghettiAPI`
 * so renderers and command formatting do not acquire storage semantics.
 */
export interface ObservationService extends SpaghettiAPI {
  snapshot(signal?: AbortSignal): Promise<ObservationHostSnapshot>;
  serveIpc(channel: SpaghettiIpcChannel, transportKind?: string): SpaghettiIpcHost;
}

interface ResolvedSession {
  project: SpaghettiEngineHistoryProject;
  session: SpaghettiEngineHistorySession;
}

interface TimelineRow {
  index: number;
  adapterId: string;
  nativeChildId?: string;
  runId: string;
  message: SessionMessage;
}

interface ProjectSummary {
  canonical: SpaghettiEngineHistoryProject;
  item: ProjectListItem;
}

/** Construct an uninitialized Rust-owned product service. */
export function createObservationService(options: ObservationServiceOptions): ObservationService {
  return new RustObservationService(options);
}

class RustObservationService extends EventEmitter implements ObservationService {
  private host: ObservationHost | null = null;
  private initializePromise: Promise<void> | null = null;
  private disposePromise: Promise<void> | null = null;
  private subscriptionAbort: AbortController | null = null;
  private startupAbort: AbortController | null = null;
  private lastProgress: InitProgress | null = null;
  private ready = false;
  // Canonical entity IDs are immutable for the lifetime of an engine. Keep
  // the identities already returned by project/session listings so a detail
  // query does not rescan the entire project catalog before every Rust call.
  private projectCatalogLoaded = false;
  private readonly projectsById = new Map<string, SpaghettiEngineHistoryProject>();
  private readonly sessionsByLocator = new Map<string, ResolvedSession>();
  private projectLoad: Promise<SpaghettiEngineHistoryProject[]> | null = null;
  private readonly sessionLoads = new Map<string, Promise<SpaghettiEngineHistorySession[]>>();
  private readonly sessionResolutions = new Map<string, Promise<ResolvedSession>>();

  constructor(private readonly options: ObservationServiceOptions) {
    super();
  }

  initialize(): Promise<void> {
    if (this.ready) return Promise.resolve();
    if (this.disposePromise) return Promise.reject(new Error('Observation service is stopping.'));
    if (!this.initializePromise) {
      const startedAt = Date.now();
      const work = (async () => {
        const startupAbort = new AbortController();
        this.startupAbort = startupAbort;
        const signal = this.options.signal
          ? AbortSignal.any([this.options.signal, startupAbort.signal])
          : startupAbort.signal;
        let host: ObservationHost | null = null;
        try {
          host = await openObservationHost({
            ...this.options,
            signal,
            onProgress: (progress) => {
              this.emitHostProgress(progress);
              this.options.onProgress?.(progress);
            },
          });
          if (this.disposePromise) throw new Error('Observation service stopped during initialization.');
          this.emitProgress('reconciling', 'Reading canonical source catalog…');
          await host.client.listSources({ limit: 1 });
          if (this.options.live !== false) await this.startSubscription(host);
          if (this.disposePromise) throw new Error('Observation service stopped during initialization.');
          this.host = host;
          this.ready = true;
          const info = { durationMs: Date.now() - startedAt };
          this.emitProgress('indexing', 'Rust observation service is ready.', {
            current: this.options.sources.length,
            total: this.options.sources.length,
            sourceCount: this.options.sources.length,
            elapsedMs: info.durationMs,
          });
          this.emit('ready', info);
        } catch (error) {
          this.subscriptionAbort?.abort();
          this.subscriptionAbort = null;
          this.host = null;
          this.ready = false;
          if (host) await host.dispose().catch(() => undefined);
          throw error;
        } finally {
          if (this.startupAbort === startupAbort) this.startupAbort = null;
        }
      })();
      const tracked = work.finally(() => {
        if (this.initializePromise === tracked) this.initializePromise = null;
      });
      this.initializePromise = tracked;
    }
    return this.initializePromise;
  }

  shutdown(): void {
    void this.dispose().catch(() => undefined);
  }

  dispose(): Promise<void> {
    if (!this.disposePromise) {
      this.disposePromise = (async () => {
        this.startupAbort?.abort();
        this.subscriptionAbort?.abort();
        this.subscriptionAbort = null;
        const initializing = this.initializePromise;
        if (initializing) await initializing.catch(() => undefined);
        const host = this.host;
        this.host = null;
        this.ready = false;
        if (host) await host.dispose();
        this.removeAllListeners();
      })();
    }
    return this.disposePromise;
  }

  async rebuildIndex(): Promise<{ durationMs: number }> {
    const startedAt = Date.now();
    this.emitProgress('reconciling', 'Reconciling all native adapters…');
    await this.requireHost().refresh();
    return { durationMs: Date.now() - startedAt };
  }

  isReady(): boolean {
    return this.ready;
  }

  async getSourceIds(): Promise<string[]> {
    const sources = await collectPages((cursor) =>
      this.requireHost().client.listSources({ cursor, limit: PAGE_LIMIT }),
    );
    return [...new Set(sources.map((item) => item.adapterId))].sort();
  }

  async getProjectList(options?: SourceFilter): Promise<ProjectListItem[]> {
    const summaries = await this.projectSummaries(options);
    const groups = new Map<string, ProjectListItem>();
    for (const { item } of summaries) {
      const key = item.projectId;
      const current = groups.get(key);
      if (!current) {
        groups.set(key, structuredClone(item));
        continue;
      }
      mergeProject(current, item);
    }
    return [...groups.values()].sort(compareProjectActivity);
  }

  async getProjectTokenActivity(project: ProjectReference, query: TokenActivityQuery): Promise<TokenActivityResult> {
    const projects = await this.resolveProjects(project, query);
    const reports = await Promise.all(
      projects.map((item) =>
        this.requireHost().client.getUsage({
          projectId: item.projectId,
          from: query.from,
          to: query.to,
        }),
      ),
    );
    const days = new Map<string, TokenActivityDay>();
    for (const report of reports) {
      for (const day of report.window?.days ?? []) {
        const values = usageValues(day.aggregate);
        const exactTokens = day.aggregate.exact.componentTotalTokens;
        const estimatedTokens = day.aggregate.estimated.componentTotalTokens;
        const unknownResponses = day.aggregate.unknownContributionCount;
        const sourceId = projects.find((item) => item.projectId === report.projectId)?.adapterId;
        const current = days.get(day.date);
        if (!current) {
          days.set(day.date, {
            date: day.date,
            tokenUsage: values,
            quality: activityQuality(exactTokens, estimatedTokens, unknownResponses),
            exactTokens,
            estimatedTokens,
            messageCount: day.aggregate.contributionCount,
            sessionCount: day.aggregate.sessionCount,
            sourceIds: sourceId ? [sourceId] : [],
          });
        } else {
          addTokenUsage(current.tokenUsage, values);
          current.exactTokens += exactTokens;
          current.estimatedTokens += estimatedTokens;
          current.messageCount += day.aggregate.contributionCount;
          current.sessionCount += day.aggregate.sessionCount;
          if (sourceId) current.sourceIds = [...new Set([...current.sourceIds, sourceId])].sort();
          current.quality = activityQuality(current.exactTokens, current.estimatedTokens, unknownResponses);
        }
      }
    }
    return { from: query.from, to: query.to, days: [...days.values()].sort((a, b) => a.date.localeCompare(b.date)) };
  }

  async getSessionList(project: ProjectReference, options?: SourceFilter): Promise<SessionListItem[]> {
    const projects = await this.resolveProjects(project, options);
    const groups = await mapConcurrent(projects, COMPATIBILITY_QUERY_CONCURRENCY, async (canonicalProject) => {
      const sessions = await this.allSessions(canonicalProject.projectId);
      return await mapConcurrent(sessions, COMPATIBILITY_QUERY_CONCURRENCY, (session) =>
        this.sessionListItem(canonicalProject, session),
      );
    });
    return groups.flat().sort((a, b) => b.lastUpdate.localeCompare(a.lastUpdate));
  }

  async getSessionMessages(
    projectSlug: string,
    sessionId: string,
    limit = 30,
    offset = 0,
    options?: SourceFilter,
  ): Promise<MessagePage> {
    const resolved = await this.resolveSession(projectSlug, sessionId, options?.sourceId);
    const boundedLimit = boundInteger(limit, 1, 100_000, 30);
    const boundedOffset = boundInteger(offset, 0, Number.MAX_SAFE_INTEGER, 0);
    const details = await this.allMessages(resolved, boundedOffset + boundedLimit + 1);
    const messages = details.slice(boundedOffset, boundedOffset + boundedLimit).map(compatibilityMessage);
    const total = await this.messageCount(resolved);
    return { messages, total, offset: boundedOffset, hasMore: boundedOffset + messages.length < total };
  }

  async getSessionTimelineFacets(
    projectSlug: string,
    sessionId: string,
    options?: SourceFilter,
  ): Promise<TimelineFacets> {
    const resolved = await this.resolveSession(projectSlug, sessionId, options?.sourceId);
    const page = await this.requireHost().client.getTimeline({
      projectId: resolved.project.projectId,
      sessionId: resolved.session.sessionId,
      limit: 1,
    });
    return compatibilityTimelineFacets(page.facets);
  }

  async getSessionTimeline(
    projectSlug: string,
    sessionId: string,
    request: TimelinePageRequest = {},
  ): Promise<TimelinePage> {
    const resolved = await this.resolveSession(projectSlug, sessionId, request.sourceId);
    // A numeric cursor can only have come from the retired synchronous
    // compatibility implementation. Keep that bounded fallback for callers
    // walking an already-started legacy page, but all new pages use the Rust
    // timeline query and its opaque keyset cursor.
    if (typeof request.before === 'number') {
      return timelinePage(await this.timelineRows(resolved), request);
    }
    try {
      const page = await this.requireHost().client.getTimeline(nativeTimelineRequest(resolved, request));
      return compatibilityTimelinePage(page, request);
    } catch (error) {
      if (typeof request.before !== 'string' || !isExpiredCursor(error)) throw error;
      // Phase 9 cursors deliberately bind one committed snapshot. Active
      // transcripts can advance between page requests, so first-party
      // compatibility consumers restart at page one instead of presenting an
      // expected snapshot race as a fatal transcript error.
      const restarted = await this.requireHost().client.getTimeline(
        nativeTimelineRequest(resolved, { ...request, before: undefined }),
      );
      return compatibilityTimelinePage(restarted, request, true);
    }
  }

  async getProjectMemory(project: ProjectReference, options?: SourceFilter): Promise<string | null> {
    const projects = await this.resolveProjects(project, options);
    for (const item of projects) {
      const documents = await collectPages((cursor) =>
        this.requireHost().client.listMemoryDocuments({ projectId: item.projectId, cursor, limit: PAGE_LIMIT }),
      );
      const index = documents.find((document) => document.isIndex);
      if (index) return index.content;
    }
    return null;
  }

  async getSessionTodos(projectSlug: string, sessionId: string): Promise<unknown[]> {
    const resolved = await this.resolveSession(projectSlug, sessionId);
    const tasks = await this.sessionTasks(resolved.session.sessionId);
    return tasks.map((task) => ({
      content: task.subject,
      status: task.taskStatus,
      ...(task.activeForm ? { activeForm: task.activeForm } : {}),
    }));
  }

  async getSessionPlan(projectSlug: string, sessionId: string): Promise<unknown | null> {
    await this.resolveSession(projectSlug, sessionId);
    // RFC 011 intentionally exposes plans as globally scoped documents until
    // an adapter supplies a durable session-to-plan relation. The retired
    // implementation tried to reconstruct that missing relation by scanning
    // every lossless message payload for a Claude-only `slug` field. Besides
    // being incomplete, that made opening the Plans accordion walk an entire
    // large transcript and race the live snapshot watermark. Do not invent a
    // relation: validate the requested session, then report no session plan.
    return null;
  }

  async getSessionTask(projectSlug: string, sessionId: string): Promise<unknown | null> {
    const resolved = await this.resolveSession(projectSlug, sessionId);
    const collections = await collectPages((cursor) =>
      this.requireHost().client.listTaskCollections({
        sessionId: resolved.session.sessionId,
        cursor,
        limit: PAGE_LIMIT,
      }),
    );
    if (collections.length === 0) return null;
    const collection = collections[0]!;
    const items = await collectPages((cursor) =>
      this.requireHost().client.listTasks({ collectionId: collection.collectionId, cursor, limit: PAGE_LIMIT }),
    );
    return {
      taskId: collection.nativeCollectionId,
      hasHighwatermark: false,
      highwatermark: null,
      lockExists: false,
      items: items.map(taskItem),
    };
  }

  async getToolResult(projectSlug: string, sessionId: string, toolUseId: string): Promise<string | null> {
    const resolved = await this.resolveSession(projectSlug, sessionId);
    const results = await collectPages((cursor) =>
      this.requireHost().client.listToolResults({
        projectId: resolved.project.projectId,
        sessionId: resolved.session.sessionId,
        cursor,
        limit: PAGE_LIMIT,
      }),
    );
    return results.find((item) => item.nativeToolUseId === toolUseId)?.content ?? null;
  }

  async getSessionSubagents(
    projectSlug: string,
    sessionId: string,
    options?: SubagentFilter,
  ): Promise<SubagentListItem[]> {
    const resolved = await this.resolveSession(projectSlug, sessionId, options?.sourceId);
    const delegations = await collectPages((cursor) =>
      this.requireHost().client.listDelegations({
        projectId: resolved.project.projectId,
        sessionId: resolved.session.sessionId,
        ...(!options?.includeNested ? { standaloneOnly: true } : {}),
        cursor,
        limit: PAGE_LIMIT,
      }),
    );
    return delegations.map((item) => delegationItem(item, ''));
  }

  async getSessionWorkflows(projectSlug: string, sessionId: string): Promise<WorkflowListItem[]> {
    const resolved = await this.resolveSession(projectSlug, sessionId);
    const workflows = await collectPages((cursor) =>
      this.requireHost().client.listWorkflows({
        projectId: resolved.project.projectId,
        sessionId: resolved.session.sessionId,
        cursor,
        limit: PAGE_LIMIT,
      }),
    );
    return workflows.map((workflow) => ({
      workflowId: workflow.workflowId,
      name: workflow.name ?? workflow.nativeWorkflowId,
      status: workflow.nativeStatus ?? workflow.workflowStatus ?? workflow.resolutionStatus,
      agentCount: workflow.agentCount ?? workflow.observedMemberCount,
      totalTokens: workflow.totalTokens ?? 0,
      totalToolCalls: workflow.totalToolCalls ?? 0,
      durationMs: workflow.durationMs ?? 0,
      subagentCount: workflow.observedMemberCount,
    }));
  }

  async getWorkflowSubagents(
    projectSlug: string,
    sessionId: string,
    workflowId: string,
    options?: SourceFilter,
  ): Promise<SubagentListItem[]> {
    const resolved = await this.resolveSession(projectSlug, sessionId, options?.sourceId);
    const workflow = await this.requireHost().client.getWorkflow({ workflowId });
    if (
      workflow.workflow.projectId !== resolved.project.projectId ||
      workflow.workflow.sessionId !== resolved.session.sessionId
    ) {
      return [];
    }
    const members = await collectPages((cursor) =>
      this.requireHost().client.listWorkflowMembers({ workflowId, cursor, limit: PAGE_LIMIT }),
    );
    return members.map((member) => workflowMemberItem(member, workflowId));
  }

  async getSubagentMessages(
    projectSlug: string,
    sessionId: string,
    agentId: string,
    limit = 30,
    offset = 0,
    workflowId?: string,
    options?: SourceFilter,
  ): Promise<SubagentMessagePage> {
    const resolved = await this.resolveSession(projectSlug, sessionId, options?.sourceId);
    const [details, envelopes] = await Promise.all([this.allMessages(resolved), this.timelineEnvelopes(resolved)]);
    let runIds: Set<string> | undefined;
    if (workflowId) {
      const members = await collectPages((cursor) =>
        this.requireHost().client.listWorkflowMembers({ workflowId, cursor, limit: PAGE_LIMIT }),
      );
      runIds = new Set(members.filter((member) => member.nativeAgentId === agentId).map((member) => member.childRunId));
    }
    const eligibleIds = new Set(
      envelopes
        .filter((envelope) => envelope.nativeChildId === agentId && (!runIds || runIds.has(envelope.runId)))
        .map((envelope) => envelope.messageId),
    );
    const matching = details.filter((detail) => eligibleIds.has(detail.messageId));
    const boundedOffset = boundInteger(offset, 0, Number.MAX_SAFE_INTEGER, 0);
    const boundedLimit = boundInteger(limit, 1, 100_000, 30);
    const messages = matching.slice(boundedOffset, boundedOffset + boundedLimit).map(compatibilityMessage);
    return {
      messages,
      total: matching.length,
      offset: boundedOffset,
      hasMore: boundedOffset + messages.length < matching.length,
    };
  }

  async getSubagentTimeline(
    projectSlug: string,
    sessionId: string,
    agentId: string,
    request: SubagentTimelinePageRequest,
  ): Promise<SubagentTimelinePage> {
    const resolved = await this.resolveSession(projectSlug, sessionId, request.sourceId);
    let rows = (await this.timelineRows(resolved, { branchKind: 'delegated' })).filter(
      (row) => row.nativeChildId === agentId,
    );
    if (request.workflowId) {
      const members = await collectPages((cursor) =>
        this.requireHost().client.listWorkflowMembers({ workflowId: request.workflowId!, cursor, limit: PAGE_LIMIT }),
      );
      const runIds = new Set(
        members.filter((member) => member.nativeAgentId === agentId).map((member) => member.childRunId),
      );
      rows = rows.filter((row) => runIds.has(row.runId));
    }
    rows = filterTimeline(rows, request);
    const offset = boundInteger(request.offset ?? 0, 0, Number.MAX_SAFE_INTEGER, 0);
    const limit = boundInteger(request.limit ?? 30, 1, 500, 30);
    const messages = rows.slice(offset, offset + limit).map((row) => row.message);
    return { messages, total: rows.length, offset, hasMore: offset + messages.length < rows.length };
  }

  async search(query: SearchQuery): Promise<SearchResultSet> {
    const limit = boundInteger(query.limit ?? 50, 1, 200, 50);
    const offset = boundInteger(query.offset ?? 0, 0, Number.MAX_SAFE_INTEGER, 0);
    let projectIds: string[] | undefined;
    if (query.projectMembers?.length) {
      const projects = await this.resolveProjects(
        { projectId: 'compat-search', members: query.projectMembers },
        query.sourceId ? { sourceId: query.sourceId } : undefined,
      );
      projectIds = projects.map((project) => project.projectId);
    } else if (query.projectSlug) {
      projectIds = (
        await this.resolveProjects(query.projectSlug, query.sourceId ? { sourceId: query.sourceId } : undefined)
      ).map((project) => project.projectId);
    }
    if (projectIds && projectIds.length === 0) return { results: [], total: 0, hasMore: false };

    let scopes: Array<{ projectId?: string; sessionId?: string }>;
    if (query.sessionId) {
      const candidates = projectIds
        ? (await this.allProjects()).filter((project) => projectIds!.includes(project.projectId))
        : (await this.allProjects()).filter((project) => !query.sourceId || project.adapterId === query.sourceId);
      scopes = [];
      for (const project of candidates) {
        const session = (await this.allSessions(project.projectId)).find(
          (item) => item.nativeSessionId === query.sessionId,
        );
        if (session) scopes.push({ projectId: project.projectId, sessionId: session.sessionId });
      }
      if (scopes.length === 0) return { results: [], total: 0, hasMore: false };
    } else {
      scopes = projectIds?.map((projectId) => ({ projectId })) ?? [{}];
    }
    const pages = await Promise.all(
      scopes.map((scope) =>
        this.requireHost().client.search({
          text: query.text,
          ...scope,
          ...(query.sourceId ? { adapterIds: [query.sourceId] } : {}),
          limit: Math.min(PAGE_LIMIT, offset + limit),
        }),
      ),
    );
    const all = pages
      .flatMap((page) => page.items)
      .sort((a, b) => a.score - b.score || a.messageId.localeCompare(b.messageId));
    const total = pages.reduce((sum, page) => sum + page.total, 0);
    const results = all.slice(offset, offset + limit).map((hit) => ({
      key: `message:${hit.messageId}`,
      type: 'message' as const,
      snippet: hit.snippet,
      rank: hit.score,
      ...(hit.nativeProjectKey ? { projectSlug: hit.nativeProjectKey } : {}),
      ...(hit.nativeSessionId ? { sessionId: hit.nativeSessionId } : {}),
      sourceId: hit.adapterId,
      ...(hit.nativeChildId ? { agentId: hit.nativeChildId } : {}),
    }));
    return { results, total, hasMore: offset + results.length < total };
  }

  async getStats(): Promise<StoreStats> {
    const stats = await this.requireHost().client.getStats();
    return {
      totalSegments: stats.factRecords,
      segmentsByType: Object.fromEntries(stats.entities.map((item) => [item.name, item.count])),
      totalFingerprints: stats.activeSourceObjects,
      dbSizeBytes: stats.allocatedDatabaseBytes,
      searchIndexed: stats.searchableMessages,
    };
  }

  async getTeams(): Promise<TeamDirectory[]> {
    const teams = await collectPages((cursor) => this.requireHost().client.listTeams({ cursor, limit: PAGE_LIMIT }));
    const output: TeamDirectory[] = [];
    for (const summary of teams) {
      const details = await this.requireHost().client.getTeam({ teamId: summary.teamId });
      const inboxes = await collectPages((cursor) =>
        this.requireHost().client.listTeamInboxes({ teamId: summary.teamId, cursor, limit: PAGE_LIMIT }),
      );
      const mappedInboxes: TeamDirectory['inboxes'] = {};
      for (const inbox of inboxes) {
        const messages = await collectPages((cursor) =>
          this.requireHost().client.listTeamInboxMessages({ inboxId: inbox.inboxId, cursor, limit: PAGE_LIMIT }),
        );
        mappedInboxes[inbox.nativeRecipientName] = messages.map((message) => ({
          from: message.nativeSenderName,
          text: message.text,
          ...(message.summary ? { summary: message.summary } : {}),
          timestamp: message.sourceTime,
          ...(message.color ? { color: message.color } : {}),
          read: message.read,
          ...(message.nativeMessageId ? { msg_id: message.nativeMessageId } : {}),
          ...(message.nativeVersion !== undefined ? { msgV: message.nativeVersion } : {}),
          ...(message.nativeKind ? { type: message.nativeKind } : {}),
        }));
      }
      output.push({
        teamId: summary.nativeTeamId,
        config: details.team.config
          ? {
              name: details.team.config.name,
              ...(details.team.config.description ? { description: details.team.config.description } : {}),
              createdAt: Date.parse(details.team.config.createdAt),
              leadAgentId: details.team.config.nativeLeadAgentId,
              leadSessionId: details.team.config.nativeLeadSessionId,
              members: details.members.map((member) => ({
                agentId: member.nativeAgentId,
                name: member.nativeName,
                ...(member.agentType ? { agentType: member.agentType } : {}),
                ...(member.model ? { model: member.model } : {}),
                ...(member.prompt ? { prompt: member.prompt } : {}),
                ...(member.color ? { color: member.color } : {}),
                ...(member.planModeRequired !== undefined ? { planModeRequired: member.planModeRequired } : {}),
                joinedAt: Date.parse(member.joinedAt),
                tmuxPaneId: member.tmuxPaneId,
                cwd: member.cwd,
                subscriptions: member.subscriptions,
                ...(member.backendType ? { backendType: member.backendType } : {}),
              })),
            }
          : null,
        inboxes: mappedInboxes,
      });
    }
    return output;
  }

  onProgress(cb: (progress: InitProgress) => void): () => void {
    this.on('progress', cb);
    const last = this.lastProgress;
    if (last) queueMicrotask(() => this.listeners('progress').includes(cb) && cb(last));
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

  snapshot(signal?: AbortSignal): Promise<ObservationHostSnapshot> {
    return this.requireHost().snapshot(signal);
  }

  serveIpc(channel: SpaghettiIpcChannel, transportKind?: string): SpaghettiIpcHost {
    return this.requireHost().serveIpc(channel, transportKind);
  }

  private requireHost(): ObservationHost {
    if (!this.host || !this.ready) throw new Error('Observation service is not ready.');
    return this.host;
  }

  private emitProgress(phase: InitProgress['phase'], message: string, detail: Partial<InitProgress> = {}): void {
    const progress = { phase, message, ...detail } satisfies InitProgress;
    this.lastProgress = progress;
    this.emit('progress', progress);
  }

  private emitHostProgress(progress: ObservationHostProgress): void {
    const sourceId = progress.adapterId;
    const commitSeq = progress.status?.observation.lastCommitSeq;
    switch (progress.stage) {
      case 'opening':
        this.emitProgress('parsing', 'Opening Rust observation owner…', {
          sourceCount: progress.sourceCount,
          elapsedMs: progress.elapsedMs,
        });
        return;
      case 'adapter-scanning':
        this.emitProgress(
          'parsing',
          `${sourceId} scan in progress${commitSeq == null ? '' : ` — commit ${commitSeq}`}…`,
          {
            sourceId,
            sourceStage: 'active',
            sourceIndex: progress.sourceIndex,
            sourceCount: progress.sourceCount,
            current: progress.sourceIndex,
            total: progress.sourceCount,
            elapsedMs: progress.elapsedMs,
            ...(commitSeq == null ? {} : { commitSeq }),
          },
        );
        return;
      case 'adapter-ready':
        this.emitProgress('indexing', `${sourceId} observation is live.`, {
          sourceId,
          sourceStage: 'done',
          sourceIndex: progress.sourceIndex,
          sourceCount: progress.sourceCount,
          current: progress.sourceIndex,
          total: progress.sourceCount,
          elapsedMs: progress.elapsedMs,
          ...(commitSeq == null ? {} : { commitSeq }),
        });
        return;
      case 'ready':
        this.emitProgress('reconciling', 'All configured Rust observations are live.', {
          current: progress.sourceCount,
          total: progress.sourceCount,
          sourceCount: progress.sourceCount,
          elapsedMs: progress.elapsedMs,
          ...(commitSeq == null ? {} : { commitSeq }),
        });
    }
  }

  private async startSubscription(host: ObservationHost): Promise<void> {
    const abort = new AbortController();
    this.subscriptionAbort = abort;
    const overview = await host.client.getOverview();
    const from = { commitSeq: overview.commitSeq, ordinal: MAX_CHANGE_ORDINAL };
    void (async () => {
      try {
        for await (const page of host.client.subscribe({ from }, { signal: abort.signal })) {
          if (page.changes.length === 0) continue;
          // Compatibility consumers treat this as a bounded invalidation, not
          // as a second semantic event bus. Empty changes deliberately mean
          // "refresh the selected snapshot" and avoid decoding source paths.
          this.emit('change', { changes: [], timestamp: Date.now() } satisfies SegmentChangeBatch);
        }
      } catch (error) {
        if (!abort.signal.aborted)
          this.emitProgress('reconciling', `Durable subscription interrupted: ${String(error)}`);
      }
    })();
  }

  private async allProjects(): Promise<SpaghettiEngineHistoryProject[]> {
    if (this.projectLoad) return await this.projectLoad;
    const work = collectPages((cursor) => this.requireHost().client.listProjects({ cursor, limit: PAGE_LIMIT })).then(
      (projects) => {
        this.projectCatalogLoaded = true;
        for (const project of projects) this.projectsById.set(project.projectId, project);
        return projects;
      },
    );
    const tracked = work.finally(() => {
      if (this.projectLoad === tracked) this.projectLoad = null;
    });
    this.projectLoad = tracked;
    return await tracked;
  }

  private async allSessions(projectId: string): Promise<SpaghettiEngineHistorySession[]> {
    const pending = this.sessionLoads.get(projectId);
    if (pending) return await pending;
    const work = collectPages((cursor) =>
      this.requireHost().client.listSessions({ projectId, cursor, limit: PAGE_LIMIT }),
    ).then((sessions) => {
      const project = this.projectsById.get(projectId);
      if (project) {
        for (const session of sessions) {
          this.sessionsByLocator.set(
            sessionLocator(project.adapterId, project.nativeProjectKey, session.nativeSessionId),
            {
              project,
              session,
            },
          );
        }
      }
      return sessions;
    });
    const tracked = work.finally(() => {
      if (this.sessionLoads.get(projectId) === tracked) this.sessionLoads.delete(projectId);
    });
    this.sessionLoads.set(projectId, tracked);
    return await tracked;
  }

  private async resolveProjects(
    project: ProjectReference,
    options?: SourceFilter,
  ): Promise<SpaghettiEngineHistoryProject[]> {
    const all = await this.allProjects();
    const members = typeof project === 'string' ? undefined : project.members;
    return all.filter((candidate) => {
      if (options?.sourceId && candidate.adapterId !== options.sourceId) return false;
      if (members) {
        return members.some(
          (member) => member.sourceId === candidate.adapterId && member.slug === candidate.nativeProjectKey,
        );
      }
      return candidate.nativeProjectKey === project;
    });
  }

  private async resolveSession(projectSlug: string, sessionId: string, sourceId?: string): Promise<ResolvedSession> {
    if (sourceId) {
      const cached = this.sessionsByLocator.get(sessionLocator(sourceId, projectSlug, sessionId));
      if (cached) return cached;
    }
    const resolutionKey = sessionLocator(sourceId ?? '*', projectSlug, sessionId);
    const pending = this.sessionResolutions.get(resolutionKey);
    if (pending) return await pending;
    const work = (async () => {
      const catalog = this.projectCatalogLoaded ? [...this.projectsById.values()] : await this.allProjects();
      const projects = catalog.filter(
        (project) => project.nativeProjectKey === projectSlug && (!sourceId || project.adapterId === sourceId),
      );
      for (const project of projects) {
        const cached = this.sessionsByLocator.get(
          sessionLocator(project.adapterId, project.nativeProjectKey, sessionId),
        );
        if (cached) return cached;
        const session = (await this.allSessions(project.projectId)).find((item) => item.nativeSessionId === sessionId);
        if (session) {
          const resolved = { project, session };
          this.sessionsByLocator.set(
            sessionLocator(project.adapterId, project.nativeProjectKey, session.nativeSessionId),
            resolved,
          );
          return resolved;
        }
      }
      throw new Error(`Canonical session '${sessionId}' was not found in project '${projectSlug}'.`);
    })();
    const tracked = work.finally(() => {
      if (this.sessionResolutions.get(resolutionKey) === tracked) this.sessionResolutions.delete(resolutionKey);
    });
    this.sessionResolutions.set(resolutionKey, tracked);
    return await tracked;
  }

  private async projectSummaries(options?: SourceFilter): Promise<ProjectSummary[]> {
    const projects = (await this.allProjects()).filter(
      (project) => !options?.sourceId || project.adapterId === options.sourceId,
    );
    return await mapConcurrent(projects, COMPATIBILITY_QUERY_CONCURRENCY, async (project) => {
      const sessions = await this.allSessions(project.projectId);
      const usage = await this.requireHost().client.getUsage({ projectId: project.projectId });
      const transcriptPath = sessions
        .map((session) => session.cwd ?? session.index?.projectPath)
        .find((value): value is string => Boolean(value && pathLikeNativeKey(value)));
      const absolutePath = project.index?.originalPath ?? transcriptPath ?? pathLikeNativeKey(project.nativeProjectKey);
      const firstActiveAt = minTimestamp(
        sessions.flatMap((session) => [session.firstMessageAt, session.index?.createdAt]),
      );
      const latest = [...sessions].sort((a, b) => sessionActivity(b).localeCompare(sessionActivity(a)))[0];
      const member: ProjectMember = { sourceId: project.adapterId, slug: project.nativeProjectKey };
      const groupKey = absolutePath ? `path:${normalizePath(absolutePath)}` : `member:${JSON.stringify(member)}`;
      return {
        canonical: project,
        item: {
          projectId: groupKey,
          members: [member],
          slug: project.nativeProjectKey,
          sourceIds: [project.adapterId],
          folderName: absolutePath ? basename(absolutePath) || absolutePath : project.nativeProjectKey,
          absolutePath,
          sessionCount: project.transcriptSessionCount,
          messageCount: project.messageCount,
          tokenUsage: usageValues(usage.aggregate),
          tokensEstimated: tokensAreQualified(usage.aggregate),
          lastActiveAt: project.latestActivityAt ?? '',
          firstActiveAt,
          latestGitBranch: latest?.gitBranch ?? latest?.index?.gitBranch ?? '',
          latestPrompt: latest?.customTitle ?? latest?.aiTitle ?? latest?.firstPrompt ?? latest?.index?.summary ?? '',
          hasMemory: project.memoryDocumentCount > 0,
        },
      };
    });
  }

  private async sessionListItem(
    project: SpaghettiEngineHistoryProject,
    session: SpaghettiEngineHistorySession,
  ): Promise<SessionListItem> {
    const [usage, collections] = await Promise.all([
      this.requireHost().client.getUsage({ projectId: project.projectId, sessionId: session.sessionId }),
      collectPages((cursor) =>
        this.requireHost().client.listTaskCollections({ sessionId: session.sessionId, cursor, limit: PAGE_LIMIT }),
      ),
    ]);
    const todoCount = collections.reduce((sum, item) => sum + item.itemCount, 0);
    const startTime = session.firstMessageAt ?? session.index?.createdAt ?? '';
    const lastUpdate = session.latestActivityAt ?? session.lastMessageAt ?? session.index?.modifiedAt ?? startTime;
    return {
      sessionId: session.nativeSessionId,
      sourceId: project.adapterId,
      projectSlug: project.nativeProjectKey,
      startTime,
      lastUpdate,
      lifespanMs: durationBetween(startTime, lastUpdate),
      tokenUsage: usageValues(usage.aggregate),
      tokensEstimated: tokensAreQualified(usage.aggregate),
      messageCount: session.messageCount,
      fullPath: session.index?.fullPath ?? '',
      title: session.customTitle ?? session.aiTitle ?? '',
      summary: session.index?.summary ?? '',
      firstPrompt: session.firstPrompt ?? session.index?.firstPrompt ?? '',
      gitBranch: session.gitBranch ?? session.index?.gitBranch ?? '',
      todoCount,
      planSlug: null,
      hasTask: collections.length > 0,
      isSidechain: session.index?.isSidechain ?? false,
    };
  }

  private async allMessages(
    resolved: ResolvedSession,
    stopAfter = Number.MAX_SAFE_INTEGER,
  ): Promise<SpaghettiEngineMessageDetail[]> {
    const output: SpaghettiEngineMessageDetail[] = [];
    let cursor: string | undefined;
    do {
      const page = await this.requireHost().client.getMessages({
        projectId: resolved.project.projectId,
        sessionId: resolved.session.sessionId,
        cursor,
        limit: PAGE_LIMIT,
      });
      output.push(...page.items);
      cursor = page.nextCursor;
    } while (cursor && output.length < stopAfter);
    return output;
  }

  private async messageCount(resolved: ResolvedSession): Promise<number> {
    const detail = await this.requireHost().client.getSession({ sessionId: resolved.session.sessionId });
    return detail.session?.messageCount ?? resolved.session.messageCount;
  }

  private async timelineRows(
    resolved: ResolvedSession,
    options: Pick<SpaghettiEngineTimelinePageOptions, 'branchKind'> = {},
  ): Promise<TimelineRow[]> {
    const envelopes = await this.timelineEnvelopes(resolved, options);
    const chronological = envelopes.reverse();
    const rows: TimelineRow[] = [];
    for (const envelope of chronological) {
      for (const message of timelineMessages(envelope)) {
        rows.push({
          index: rows.length + 1,
          adapterId: envelope.adapterId,
          nativeChildId: envelope.nativeChildId,
          runId: envelope.runId,
          message,
        });
      }
    }
    return attachToolResults(rows);
  }

  private async timelineEnvelopes(
    resolved: ResolvedSession,
    options: Pick<SpaghettiEngineTimelinePageOptions, 'branchKind'> = {},
  ): Promise<SpaghettiEngineTimelineMessage[]> {
    return await collectPages((cursor) =>
      this.requireHost().client.getTimeline({
        projectId: resolved.project.projectId,
        sessionId: resolved.session.sessionId,
        ...options,
        cursor,
        limit: PAGE_LIMIT,
      }),
    );
  }

  private async sessionTasks(sessionId: string): Promise<SpaghettiEngineTask[]> {
    const collections = await collectPages((cursor) =>
      this.requireHost().client.listTaskCollections({ sessionId, cursor, limit: PAGE_LIMIT }),
    );
    const output: SpaghettiEngineTask[] = [];
    for (const collection of collections) {
      output.push(
        ...(await collectPages((cursor) =>
          this.requireHost().client.listTasks({ collectionId: collection.collectionId, cursor, limit: PAGE_LIMIT }),
        )),
      );
    }
    return output;
  }
}

function sessionLocator(sourceId: string, projectSlug: string, sessionId: string): string {
  return JSON.stringify([sourceId, projectSlug, sessionId]);
}

async function collectPages<T>(read: (cursor?: string) => Promise<{ items: T[]; nextCursor?: string }>): Promise<T[]> {
  for (let attempt = 0; ; attempt++) {
    try {
      const output: T[] = [];
      let cursor: string | undefined;
      const seen = new Set<string>();
      do {
        const page = await read(cursor);
        output.push(...page.items);
        const next = page.nextCursor;
        if (next && (next === cursor || seen.has(next))) throw new Error('Canonical page cursor did not advance.');
        if (next) seen.add(next);
        cursor = next;
      } while (cursor);
      return output;
    } catch (error) {
      if (attempt >= CURSOR_SNAPSHOT_RESTARTS || !isExpiredCursor(error)) throw error;
    }
  }
}

async function mapConcurrent<T, U>(
  values: readonly T[],
  concurrency: number,
  map: (value: T, index: number) => Promise<U>,
): Promise<U[]> {
  const output = new Array<U>(values.length);
  let nextIndex = 0;
  const workers = Array.from({ length: Math.min(values.length, Math.max(1, concurrency)) }, async () => {
    for (;;) {
      const index = nextIndex++;
      if (index >= values.length) return;
      output[index] = await map(values[index]!, index);
    }
  });
  await Promise.all(workers);
  return output;
}

function isExpiredCursor(error: unknown): boolean {
  if (!error || typeof error !== 'object') return false;
  const value = error as { code?: unknown; reason?: unknown; message?: unknown };
  return (
    value.code === 'cursor_invalid' &&
    (value.reason === 'expired' || (typeof value.message === 'string' && /expired/i.test(value.message)))
  );
}

function nativeTimelineRequest(
  resolved: ResolvedSession,
  request: TimelinePageRequest,
): SpaghettiEngineTimelinePageOptions {
  const options: SpaghettiEngineTimelinePageOptions = {
    projectId: resolved.project.projectId,
    sessionId: resolved.session.sessionId,
    limit: boundInteger(request.limit ?? 30, 1, PAGE_LIMIT, 30),
  };
  if (typeof request.before === 'string') options.cursor = request.before;
  const search = request.search?.trim();
  if (search) options.search = search;

  const includeTypes = request.includeTypes?.filter(Boolean) ?? [];
  const includeTools = request.includeTools?.filter(Boolean) ?? [];
  const contentKinds = includeTypes.map(displayTypeContentKind);
  if (includeTypes.length > 0 && contentKinds.every((kind) => kind !== undefined)) {
    options.includeContentKinds = contentKinds as NonNullable<
      SpaghettiEngineTimelinePageOptions['includeContentKinds']
    >;
    if (includeTools.length > 0) options.includeToolNames = includeTools;
  } else if (includeTypes.length === 0 && includeTools.length > 0) {
    options.includeToolNames = includeTools;
  } else if (includeTools.length === 0 && includeTypes.length > 0 && includeTypes.every(isDisplayRole)) {
    options.roles = includeTypes;
  }
  return options;
}

function displayTypeContentKind(
  type: string,
): NonNullable<SpaghettiEngineTimelinePageOptions['includeContentKinds']>[number] | undefined {
  if (type === 'thinking') return 'thinking';
  if (type === 'tool_use') return 'tool_call';
  if (type === 'tool_result') return 'tool_result';
  return undefined;
}

function isDisplayRole(type: string): boolean {
  return type === 'user' || type === 'assistant' || type === 'system';
}

function compatibilityTimelinePage(
  page: SpaghettiEngineTimelinePage,
  request: TimelinePageRequest,
  snapshotReset = false,
): TimelinePage {
  const rows: TimelineRow[] = [];
  for (const envelope of [...page.items].reverse()) {
    for (const message of timelineMessages(envelope)) {
      rows.push({
        index: rows.length + 1,
        adapterId: envelope.adapterId,
        nativeChildId: envelope.nativeChildId,
        runId: envelope.runId,
        message,
      });
    }
  }
  const filtered = filterTimeline(attachToolResults(rows), request);
  return {
    messages: filtered.map((row) => row.message),
    total: page.total,
    facets: compatibilityTimelineFacets(page.facets),
    ...(page.nextCursor ? { nextCursor: page.nextCursor } : {}),
    hasMore: Boolean(page.nextCursor),
    ...(snapshotReset ? { snapshotReset: true } : {}),
  };
}

function compatibilityTimelineFacets(facets: SpaghettiEngineTimelineFacets): TimelineFacets {
  const messageCounts: Record<string, number> = {};
  const toolCounts = Object.fromEntries(facets.toolNames.map((item) => [item.name, item.count]));
  for (const item of facets.roles) messageCounts[item.name] = (messageCounts[item.name] ?? 0) + item.count;
  for (const item of facets.nativeKinds) {
    const type = nativeKindDisplayType(item.name);
    if (type) messageCounts[type] = (messageCounts[type] ?? 0) + item.count;
  }
  for (const item of facets.contentKinds) {
    const type =
      item.name === 'thinking'
        ? 'thinking'
        : item.name === 'tool_call'
          ? 'tool_use'
          : item.name === 'tool_result'
            ? 'tool_result'
            : undefined;
    if (type) messageCounts[type] = (messageCounts[type] ?? 0) + item.count;
  }
  return { total: facets.totalMessages, messageCounts, toolCounts };
}

function usageValues(aggregate: SpaghettiEngineUsageAggregate): TokenUsageSummary {
  const values = aggregate.combined;
  return {
    inputTokens: values.inputTokens,
    outputTokens: values.outputTokens,
    cacheCreationTokens: values.cacheCreationTokens,
    cacheReadTokens: values.cacheReadTokens,
    totalTokens: values.componentTotalTokens,
  };
}

/**
 * True when the displayed total is not fully exact: either some bucket was
 * derived or estimated, or some response asserted no bucket at all and the
 * total is a floor rather than a measurement.
 */
function tokensAreQualified(aggregate: SpaghettiEngineUsageAggregate): boolean {
  return aggregate.estimatedContributionCount > 0 || aggregate.unknownContributionCount > 0;
}

function addTokenUsage(target: TokenUsageSummary, source: TokenUsageSummary): void {
  target.inputTokens += source.inputTokens;
  target.outputTokens += source.outputTokens;
  target.cacheCreationTokens += source.cacheCreationTokens;
  target.cacheReadTokens += source.cacheReadTokens;
  target.totalTokens += source.totalTokens;
}

/**
 * A day is only `exact` when every response it contains asserted every bucket
 * exactly. A response that asserted nothing makes the day's total a floor, so
 * it downgrades the day rather than passing as exact.
 */
function activityQuality(exact: number, estimated: number, unknownResponses: number): TokenActivityDay['quality'] {
  if (exact > 0 && (estimated > 0 || unknownResponses > 0)) return 'mixed';
  if (estimated > 0 || unknownResponses > 0) return 'estimated';
  if (exact > 0) return 'exact';
  return 'unavailable';
}

function mergeProject(target: ProjectListItem, source: ProjectListItem): void {
  const keys = new Set(target.members.map(memberKey));
  for (const member of source.members) if (!keys.has(memberKey(member))) target.members.push(member);
  target.members.sort((a, b) => memberKey(a).localeCompare(memberKey(b)));
  target.sourceIds = [...new Set(target.members.map((member) => member.sourceId))].sort();
  target.sessionCount += source.sessionCount;
  target.messageCount += source.messageCount;
  addTokenUsage(target.tokenUsage, source.tokenUsage);
  target.tokensEstimated ||= source.tokensEstimated;
  target.hasMemory ||= source.hasMemory;
  if (!target.firstActiveAt || (source.firstActiveAt && source.firstActiveAt < target.firstActiveAt)) {
    target.firstActiveAt = source.firstActiveAt;
  }
  if (source.lastActiveAt > target.lastActiveAt) {
    target.lastActiveAt = source.lastActiveAt;
    target.latestGitBranch = source.latestGitBranch;
    target.latestPrompt = source.latestPrompt;
    target.slug = source.slug;
  }
}

function memberKey(member: ProjectMember): string {
  return JSON.stringify([member.sourceId, member.slug]);
}

function compareProjectActivity(a: ProjectListItem, b: ProjectListItem): number {
  return b.lastActiveAt.localeCompare(a.lastActiveAt) || a.projectId.localeCompare(b.projectId);
}

function pathLikeNativeKey(value: string): string {
  return value.startsWith('/') || /^[A-Za-z]:[\\/]/.test(value) ? value : '';
}

function normalizePath(value: string): string {
  const normalized = value.trim().replace(/\\/g, '/');
  return normalized.length > 1 ? normalized.replace(/\/+$/, '') : normalized;
}

function minTimestamp(values: Array<string | undefined>): string {
  return values.filter((value): value is string => Boolean(value)).sort()[0] ?? '';
}

function sessionActivity(session: SpaghettiEngineHistorySession): string {
  return session.latestActivityAt ?? session.lastMessageAt ?? session.index?.modifiedAt ?? '';
}

function durationBetween(first: string, last: string): number {
  const start = Date.parse(first);
  const end = Date.parse(last);
  return Number.isFinite(start) && Number.isFinite(end) ? Math.max(0, end - start) : 0;
}

function boundInteger(value: number, min: number, max: number, fallback: number): number {
  return Number.isFinite(value) ? Math.max(min, Math.min(max, Math.floor(value))) : fallback;
}

/**
 * Preserve the former message DTO without exposing source-format semantics to
 * TypeScript. Rust has already normalized every content block; this mapping is
 * presentation-only and is identical for every adapter.
 */
function compatibilityMessage(detail: SpaghettiEngineMessageDetail): MessagePage['messages'][number] {
  const content = Array.isArray(detail.content) ? (detail.content as Array<Record<string, unknown>>) : [];
  const blocks = content.map(compatibilityContentBlock);
  const text = content
    .filter((block) => block.kind === 'text')
    .map((block) => String(block.text ?? ''))
    .join('\n');
  const base = {
    uuid: detail.nativeMessageId ?? detail.messageId,
    parentUuid: detail.parentNativeMessageId ?? null,
    timestamp: detail.sourceTime ?? '',
    sessionId: detail.nativeSessionId,
    cwd: '',
    version: '',
    gitBranch: '',
    isSidechain: false,
    userType: 'external',
  };

  if (detail.role === 'user') {
    const onlyText = blocks.every((block) => block.type === 'text');
    return {
      ...base,
      type: 'user',
      message: { role: 'user', content: onlyText ? text : blocks },
    } as unknown as MessagePage['messages'][number];
  }
  if (detail.role === 'assistant') {
    return {
      ...base,
      type: 'assistant',
      message: {
        role: 'assistant',
        content: blocks,
        ...(detail.model ? { model: detail.model } : {}),
      },
    } as unknown as MessagePage['messages'][number];
  }
  return {
    ...base,
    type: detail.nativeKind === 'summary' ? 'summary' : 'system',
    content: text || textValue(detail.content),
    ...(detail.nativeKind ? { subtype: detail.nativeKind } : {}),
  } as unknown as MessagePage['messages'][number];
}

function compatibilityContentBlock(block: Record<string, unknown>): Record<string, unknown> {
  switch (block.kind) {
    case 'text':
      return { type: 'text', text: String(block.text ?? '') };
    case 'thinking':
      return block.redacted === true
        ? { type: 'redacted_thinking', data: String(block.text ?? '') }
        : { type: 'thinking', thinking: String(block.text ?? '') };
    case 'tool_call':
      return {
        type: 'tool_use',
        id: String(block.native_id ?? ''),
        name: String(block.name ?? 'Unknown Tool'),
        input: objectValue(block.input),
      };
    case 'tool_result':
      return {
        type: 'tool_result',
        tool_use_id: String(block.native_call_id ?? ''),
        content: textValue(block.content),
        is_error: block.is_error === true,
      };
    default:
      return { type: 'text', text: textValue(block.value ?? block) };
  }
}

function taskItem(task: SpaghettiEngineTask): Record<string, unknown> {
  return {
    id: task.nativeTaskId ?? String(task.itemOrdinal),
    subject: task.subject,
    description: task.description ?? '',
    ...(task.activeForm ? { activeForm: task.activeForm } : {}),
    ...(task.nativeOwner ? { owner: task.nativeOwner } : {}),
    status: task.taskStatus,
    blocks: task.blocks,
    blockedBy: task.blockedBy,
  };
}

function delegationItem(item: SpaghettiEngineDelegation, workflowId: string): SubagentListItem {
  return {
    sourceId: item.adapterId,
    agentId: item.nativeChildId ?? item.nativeRunId ?? item.runId,
    agentType: item.agentType ?? item.requestedAgentType ?? item.relationKind,
    messageCount: item.messageCount,
    workflowId,
    spawnToolId: item.branchAnchorMessageId ?? null,
    linkMethod: item.branchAnchorMessageId
      ? 'tool_result'
      : item.relationStrength === 'ordinal'
        ? 'ordinal'
        : 'unlinked',
    ...(item.worktreePath ? { worktreePath: item.worktreePath } : {}),
  };
}

function workflowMemberItem(item: SpaghettiEngineWorkflowMember, workflowId: string): SubagentListItem {
  return {
    sourceId: item.adapterId,
    agentId: item.nativeAgentId,
    agentType: item.agentType ?? 'unknown',
    messageCount: item.messageCount,
    workflowId,
    spawnToolId: null,
    linkMethod: 'unlinked',
    ...(item.worktreePath ? { worktreePath: item.worktreePath } : {}),
  };
}

function timelineMessages(item: SpaghettiEngineTimelineMessage): SessionMessage[] {
  const content = Array.isArray(item.content) ? (item.content as Array<Record<string, unknown>>) : [];
  const base = {
    parentUuid: item.parentNativeMessageId ?? null,
    timestamp: item.sourceTime ?? '',
    sessionId: item.nativeSessionId,
    role: item.role,
    model: item.model,
    agentId: item.nativeChildId,
    isSidechain: item.branchKind === 'delegated' || undefined,
    branchKey: item.runId,
    branchToolId: item.branchAnchorMessageId,
  };
  const output: SessionMessage[] = [];
  if (content.length === 0) {
    const type = nativeKindDisplayType(item.nativeKind) ?? item.role;
    output.push({
      ...base,
      timelineId: `${item.messageId}:empty`,
      uuid: item.nativeMessageId ?? item.messageId,
      type,
      content: type === 'checkpoint' ? 'Checkpoint Created' : '',
      ...(type === 'checkpoint'
        ? { checkpointData: { messageId: item.nativeMessageId ?? item.messageId, isUpdate: false, fileCount: 0 } }
        : {}),
      ...(type === 'queue-operation' ? { queueOperation: '' } : {}),
    });
  }
  for (const [ordinal, block] of content.entries()) {
    const kind = String(block.kind ?? 'native');
    const timelineId = `${item.messageId}:${ordinal}`;
    const uuid =
      ordinal === 0 && item.nativeMessageId
        ? item.nativeMessageId
        : `${item.nativeMessageId ?? item.messageId}-${ordinal}`;
    if (kind === 'text') {
      const type = nativeKindDisplayType(item.nativeKind) ?? item.role;
      output.push({
        ...base,
        timelineId,
        uuid,
        type,
        content: String(block.text ?? ''),
        ...(type === 'compact_summary' ? { isCompactSummary: true } : {}),
      });
    } else if (kind === 'thinking') {
      output.push({
        ...base,
        timelineId,
        uuid,
        type: 'thinking',
        content: String(block.text ?? ''),
      });
    } else if (kind === 'tool_call') {
      output.push({
        ...base,
        timelineId,
        uuid,
        type: 'tool_use',
        toolUse: {
          toolId: String(block.native_id ?? ''),
          toolName: String(block.name ?? 'Unknown Tool'),
          input: objectValue(block.input),
        },
      });
    } else if (kind === 'tool_result') {
      output.push({
        ...base,
        timelineId,
        uuid,
        type: 'tool_result',
        toolResult: {
          toolId: String(block.native_call_id ?? ''),
          isError: block.is_error === true,
          content: textValue(block.content),
        },
      });
    } else {
      output.push({
        ...base,
        timelineId,
        uuid,
        type: item.nativeKind || 'system',
        content: textValue(block.value ?? block),
      });
    }
  }
  return output;
}

function nativeKindDisplayType(nativeKind: string): string | undefined {
  if (nativeKind === 'summary') return 'summary';
  if (nativeKind === 'compact_summary') return 'compact_summary';
  if (nativeKind === 'file-history-snapshot') return 'checkpoint';
  if (nativeKind === 'queue-operation') return 'queue-operation';
  return undefined;
}

function objectValue(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

function textValue(value: unknown): string {
  if (typeof value === 'string') return value;
  if (value == null) return '';
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

function attachToolResults(rows: TimelineRow[]): TimelineRow[] {
  const calls = new Map<string, SessionMessage[]>();
  const results = new Map<string, TimelineRow[]>();
  for (const row of rows) {
    const call = row.message.toolUse;
    if (call?.toolId) {
      const matching = calls.get(call.toolId) ?? [];
      matching.push(row.message);
      calls.set(call.toolId, matching);
    }
    const result = row.message.toolResult;
    if (result?.toolId) {
      const matching = results.get(result.toolId) ?? [];
      matching.push(row);
      results.set(result.toolId, matching);
    }
  }
  const attached = new Set<TimelineRow>();
  for (const [toolId, resultRows] of results) {
    const owners = calls.get(toolId);
    const result = resultRows.at(-1)?.message.toolResult;
    if (!owners?.length || !result) continue;
    for (const owner of owners) {
      if (owner.toolUse) owner.toolUse = { ...owner.toolUse, result };
    }
    for (const row of resultRows) attached.add(row);
  }
  // Claude/Codex result envelopes historically decorate their tool call and
  // are not rendered a second time. Preserve an unmatched result as a visible
  // row so page boundaries and genuinely orphaned results never lose data.
  return rows.filter((row) => !attached.has(row));
}

function timelinePage(rows: TimelineRow[], request: TimelinePageRequest): TimelinePage {
  const filtered = filterTimeline(rows, request);
  const before = typeof request.before === 'number' ? request.before : Number.MAX_SAFE_INTEGER;
  const eligible = filtered.filter((row) => row.index < before);
  const limit = boundInteger(request.limit ?? 30, 1, 500, 30);
  const newest = eligible.slice(-limit);
  return {
    messages: newest.map((row) => row.message),
    total: filtered.length,
    nextCursor: eligible.length > newest.length ? newest[0]?.index : undefined,
    hasMore: eligible.length > newest.length,
  };
}

function filterTimeline<T extends TimelinePageRequest | SubagentTimelinePageRequest>(
  rows: TimelineRow[],
  request: T,
): TimelineRow[] {
  const includeTypes = new Set(request.includeTypes ?? []);
  const includeTools = new Set(request.includeTools ?? []);
  const excludeTypes = new Set(request.excludeTypes ?? []);
  const excludeTools = new Set(request.excludeTools ?? []);
  const search = request.search?.trim().toLocaleLowerCase();
  return rows.filter((row) => {
    if (request.sourceId && row.adapterId !== request.sourceId) return false;
    const type = row.message.type;
    const tool = row.message.toolUse?.toolName;
    if (includeTypes.size > 0 || includeTools.size > 0) {
      if (!includeTypes.has(type) && (!tool || !includeTools.has(tool))) return false;
    } else if (excludeTypes.has(type) || (tool && excludeTools.has(tool))) {
      return false;
    }
    return !search || timelineSearchText(row.message).includes(search);
  });
}

function timelineSearchText(message: SessionMessage): string {
  return [message.content, message.toolUse?.toolName, message.toolUse?.input, message.toolResult?.content]
    .map(textValue)
    .join(' ')
    .toLocaleLowerCase();
}
