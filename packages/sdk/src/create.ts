/**
 * Factory — createSpaghettiService()
 *
 * Wires AgentSource → DurableStore → StaticIngest / LiveDiskIngest → SpaghettiAPI.
 * Lifecycle owners are built via {@link createLifecycleOwnerForSource} (Phase E)
 * so product branches do not live here.
 *
 * See `docs/THREE-PLANE-INGEST-ARCHITECTURE.md` and `docs/PR-PLAN-THREE-PLANE-SHAPE.md`.
 */

import { createFileService } from './io/file-service.js';
import { createSqliteService } from './io/sqlite-service.js';
import { createConsoleErrorSink, type ErrorSink } from './io/error-sink.js';
import { createSpaghettiAppService } from './app-service.js';
import type { AgentDataService, LifecycleOwner } from './data/agent-data-service.js';
import { SpaghettiDataService } from './data/multi-source-service.js';
import { loadNativeAddon } from './native.js';
import { defaultDbPathForEngine, resolveEngine, type IngestEngine } from './settings.js';
import type { SpaghettiAPI } from './api.js';
import { createClaudeCodeSource, type AgentSource } from './sources/index.js';
import { createLifecycleOwnerForSource } from './sources/registry.js';
import { createClaudeCodeLiveDiskIngest } from './sources/claude-code/live/disk-ingest.js';
import { createDurableStore } from './store/durable-store.js';
import { createRuntimeBridge } from './planes/runtime-bridge.js';

export interface SpaghettiServiceOptions {
  /** Override the data service implementation (for testing or custom setups) */
  dataService?: AgentDataService;
  /** Override the default DB path */
  dbPath?: string;
  /**
   * Primary agent data root when using the default Claude Code source
   * (default `~/.claude`). Ignored when {@link source} is provided.
   */
  rootDir?: string;
  /**
   * @deprecated Use {@link rootDir}.
   */
  claudeDir?: string;
  /**
   * Explicit agent source adapter. Defaults to Claude Code using
   * `rootDir` / `claudeDir` (or `~/.claude`) as the root.
   */
  source?: AgentSource;
  /**
   * Additional agent sources to ingest into the SAME store (RFC 006
   * multi-source). Each gets its own `LifecycleOwner`; reads unify across all
   * of them. Opt-in — nothing extra is read unless a source is passed here,
   * e.g. `additionalSources: [createCodexSource({ rootDir: '~/.codex' })]`.
   */
  additionalSources?: AgentSource[];
  /**
   * Pin the ingest engine for this service. When set, takes precedence
   * over the process-wide `SPAG_ENGINE` env var and the persisted
   * `~/.spaghetti/config.json` engine setting — useful when an app wants
   * to carry its own engine preference independent of the shared
   * user-level config.
   */
  engine?: IngestEngine;
  /**
   * Opt in to Plane 2 (live disk ingest). When `true`, `initialize()`
   * starts the filesystem watcher pipeline on the agent source root
   * and exposes `api.live`.
   *
   * Defaults to `false`: CLI one-shots and cold-start-only consumers
   * pay zero watcher/queue/parser overhead.
   *
   * When `true` (and {@link safeBulk} is not overridden), bulk cold/warm
   * ingest uses crash-safer SQLite settings (WAL + `synchronous=NORMAL`)
   * so a kill mid-ingest is less likely to leave a corrupt cache.
   */
  live?: boolean;
  /**
   * Prefer crash-safer bulk SQLite settings during cold/warm ingest.
   * Defaults to `true` when {@link live} is true, otherwise `false`.
   */
  safeBulk?: boolean;
  /**
   * Override the unified error sink (RFC 005). Every internal live
   * component routes through this sink. Defaults to a console.warn
   * variant prefixed with `[spaghetti-sdk]`.
   */
  errorSink?: ErrorSink;
}

/**
 * Create a fully wired SpaghettiAPI instance.
 *
 * Usage:
 *   import { createSpaghettiService } from '@vibecook/spaghetti-sdk';
 *
 *   const spaghetti = createSpaghettiService();
 *   await spaghetti.initialize();
 *   const projects = spaghetti.getProjectList();
 */
