/** Sole owner of the Rust observation engine in the Electron utility process. */

import { existsSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import {
  createObservationService,
  MessagePortIpcChannel,
  type ObservationHostSource,
  type ObservationService,
  type SpaghettiMessagePort,
} from '@vibecook/spaghetti-sdk/observation';
import type { InitProgress, SegmentChangeBatch, TimelinePageRequest } from '@vibecook/spaghetti-sdk';
import type {
  ActiveSessionChange,
  ObservationHostReport,
  ObservationOwnerStatus,
  SessionStreamSnapshot,
} from '../shared/ipc.js';
import { attachPlaygroundEventForwarding } from './live-forwarding.js';

export interface SdkRuntimeOptions {
  /** Canonical Rust-owned database. */
  dbPath: string;
  rootDir?: string;
  /** Override auto-detected secondary native adapters. */
  additionalSources?: ObservationHostSource[];
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

function primarySource(rootDir?: string): ObservationHostSource {
  return { adapterId: 'claude-code', roots: [rootDir ?? join(homedir(), '.claude')] };
}

function detectedAdditionalSources(): ObservationHostSource[] {
  const sources: ObservationHostSource[] = [];
  const codex = join(homedir(), '.codex');
  if (existsSync(join(codex, 'sessions'))) sources.push({ adapterId: 'codex', roots: [codex] });
  const grok = join(homedir(), '.grok');
  if (existsSync(join(grok, 'sessions'))) sources.push({ adapterId: 'grok', roots: [grok] });
  return sources;
}

export class SdkRuntime {
  private service: ObservationService | null = null;
  private initPromise: Promise<void> | null = null;
  private eventCleanup: (() => void) | null = null;
  private recoveryPromise: Promise<void> | null = null;
  private maintenancePromise: Promise<unknown> | null = null;
  private disposed = false;
  private nextStreamId = 1;
  private activeStream: ActiveStreamIdentity | null = null;
  private readonly sessionSnapshotLoads = new Map<
    string,
    Promise<Pick<SessionStreamSnapshot, 'page' | 'facets' | 'subagents'>>
  >();
  private ownerState: ObservationOwnerStatus['state'] = 'starting';
  private ownerError: string | null = null;
  private lastProgress: InitProgress | null = null;
  private sourceCount = 0;

  constructor(
    private readonly options: SdkRuntimeOptions,
    private readonly sink: SdkRuntimeEventSink,
  ) {}

  get engine(): 'rs' {
    return 'rs';
  }

  isReady(): boolean {
    return !this.recoveryPromise && !this.maintenancePromise && (this.service?.isReady() ?? false);
  }

  /** Create synchronously so lifecycle listeners are attached before initialize emits. */
  start(): void {
    if (this.disposed || this.service) return;
    const service = this.createService();
    this.service = service;
    this.ownerState = 'starting';
    this.ownerError = null;
    this.attachEvents(service);
    const init = service.initialize();
    const tracked = init
      .then(() => {
        this.ownerState = 'running';
      })
      .catch((error: unknown) => {
        if (this.disposed) {
          this.ownerState = 'stopped';
        } else {
          this.ownerState = 'failed';
          this.ownerError = String(error);
          this.sink.initError(String(error));
        }
        throw error;
      })
      .finally(() => {
        if (this.initPromise === tracked) this.initPromise = null;
      });
    this.initPromise = tracked;
    void tracked.catch(() => undefined);
  }

  async read<T>(operation: (service: ObservationService) => T | Promise<T>): Promise<T> {
    if (this.recoveryPromise) await this.recoveryPromise;
    if (this.maintenancePromise) await this.maintenancePromise;
    if (this.initPromise) await this.initPromise;
    return await operation(this.requireService());
  }

  async getObservationHostStatus(): Promise<ObservationHostReport> {
    const owner = this.getObservationOwnerStatus();
    const databasePath = this.options.dbPath;
    if (owner.state !== 'running') return { ...owner, databasePath };
    try {
      const snapshot = await this.requireService().snapshot();
      return {
        enabled: true,
        state: snapshot.health.healthy ? 'running' : 'degraded',
        databasePath,
        ...(!snapshot.health.healthy && snapshot.health.detail ? { error: snapshot.health.detail } : {}),
        snapshot,
      };
    } catch (error) {
      return { enabled: true, state: 'degraded', databasePath, error: String(error) };
    }
  }

  getObservationOwnerStatus(): ObservationOwnerStatus {
    return {
      enabled: true,
      state: this.ownerState,
      ...(this.ownerError ? { error: this.ownerError } : {}),
      ...(this.lastProgress ? { progress: this.lastProgress } : {}),
    };
  }

  /** Attach one negotiated framed client to the production Rust owner. */
  async attachObservationClient(port: SpaghettiMessagePort): Promise<void> {
    let channel: MessagePortIpcChannel | undefined;
    try {
      if (this.disposed) throw new Error('SDK utility is shutting down.');
      this.start();
      if (this.initPromise) await this.initPromise;
      channel = new MessagePortIpcChannel(port, 'playground-utility');
      this.requireService().serveIpc(channel, 'playground-utility');
    } catch (error) {
      if (channel) await channel.close().catch(() => undefined);
      else port.close?.();
      throw error;
    }
  }

  async openSessionStream(
    projectSlug: string,
    sessionId: string,
    request: TimelinePageRequest,
  ): Promise<SessionStreamSnapshot> {
    const sourceId = request.sourceId;
    if (!sourceId) throw new Error('openSessionStream requires sourceId');
    return await this.read(async (sdk) => {
      const streamId = `session-stream-${this.nextStreamId++}`;
      const stream = { streamId, sourceId, projectSlug, sessionId };
      this.activeStream = stream;
      const snapshotKey = JSON.stringify([sourceId, projectSlug, sessionId, request]);
      let snapshotLoad = this.sessionSnapshotLoads.get(snapshotKey);
      if (!snapshotLoad) {
        const work = (async () => {
          const [page, subagents] = await Promise.all([
            sdk.getSessionTimeline(projectSlug, sessionId, request),
            sdk.getSessionSubagents(projectSlug, sessionId, { sourceId, includeNested: true }),
          ]);
          const facets = page.facets ?? (await sdk.getSessionTimelineFacets(projectSlug, sessionId, { sourceId }));
          return { page, facets, subagents };
        })();
        const tracked = work.finally(() => {
          if (this.sessionSnapshotLoads.get(snapshotKey) === tracked) this.sessionSnapshotLoads.delete(snapshotKey);
        });
        this.sessionSnapshotLoads.set(snapshotKey, tracked);
        snapshotLoad = tracked;
      }
      return { ...stream, ...(await snapshotLoad) };
    });
  }

  closeSessionStream(streamId: string): void {
    if (this.activeStream?.streamId === streamId) this.activeStream = null;
  }

  async rebuildIndex(): Promise<{ durationMs: number }> {
    if (this.recoveryPromise) await this.recoveryPromise;
    if (this.maintenancePromise) return (await this.maintenancePromise) as { durationMs: number };
    const work = this.requireService().rebuildIndex();
    const tracked = work.finally(() => {
      if (this.maintenancePromise === tracked) this.maintenancePromise = null;
    });
    this.maintenancePromise = tracked;
    return await work;
  }

  async retry(): Promise<void> {
    if (this.recoveryPromise) return await this.recoveryPromise;
    const recovery = this.recreate();
    const tracked = recovery.finally(() => {
      if (this.recoveryPromise === tracked) this.recoveryPromise = null;
    });
    this.recoveryPromise = tracked;
    return await tracked;
  }

  async dispose(): Promise<void> {
    if (this.disposed) return;
    this.disposed = true;
    await this.disposeService();
    if (this.recoveryPromise) await this.recoveryPromise.catch(() => undefined);
    if (this.maintenancePromise) await this.maintenancePromise.catch(() => undefined);
    this.ownerState = 'stopped';
  }

  private createService(): ObservationService {
    const sources = [
      primarySource(this.options.rootDir),
      ...(this.options.additionalSources ?? detectedAdditionalSources()),
    ];
    this.sourceCount = sources.length;
    return createObservationService({
      dbPath: this.options.dbPath,
      sources,
      ownerLabel: 'playground-utility-production',
      live: true,
    });
  }

  private attachEvents(service: ObservationService): void {
    this.clearEvents();
    this.eventCleanup = attachPlaygroundEventForwarding(service, {
      progress: (progress) => {
        this.lastProgress = progress;
        this.sink.progress(progress);
      },
      ready: (info) => {
        const sourceCount = this.sourceCount;
        this.lastProgress = {
          phase: 'indexing',
          message: 'Rust observation service is ready.',
          current: sourceCount,
          total: sourceCount,
          sourceCount,
          elapsedMs: info.durationMs,
        };
        this.sink.ready(info);
      },
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

  private requireService(): ObservationService {
    if (!this.service) throw new Error('SDK utility is restarting.');
    return this.service;
  }

  private async recreate(): Promise<void> {
    await this.disposeService();
    if (this.disposed) return;
    const service = this.createService();
    this.service = service;
    this.ownerState = 'starting';
    this.ownerError = null;
    this.attachEvents(service);
    try {
      await service.initialize();
      this.ownerState = 'running';
    } catch (error) {
      if (this.disposed) {
        this.ownerState = 'stopped';
        return;
      }
      this.ownerState = 'failed';
      this.ownerError = String(error);
      this.sink.initError(String(error));
      throw error;
    }
  }

  private async disposeService(): Promise<void> {
    this.clearEvents();
    this.activeStream = null;
    this.sessionSnapshotLoads.clear();
    const service = this.service;
    this.service = null;
    this.initPromise = null;
    if (!service) return;
    try {
      await service.dispose();
    } catch (error) {
      console.error('[sdk-host] observation service dispose failed', error);
    }
  }
}
