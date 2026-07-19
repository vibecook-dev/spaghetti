/**
 * AgentDataStore — Read layer + cache owner for the Phase 3 schema.
 *
 * This file is the first landing point for the RFC 005 refactor that
 * splits `AgentDataServiceImpl` into a lifecycle owner (future
 * `LifecycleOwner`) and a store that concentrates every read path plus
 * the in-memory config/analytics caches.
 *
 * For this commit (C1.1) the store only exposes the current read
 * methods, delegating each one to an injected `QueryService`. Later
 * commits move the `cachedConfig`/`cachedAnalytics` fields in (C1.2),
 * route `AgentDataServiceImpl`'s reads through the store (C1.3), and
 * attach the subscriber/registry stubs (C1.4).
 *
 * See `docs/rfcs/005-live-updates.md` and
 * `docs/LIVE-UPDATES-DESIGN.md` §2.2 for the full intended shape.
 */

import type { PaginatedSegmentResult, SearchQuery, SearchResultSet, StoreStats } from './segment-types.js';
import type { ProjectSummaryData, SessionSummaryData } from './summary-types.js';
import type { AgentAnalytic, AgentConfig, SessionMessage } from '../types/index.js';
import type { QueryService } from './query-service.js';
import type {
  SubagentTimelinePage,
  SubagentTimelinePageRequest,
  TimelineFacets,
  TimelinePage,
  TimelinePageRequest,
} from './timeline-query.js';
import type {
  Change,
  ChangeTopic,
  Dispose,
  SubscribeOptions,
  SubscribeOptionsCoalesced,
  SubscribeOptionsLatest,
} from '../live/change-events.js';
import { createSubscriberRegistry, type SubscriberRegistry } from '../live/subscriber-registry.js';
import type { ErrorSink } from '../io/error-sink.js';

// ═══════════════════════════════════════════════════════════════════════════
// INTERFACE
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Read-only view over the SDK's SQLite cache. Every method here mirrors
 * the corresponding read method currently on `AgentDataServiceImpl` —
 * same name, same signature, same return shape — so later commits can
 * swap the service's implementation over to `this.store.getX(...)` with
 * no observable behavior change.
 */
export interface AgentDataStore {
  // ── Projects ─────────────────────────────────────────────────────────────
  getProjectSlugs(): string[];
  getSourceIds(): string[];
  getProjectSummaries(options?: { sourceId?: string }): ProjectSummaryData[];
  getSessionSummaries(projectSlug: string, options?: { sourceId?: string }): SessionSummaryData[];

  // ── Messages ─────────────────────────────────────────────────────────────
  getSessionMessages(
    slug: string,
    sessionId: string,
    limit: number,
    offset: number,
    options?: { sourceId?: string },
  ): { messages: unknown[]; total: number; offset: number; hasMore: boolean };
  getSessionTimelineFacets(slug: string, sessionId: string, options?: { sourceId?: string }): TimelineFacets;
  getSessionTimeline(slug: string, sessionId: string, request?: TimelinePageRequest): TimelinePage;

  // ── Subagents ────────────────────────────────────────────────────────────
  getSessionSubagents(
    slug: string,
    sessionId: string,
    options?: { sourceId?: string; includeNested?: boolean },
  ): ReturnType<QueryService['getSessionSubagents']>;
  getSessionWorkflows(slug: string, sessionId: string): ReturnType<QueryService['getSessionWorkflows']>;
  getWorkflowSubagents(
    slug: string,
    sessionId: string,
    workflowId: string,
    options?: { sourceId?: string },
  ): ReturnType<QueryService['getWorkflowSubagents']>;
  getSubagentMessages(
    slug: string,
    sessionId: string,
    agentId: string,
    limit: number,
    offset: number,
    workflowId?: string,
    options?: { sourceId?: string },
  ): { messages: unknown[]; total: number; offset: number; hasMore: boolean };
  getSubagentTimeline(
    slug: string,
    sessionId: string,
    agentId: string,
    request: SubagentTimelinePageRequest,
  ): SubagentTimelinePage;

  // ── Details ──────────────────────────────────────────────────────────────
  getProjectMemory(slug: string, options?: { sourceId?: string }): string | null;
  getSessionTodos(slug: string, sessionId: string): unknown[];
  getSessionPlan(slug: string, sessionId: string): unknown | null;
  getSessionTask(slug: string, sessionId: string): unknown | null;
  getToolResult(slug: string, sessionId: string, toolUseId: string): string | null;

