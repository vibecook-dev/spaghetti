/**
 * Lifecycle contracts + compat re-exports (generic / shared layer).
 *
 * Product implementations live under `sources/`:
 * - Claude Code → `sources/claude-code/lifecycle-owner.ts`
 * - Codex → `sources/codex/lifecycle-owner.ts`
 *
 * This module keeps the shared interfaces (`LifecycleOwner`,
 * `AgentDataService`, options) and re-exports the Claude owner class so
 * existing imports from `./lifecycle-owner.js` / `agent-data-service`
 * keep working without churn.
 */

import type { EventEmitter } from 'events';
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
import type { IngestEngine } from '../settings.js';
import type {
  SubagentTimelinePage,
  SubagentTimelinePageRequest,
  TimelineFacets,
  TimelinePage,
  TimelinePageRequest,
} from './timeline-query.js';

// Re-export types used by app-service / agent-data-service shim
export {
  type SegmentType,
  type SegmentKey,
  type Segment,
  type SegmentChangeBatch,
  type InitProgress,
  type PaginatedSegmentQuery,
  type PaginatedSegmentResult,
  type SearchQuery,
  type SearchResultSet,
  type StoreStats,
  segmentKey,
  parseSegmentKey,
} from './segment-types.js';

export type { SearchIndexEntry } from './search-indexer.js';
export { type SearchIndexer, createSearchIndexer } from './search-indexer.js';
export type { SegmentStore } from './segment-store.js';
export { createSegmentStore } from './segment-store.js';
export type { TokenUsageSummary, SessionSummaryData, ProjectSummaryData } from './summary-types.js';

// Product impl — re-exported for backward-compat import paths
export { ClaudeCodeLifecycleOwner } from '../sources/claude-code/lifecycle-owner.js';

// ═══════════════════════════════════════════════════════════════════════════
// INTERFACE
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Multi-source data service surface (reads + lifecycle).
 * Formerly Claude-branded; the implementation is agent-agnostic.
 */
export interface AgentDataService extends EventEmitter {
  initialize(): Promise<void>;
  shutdown(): void;
  /**
   * Awaitable teardown (RFC 005 C3.4). Stops the live pipeline,
   * drains in-flight writes, disposes the subscriber registry, and
   * closes SQLite. Optional on the interface so external impls that
   * predate C3.4 still type-check; the default `LifecycleOwner`
   * always provides it.
   */
  shutdownAsync?(): Promise<void>;
  /** Force a full cold rebuild — wipes the DB file and re-ingests. */
  rebuildIndex(): Promise<{ durationMs: number }>;
  isReady(): boolean;

  getSegment<T>(key: SegmentKey): Segment<T> | null;
  getSegmentsByType<T>(type: SegmentType): Segment<T>[];
  getSegmentsPaginated<T>(query: PaginatedSegmentQuery): PaginatedSegmentResult<T>;

  getProjectSlugs(): string[];
  getProject(slug: string): Segment<Project> | null;
  getProjectSessions(slug: string): Segment<Session>[];
  getSessionMessages(
    slug: string,
    sessionId: string,
    limit: number,
    offset: number,
    options?: { sourceId?: string },
  ): PaginatedSegmentResult<SessionMessage>;
  getSessionTimelineFacets(slug: string, sessionId: string, options?: { sourceId?: string }): TimelineFacets;
  getSessionTimeline(slug: string, sessionId: string, request?: TimelinePageRequest): TimelinePage;
  getConfig(): AgentConfig;
  getAnalytics(): AgentAnalytic;

  getSourceIds(): string[];
  getProjectSummaries(options?: { sourceId?: string }): ProjectSummaryData[];
  getSessionSummaries(projectSlug: string, options?: { sourceId?: string }): SessionSummaryData[];

  getProjectMemory(slug: string, options?: { sourceId?: string }): string | null;
  getSessionTodos(slug: string, sessionId: string): unknown[];
  getSessionPlan(slug: string, sessionId: string): unknown | null;
  getSessionTask(slug: string, sessionId: string): unknown | null;
  getPersistedToolResult(slug: string, sessionId: string, toolUseId: string): string | null;
  getSessionSubagents(
    slug: string,
    sessionId: string,
    options?: { sourceId?: string; includeNested?: boolean },
  ): ReturnType<AgentDataStore['getSessionSubagents']>;
  getSessionWorkflows(slug: string, sessionId: string): ReturnType<AgentDataStore['getSessionWorkflows']>;
  getWorkflowSubagents(
    slug: string,
    sessionId: string,
    workflowId: string,
    options?: { sourceId?: string },
  ): ReturnType<AgentDataStore['getWorkflowSubagents']>;
  getSubagentMessages(
    slug: string,
    sessionId: string,
    agentId: string,
    limit: number,
    offset: number,
    workflowId?: string,
    options?: { sourceId?: string },
  ): PaginatedSegmentResult<SessionMessage>;
  getSubagentTimeline(
    slug: string,
    sessionId: string,
    agentId: string,
    request: SubagentTimelinePageRequest,
  ): SubagentTimelinePage;

  search(query: SearchQuery): SearchResultSet;
  rebuild(): Promise<void>;
  getStoreStats(): StoreStats;
}

