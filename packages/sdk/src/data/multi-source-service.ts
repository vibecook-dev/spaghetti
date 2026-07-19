/**
 * SpaghettiDataService — the agent-agnostic data service (RFC 006 multi-source).
 *
 * The app consumes ONE data service. This is it: reads delegate to the shared
 * `AgentDataStore` (which unifies every source's rows), and lifecycle
 * (`initialize`/`shutdown`/`rebuild`) fans across a set of per-source
 * `LifecycleOwner`s. It is deliberately NOT coupled to any agent — no source's
 * owner serves reads; the store does. A single-source app passes one owner; a
 * multi-source app passes several, all writing into the same store.
 *
 * Implements the same `AgentDataService` surface `app-service.ts` already
 * consumes (agent-agnostic reads + multi-source lifecycle), plus
 * `LifecycleInternal` for `getStore`/`getLiveWatch`.
 */

import { EventEmitter } from 'events';
import type {
  SegmentType,
  SegmentKey,
  Segment,
  PaginatedSegmentQuery,
  PaginatedSegmentResult,
  SearchQuery,
  SearchResultSet,
  StoreStats,
} from './segment-types.js';
import type { SessionSummaryData, ProjectSummaryData } from './summary-types.js';
import type { Project, Session, SessionMessage, AgentConfig, AgentAnalytic } from '../types/index.js';
import type { AgentDataStore } from './agent-data-store.js';
import type { LiveWatch } from '../live/live-watch.js';
import type { AgentDataService, LifecycleInternal, LifecycleOwner } from './lifecycle-owner.js';
import type {
  SubagentTimelinePage,
  SubagentTimelinePageRequest,
  TimelineFacets,
  TimelinePage,
  TimelinePageRequest,
} from './timeline-query.js';
import { ensureSqliteCacheHealthy, isSqliteCorruptError, wipeSqliteCacheFiles } from '../io/sqlite-health.js';

export class SpaghettiDataService extends EventEmitter implements AgentDataService, LifecycleInternal {
  private ready = false;

  constructor(
    private readonly store: AgentDataStore,
    private readonly owners: LifecycleOwner[],
  ) {
    super();
    // Forward progress/change/error from every owner. `ready` is emitted once,
    // by this service's own `initialize()`, after ALL owners finish — an
    // individual owner's `ready` would fire prematurely for the whole app.
    for (const owner of this.owners) {
      owner.on('progress', (data) => this.emit('progress', data));
      owner.on('change', (data) => this.emit('change', data));
      owner.on('error', (data) => this.emit('error', data));
    }
  }

  // ── Lifecycle — exclusive native queue ───────────────────────────────────
  //
  //   exclusiveIngest() × N  (serial, shared SQLite CLOSED — all may use rs)
  //   attachShared()    × N  (open shared handle once, light post-steps)
  //   startLivePipeline() × N
  //
  // This is how N agents each use native bulk without racing better-sqlite3.

  async initialize(): Promise<void> {
    this.ready = false;
    const start = Date.now();
    await this.runPhasedIngestWithCorruptRecovery();
    this.ready = true;
    this.emit('ready', { durationMs: Date.now() - start });
  }

  /**
   * Serial exclusive queue + shared attach + live. Used by initialize and
   * rebuildIndex so multi-source always prefers native rs for every owner.
   */
  private async runPhasedIngest(): Promise<void> {
    this.preflightCacheHealth();
    // Phase 1 — each owner gets the file alone (native MEMORY journal OK).
    for (const owner of this.owners) {
      await owner.exclusiveIngest();
    }
    // Phase 2 — open shared better-sqlite3 once; attach every source.
    for (const owner of this.owners) {
      await owner.attachShared();
    }
    // Native cold/warm ingest writes canonical rows without running the
    // TypeScript display transformer. Finish those rebuildable indexes now so
    // opening a session never performs an O(session length) write transaction.
    this.store.prepareTimelineProjections(({ kind, current, total }) => {
      // Projection preparation can cover hundreds of sessions. Keep startup
      // visible without flooding the UtilityProcess -> main -> renderer port.
      if (current !== 1 && current !== total && current % 10 !== 0) return;
      this.emit('progress', {
        phase: 'storing',
        message: kind === 'session' ? 'Normalizing session transcripts…' : 'Normalizing subagent transcripts…',
        current,
        total,
      });
    });
    // Phase 3 — Plane 2 only after every source and projection is warm.
    for (const owner of this.owners) {
      await owner.startLivePipeline();
    }
  }

