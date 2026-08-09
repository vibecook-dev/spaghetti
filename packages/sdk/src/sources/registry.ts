/**
 * Lifecycle owner registry — maps AgentSourceId → owner construction.
 *
 * Phase E: `createSpaghettiService` builds owners only through this table.
 * A new agent is a `sources/<id>/` package + one entry here (no create.ts
 * product branches).
 */

import type { FileService } from '../io/file-service.js';
import type { ErrorSink } from '../io/error-sink.js';
import type { LifecycleOwner } from '../data/lifecycle-owner.js';
import { createIngestService } from '../data/ingest-service.js';
import type { DurableStore } from '../store/durable-store.js';
import type { NativeAddon } from '../native.js';
import type { IngestEngine } from '../settings.js';
import type { ClaudeCodeLiveDiskIngest } from './claude-code/live/disk-ingest.js';
import { toLifecycleOptions } from '../planes/static-ingest.js';
import { createClaudeCodeParser } from './claude-code/parser/index.js';
import { ClaudeCodeLifecycleOwner } from './claude-code/lifecycle-owner.js';
import { CodexLifecycleOwner } from './codex/lifecycle-owner.js';
import { createCodexIngestHooks } from './codex/ingest-hooks.js';
import { GrokLifecycleOwner } from './grok/lifecycle-owner.js';
import type { AgentSource, AgentSourceId } from './types.js';

/** Shared deps every factory needs to build a LifecycleOwner. */
export interface LifecycleOwnerFactoryDeps {
  source: AgentSource;
  fileService: FileService;
  store: DurableStore;
  dbPath: string;
  errorSink: ErrorSink;
  /** Plane 2 requested for this service (owners decide how to watch). */
  live: boolean;
  /**
   * Crash-safer bulk SQLite settings for cold/warm exclusive ingest.
   * Defaulted by createSpaghettiService when `live` is true.
   */
  safeBulk?: boolean;
  engine: IngestEngine;
  native: NativeAddon | null;
  /**
   * Prebuilt Claude live pipeline for the primary Claude source only.
   * Undefined for secondary sources and non-Claude primaries (they use
   * their own LiveWatch implementations inside the owner).
   */
  claudeLive?: ClaudeCodeLiveDiskIngest;
}

export type LifecycleOwnerFactory = (deps: LifecycleOwnerFactoryDeps) => LifecycleOwner;

const REGISTRY: Record<AgentSourceId, LifecycleOwnerFactory> = {
  'claude-code': (deps) => {
    const parser = createClaudeCodeParser(deps.fileService);
    return new ClaudeCodeLifecycleOwner(
      deps.fileService,
      parser,
      deps.store.query,
      // Shared primary ingest (default claude extractor / no token hooks).
      deps.store.ingest,
      deps.store.data,
      toLifecycleOptions({
        source: deps.source,
        engine: deps.engine,
        dbPath: deps.dbPath,
        safeBulk: deps.safeBulk,
        errorSink: deps.errorSink,
      }),
      deps.claudeLive,
    );
  },

  codex: (deps) => {
    // Live writeBatch stays on TS (native liveIngestBatch is Claude-shaped).
    // Cold/warm still uses rs via exclusiveIngest when engine is rs.
    const ingest = createIngestService(() => deps.store.sqlite, {
      sourceId: 'codex',
      messages: deps.source.messages,
      hooks: createCodexIngestHooks(),
      engine: 'ts',
      safeBulk: deps.safeBulk,
    });
    return new CodexLifecycleOwner(
      deps.fileService,
      deps.source,
      deps.store.data,
      ingest,
      deps.dbPath,
      deps.errorSink,
      deps.live,
      deps.engine,
      deps.safeBulk,
    );
  },

  grok: (deps) => {
    // Cold/warm uses rs via exclusiveIngest when engine is rs.
    // Live writeBatch stays on TS (like Codex): native liveIngestBatch races
    // the open better-sqlite3 handle (SQLITE_BUSY) and historically mishandled
    // extract()→null. TS live keeps skip contract + single-writer semantics.
    const ingest = createIngestService(() => deps.store.sqlite, {
      sourceId: 'grok',
      messages: deps.source.messages,
      engine: 'ts',
      safeBulk: deps.safeBulk,
    });
    return new GrokLifecycleOwner(
      deps.fileService,
      deps.source,
      deps.store.data,
      ingest,
      deps.dbPath,
      deps.errorSink,
      deps.live,
      deps.engine,
      deps.safeBulk,
    );
  },
};

/** True if a LifecycleOwner factory is registered for this source id. */
export function isLifecycleOwnerRegistered(id: string): id is AgentSourceId {
  return Object.prototype.hasOwnProperty.call(REGISTRY, id);
}

/**
 * Build a LifecycleOwner for `deps.source`, or `null` if the id is unknown.
 * Callers should log/skip unknowns rather than throw (additive sources).
 */
export function createLifecycleOwnerForSource(deps: LifecycleOwnerFactoryDeps): LifecycleOwner | null {
  const factory = REGISTRY[deps.source.id as AgentSourceId];
  if (!factory) return null;
  return factory(deps);
}

/** Registered source ids (for tests / diagnostics). */
export function registeredLifecycleOwnerIds(): AgentSourceId[] {
  return Object.keys(REGISTRY) as AgentSourceId[];
}