  // ── Search ───────────────────────────────────────────────────────────────
  search(query: SearchQuery): SearchResultSet;

  // ── Stats ────────────────────────────────────────────────────────────────
  getStats(): StoreStats;

  // ── In-memory caches (config + analytics) ────────────────────────────────
  /**
   * Return the last-set `AgentConfig`. Throws if the lifecycle owner
   * hasn't populated the cache yet (i.e. before `initialize()` has
   * finished). Callers that want a lazy-populating fallback should
   * keep using `AgentDataServiceImpl.getConfig()` — that surface still
   * handles re-parsing when the cache is empty.
   */
  getConfig(): AgentConfig;
  /** Same contract as `getConfig()`, for the analytics half. */
  getAnalytics(): AgentAnalytic;
  /** Replace the cached config snapshot. */
  setConfig(config: AgentConfig): void;
  /** Replace the cached analytics snapshot. */
  setAnalytics(analytics: AgentAnalytic): void;
  /** True once both caches have been populated at least once. */
  hasConfig(): boolean;
  hasAnalytics(): boolean;

  // TODO(future): add `open(dbPath)` / `close()` so the store owns
  // connection lifecycle; today `LifecycleOwner` drives
  // `QueryService.open/close` and the store is a pass-through
  // consumer. Folding that in lets the store become the sole owner of
  // the SQLite handle and removes the open-via-LifecycleOwner +
  // subscribe-via-store split. Out of scope for RFC 005.

  // ── Subscriber registry (RFC 005 Phase 3) ──────────────────────────────
  /**
   * Publish a `Change` event to matching subscribers.
   *
   * Stamps the change with a process-lifetime-monotonic `seq` (the
   * store owns the counter — see RFC 005 §Event sequence numbering)
   * and dispatches through the subscriber registry. `change.seq` as
   * passed in is overwritten: producers (today just
   * `IngestService.writeBatch` via the `LiveUpdates` writer loop)
   * should set `seq: 0` as a placeholder; the store assigns the real
   * value here.
   */
  emit(change: Change): void;
  /**
   * Register a listener for changes matching `topic` (undefined for the
   * firehose). Returns a `Dispose` that deregisters the listener.
   *
   * Partial topics widen the scope: `{ kind: 'session' }` matches
   * every session event across every project; adding `slug` narrows
   * to one project, adding `sessionId` narrows to one session. See
   * `live/change-events.ts` for the full topic vocabulary.
   *
   * `options.throttleMs` (optional) gates delivery: at most one
   * listener invocation per `throttleMs`. `options.latest: true`
   * (default when `throttleMs` is set) delivers the most recent event
   * at each boundary; `latest: false` coalesces pending changes into
   * an array.
   */
  subscribe(topic: ChangeTopic | undefined, listener: (e: Change) => void, options?: SubscribeOptionsLatest): Dispose;
  subscribe(
    topic: ChangeTopic | undefined,
    listener: (e: Change[]) => void,
    options: SubscribeOptionsCoalesced,
  ): Dispose;
  /**
   * Last `seq` the store has assigned to an emitted `Change` this
   * process. Monotonically increases with every `emit()` call
   * regardless of whether any listener was registered at the time.
   * Useful for tests + instrumentation; never persisted.
   */
  lastEmittedSeq(): number;

  /**
   * Tear down the underlying subscriber registry — clears pending
   * throttle timers and marks straggling entries as no-ops. Optional
   * because not every store implementation owns a registry; the
   * default `AgentDataStoreImpl` always provides it. Called by
   * `LifecycleOwner.shutdownAsync()` after `liveUpdates.stop()` so no
   * more events can race a half-disposed registry.
   */
  disposeRegistry?(): void;
}

/**
 * Marker type re-exported so lifecycle-layer code can wrap the store's
 * paginated message shape in the existing `Segment<SessionMessage>[]`
 * adapter without importing `query-service.ts` directly. Today this is
 * just an alias for `PaginatedSegmentResult<SessionMessage>`; it exists
 * so later commits have a stable name to reach for.
 */
export type StorePaginatedMessages = PaginatedSegmentResult<SessionMessage>;

// ═══════════════════════════════════════════════════════════════════════════
// IMPLEMENTATION
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Thin delegating implementation. The long-term plan (RFC 005) has the
 * store composing `QueryService` privately and exposing the richer
 * subscriber/registry surface, but C1.1 keeps it a strict pass-through
 * so the refactor can land in small, verifiable steps.
 */
