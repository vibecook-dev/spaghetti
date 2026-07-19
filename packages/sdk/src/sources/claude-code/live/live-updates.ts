/**
 * Claude Code live-updates orchestrator (RFC 005 C2.7).
 *
 * Product-owned under `sources/claude-code/live/`. Composes watcher,
 * checkpoints, coalescing queue, incremental parser, and writeBatch for
 * the `~/.claude` layout (projects / todos / tasks / file-history / plans /
 * settings). Other agents implement {@link LiveWatch} separately.
 *
 * Flow:
 *   watcher → classify → queue → parseFileDelta → writeBatch → store.emit
 *
 * See `docs/LIVE-UPDATES-DESIGN.md`.
 */

import * as path from 'node:path';
import * as os from 'node:os';
import { createHash } from 'node:crypto';

import type { FileService } from '../../../io/file-service.js';
import type { SqliteService } from '../../../io/sqlite-service.js';
import type { ErrorSink } from '../../../io/error-sink.js';
import { errorSinkFromCallback } from '../../../io/error-sink.js';
import type { IngestService } from '../../../data/ingest-service.js';
import type { AgentDataStore } from '../../../data/agent-data-store.js';
import type { ChangeTopic, Dispose } from '../../../live/change-events.js';
import { createIdleMaintenance, type IdleMaintenance } from '../../../data/idle-maintenance.js';

import { createCheckpointStore, type Checkpoint, type CheckpointStore } from './checkpoints.js';
import { createCoalescingQueue, type CoalescingQueue, type QueuedReason } from './coalescing-queue.js';
import {
  createIncrementalParser,
  type IncrementalParser,
  type ParsedRow,
  type ParsedRowCategory,
} from './incremental-parser.js';
import { classify, type Category, type RouteResult } from '../../../live/router.js';
import type { LiveWatch } from '../../../live/live-watch.js';
import { createChokidarWatcher, createResilientWatcher, type Watcher, type WatchEvent } from '../../../live/watcher.js';
import { createSettingsHandler, type SettingsHandler } from './settings-handler.js';
import { createScopeAttacher, topicToScopes, type ScopeAttacher } from './scope-attacher.js';

// ═══════════════════════════════════════════════════════════════════════════
// PUBLIC TYPES
// ═══════════════════════════════════════════════════════════════════════════

export interface ClaudeCodeLiveUpdatesOptions {
  /** Claude Code data root (e.g. `~/.claude`). */
  rootDir?: string;
  /**
   * @deprecated Use {@link rootDir}.
   */
  claudeDir?: string;
  /**
   * Path→category classifier for this agent source. Injected from
   * `AgentSource.classify`. Defaults to Claude Code classify bound to rootDir.
   */
  classify?: (absPath: string) => RouteResult;
  /**
   * Absolute path to the checkpoint JSON. Defaults to
   * `<dbPath>.live-state.json` next to the SQLite cache (or a tmpdir
   * path when no dbPath is configured). Never defaults inside
   * `rootDir` — the watched source tree is read-only.
   */
  stateFilePath?: string;
  /** Time-window batching window, ms. Default 75. */
  batchWindowMs?: number;
  /** Hard cap on rows per drain. Default 200. */
  maxBatchRows?: number;
  /** Trailing-edge debounce (append only), ms. Default 30. */
  debounceMs?: number;
  /** Trailing-edge hard flush (append only), ms. Default 200. */
  hardFlushMs?: number;
  /** Saturation detection threshold on the queue, ms. Default 5000. */
  saturationThresholdMs?: number;
  /**
   * Observed error sink. Errors from the watcher (e.g. "directory missing")
   * and from `writeBatch` (e.g. transient SQLite busy) land here; the
   * pipeline keeps running. When unset, errors are swallowed silently.
   *
   * Prefer `errorSink` for new code — it's the unified surface every
   * live component uses (RFC 005). `onError` stays as a back-compat
   * shim that's adapted to the same `ErrorSink` interface internally.
   */
  onError?: (err: Error) => void;
  /**
   * Unified error sink (RFC 005). When set, takes precedence over
   * `onError` and is the surface every internal component routes
   * through. The orchestrator stamps `context.component` so a single
   * sink can format messages with the originating subsystem visible.
   */
  errorSink?: ErrorSink;
  /**
   * Watcher factory override — defaults to `createParcelWatcher()`.
   * Tests inject `createChokidarWatcher()` or a custom `Watcher` stub.
   */
  watcherFactory?: () => Watcher;
  /**
   * Force the chokidar fallback without touching env vars. Ignored when
   * `watcherFactory` is set.
   */
  useChokidarFallback?: boolean;
  /**
   * Knobs for the idle-window WAL + FTS5 maintenance loop (RFC 005
   * C5.1). Only consulted when `deps.sqlite` + `deps.dbPath` are set;
   * otherwise idle maintenance is disabled regardless.
   */
  idleMaintenance?: {
    idleMs?: number;
    walCheckpointThresholdBytes?: number;
    ftsMergeChunk?: number;
    checkIntervalMs?: number;
  };
}

