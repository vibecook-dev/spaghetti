/**
 * Owns the single SpaghettiService instance used by the main process.
 *
 * Multi-source: primary is Claude Code (`~/.claude`); Codex and Grok are
 * auto-detected and ingested into the same SQLite index.
 */

import { existsSync } from 'node:fs';
import { join } from 'node:path';
import {
  createCodexSource,
  createGrokSource,
  createSpaghettiService,
  defaultCodexDir,
  defaultGrokDir,
  wipeSqliteCacheFiles,
  type AgentSource,
  type IngestEngine,
  type SpaghettiAPI,
} from '@vibecook/spaghetti-sdk';

let sdkInstance: SpaghettiAPI | null = null;
let initPromise: Promise<SpaghettiAPI> | null = null;
let activeEngine: IngestEngine | null = null;
/** Last options used to construct the service — required for retryInit. */
let lastOptions: InitSdkOptions | null = null;
/** True once progress/ready/change listeners are attached for the current instance. */
let eventsWired = false;

/** Match CLI: only attach sources whose on-disk sessions tree exists. */
export function detectAdditionalSources(): AgentSource[] {
  const sources: AgentSource[] = [];
  if (existsSync(join(defaultCodexDir(), 'sessions'))) {
    sources.push(createCodexSource());
  }
  if (existsSync(join(defaultGrokDir(), 'sessions'))) {
    sources.push(createGrokSource());
  }
  return sources;
}

export function getSdk(): SpaghettiAPI {
  if (!sdkInstance) {
    throw new Error('SDK not initialized — call initSdk() first');
  }
  return sdkInstance;
}

export function getEngine(): IngestEngine {
  if (!activeEngine) {
    throw new Error('SDK not initialized — engine unresolved');
  }
  return activeEngine;
}

export function getLastInitOptions(): InitSdkOptions | null {
  return lastOptions;
}

export interface InitSdkOptions {
  /** Absolute path to the SQLite index file. */
  dbPath: string;
  /** Ingest engine to run for this process. */
  engine: IngestEngine;
  /** Optional primary agent root; defaults to the SDK's `~/.claude` for Claude. */
  rootDir?: string;
  /** @deprecated Use {@link rootDir}. */
  claudeDir?: string;
}

/**
 * Construct the service (sync) without calling `initialize()`. Callers that
 * need progress/ready events must subscribe on the returned instance first,
 * then call {@link startSdk}.
 */
export function createSdk(options: InitSdkOptions): SpaghettiAPI {
  if (sdkInstance) return sdkInstance;

  lastOptions = options;
  activeEngine = options.engine;
  const rootDir = options.rootDir ?? options.claudeDir;
  const additionalSources = detectAdditionalSources();
  const service = createSpaghettiService({
    dbPath: options.dbPath,
    engine: options.engine,
    live: true,
    additionalSources,
    ...(rootDir ? { rootDir } : {}),
  });
  sdkInstance = service;
  eventsWired = false;
  return service;
}

/**
 * Run `initialize()` once per construction. On failure, clears
 * `initPromise` so {@link retrySdkInit} can start a new cycle.
 */
export function startSdk(): Promise<SpaghettiAPI> {
  if (!sdkInstance) {
    throw new Error('SDK not created — call createSdk() first');
  }
  if (initPromise) return initPromise;
  const service = sdkInstance;
  initPromise = service
    .initialize()
    .then(() => service)
    .catch((err) => {
      // Allow a later retryInit to call initialize / recreate.
      initPromise = null;
      throw err;
    });
  return initPromise;
}

/** Create + initialize (for callers that do not need early event wiring). */
export function initSdk(options: InitSdkOptions): Promise<SpaghettiAPI> {
  createSdk(options);
  return startSdk();
}

/**
 * Wipe the SQLite cache and re-initialize from disk.
 * Used when the UI hits a corrupt/malformed DB and the user clicks Retry.
 */
export async function retrySdkInit(wireEvents: (service: SpaghettiAPI) => void): Promise<SpaghettiAPI> {
  const options = lastOptions;
  if (!options) {
    throw new Error('Cannot retry — SDK was never configured');
  }

  await disposeSdk();
  wipeSqliteCacheFiles(options.dbPath);

  const service = createSdk(options);
  wireEvents(service);
  eventsWired = true;
  return startSdk();
}

/** Fire-and-forget shutdown (legacy). Prefer {@link disposeSdk} on quit. */
export function shutdownSdk(): void {
  if (sdkInstance) {
    try {
      sdkInstance.shutdown();
    } catch (err) {
      console.error('[sdk] shutdown failed', err);
    }
    sdkInstance = null;
    initPromise = null;
    activeEngine = null;
    eventsWired = false;
  }
}

/**
 * Awaitable teardown: stop live pipelines, drain writers, close SQLite.
 */
export async function disposeSdk(): Promise<void> {
  if (!sdkInstance) {
    initPromise = null;
    activeEngine = null;
    eventsWired = false;
    return;
  }
  const service = sdkInstance;
  sdkInstance = null;
  initPromise = null;
  activeEngine = null;
  eventsWired = false;
  try {
    await service.dispose();
  } catch (err) {
    console.error('[sdk] dispose failed', err);
    try {
      service.shutdown();
    } catch {
      /* ignore */
    }
  }
}
