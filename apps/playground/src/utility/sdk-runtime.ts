/** Sole owner of Spaghetti SDK lifecycle, watchers, and SQLite in the utility process. */

import { existsSync } from 'node:fs';
import { join } from 'node:path';
import {
  createCodexSource,
  createGrokSource,
  createSpaghettiService,
  compareClaudeObservationHistory,
  defaultCodexDir,
  defaultClaudeDir,
  defaultClaudeObservationShadowDbPath,
  defaultGrokDir,
  isSqliteCorruptError,
  MessagePortIpcChannel,
  openClaudeObservationShadow,
  wipeSqliteCacheFiles,
  type AgentSource,
  type ClaudeObservationShadow,
  type IngestEngine,
  type InitProgress,
  type SegmentChangeBatch,
  type SpaghettiMessagePort,
  type SpaghettiAPI,
  type TimelinePageRequest,
} from '@vibecook/spaghetti-sdk';
import type {
  ActiveSessionChange,
  ObservationOwnerStatus,
  ObservationShadowReport,
  SessionStreamSnapshot,
} from '../shared/ipc.js';
import { attachPlaygroundEventForwarding } from './live-forwarding.js';

export interface SdkRuntimeOptions {
  dbPath: string;
  engine: IngestEngine;
  rootDir?: string;
  /** Override auto-detected secondary sources (primarily for host conformance). */
  additionalSources?: AgentSource[];
  /** Opt-in RFC 011 parity owner. Its database is always isolated. */
  observationShadow?: { dbPath?: string };
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
  private observationShadow: ClaudeObservationShadow | null = null;
  private observationShadowStart: Promise<void> | null = null;
  private observationShadowStartAbort: AbortController | null = null;
  private observationShadowState: ObservationOwnerStatus['state'];
  private observationShadowError: string | null = null;

  constructor(
    private readonly options: SdkRuntimeOptions,
    private readonly sink: SdkRuntimeEventSink,
  ) {
    this.observationShadowState = options.observationShadow ? 'starting' : 'disabled';
  }

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
      .then(() => this.startObservationShadow())
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

  async getObservationShadowStatus(): Promise<ObservationShadowReport> {
    const owner = this.getObservationOwnerStatus();
    if (!owner.enabled) return owner;
    const databasePath = this.observationShadowDatabasePath();
    if (this.observationShadowState === 'failed') {
      return {
        ...owner,
        databasePath,
      };
    }
    const shadow = this.observationShadow;
    if (!shadow) {
      return { ...owner, databasePath };
    }

    let snapshot;
    try {
      snapshot = await shadow.snapshot();
    } catch (error) {
      return {
        enabled: true,
        state: 'degraded',
        databasePath: shadow.databasePath,
        error: String(error),
      };
    }
    const report: ObservationShadowReport = {
      enabled: true,
      state: snapshot.health.healthy ? 'running' : 'degraded',
      databasePath: shadow.databasePath,
      ...(!snapshot.health.healthy && snapshot.health.detail ? { error: snapshot.health.detail } : {}),
      snapshot,
    };
    try {
      report.historyParity = compareClaudeObservationHistory(snapshot.overview, this.collectClaudeHistoryCounts());
    } catch (error) {
      // The legacy oracle can be unavailable during its own maintenance while
      // the isolated Rust observation engine remains healthy.
      report.parityError = String(error);
    }
    return report;
  }

  /** Report lifecycle only; this performs no canonical or legacy database reads. */
  getObservationOwnerStatus(): ObservationOwnerStatus {
    if (!this.options.observationShadow) return { enabled: false, state: 'disabled' };
    return {
      enabled: true,
      state: this.observationShadowState,
      ...(this.observationShadowError ? { error: this.observationShadowError } : {}),
    };
  }

