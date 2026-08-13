/**
 * Production-shaped RFC 011 observation host.
 *
 * One persistent Rust engine owns the database, all configured adapter
 * supervisors, the read-only query pool, and durable subscriptions. This
 * module deliberately imports no TypeScript source reader, watcher, parser,
 * SQLite service, or projection implementation.
 */

import { resolve } from 'node:path';

import {
  openSpaghettiEngine,
  type SpaghettiEngine,
  type SpaghettiEngineHealth,
  type SpaghettiEngineStatus,
} from './native.js';
import {
  NapiTransport,
  openSpaghettiClient,
  serveSpaghettiIpc,
  type SpaghettiClient,
  type SpaghettiClientInfo,
  type SpaghettiIpcChannel,
  type SpaghettiIpcHost,
} from './client/index.js';

export interface ObservationHostSource {
  /** Open identifier of an adapter compiled into the native registry. */
  adapterId: string;
  /** Explicit native data roots understood by that adapter. */
  roots: string[];
}

export interface ObservationHostOptions {
  /** Sole Rust-owned canonical database. */
  dbPath: string;
  /** One entry per adapter; duplicate adapter identifiers are rejected. */
  sources: ObservationHostSource[];
  /** Persistent read-only query workers. */
  queryWorkers?: number;
  /** Diagnostic label written to the owner metadata sidecar. */
  ownerLabel?: string;
  /** Cancels startup; every partially started supervisor is still disposed. */
  signal?: AbortSignal;
  /** Structured startup snapshots, including heartbeats during long scans. */
  onProgress?: (progress: ObservationHostProgress) => void;
}

export interface ObservationHostProgress {
  stage: 'opening' | 'adapter-scanning' | 'adapter-ready' | 'ready';
  adapterId?: string;
  /** One-based configured source position. */
  sourceIndex?: number;
  sourceCount: number;
  elapsedMs: number;
  status?: SpaghettiEngineStatus;
}

export interface ObservationHostSnapshot {
  databasePath: string;
  sources: ReadonlyArray<{ adapterId: string; roots: readonly string[] }>;
  status: SpaghettiEngineStatus;
  health: SpaghettiEngineHealth;
}

export interface ObservationHost {
  readonly databasePath: string;
  readonly sources: ReadonlyArray<{ adapterId: string; roots: readonly string[] }>;
  readonly status: SpaghettiEngineStatus;
  readonly client: SpaghettiClient;
  readonly clientInfo: SpaghettiClientInfo;
  snapshot(signal?: AbortSignal): Promise<ObservationHostSnapshot>;
  refresh(adapterId?: string, signal?: AbortSignal): Promise<SpaghettiEngineStatus>;
  stop(adapterId: string, signal?: AbortSignal): Promise<SpaghettiEngineStatus>;
  serveIpc(channel: SpaghettiIpcChannel, transportKind?: string): SpaghettiIpcHost;
  dispose(): Promise<SpaghettiEngineStatus>;
}

/** Open one sole-owner Rust host and start every configured adapter. */
export async function openObservationHost(options: ObservationHostOptions): Promise<ObservationHost> {
  options.signal?.throwIfAborted();
  const sources = normalizeSources(options.sources);
  const startedAt = Date.now();
  emitHostProgress(options, {
    stage: 'opening',
    sourceCount: sources.length,
    elapsedMs: 0,
  });
  const engine = await openSpaghettiEngine({
    dbPath: resolveDatabasePath(options.dbPath),
    queryWorkers: options.queryWorkers,
    ownerLabel: options.ownerLabel ?? 'sdk-observation-host',
  });
  let client: SpaghettiClient | undefined;
  try {
    for (const [index, source] of sources.entries()) {
      options.signal?.throwIfAborted();
      const report = (stage: ObservationHostProgress['stage']): void =>
        emitHostProgress(options, {
          stage,
          adapterId: source.adapterId,
          sourceIndex: index + 1,
          sourceCount: sources.length,
          elapsedMs: Date.now() - startedAt,
          status: engine.status,
        });
      report('adapter-scanning');
      const heartbeat = setInterval(() => {
        if (!options.signal?.aborted) report('adapter-scanning');
      }, 1_000);
      try {
        await engine.startObservation(
          {
            adapterId: source.adapterId,
            roots: [...source.roots],
            reason: 'production_observation',
          },
          options.signal,
        );
      } finally {
        clearInterval(heartbeat);
      }
      report('adapter-ready');
    }
    client = await openSpaghettiClient({
      transport: new NapiTransport({ engine, ownsEngine: false }),
      clientName: 'observation-host',
    });
    emitHostProgress(options, {
      stage: 'ready',
      sourceCount: sources.length,
      elapsedMs: Date.now() - startedAt,
      status: engine.status,
    });
    return new NativeObservationHost(engine, client, sources);
  } catch (error) {
    await client?.dispose().catch(() => undefined);
    await engine.dispose();
    throw error;
  }
}