/**
 * Internal accessors the app-service consumes via structural typing.
 *
 * NOT exported from the SDK barrel — `getStore()` returns the
 * `AgentDataStore` (an internal type with the subscriber registry on
 * it) and `getLiveUpdates()` returns the orchestrator (also internal).
 * Exposing either on the public `AgentDataService` interface would leak
 * implementation types into consumer code (audit finding); keeping them on
 * a separate interface that only `LifecycleOwner` implements means
 * `app-service.ts` reaches them via duck-typing without the public surface
 * advertising them.
 */
export interface LifecycleInternal {
  getStore(): AgentDataStore;
  /** This source's live pipeline (RFC 006), or undefined when not live. */
  getLiveWatch(): LiveWatch | undefined;
}

/**
 * The per-source ingest-lifecycle contract (RFC 006 multi-source).
 *
 * A `LifecycleOwner` owns ONE agent source's ingest into the shared store:
 * engine selection (rs/ts), cold-start, warm-start, and (optionally) the live
 * pipeline. It deliberately does NOT own reads — `getProjectSummaries`,
 * `search`, etc. query the shared store, which already unifies every source's
 * rows, so reads are a shared facade, not a per-source concern.
 *
 * ## Multi-source exclusive queue (all agents may use native `rs`)
 *
 * Native bulk ingest switches SQLite to `journal_mode=MEMORY` and must own the
 * file alone. {@link SpaghettiDataService} therefore runs a three-phase protocol
 * so N agents can each use the Rust path without racing better-sqlite3:
 *
 * 1. **`exclusiveIngest()`** × N (serial) — cold/warm; shared better-sqlite3
 *    MUST stay closed. Prefer `native.ingest`. Safe for MEMORY journal.
 * 2. **`attachShared()`** × N — open the shared handle (idempotent) + light
 *    post-steps (config, meta). Do **not** start live.
 * 3. **`startLivePipeline()`** × N — Plane 2 watchers after every source is warm.
 *
 * Solo `initialize()` is the composition of those three phases.
 *
 * Implementations: `ClaudeCodeLifecycleOwner` (`sources/claude-code/`),
 * `CodexLifecycleOwner` (`sources/codex/`).
 */
export interface LifecycleOwner extends LifecycleInternal {
  /** The `AgentSource.id` this owner ingests for (stamped on its rows). */
  readonly sourceId: string;

  /**
   * Phase 1 — exclusive cold/warm. Shared better-sqlite3 must remain **closed**.
   * Prefer native rs when available. May open temporarily for a pure-TS path
   * but must close before returning so the next owner can run native too.
   */
  exclusiveIngest(): Promise<void>;

  /**
   * Phase 2 — shared handle is available. Open it idempotently if needed, then
   * run lightweight attach (config parse, schema meta). Must not start live.
   */
  attachShared(): Promise<void>;

  /**
   * Phase 3 — start Plane 2 after all owners finished attach. No-op when live
   * was not requested for this source.
   */
  startLivePipeline(): Promise<void>;

  /**
   * Optional: close this owner's view of the shared handle (and delete the
   * cache file for a full rebuild). Multi-source wipe runs only on owners that
   * implement file deletion; others only release connections.
   */
  releaseShared?(): void;

  /**
   * Optional: delete the on-disk cache (and WAL). Only one owner should delete
   * the shared file — typically the primary. Called once before a full rebuild.
   */
  wipeCache?(): void;

  /**
   * Optional: absolute path of the shared SQLite cache this owner writes.
   * Used by the multi-source coordinator for preflight health checks.
   */
  getCacheDbPath?(): string;

  /**
   * Solo convenience: exclusiveIngest → attachShared → startLivePipeline.
   * Multi-source coordinators should call the three phases instead so every
   * agent gets a turn at exclusive native access.
   */
  initialize(): Promise<void>;
  shutdown(): void;
  shutdownAsync?(): Promise<void>;
  rebuild(): Promise<void>;
  /** Full cold rebuild of this source's rows (solo). Multi-source uses coordinator. */
  rebuildIndex(): Promise<{ durationMs: number }>;
  /** Owners emit `progress`/`change`/`error`; the coordinator forwards them. */
  on(event: string, listener: (...args: unknown[]) => void): this;
}

// ═══════════════════════════════════════════════════════════════════════════
// OPTIONS
// ═══════════════════════════════════════════════════════════════════════════

export interface AgentDataServiceOptions {
  dbPath?: string;
  /**
   * Primary agent data root (e.g. `~/.claude` for Claude Code).
   * Prefer this over the deprecated {@link claudeDir} alias.
   */
  rootDir?: string;
  /**
   * @deprecated Use {@link rootDir}.
   */
  claudeDir?: string;
  /**
   * Ingest engine to use for this service. When set, takes precedence over
   * the process-wide `SPAG_ENGINE` env var and the persisted
   * `~/.spaghetti/config.json` engine setting — useful for apps that want
   * to carry their own engine preference without touching the shared
   * user-level config.
   */
  engine?: IngestEngine;
  /**
   * Prefer crash-safer bulk SQLite settings (WAL + synchronous=NORMAL)
   * during cold/warm ingest. Default false for CLI one-shots; long-lived
   * surfaces with `{ live: true }` enable this so a kill mid-ingest is
   * less likely to leave a corrupt cache.
   */
  safeBulk?: boolean;
}

/** @deprecated Use {@link AgentDataService}. */
export type ClaudeCodeAgentDataService = AgentDataService;