  /** Attach one negotiated framed client to the Rust owner in this utility process. */
  async attachObservationClient(port: SpaghettiMessagePort): Promise<void> {
    let channel: MessagePortIpcChannel | undefined;
    try {
      if (!this.options.observationShadow) throw new Error('RFC 011 observation shadow is disabled.');
      if (this.disposed) throw new Error('SDK utility is shutting down.');

      // The utility announces its control transport before cold ingest begins.
      // Starting here is idempotent and closes that short attachment race.
      this.start();
      if (this.initPromise) await this.initPromise;
      if (this.observationShadowStart) await this.observationShadowStart;
      const shadow = this.observationShadow;
      if (!shadow) {
        throw new Error(this.observationShadowError ?? 'RFC 011 observation shadow is unavailable.');
      }
      // Leave the raw port paused while cold ingest runs. MessagePort queues
      // the client's connect frame until the channel installs its listener.
      channel = new MessagePortIpcChannel(port, 'playground-utility');
      shadow.serveIpc(channel, 'playground-utility');
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
    this.observationShadowStartAbort?.abort();
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
    await this.disposeObservationShadow();
    await this.disposeService();
  }

  private createService(): SpaghettiAPI {
    return createSpaghettiService({
      dbPath: this.options.dbPath,
      engine: this.options.engine,
      live: true,
      additionalSources: this.options.additionalSources ?? detectAdditionalSources(),
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
      await this.startObservationShadow();
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

  private async startObservationShadow(): Promise<void> {
    if (!this.options.observationShadow || this.observationShadow || this.disposed) return;
    if (this.observationShadowStart) return await this.observationShadowStart;
    this.observationShadowState = 'starting';
    this.observationShadowError = null;
    const abort = new AbortController();
    this.observationShadowStartAbort = abort;
    const work = (async () => {
      try {
        const shadow = await openClaudeObservationShadow({
          productionDbPath: this.options.dbPath,
          shadowDbPath: this.options.observationShadow?.dbPath,
          roots: [this.options.rootDir ?? defaultClaudeDir()],
          ownerLabel: 'playground-utility-shadow',
          signal: abort.signal,
        });
        if (this.disposed) {
          await shadow.dispose();
          this.observationShadowState = 'stopped';
          return;
        }
        this.observationShadow = shadow;
        this.observationShadowState = 'running';
        console.info(`[sdk-host] RFC 011 observation shadow ready at ${shadow.databasePath}`);
      } catch (error) {
        this.observationShadowState = this.disposed && abort.signal.aborted ? 'stopped' : 'failed';
        this.observationShadowError = this.observationShadowState === 'failed' ? String(error) : null;
        // Shadow mode is an oracle, not the production path. Keep the legacy
        // service ready and expose this failure through its diagnostic RPC.
        if (this.observationShadowState === 'failed') {
          console.error('[sdk-host] RFC 011 observation shadow failed', error);
        }
      }
    })();
    const tracked = work.finally(() => {
      if (this.observationShadowStart === tracked) this.observationShadowStart = null;
      if (this.observationShadowStartAbort === abort) this.observationShadowStartAbort = null;
    });
    this.observationShadowStart = tracked;
    await tracked;
  }

  private async disposeObservationShadow(): Promise<void> {
    if (this.observationShadowStart) await this.observationShadowStart;
    const shadow = this.observationShadow;
    this.observationShadow = null;
    this.observationShadowState = 'stopped';
    if (!shadow) return;
    try {
      await shadow.dispose();
    } catch (error) {
      console.error('[sdk-host] RFC 011 observation shadow dispose failed', error);
    }
  }

  private observationShadowDatabasePath(): string {
    return (
      this.observationShadow?.databasePath ??
      this.options.observationShadow?.dbPath ??
      defaultClaudeObservationShadowDbPath(this.options.dbPath)
    );
  }

  private collectClaudeHistoryCounts(): { sessions: number; messages: number; subagentMessages: number } {
    const service = this.requireService();
    const projects = service.getProjectList({ sourceId: 'claude-code' });
    const sessions = projects.flatMap((project) => service.getSessionList(project, { sourceId: 'claude-code' }));
    let messages = 0;
    let subagentMessages = 0;
    for (const session of sessions) {
      messages += session.messageCount;
      for (const subagent of service.getSessionSubagents(session.projectSlug, session.sessionId, {
        sourceId: 'claude-code',
        includeNested: true,
      })) {
        subagentMessages += subagent.messageCount;
      }
    }
    return { sessions: sessions.length, messages, subagentMessages };
  }
}