function emitHostProgress(options: ObservationHostOptions, progress: ObservationHostProgress): void {
  try {
    options.onProgress?.(progress);
  } catch {
    // Observability callbacks cannot take down the sole database owner.
  }
}

class NativeObservationHost implements ObservationHost {
  readonly databasePath: string;
  readonly sources: ReadonlyArray<{ adapterId: string; roots: readonly string[] }>;
  private readonly ipcHosts = new Set<SpaghettiIpcHost>();
  private disposePromise: Promise<SpaghettiEngineStatus> | null = null;

  constructor(
    private readonly engine: SpaghettiEngine,
    readonly client: SpaghettiClient,
    sources: Array<{ adapterId: string; roots: string[] }>,
  ) {
    this.databasePath = engine.status.databasePath;
    this.sources = Object.freeze(
      sources.map((source) => Object.freeze({ adapterId: source.adapterId, roots: Object.freeze([...source.roots]) })),
    );
  }

  get status(): SpaghettiEngineStatus {
    return this.engine.status;
  }

  get clientInfo(): SpaghettiClientInfo {
    return this.client.info;
  }

  async snapshot(signal?: AbortSignal): Promise<ObservationHostSnapshot> {
    const health = await this.client.getHealth(signal ? { signal } : undefined);
    return {
      databasePath: this.databasePath,
      sources: this.sources,
      status: health.status,
      health,
    };
  }

  async refresh(adapterId?: string, signal?: AbortSignal): Promise<SpaghettiEngineStatus> {
    this.assertRunning();
    const adapterIds = adapterId
      ? [this.requireConfiguredAdapter(adapterId)]
      : this.sources.map((source) => source.adapterId);
    let status = this.engine.status;
    for (const id of adapterIds) status = await this.engine.refreshObservation(id, signal);
    return status;
  }

  stop(adapterId: string, signal?: AbortSignal): Promise<SpaghettiEngineStatus> {
    this.assertRunning();
    return this.engine.stopObservation(this.requireConfiguredAdapter(adapterId), signal);
  }

  serveIpc(channel: SpaghettiIpcChannel, transportKind = 'ipc'): SpaghettiIpcHost {
    this.assertRunning();
    let host: SpaghettiIpcHost | undefined;
    let unsubscribeClose = (): void => undefined;
    unsubscribeClose = channel.onClose(() => {
      if (host) this.ipcHosts.delete(host);
      unsubscribeClose();
    });
    try {
      host = serveSpaghettiIpc({
        channel,
        transport: new NapiTransport({ engine: this.engine, ownsEngine: false }),
        ownsTransport: true,
        transportKind,
      });
      this.ipcHosts.add(host);
      return host;
    } catch (error) {
      unsubscribeClose();
      void channel.close().catch(() => undefined);
      throw error;
    }
  }

  async dispose(): Promise<SpaghettiEngineStatus> {
    if (!this.disposePromise) {
      this.disposePromise = (async () => {
        const ipcHosts = [...this.ipcHosts];
        this.ipcHosts.clear();
        await Promise.allSettled(ipcHosts.map((host) => host.dispose()));
        await this.client.dispose();
        return await this.engine.dispose();
      })();
    }
    return await this.disposePromise;
  }

  private assertRunning(): void {
    if (this.disposePromise) throw new Error('Observation host is stopping.');
  }

  private requireConfiguredAdapter(adapterId: string): string {
    const normalized = adapterId.trim();
    if (!this.sources.some((source) => source.adapterId === normalized)) {
      throw new Error(`Observation adapter '${normalized}' is not configured in this host.`);
    }
    return normalized;
  }
}

function normalizeSources(sources: ObservationHostSource[]): Array<{ adapterId: string; roots: string[] }> {
  if (!Array.isArray(sources) || sources.length === 0) {
    throw new Error('Observation host requires at least one configured adapter.');
  }
  const adapterIds = new Set<string>();
  return sources.map((source) => {
    const adapterId = source.adapterId.trim();
    if (!adapterId) throw new Error('Observation adapterId must not be empty.');
    if (adapterIds.has(adapterId)) throw new Error(`Observation adapter '${adapterId}' is configured more than once.`);
    adapterIds.add(adapterId);
    if (!Array.isArray(source.roots) || source.roots.length === 0) {
      throw new Error(`Observation adapter '${adapterId}' requires at least one explicit source root.`);
    }
    const roots = [...new Set(source.roots.map(resolveSourceRoot))];
    return { adapterId, roots };
  });
}

function resolveDatabasePath(value: string): string {
  if (typeof value !== 'string' || value.trim() === '' || value === ':memory:') {
    throw new Error('Observation host database must be a non-empty file-backed path.');
  }
  return resolve(value);
}

function resolveSourceRoot(value: string): string {
  if (typeof value !== 'string' || value.trim() === '') {
    throw new Error('Observation source root must not be empty.');
  }
  return resolve(value);
}