/**
 * Claude Code's live pipeline — the rich {@link LiveWatch} implementation
 * (multi-scope watcher + coalescing queue). `prewarm` / `isSaturated` are
 * Claude-specific and required here; the general `LiveWatch` keeps them optional.
 */
export interface ClaudeCodeLiveUpdates extends LiveWatch {
  readonly sourceId: 'claude-code';
  /**
   * Load checkpoints + spawn the writer loop. As of C3.2 this does NOT
   * attach any filesystem watchers — attachment is driven on demand by
   * `prewarm()` / consumer subscriptions hitting `api.live.onChange`.
   * A consumer that calls neither pays zero watcher overhead.
   */
  start(): Promise<void>;
  stop(): Promise<void>;
  isRunning(): boolean;
  isSaturated(): boolean;

  /**
   * Register interest in a scope. The underlying `~/.claude/` subtree
   * is watched for as long as at least one `prewarm` or subscription
   * holds a ref on it. The returned `Dispose` drops that ref; when the
   * last ref goes away, the watcher detaches.
   *
   * Topics whose category is not yet live-wired in Phase 2/3 (e.g.
   * `task`, `file-history`, `plan`, `settings`) register the ref but
   * defer actual watcher attachment to Phase 5 — `prewarm` still
   * returns a valid Dispose so callers can wire up future-proof
   * subscriptions today.
   */
  prewarm(topic: ChangeTopic): Dispose;
}

/**
 * Dependencies the orchestrator composes. Passed in so tests can wire
 * up everything with a shared SQLite connection.
 *
 * `sqlite` + `dbPath` are optional but land together: when both are
 * set, `ClaudeCodeLiveUpdates` constructs an `IdleMaintenance` (RFC 005 C5.1)
 * and ticks it across writer-loop activity. Omitting either disables
 * the idle maintenance loop cleanly — useful for tests that don't
 * care about WAL reclamation and for the engine=rs path where the
 * writer runs inside the native addon.
 */
export interface ClaudeCodeLiveUpdatesDeps {
  fileService: FileService;
  ingestService: IngestService;
  store: AgentDataStore;
  /** Shared SqliteService for idle WAL + FTS maintenance (RFC 005 C5.1). */
  sqlite?: SqliteService;
  /** Absolute DB path — used to stat the `-wal` sidecar during idle ticks. */
  dbPath?: string;
}

// ═══════════════════════════════════════════════════════════════════════════
// DEFAULTS
// ═══════════════════════════════════════════════════════════════════════════

const DEFAULT_BATCH_WINDOW_MS = 75;
const DEFAULT_MAX_BATCH_ROWS = 200;
const DEFAULT_DEBOUNCE_MS = 30;
const DEFAULT_HARD_FLUSH_MS = 200;
const DEFAULT_SATURATION_THRESHOLD_MS = 5000;

