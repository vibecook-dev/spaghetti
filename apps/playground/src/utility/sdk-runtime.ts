/** Sole owner of Spaghetti SDK lifecycle, watchers, and SQLite in the utility process. */

import { existsSync } from 'node:fs';
import { join } from 'node:path';
import {
  createCodexSource,
  createGrokSource,
  createSpaghettiService,
  defaultCodexDir,
  defaultGrokDir,
  isSqliteCorruptError,
  wipeSqliteCacheFiles,
  type AgentSource,
  type IngestEngine,
  type InitProgress,
  type SegmentChangeBatch,
  type SpaghettiAPI,
  type TimelinePageRequest,
} from '@vibecook/spaghetti-sdk';
import type { ActiveSessionChange, SessionStreamSnapshot } from '../shared/ipc.js';
import { attachPlaygroundEventForwarding } from './live-forwarding.js';

export interface SdkRuntimeOptions {
  dbPath: string;
  engine: IngestEngine;
  rootDir?: string;
}

export interface SdkRuntimeEventSink {
  progress(progress: InitProgress): void;
  ready(info: { durationMs: number }): void;
  change(batch: SegmentChangeBatch): void;
  activeSessionChange(change: ActiveSessionChange): void;
  initError(message: string): void;
}

export interface ActiveStreamIdentity {
  streamId: string;
  sourceId: string;
  projectSlug: string;
  sessionId: string;
}

export function activeSessionChangeForBatch(
  stream: ActiveStreamIdentity,
  batch: SegmentChangeBatch,
): ActiveSessionChange | null {
  const matching = batch.changes.filter(
    (change) =>
      change.sessionId === stream.sessionId &&
      change.projectSlug === stream.projectSlug &&
      change.sourceId === stream.sourceId,
  );
  if (matching.length === 0) {
    return batch.changes.length === 0 ? { ...stream, revision: batch.timestamp, reason: 'reset' } : null;
  }
  const revision = Math.max(...matching.map((change) => change.revision ?? batch.timestamp));
  const reason: ActiveSessionChange['reason'] = matching.some((change) => change.type === 'session')
    ? 'reset'
    : matching.some((change) => change.type === 'subagent')
      ? 'subagent'
      : matching.some((change) => change.type === 'tool_result')
        ? 'upsert'
        : 'append';
  return { ...stream, revision, reason };
}

function detectAdditionalSources(): AgentSource[] {
  const sources: AgentSource[] = [];
  if (existsSync(join(defaultCodexDir(), 'sessions'))) sources.push(createCodexSource());
  if (existsSync(join(defaultGrokDir(), 'sessions'))) sources.push(createGrokSource());
  return sources;
}

export class SdkRuntime {
  private service: SpaghettiAPI | null = null;
  private initPromise: Promise<void> | null = null;
  private eventCleanup: (() => void) | null = null;
  private recoveryPromise: Promise<void> | null = null;
  private maintenancePromise: Promise<unknown> | null = null;
  private disposed = false;
  private nextStreamId = 1;
  private activeStream: ActiveStreamIdentity | null = null;

  constructor(
    private readonly options: SdkRuntimeOptions,
    private readonly sink: SdkRuntimeEventSink,
  ) {}

  get engine(): IngestEngine {
    return this.options.engine;
  }

  isReady(): boolean {
    return !this.recoveryPromise && !this.maintenancePromise && (this.service?.isReady() ?? false);
  }

  /** Create synchronously so lifecycle listeners are attached before initialize emits. */
  start(): void {
    if (this.disposed || this.service) return;
    const service = this.createService();
    this.service = service;
    this.attachEvents(service);
    const init = service.initialize();
    const trackedInit = init
      .then(() => undefined)
      .catch((err: unknown) => {
        this.sink.initError(String(err));
        throw err;
      })
      .finally(() => {
        if (this.initPromise === trackedInit) this.initPromise = null;
      });
    this.initPromise = trackedInit;
    // The host remains alive after an initialization error so Retry can rebuild.
    void this.initPromise.catch(() => {});
  }

  async read<T>(operation: (service: SpaghettiAPI) => T | Promise<T>): Promise<T> {
    if (this.recoveryPromise) await this.recoveryPromise;
    if (this.maintenancePromise) await this.maintenancePromise;
    try {
      return await operation(this.requireService());
    } catch (err) {
      if (!isSqliteCorruptError(err)) throw err;
      await this.recoverFromCorruption();
      return await operation(this.requireService());
    }
  }