  /**
   * Run phased ingest; on SQLITE_CORRUPT / "database disk image is malformed"
   * wipe the shared cache once and retry cold ingest from disk.
   */
  private async runPhasedIngestWithCorruptRecovery(): Promise<void> {
    try {
      await this.runPhasedIngest();
    } catch (err) {
      if (!isSqliteCorruptError(err)) throw err;
      await this.recoverCorruptCache(err);
      // Second attempt — if this also fails, surface that error.
      await this.runPhasedIngest();
    }
  }

  /**
   * Before exclusive ingest: if the shared cache is structurally corrupt,
   * wipe it so cold re-ingest can proceed. Emits progress when wiped.
   */
  private preflightCacheHealth(): void {
    const dbPath = this.resolveCacheDbPath();
    if (!dbPath) return;
    const result = ensureSqliteCacheHealthy(dbPath);
    if (result.wiped) {
      this.emit('progress', {
        phase: 'parsing',
        message: `Cache corrupted — wiped for re-ingest${result.detail ? ` (${result.detail})` : ''}`,
      });
    }
  }

  private resolveCacheDbPath(): string | undefined {
    return this.owners.map((o) => o.getCacheDbPath?.()).find((p) => !!p);
  }

  /**
   * Stop live, close handles, delete the shared cache file. Emits progress
   * so the playground loading screen can show recovery.
   */
  private async recoverCorruptCache(err: unknown): Promise<void> {
    const detail = err instanceof Error ? err.message : String(err);
    this.emit('progress', {
      phase: 'parsing',
      message: 'SQLite cache malformed — deleting and re-ingesting from disk…',
    });
    this.emit('error', {
      error: `Recovering from corrupt cache: ${detail}`,
    });
    try {
      await this.stopAllLive();
    } catch {
      /* best-effort */
    }
    this.releaseAllShared();
    const dbPath = this.resolveCacheDbPath();
    if (dbPath) {
      wipeSqliteCacheFiles(dbPath);
    }
    // Owner wipe also closes + deletes (primary owns path); idempotent.
    const wiper = this.owners.find((o) => typeof o.wipeCache === 'function');
    wiper?.wipeCache?.();
  }

  private async stopAllLive(): Promise<void> {
    for (const owner of this.owners) {
      const live = owner.getLiveWatch();
      if (live) {
        try {
          await live.stop();
        } catch {
          /* best-effort */
        }
      }
    }
  }

  private releaseAllShared(): void {
    for (const owner of this.owners) {
      owner.releaseShared?.();
    }
  }

  shutdown(): void {
    this.ready = false;
    // Reverse order so the primary (first-constructed) owner tears down last.
    for (const owner of [...this.owners].reverse()) {
      owner.shutdown();
    }
  }

  async shutdownAsync(): Promise<void> {
    this.ready = false;
    for (const owner of [...this.owners].reverse()) {
      if (owner.shutdownAsync) {
        await owner.shutdownAsync();
      } else {
        owner.shutdown();
      }
    }
  }

  async rebuild(): Promise<void> {
    // Full phased re-warm without deleting the file (fingerprints still apply).
    this.ready = false;
    await this.stopAllLive();
    this.releaseAllShared();
    await this.runPhasedIngestWithCorruptRecovery();
    this.ready = true;
    this.emit('change', { changes: [], timestamp: Date.now() });
  }

  async rebuildIndex(): Promise<{ durationMs: number }> {
    const start = Date.now();
    this.ready = false;
    await this.stopAllLive();
    this.releaseAllShared();
    // Wipe the shared cache once (primary owner owns the path), then re-queue
    // every source through exclusive native ingest.
    const wiper = this.owners.find((o) => typeof o.wipeCache === 'function');
    wiper?.wipeCache?.();
    await this.runPhasedIngestWithCorruptRecovery();
    this.ready = true;
    return { durationMs: Date.now() - start };
  }

  isReady(): boolean {
    return this.ready;
  }

  // ── Internal accessors ────────────────────────────────────────────────────

  getStore(): AgentDataStore {
    return this.store;
  }

  getLiveWatch(): LiveWatch | undefined {
    // Each source runs its own LiveWatch (started by its owner) and emits into
    // the shared store, which `api.live` observes. This returns the primary
    // (first) owner's — Claude's, which carries the `prewarm` scope surface
    // `api.live` exposes; other sources' watches run alongside via the store.
    for (const owner of this.owners) {
      const live = owner.getLiveWatch();
      if (live) return live;
    }
    return undefined;
  }

  // ── Reads (delegate to the shared, unified store) ─────────────────────────

  getSegment<T>(_key: SegmentKey): Segment<T> | null {
    return null;
  }

  getSegmentsByType<T>(_type: SegmentType): Segment<T>[] {
    return [];
  }

  getSegmentsPaginated<T>(query: PaginatedSegmentQuery): PaginatedSegmentResult<T> {
    return { segments: [], total: 0, offset: query.offset, hasMore: false };
  }