export function createSpaghettiService(options?: SpaghettiServiceOptions): SpaghettiAPI {
  const errorSink = options?.errorSink ?? createConsoleErrorSink('[spaghetti-sdk]');

  if (options?.dataService) {
    return createSpaghettiAppService(options.dataService, errorSink);
  }

  // ── Agent sources (primary + optional additional) ──────────────────────
  const primary =
    options?.source ??
    createClaudeCodeSource({
      rootDir: options?.rootDir ?? options?.claudeDir,
    });
  const allSources: AgentSource[] = [primary, ...(options?.additionalSources ?? [])];

  // ── Shared I/O ─────────────────────────────────────────────────────────
  const fileService = createFileService();
  // CRITICAL: Share a single SqliteService between query and ingest
  // to prevent SQLITE_BUSY. Two connections to the same DB conflict
  // when ingest holds an open transaction.
  const sharedSqlite = createSqliteService();

  const resolvedEngine = options?.engine ?? resolveEngine();
  const nativeAddon = resolvedEngine === 'rs' ? loadNativeAddon() : null;
  const live = options?.live ?? false;

  // ── DurableStore (SQLite + FTS) ────────────────────────────────────────
  const store = createDurableStore({
    sqlite: sharedSqlite,
    errorSink,
    // Live updates must stay on the process-owned better-sqlite3 handle.
    // Opening a short-lived native writer for every batch both blocks the
    // Electron main thread and breaks the single-writer contract when the
    // Codex/Grok live pipelines are using this same connection. Lifecycle
    // owners still receive resolvedEngine/nativeAddon below, so exclusive
    // cold-start ingest continues to use Rust.
    engine: live ? 'ts' : resolvedEngine,
    native: live ? null : nativeAddon,
  });

  // Align StaticIngest + LiveDiskIngest on the same DB file (was a bug:
  // live defaulted to ~/.spaghetti/cache.db while lifecycle used
  // spaghetti-{rs,ts}.db).
  const dbPath = options?.dbPath ?? defaultDbPathForEngine(resolvedEngine);

  // Claude live pipeline only when primary is Claude Code and live is on.
  // Codex/Grok own their LiveWatch implementations inside their owners.
  // Long-lived surfaces (`live: true`) default to safer bulk so a crash
  // mid-cold-ingest is less likely to leave SQLITE_CORRUPT for next launch.
  const safeBulk = options?.safeBulk ?? live;
  const claudeLive =
    live && primary.id === 'claude-code'
      ? createClaudeCodeLiveDiskIngest({
          source: primary,
          store,
          fileService,
          dbPath,
          errorSink,
        })
      : undefined;

  // ── Plane 1: one LifecycleOwner per source (registry) ──────────────────
  const owners: LifecycleOwner[] = [];
  for (const [i, source] of allSources.entries()) {
    const owner = createLifecycleOwnerForSource({
      source,
      fileService,
      store,
      dbPath,
      errorSink,
      live,
      safeBulk,
      engine: resolvedEngine,
      native: nativeAddon,
      claudeLive: i === 0 ? claudeLive : undefined,
    });
    if (!owner) {
      errorSink.error(new Error(`No LifecycleOwner registered for source '${source.id}' — skipping.`));
      continue;
    }
    owners.push(owner);
  }

  if (owners.length === 0) {
    throw new Error('createSpaghettiService: no LifecycleOwners could be constructed for the given sources');
  }

  const dataService = new SpaghettiDataService(store.data, owners);

  // Plane 3: RuntimeBridge — always attached on the default factory path.
  // Watchers start lazily on first api.runtime subscribe. The hook/channel
  // paths it consumes are Claude Code concepts, so bind to the Claude
  // source wherever it sits in the source list — a non-Claude primary
  // (e.g. Codex-only setup) must not feed its roots to the bridge.
  const bridgeSource = allSources.find((s) => s.id === 'claude-code') ?? primary;
  const runtimeBridge = createRuntimeBridge(bridgeSource, { errorSink });

  return createSpaghettiAppService(dataService, errorSink, runtimeBridge);
}

// Re-export the service factories for manual wiring
export { createFileService } from './io/file-service.js';
export { createSqliteService } from './io/sqlite-service.js';
export { createSpaghettiAppService } from './app-service.js';