/**
 * Hard-ignore globs handed to the watcher. Mirrors `HARD_IGNORE_SEGMENTS`
 * + `HARD_IGNORE_SUFFIXES` in `router.ts` — the router is the authority
 * but the watcher-side ignores let parcel drop traffic at the source
 * (cheaper than classify-then-reject). The two layers are belt-and-
 * braces on purpose: anything that slips through still hits
 * `classify()` and returns `{ category: 'ignored' }`.
 */
const WATCHER_IGNORE_GLOBS = [
  '**/debug/**',
  '**/telemetry/**',
  '**/paste-cache/**',
  '**/session-env/**',
  '**/*.tmp',
  '**/.DS_Store',
  // Legacy checkpoint artifact (pre-2026-07 versions wrote it into
  // rootDir; the default now lives next to the DB). Belt-and-braces so
  // a stale copy in a watched tree never generates traffic.
  '**/.spaghetti-live-state.json',
];

// ═══════════════════════════════════════════════════════════════════════════
// INTERNAL HELPERS
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Map a router `Category` onto the parser's `ParsedRowCategory`. The
 * two enums match for every live category except `session` (router
 * says "session", parser says "message"), and the router's
 * `settings` / `settings_local` buckets are handled by the settings
 * handler outside the writer loop.
 *
 * Returns `null` for categories the writer-loop pipeline does not
 * handle directly.
 */
/**
 * Canonical "one row per sessionId" path for task events. Every
 * watcher event inside `tasks/<sid>/` (`.lock`, `.highwatermark`,
 * numbered `N.json`) normalizes onto this synthetic filename so the
 * CoalescingQueue's path-dedup collapses a burst of rapid edits into
 * a single queued entry per debounce window. The parser reads the
 * whole task dir on any event under it, so losing the per-file
 * identity here is harmless — `parseTaskDelta` rebuilds the
 * `TaskEntry` from disk regardless of which file woke it.
 *
 * The filename must satisfy the router's `^tasks/<sid>/[^/]+$` shape
 * so `classify()` still returns `{ category: 'task', sessionId }`
 * downstream. `.coalesced` is a literal name (no `.tmp` / `.DS_Store`
 * suffix) so the hard-ignore list doesn't accidentally eat it.
 */
export const TASK_COALESCE_FILENAME = '.coalesced';

/**
 * Map a watcher event path to the path that actually gets enqueued.
 * For tasks this collapses every file under `tasks/<sid>/` onto a
 * single per-session coalesce point; for every other category it's
 * the identity mapping.
 *
 * Pure helper at module scope so unit tests can exercise it without
 * spinning up a full ClaudeCodeLiveUpdates instance.
 */
export function coalescePath(evtPath: string, route: RouteResult, rootDir: string): string {
  if (route.category === 'task' && route.sessionId) {
    return path.join(rootDir, 'tasks', route.sessionId, TASK_COALESCE_FILENAME);
  }
  return evtPath;
}

function routerCategoryToParserCategory(category: Category): ParsedRowCategory | null {
  switch (category) {
    case 'session':
      return 'message';
    case 'session_index':
      return 'session_index';
    case 'subagent':
      return 'subagent';
    case 'tool_result':
      return 'tool_result';
    case 'project_memory':
      return 'project_memory';
    case 'file_history':
      return 'file_history';
    case 'todo':
      return 'todo';
    case 'task':
      return 'task';
    case 'plan':
      return 'plan';
    case 'settings':
    case 'settings_local':
    case 'ignored':
      return null;
  }
}

// ═══════════════════════════════════════════════════════════════════════════
// FACTORY
// ═══════════════════════════════════════════════════════════════════════════