  getProjectSlugs(): string[] {
    return this.store.getProjectSlugs();
  }

  getProject(_slug: string): Segment<Project> | null {
    return null;
  }

  getProjectSessions(_slug: string): Segment<Session>[] {
    return [];
  }

  getSessionMessages(
    slug: string,
    sessionId: string,
    limit: number,
    offset: number,
    options?: { sourceId?: string },
  ): PaginatedSegmentResult<SessionMessage> {
    const result = this.store.getSessionMessages(slug, sessionId, limit, offset, options);
    const segments: Segment<SessionMessage>[] = result.messages.map((msg, i) => ({
      key: `message:${slug}/${sessionId}/${offset + i}`,
      type: 'message' as SegmentType,
      data: msg as SessionMessage,
      version: 1,
      updatedAt: Date.now(),
    }));
    return { segments, total: result.total, offset: result.offset, hasMore: result.hasMore };
  }

  getSessionTimelineFacets(slug: string, sessionId: string, options?: { sourceId?: string }): TimelineFacets {
    return this.store.getSessionTimelineFacets(slug, sessionId, options);
  }

  getSessionTimeline(slug: string, sessionId: string, request?: TimelinePageRequest): TimelinePage {
    return this.store.getSessionTimeline(slug, sessionId, request);
  }

  getConfig(): AgentConfig {
    // Config/analytics are populated by each source's initialize() into the
    // shared store; reads just return what's there (no parser here — that keeps
    // this service source-neutral).
    return this.store.getConfig();
  }

  getAnalytics(): AgentAnalytic {
    return this.store.getAnalytics();
  }

  getSourceIds(): string[] {
    return this.store.getSourceIds();
  }

  getProjectSummaries(options?: { sourceId?: string }): ProjectSummaryData[] {
    return this.store.getProjectSummaries(options);
  }

  getSessionSummaries(projectSlug: string, options?: { sourceId?: string }): SessionSummaryData[] {
    return this.store.getSessionSummaries(projectSlug, options);
  }

  getProjectMemory(slug: string, options?: { sourceId?: string }): string | null {
    return this.store.getProjectMemory(slug, options);
  }

  getSessionTodos(slug: string, sessionId: string): unknown[] {
    return this.store.getSessionTodos(slug, sessionId);
  }

  getSessionPlan(slug: string, sessionId: string): unknown | null {
    return this.store.getSessionPlan(slug, sessionId);
  }

  getSessionTask(slug: string, sessionId: string): unknown | null {
    return this.store.getSessionTask(slug, sessionId);
  }

  getPersistedToolResult(slug: string, sessionId: string, toolUseId: string): string | null {
    return this.store.getToolResult(slug, sessionId, toolUseId);
  }

  getSessionSubagents(
    slug: string,
    sessionId: string,
    options?: { sourceId?: string; includeNested?: boolean },
  ): ReturnType<AgentDataStore['getSessionSubagents']> {
    return this.store.getSessionSubagents(slug, sessionId, options);
  }

  getSessionWorkflows(slug: string, sessionId: string): ReturnType<AgentDataStore['getSessionWorkflows']> {
    return this.store.getSessionWorkflows(slug, sessionId);
  }

  getWorkflowSubagents(
    slug: string,
    sessionId: string,
    workflowId: string,
    options?: { sourceId?: string },
  ): ReturnType<AgentDataStore['getWorkflowSubagents']> {
    return this.store.getWorkflowSubagents(slug, sessionId, workflowId, options);
  }

  getSubagentMessages(
    slug: string,
    sessionId: string,
    agentId: string,
    limit: number,
    offset: number,
    workflowId?: string,
    options?: { sourceId?: string },
  ): PaginatedSegmentResult<SessionMessage> {
    const result = this.store.getSubagentMessages(slug, sessionId, agentId, limit, offset, workflowId, options);
    const segments: Segment<SessionMessage>[] = result.messages.map((msg, i) => ({
      key: `subagent:${slug}/${sessionId}/${agentId}/${offset + i}`,
      type: 'subagent' as SegmentType,
      data: msg as SessionMessage,
      version: 1,
      updatedAt: Date.now(),
    }));
    return { segments, total: result.total, offset: result.offset, hasMore: result.hasMore };
  }

  getSubagentTimeline(
    slug: string,
    sessionId: string,
    agentId: string,
    request: SubagentTimelinePageRequest,
  ): SubagentTimelinePage {
    return this.store.getSubagentTimeline(slug, sessionId, agentId, request);
  }

  search(query: SearchQuery): SearchResultSet {
    return this.store.search(query);
  }

  getStoreStats(): StoreStats {
    return this.store.getStats();
  }
}