  async openSessionStream(
    projectSlug: string,
    sessionId: string,
    request: TimelinePageRequest,
  ): Promise<SessionStreamSnapshot> {
    const sourceId = request.sourceId;
    if (!sourceId) throw new Error('openSessionStream requires sourceId');
    return this.read((sdk) => {
      const streamId = `session-stream-${this.nextStreamId++}`;
      const stream = { streamId, sourceId, projectSlug, sessionId };
      // Register before reading. Any commit after this synchronous snapshot
      // produces an active-session event with a later live revision.
      this.activeStream = stream;
      return {
        ...stream,
        page: sdk.getSessionTimeline(projectSlug, sessionId, request),
        facets: sdk.getSessionTimelineFacets(projectSlug, sessionId, { sourceId }),
        subagents: sdk.getSessionSubagents(projectSlug, sessionId, { sourceId, includeNested: true }),
      };
    });
  }

  closeSessionStream(streamId: string): void {
    if (this.activeStream?.streamId === streamId) this.activeStream = null;
  }

  async rebuildIndex(): Promise<{ durationMs: number }> {
    if (this.recoveryPromise) await this.recoveryPromise;
    if (this.maintenancePromise) return (await this.maintenancePromise) as { durationMs: number };
    const work = this.requireService().rebuildIndex();
    const trackedWork = work.finally(() => {
      if (this.maintenancePromise === trackedWork) this.maintenancePromise = null;
    });
    this.maintenancePromise = trackedWork;
    return await work;
  }

  async retry(): Promise<void> {
    if (this.recoveryPromise) return await this.recoveryPromise;
    if (this.maintenancePromise) {
      try {
        await this.maintenancePromise;
      } catch {
        /* retry replaces a failed maintenance run below */
      }
    }
    const recovery = this.recreate(true);
    const tracked = recovery.finally(() => {
      if (this.recoveryPromise === tracked) this.recoveryPromise = null;
    });
    this.recoveryPromise = tracked;
    return await tracked;
  }

  async dispose(): Promise<void> {
    if (this.disposed) return;
    this.disposed = true;
    if (this.recoveryPromise) {
      try {
        await this.recoveryPromise;
      } catch {
        /* dispose the best available instance below */
      }
    }
    if (this.maintenancePromise) {
      try {
        await this.maintenancePromise;
      } catch {
        /* dispose the best available instance below */
      }
    }
    if (this.initPromise) {
      try {
        await this.initPromise;
      } catch {
        /* dispose the failed instance below */
      }
    }
    await this.disposeService();
  }

  private createService(): SpaghettiAPI {
    return createSpaghettiService({
      dbPath: this.options.dbPath,
      engine: this.options.engine,
      live: true,
      additionalSources: detectAdditionalSources(),
      ...(this.options.rootDir ? { rootDir: this.options.rootDir } : {}),
    });
  }

  private attachEvents(service: SpaghettiAPI): void {
    this.clearEvents();
    this.eventCleanup = attachPlaygroundEventForwarding(service, {
      progress: this.sink.progress,
      ready: this.sink.ready,
      change: (batch) => {
        this.sink.change(batch);
        this.forwardActiveSessionChange(batch);
      },
    });
  }

  private forwardActiveSessionChange(batch: SegmentChangeBatch): void {
    const stream = this.activeStream;
    if (!stream) return;
    const change = activeSessionChangeForBatch(stream, batch);
    if (change) this.sink.activeSessionChange(change);
  }

  private clearEvents(): void {
    const cleanup = this.eventCleanup;
    this.eventCleanup = null;
    cleanup?.();
  }

  private requireService(): SpaghettiAPI {
    if (!this.service) throw new Error('SDK utility is restarting');
    return this.service;
  }

  private async recoverFromCorruption(): Promise<void> {
    if (!this.recoveryPromise) {
      this.sink.progress({
        phase: 'parsing',
        message: 'Database corrupted during live update — rebuilding cache…',
      });
      const recovery = this.recreate(true);
      const tracked = recovery.finally(() => {
        if (this.recoveryPromise === tracked) this.recoveryPromise = null;
      });
      this.recoveryPromise = tracked;
    }
    return await this.recoveryPromise;
  }

  private async recreate(wipe: boolean): Promise<void> {
    await this.disposeService();
    if (wipe) wipeSqliteCacheFiles(this.options.dbPath);
    if (this.disposed) return;
    const service = this.createService();
    this.service = service;
    this.attachEvents(service);
    try {
      await service.initialize();
    } catch (err) {
      this.sink.initError(String(err));
      throw err;
    }
  }

  private async disposeService(): Promise<void> {
    this.clearEvents();
    this.activeStream = null;
    const service = this.service;
    this.service = null;
    this.initPromise = null;
    if (!service) return;
    try {
      await service.dispose();
    } catch (err) {
      console.error('[sdk-host] SDK dispose failed', err);
      try {
        service.shutdown();
      } catch {
        /* already stopped */
      }
    }
  }
}