export function createClaudeCodeLiveUpdates(
  deps: ClaudeCodeLiveUpdatesDeps,
  options: ClaudeCodeLiveUpdatesOptions,
): ClaudeCodeLiveUpdates {
  const { fileService, ingestService, store } = deps;

  // ── resolve options with defaults ──────────────────────────────────────
  const resolvedRoot = options.rootDir ?? options.claudeDir;
  if (!resolvedRoot) {
    throw new Error('ClaudeCodeLiveUpdatesOptions requires rootDir (or deprecated claudeDir)');
  }
  const rootDir: string = resolvedRoot;
  // Source-provided classifier, or the built-in Claude Code one bound to root.
  const classifyPath = options.classify ?? ((absPath: string) => classify(absPath, rootDir));
  // Checkpoint state must NEVER live inside rootDir: the source tree is a
  // read-only data plane (writing there also polluted test fixtures and
  // fed the watcher its own artifact). Anchor it to the SQLite cache path
  // — per-instance and naturally test-isolated — falling back to a
  // rootDir-keyed tmpdir file when no dbPath is configured.
  const stateFilePath =
    options.stateFilePath ??
    (deps.dbPath
      ? `${deps.dbPath}.live-state.json`
      : path.join(
          os.tmpdir(),
          `spaghetti-live-state-${createHash('sha1').update(rootDir).digest('hex').slice(0, 12)}.json`,
        ));
  const batchWindowMs = options.batchWindowMs ?? DEFAULT_BATCH_WINDOW_MS;
  const maxBatchRows = options.maxBatchRows ?? DEFAULT_MAX_BATCH_ROWS;
  const debounceMs = options.debounceMs ?? DEFAULT_DEBOUNCE_MS;
  const hardFlushMs = options.hardFlushMs ?? DEFAULT_HARD_FLUSH_MS;
  const saturationThresholdMs = options.saturationThresholdMs ?? DEFAULT_SATURATION_THRESHOLD_MS;
  // Unify on a single ErrorSink (RFC 005). `errorSink` wins; `onError`
  // is adapted into one for back-compat. When neither is set, errors
  // are swallowed silently — matches the pre-RFC behavior.
  const errorSink: ErrorSink =
    options.errorSink ?? (options.onError ? errorSinkFromCallback(options.onError) : { error: () => {} });
  /**
   * Component-stamped sink for the orchestrator's own error sites.
   * Sub-modules (settings handler, scope attacher) carry their own
   * stamped wrappers so a single sink can route by component name.
   */
  const liveErrorSink: ErrorSink = {
    error: (err, ctx) => errorSink.error(err, { component: ctx?.component ?? 'ClaudeCodeLiveUpdates', ...ctx }),
  };
  /**
   * Legacy `(err: Error) => void` shape consumed by sub-modules that
   * still take the callback form. They each get a component-stamped
   * variant so the unified sink sees `context.component` regardless
   * of which subsystem surfaced the error.
   */
  const onError = (err: Error): void => liveErrorSink.error(err);
  const settingsErrorCb = (err: Error): void => errorSink.error(err, { component: 'ClaudeCodeLiveUpdates.settings' });
  const scopeErrorCb = (err: Error): void => errorSink.error(err, { component: 'ClaudeCodeLiveUpdates.scopeAttacher' });

  // ── internal state ─────────────────────────────────────────────────────

  let running = false;
  let checkpoints: CheckpointStore | null = null;
  let queue: CoalescingQueue | null = null;
  let parser: IncrementalParser | null = null;
  let watcher: Watcher | null = null;
  let writerLoopDone: Promise<void> | null = null;
  /**
   * Idle-window WAL + FTS5 maintenance handle (RFC 005 C5.1). Built
   * only when `deps.sqlite` + `deps.dbPath` are both provided; its
   * lifetime matches the running state of the pipeline.
   */
  let idleMaintenance: IdleMaintenance | null = null;

  // ── extracted modules ──────────────────────────────────────────────────

  const settingsHandler: SettingsHandler = createSettingsHandler({
    fileService,
    getStore: () => store,
    onError: settingsErrorCb,
    isRunning: () => running,
  });

  const scopeAttacher: ScopeAttacher = createScopeAttacher({
    rootDir,
    getWatcher: () => watcher,
    isRunning: () => running,
    onEvents: (events) => handleWatchEvents(events),
    watcherIgnoreGlobs: WATCHER_IGNORE_GLOBS,
    onError: scopeErrorCb,
  });

  /**
   * Per-path trailing-edge debounce for `append` events. On every
   * update fsevent we reset the debounce timer; a separate hard-flush
   * timer is set on the first update and fires regardless of further
   * activity so a steady stream of updates can't starve the enqueue.
   */
  type DebounceState = {
    debounceTimer: ReturnType<typeof setTimeout> | null;
    hardFlushTimer: ReturnType<typeof setTimeout> | null;
  };
  const debounceByPath = new Map<string, DebounceState>();

  /**
   * Per-session `msg_index` high-water mark, so successive `update`
   * events on the same session JSONL continue the monotonic row index
   * the cold-start ingest established. Keyed by absolute file path so
   * two sessions in different projects don't collide.
   */
  const nextMsgIndexByPath = new Map<string, number>();

  // ── helpers ────────────────────────────────────────────────────────────

  function clearDebounceForPath(p: string): void {
    const state = debounceByPath.get(p);
    if (!state) return;
    if (state.debounceTimer !== null) clearTimeout(state.debounceTimer);
    if (state.hardFlushTimer !== null) clearTimeout(state.hardFlushTimer);
    debounceByPath.delete(p);
  }

  function clearAllDebounces(): void {
    for (const state of debounceByPath.values()) {
      if (state.debounceTimer !== null) clearTimeout(state.debounceTimer);
      if (state.hardFlushTimer !== null) clearTimeout(state.hardFlushTimer);
    }
    debounceByPath.clear();
    settingsHandler.stop();
  }

  function enqueueNow(evtPath: string, reason: QueuedReason): void {
    if (!queue) return;
    clearDebounceForPath(evtPath);
    queue.enqueue({ path: evtPath, reason });
  }

  /**
   * Debounce an `append` event: restart the trailing timer on every
   * call; set a one-shot hard-flush timer on the first call so
   * continuous activity can't starve enqueue forever.
   */
  function scheduleDebouncedAppend(evtPath: string): void {
    if (!queue) return;
    let state = debounceByPath.get(evtPath);
    if (!state) {
      state = { debounceTimer: null, hardFlushTimer: null };
      debounceByPath.set(evtPath, state);
    }

    if (state.debounceTimer !== null) clearTimeout(state.debounceTimer);
    state.debounceTimer = setTimeout(() => {
      const s = debounceByPath.get(evtPath);
      if (!s) return;
      if (s.hardFlushTimer !== null) clearTimeout(s.hardFlushTimer);
      debounceByPath.delete(evtPath);
      if (queue) queue.enqueue({ path: evtPath, reason: 'append' });
    }, debounceMs);
    // Let the debounce timer keep the loop alive so tests awaiting an
    // eventual SQLite write don't return early. Watcher sub is already
    // pinning the loop anyway; this is a no-op in practice.

    if (state.hardFlushTimer === null) {
      state.hardFlushTimer = setTimeout(() => {
        const s = debounceByPath.get(evtPath);
        if (!s) return;
        if (s.debounceTimer !== null) clearTimeout(s.debounceTimer);
        debounceByPath.delete(evtPath);
        if (queue) queue.enqueue({ path: evtPath, reason: 'append' });
      }, hardFlushMs);
    }
  }

  // ── watcher event dispatch ─────────────────────────────────────────────

  function handleWatchEvents(events: WatchEvent[]): void {
    if (!running) return;
    for (const event of events) {
      const route: RouteResult = classifyPath(event.path);
      if (route.category === 'ignored') continue;

      // Settings (RFC 005 C5.5) bypass the SQLite path entirely: they
      // re-parse into an in-memory AgentConfig and emit
      // `settings.changed`. The handler owns the 150 ms trailing
      // coalescer that absorbs the delete+create flicker common to
      // atomic-rename saves.
      if (route.category === 'settings' || route.category === 'settings_local') {
        settingsHandler.handle(event.path, route.category);
        continue;
      }

      const enqueuePath = coalescePath(event.path, route, rootDir);

      // create + delete are rare; enqueue immediately so priority
      // collapse can't strand them behind a debounced append.
      if (event.type === 'delete') {
        enqueueNow(enqueuePath, 'delete');
      } else if (event.type === 'create') {
        enqueueNow(enqueuePath, 'rewrite');
      } else {
        // 'update' → trailing-edge debounced append.
        scheduleDebouncedAppend(enqueuePath);
      }
    }
  }

  // ── writer loop ────────────────────────────────────────────────────────

  /**
   * Result of parsing one queued event. The checkpoint + msg-index
   * advances are returned as pending values so the writer loop can
   * apply them only after `writeBatch` commits successfully — per
   * RFC 005 §4 ("Checkpoints not advanced until write succeeds").
   */
  interface ProcessedEvent {
    rows: ParsedRow[];
    /** Null for delete events; otherwise the new checkpoint to persist on success. */
    pendingCheckpoint: { path: string; checkpoint: Checkpoint } | null;
    /** Set for message-category events; msg_index high-water to advance on success. */
    pendingMsgIndex: { path: string; next: number } | null;
    /** For delete events: drop this path's state (no-op if already absent). */
    dropPath: string | null;
  }

  async function processEvent(evtPath: string, reason: QueuedReason): Promise<ProcessedEvent> {
    const empty: ProcessedEvent = { rows: [], pendingCheckpoint: null, pendingMsgIndex: null, dropPath: null };
    if (!parser || !checkpoints) return empty;

    const route = classifyPath(evtPath);
    const parserCategory = routerCategoryToParserCategory(route.category);
    if (!parserCategory) return empty;

    // Delete is side-effect-only; surface via dropPath so the writer
    // loop can tear state down after any in-flight batch settles.
    // TODO(future): promote this to a `session.rewritten` emit; today
    // it's purely local cleanup.
    if (reason === 'delete') {
      return { ...empty, dropPath: evtPath };
    }

    const priorCheckpoint: Checkpoint | undefined = checkpoints.get(evtPath);
    // First event for a file in this process: the in-memory high-water
    // is empty, but a persisted checkpoint may resume the tail mid-file.
    // Seed from the DB (MAX(msg_index) + 1) or the appended rows would
    // be written at index 0 and overwrite the head of the session.
    const startMsgIndex =
      nextMsgIndexByPath.get(evtPath) ??
      (parserCategory === 'message' && route.sessionId ? ingestService.getNextMessageIndex(route.sessionId) : 0);

    const parseResult = await parser.parseFileDelta({
      path: evtPath,
      category: parserCategory,
      slug: route.slug,
      sessionId: route.sessionId,
      checkpoint: priorCheckpoint,
      startMsgIndex,
      rootDir,
      // Only set for nested workflow subagent transcripts; groups the
      // live-ingested transcript under its run (undefined → '').
      workflowId: route.workflowId,
    });

    // Defer every state advance until writeBatch commits — a transient
    // SQLite failure must not leave the checkpoint pointing past bytes
    // that never made it to disk (which would permanently skip them on
    // the next tail).
    const pendingMsgIndex =
      parserCategory === 'message'
        ? {
            path: evtPath,
            next: parseResult.rewrite ? parseResult.rows.length : startMsgIndex + parseResult.rows.length,
          }
        : null;

    return {
      rows: parseResult.rows,
      pendingCheckpoint: { path: evtPath, checkpoint: parseResult.newCheckpoint },
      pendingMsgIndex,
      dropPath: null,
    };
  }

  /**
   * Apply the checkpoint + msg-index advances collected from a batch.
   * Called only after the batch's `writeBatch` has committed, so we
   * never advance past bytes that failed to reach SQLite.
   */
  function applyPending(processed: ProcessedEvent[]): void {
    if (!checkpoints) return;
    let touched = false;
    for (const p of processed) {
      if (p.dropPath !== null) {
        checkpoints.delete(p.dropPath);
        nextMsgIndexByPath.delete(p.dropPath);
        touched = true;
      }
      if (p.pendingCheckpoint !== null) {
        checkpoints.set(p.pendingCheckpoint.path, p.pendingCheckpoint.checkpoint);
        touched = true;
      }
      if (p.pendingMsgIndex !== null) {
        nextMsgIndexByPath.set(p.pendingMsgIndex.path, p.pendingMsgIndex.next);
      }
    }
    if (touched) checkpoints.scheduleFlush();
  }

  async function writerLoop(): Promise<void> {
    if (!queue || !parser || !ingestService) return;

    while (running) {
      let batch: Awaited<ReturnType<CoalescingQueue['drain']>>;
      try {
        batch = await queue.drain(maxBatchRows, batchWindowMs);
      } catch (err) {
        onError(err instanceof Error ? err : new Error(String(err)));
        continue;
      }

      if (!running) break;
      if (batch.length === 0) continue;

      // Parse each queued event into parsed rows. Parser errors are
      // reported via onError and the event is skipped — the pipeline
      // must not crash on a malformed JSONL. All checkpoint/state
      // advances are DEFERRED onto the processed list and applied only
      // after writeBatch commits (see applyPending below).
      const processed: ProcessedEvent[] = [];
      const rows: ParsedRow[] = [];
      for (const evt of batch) {
        try {
          const p = await processEvent(evt.path, evt.reason);
          processed.push(p);
          for (const r of p.rows) rows.push(r);
        } catch (err) {
          onError(err instanceof Error ? err : new Error(String(err)));
        }
      }

      // Delete-only batches (no row work) still need their dropPath
      // side-effects applied; writeBatch won't be called in that case.
      if (rows.length === 0) {
        applyPending(processed);
        continue;
      }

      try {
        const result = await ingestService.writeBatch(rows);
        // Success — only now is it safe to advance the checkpoint and
        // msg-index bookkeeping. On failure we fall through to the
        // catch below and the advances are discarded; the next watcher
        // event will re-parse from the prior checkpoint.
        applyPending(processed);
        idleMaintenance?.noteActivity();
        for (const change of result.changes) {
          store.emit(change);
        }
      } catch (err) {
        onError(err instanceof Error ? err : new Error(String(err)));
        // State advances intentionally discarded — next tail will
        // re-read the same bytes and retry the write. Upserts on
        // (session_id, msg_index) make this idempotent for messages;
        // all other categories re-read whole files on rewrite anyway.
      }
    }
  }

  // ── public surface ─────────────────────────────────────────────────────

  return {
    sourceId: 'claude-code',
    async start(): Promise<void> {
      if (running) return;

      // 1. Checkpoints (never throw — corrupt/missing state → empty).
      checkpoints = createCheckpointStore({ filePath: stateFilePath });
      await checkpoints.load();

      // 2. Queue.
      queue = createCoalescingQueue({ saturationThresholdMs });

      // 3. Parser (holds a ref to fileService).
      parser = createIncrementalParser({ fileService });

      // 4. Watcher factory.
      if (options.watcherFactory) {
        watcher = options.watcherFactory();
      } else if (options.useChokidarFallback) {
        watcher = createChokidarWatcher();
      } else {
        watcher = createResilientWatcher({
          shouldInclude: (filePath) => classifyPath(filePath).category !== 'ignored',
          // projects/<slug>/<session>.jsonl: catch new parent sessions on the
          // fast lane without repeatedly walking historical subagent trees.
          quickDiscoveryDepth: 1,
          onError: (error) => errorSink.error(error, { component: 'PollingWatcher' }),
          onPrimaryUnavailable: (error) => errorSink.error(error, { component: 'ParcelWatcher', fallback: 'polling' }),
        });
      }

      // 5. Flip running BEFORE spawning the writer loop so the event
      //    handler sees `running === true` on the very first batch.
      running = true;

      // 6. Start the writer loop (detached — stored for `stop()` await).
      writerLoopDone = writerLoop();

      // 6a. Idle maintenance (RFC 005 C5.1). Only wired up when the
      //     caller supplied a SqliteService + dbPath — otherwise the
      //     handle stays null and noteActivity()/stop() paths no-op.
      if (deps.sqlite && deps.dbPath) {
        idleMaintenance = createIdleMaintenance(
          { sqlite: deps.sqlite, dbPath: deps.dbPath },
          {
            ...(options.idleMaintenance?.idleMs !== undefined && {
              idleMs: options.idleMaintenance.idleMs,
            }),
            ...(options.idleMaintenance?.walCheckpointThresholdBytes !== undefined && {
              walCheckpointThresholdBytes: options.idleMaintenance.walCheckpointThresholdBytes,
            }),
            ...(options.idleMaintenance?.ftsMergeChunk !== undefined && {
              ftsMergeChunk: options.idleMaintenance.ftsMergeChunk,
            }),
            ...(options.idleMaintenance?.checkIntervalMs !== undefined && {
              checkIntervalMs: options.idleMaintenance.checkIntervalMs,
            }),
            onError: (err) => errorSink.error(err, { component: 'IdleMaintenance' }),
          },
        );
        idleMaintenance.start();
      }

      // C3.2: watchers are NOT attached here. `prewarm()` or a
      //       subscription landing via `api.live.onChange` will
      //       ref-bump the scope and trigger attach. A process that
      //       never calls either pays zero watcher/FS-event overhead.
      //
      // If any scopes were prewarm()'d before start() resolved
      // (exotic, but legal — we accept bumps at any time), bring them
      // online now.
      scopeAttacher.attachPending();
    },

    async stop(): Promise<void> {
      if (!running) return;
      running = false;

      // Halt idle maintenance first so a tick racing stop() can't
      // try to run SQL against a DB that's about to close.
      if (idleMaintenance) {
        idleMaintenance.stop();
        idleMaintenance = null;
      }

      // Detach every scope (awaits any in-flight attach so unsub
      // handles land before we drop the watcher reference).
      await scopeAttacher.detachAll();
      // Leave the ref-count entries in place — callers can re-acquire
      // after a re-start. Just drop the watcher handle.
      watcher = null;

      // Wake the drain loop.
      if (queue) queue.stop();

      // Wait for the writer loop to exit so in-flight writes complete.
      if (writerLoopDone) {
        try {
          await writerLoopDone;
        } catch {
          /* writer errors already surfaced via onError */
        }
        writerLoopDone = null;
      }

      // Force a final checkpoint flush.
      if (checkpoints) {
        try {
          await checkpoints.stop();
        } catch {
          /* best-effort */
        }
        checkpoints = null;
      }

      queue = null;
      parser = null;

      clearAllDebounces();
      nextMsgIndexByPath.clear();
    },

    isRunning(): boolean {
      return running;
    },

    isSaturated(): boolean {
      return queue?.saturated() ?? false;
    },

    prewarm(topic: ChangeTopic): Dispose {
      const acquired = topicToScopes(topic);
      for (const scope of acquired) scopeAttacher.acquire(scope);
      let disposed = false;
      return () => {
        if (disposed) return;
        disposed = true;
        for (const scope of acquired) scopeAttacher.release(scope);
      };
    },
  };
}

// ── Backward-compat aliases (old LiveUpdates names) ────────────────────────
/** @deprecated Use {@link createClaudeCodeLiveUpdates}. */
export const createLiveUpdates = createClaudeCodeLiveUpdates;
/** @deprecated Use {@link ClaudeCodeLiveUpdates}. */
export type LiveUpdates = ClaudeCodeLiveUpdates;
/** @deprecated Use {@link ClaudeCodeLiveUpdatesOptions}. */
export type LiveUpdatesOptions = ClaudeCodeLiveUpdatesOptions;
/** @deprecated Use {@link ClaudeCodeLiveUpdatesDeps}. */
export type LiveUpdatesDeps = ClaudeCodeLiveUpdatesDeps;