export class AgentDataStoreImpl implements AgentDataStore {
  private readonly queryService: QueryService;

  // In-memory caches moved here from `AgentDataServiceImpl` in C1.2.
  // The lifecycle owner still owns when these are populated (after cold
  // or warm start runs `parser.parseSync({ skipProjects: true, ... })`)
  // but the storage + accessor surface belongs to the store.
  private cachedConfig: AgentConfig | null = null;
  private cachedAnalytics: AgentAnalytic | null = null;

  /**
   * Subscriber registry (RFC 005 C3.1). Delegated to for `emit` +
   * `subscribe`; the store owns the monotonic `seq` counter that
   * stamps every emitted Change (see §Event sequence numbering).
   */
  private readonly registry: SubscriberRegistry;

  /**
   * Process-lifetime-monotonic seq counter. Bumped inside `emit()`
   * regardless of whether the registry has listeners — callers that
   * poll `lastEmittedSeq()` rely on it advancing in lock-step with
   * the number of `emit()` invocations.
   */
  private emittedSeq = 0;

  constructor(queryService: QueryService, registry?: SubscriberRegistry) {
    this.queryService = queryService;
    this.registry = registry ?? createSubscriberRegistry();
  }

  // ── Projects ───────────────────────────────────────────────────────────

  getProjectSlugs(): string[] {
    return this.queryService.getProjectSlugs();
  }

  getSourceIds(): string[] {
    return this.queryService.getSourceIds();
  }

  getProjectSummaries(options?: { sourceId?: string }): ProjectSummaryData[] {
    return this.queryService.getProjectSummaries(options);
  }

  getSessionSummaries(projectSlug: string, options?: { sourceId?: string }): SessionSummaryData[] {
    return this.queryService.getSessionSummaries(projectSlug, options);
  }

  // ── Messages ───────────────────────────────────────────────────────────

  getSessionMessages(
    slug: string,
    sessionId: string,
    limit: number,
    offset: number,
    options?: { sourceId?: string },
  ): { messages: unknown[]; total: number; offset: number; hasMore: boolean } {
    return this.queryService.getSessionMessages(slug, sessionId, limit, offset, options);
  }

  getSessionTimelineFacets(slug: string, sessionId: string, options?: { sourceId?: string }): TimelineFacets {
    return this.queryService.getSessionTimelineFacets(slug, sessionId, options);
  }

  getSessionTimeline(slug: string, sessionId: string, request?: TimelinePageRequest): TimelinePage {
    return this.queryService.getSessionTimeline(slug, sessionId, request);
  }

  // ── Subagents ──────────────────────────────────────────────────────────

  getSessionSubagents(
    slug: string,
    sessionId: string,
    options?: { sourceId?: string },
  ): ReturnType<QueryService['getSessionSubagents']> {
    return this.queryService.getSessionSubagents(slug, sessionId, options);
  }

  getSessionWorkflows(slug: string, sessionId: string): ReturnType<QueryService['getSessionWorkflows']> {
    return this.queryService.getSessionWorkflows(slug, sessionId);
  }

  getWorkflowSubagents(
    slug: string,
    sessionId: string,
    workflowId: string,
    options?: { sourceId?: string },
  ): ReturnType<QueryService['getWorkflowSubagents']> {
    return this.queryService.getWorkflowSubagents(slug, sessionId, workflowId, options);
  }

  getSubagentMessages(
    slug: string,
    sessionId: string,
    agentId: string,
    limit: number,
    offset: number,
    workflowId?: string,
    options?: { sourceId?: string },
  ): { messages: unknown[]; total: number; offset: number; hasMore: boolean } {
    return this.queryService.getSubagentMessages(slug, sessionId, agentId, limit, offset, workflowId, options);
  }

  getSubagentTimeline(
    slug: string,
    sessionId: string,
    agentId: string,
    request: SubagentTimelinePageRequest,
  ): SubagentTimelinePage {
    return this.queryService.getSubagentTimeline(slug, sessionId, agentId, request);
  }

  // ── Details ────────────────────────────────────────────────────────────

  getProjectMemory(slug: string, options?: { sourceId?: string }): string | null {
    return this.queryService.getProjectMemory(slug, options);
  }

  getSessionTodos(slug: string, sessionId: string): unknown[] {
    return this.queryService.getSessionTodos(slug, sessionId);
  }

  getSessionPlan(slug: string, sessionId: string): unknown | null {
    return this.queryService.getSessionPlan(slug, sessionId);
  }

  getSessionTask(slug: string, sessionId: string): unknown | null {
    return this.queryService.getSessionTask(slug, sessionId);
  }

  getToolResult(slug: string, sessionId: string, toolUseId: string): string | null {
    return this.queryService.getToolResult(slug, sessionId, toolUseId);
  }

  // ── Search ─────────────────────────────────────────────────────────────

  search(query: SearchQuery): SearchResultSet {
    return this.queryService.search(query);
  }

  // ── Stats ──────────────────────────────────────────────────────────────

  getStats(): StoreStats {
    return this.queryService.getStats();
  }

  // ── In-memory caches ───────────────────────────────────────────────────

  getConfig(): AgentConfig {
    if (!this.cachedConfig) {
      throw new Error('AgentDataStore: config not set. The lifecycle owner must call setConfig() during initialize().');
    }
    return this.cachedConfig;
  }

  getAnalytics(): AgentAnalytic {
    if (!this.cachedAnalytics) {
      throw new Error(
        'AgentDataStore: analytics not set. The lifecycle owner must call setAnalytics() during initialize().',
      );
    }
    return this.cachedAnalytics;
  }

  setConfig(config: AgentConfig): void {
    this.cachedConfig = config;
  }

  setAnalytics(analytics: AgentAnalytic): void {
    this.cachedAnalytics = analytics;
  }

  hasConfig(): boolean {
    return this.cachedConfig !== null;
  }

  hasAnalytics(): boolean {
    return this.cachedAnalytics !== null;
  }

  // ── Subscriber registry (RFC 005 C3.1) ───────────────────────────────────

  emit(change: Change): void {
    // Bump the seq counter BEFORE fanout so `lastEmittedSeq()` reflects
    // a valid monotonic value even if there are zero listeners (the
    // early-return case inside the registry). The change object is
    // mutated in place: producers set `seq: 0` as a placeholder and the
    // store stamps the real value here — see RFC 005 §Event sequence
    // numbering for why the store (not the ingest service) owns this.
    const stamped = change as Change & { seq: number };
    stamped.seq = ++this.emittedSeq;
    this.registry.emit(stamped);
  }

  subscribe(
    topic: ChangeTopic | undefined,
    listener: ((e: Change) => void) | ((e: Change[]) => void),
    options?: SubscribeOptions,
  ): Dispose {
    // The public overloads pin the listener shape to whichever arm of
    // SubscribeOptions the caller selected. Internally we forward to the
    // registry — which has the same overload set — via a widened
    // signature so TypeScript accepts both arms through one call site.
    return (
      this.registry.subscribe as (
        t: ChangeTopic | undefined,
        l: ((e: Change) => void) | ((e: Change[]) => void),
        o?: SubscribeOptions,
      ) => Dispose
    )(topic, listener, options);
  }

  lastEmittedSeq(): number {
    return this.emittedSeq;
  }

  /**
   * Tear down the subscriber registry. Declared optional on
   * `AgentDataStore` so alternative implementations without a
   * registry can omit it; `LifecycleOwner.shutdownAsync()` calls it
   * after stopping the live pipeline.
   */
  disposeRegistry(): void {
    this.registry.dispose();
  }
}

// ═══════════════════════════════════════════════════════════════════════════
// FACTORY
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Construct a store backed by the given `QueryService`. The
 * `QueryService` must already be opened against the target SQLite
 * database — the store does not manage the connection lifecycle.
 *
 * Pass a custom `SubscriberRegistry` via `options.registry` for tests
 * that want to spy on listener errors; otherwise pass `options.errorSink`
 * to route subscriber listener errors through the unified
 * `ErrorSink` (RFC 005) — `create.ts` does this so all live
 * components share one sink.
 */
export function createAgentDataStore(
  queryService: QueryService,
  options?: { registry?: SubscriberRegistry; errorSink?: ErrorSink },
): AgentDataStore {
  if (options?.registry) {
    return new AgentDataStoreImpl(queryService, options.registry);
  }
  // Wrap the unified sink into the registry's
  // `onListenerError(err, change)` shape — the registry needs the
  // offending Change for context, but routes through the same sink
  // so observers see "everything logged the same way".
  const sink = options?.errorSink;
  const registry = createSubscriberRegistry(
    sink
      ? {
          onListenerError: (err, change) => {
            const e = err instanceof Error ? err : new Error(String(err));
            sink.error(e, { component: 'SubscriberRegistry', changeType: change.type });
          },
        }
      : undefined,
  );
  return new AgentDataStoreImpl(queryService, registry);
}
