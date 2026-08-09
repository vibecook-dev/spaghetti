/**
 * GrokLifecycleOwner — Grok's ingest-lifecycle owner (RFC 006 multi-source).
 *
 * Lives under `sources/grok/` (product code). Implements the shared
 * `LifecycleOwner` contract: ingests Grok sessions into the SHARED store under
 * `sourceId: 'grok'`.
 *
 * ## Multi-source exclusive queue
 *
 * Participates in the three-phase protocol so **native rs is always used when
 * available** — even when Claude already warm-started the same cache:
 *
 * 1. `exclusiveIngest` — `native.ingest({ sourceId: 'grok' })` with the shared
 *    better-sqlite3 handle **closed** (MEMORY journal is safe).
 * 2. `attachShared` — open shared handle + stamp extract meta.
 * 3. `startLivePipeline` — TS live tail for Change events.
 *
 * Failures in exclusive/attach are non-fatal (emit `error`, leave primary up).
 * Live writeBatch stays on TS (registry pins Grok ingest `engine: 'ts'`) so
 * Change events and extract-null skips stay on one connection.
 */

import { reportIngestErrors } from '../../data/ingest-error-report.js';
import { EventEmitter } from 'events';

import type { FileService } from '../../io/index.js';
import type { ErrorSink } from '../../io/error-sink.js';
import type { AgentSource } from '../types.js';
import type { AgentDataStore } from '../../data/agent-data-store.js';
import type { IngestService } from '../../data/ingest-service.js';
import type { LifecycleOwner } from '../../data/lifecycle-owner.js';
import type { LiveWatch } from '../../live/live-watch.js';
import { loadNativeAddon } from '../../native.js';
import { resolveEngine, type IngestEngine } from '../../settings.js';
import { GrokReader } from './reader.js';
import { createGrokLiveWatch, type GrokLiveWatch } from './live-watch.js';

/**
 * Bump when Grok message extraction changes in a way that requires re-reading
 * chat_history files even if mtime is unchanged. Absent/mismatched → force a
 * full Grok re-read once, then stamp the new version.
 */
/** Bump when Grok extract/sidecar behaviour changes (forces cold re-read). */
const GROK_EXTRACT_VERSION = 'grok_v4_rich_human_timeline';
const GROK_EXTRACT_META_KEY = 'grok_extract_version';

export class GrokLifecycleOwner extends EventEmitter implements LifecycleOwner {
  readonly sourceId = 'grok';
  private ready = false;
  private liveWatch: GrokLiveWatch | undefined;
  private readonly engine: IngestEngine;
  private readonly safeBulk: boolean;

  constructor(
    private readonly fileService: FileService,
    private readonly source: AgentSource,
    private readonly store: AgentDataStore,
    private readonly ingestService: IngestService,
    private readonly dbPath: string,
    private readonly errorSink: ErrorSink,
    private readonly live: boolean = false,
    engine?: IngestEngine,
    safeBulk?: boolean,
  ) {
    super();
    this.engine = engine ?? resolveEngine();
    this.safeBulk = safeBulk ?? false;
  }

  getCacheDbPath(): string {
    return this.dbPath;
  }

  /** Solo composition of the three multi-source phases. */
  async initialize(): Promise<void> {
    this.ready = false;
    const start = Date.now();
    try {
      await this.exclusiveIngest();
      await this.attachShared();
      await this.startLivePipeline();
      this.ready = true;
      this.emit('ready', { durationMs: Date.now() - start });
    } catch (error) {
      this.emit('error', { error: error instanceof Error ? error.message : String(error) });
    }
  }

  /**
   * Phase 1 — exclusive cold/warm. Prefers native rs; leaves shared handle closed.
   */
  async exclusiveIngest(): Promise<void> {
    this.ready = false;
    this.emit('progress', { phase: 'parsing', message: 'Ingesting Grok sessions…' });

    // Never hold better-sqlite3 open during exclusive native / before peer native.
    this.releaseShared();

    const native = this.engine === 'rs' ? loadNativeAddon() : null;
    if (native) {
      await this.initializeWithNative(native);
      return;
    }

    // Pure-TS exclusive: open → ingest → close so peers can still take native next.
    this.ingestService.open(this.dbPath);
    try {
      this.readWithTypeScript();
    } finally {
      this.releaseShared();
    }
  }

  /** Phase 2 — reopen shared handle + stamp extract version meta. */
  async attachShared(): Promise<void> {
    this.ingestService.open(this.dbPath);
    // Native path stamps meta here (after open). TS path may have stamped already;
    // re-stamp is idempotent.
    this.ingestService.setMeta(GROK_EXTRACT_META_KEY, GROK_EXTRACT_VERSION);
  }

  /** Phase 3 — live tail of chat_history.jsonl (TS writer for Change events). */
  async startLivePipeline(): Promise<void> {
    if (this.live) {
      if (!this.liveWatch) {
        this.liveWatch = createGrokLiveWatch({
          fileService: this.fileService,
          sessionsDir: this.source.paths.sessionsDir,
          ingestService: this.ingestService,
          store: this.store,
          errorSink: this.errorSink,
        });
      }
      await this.liveWatch.start();
    }
    this.ready = true;
  }

  releaseShared(): void {
    try {
      this.ingestService.close();
    } catch {
      /* ignore — may already be closed by a peer that shares the handle */
    }
  }

  /**
   * True when extract/sidecar behaviour changed and a full Grok re-read is
   * required even if file mtimes match. Reads meta with a brief open/close so
   * exclusive native peers still see a free file handle.
   */
  private needsExtractForceReread(): boolean {
    try {
      this.ingestService.open(this.dbPath);
      return this.ingestService.getMeta(GROK_EXTRACT_META_KEY) !== GROK_EXTRACT_VERSION;
    } catch {
      // Missing/corrupt DB → let native cold path create it; no force wipe needed.
      return false;
    } finally {
      this.releaseShared();
    }
  }

  /** Native Grok cold/warm (exclusive connection inside the addon). */
  private async initializeWithNative(native: NonNullable<ReturnType<typeof loadNativeAddon>>): Promise<void> {
    // Native warm-skip is fingerprint-only. If extract meta mismatches, wipe
    // this source's rows so warm cannot skip and stale projections cannot linger
    // after attachShared stamps the new version.
    if (this.needsExtractForceReread()) {
      this.emit('progress', {
        phase: 'parsing',
        message: 'Grok extract version changed — clearing Grok index for full re-read…',
      });
      try {
        this.ingestService.open(this.dbPath);
        this.ingestService.clearSourceData();
      } finally {
        this.releaseShared();
      }
    }

    this.emit('progress', {
      phase: 'parsing',
      message: `Running native Grok ingest (${native.nativeVersion()})…`,
    });
    const stats = await native.ingest(
      {
        agentDir: this.source.rootDir,
        dbPath: this.dbPath,
        mode: 'warm',
        sourceId: 'grok',
        ...(this.safeBulk ? { safeBulk: true } : {}),
      },
      (progress) => {
        this.emit('progress', {
          phase: progress.phase === 'finalizing' ? 'storing' : 'parsing',
          message:
            progress.phase === 'scanning'
              ? 'Scanning Grok sessions…'
              : progress.phase === 'finalizing'
                ? 'Finalizing Grok index…'
                : `Ingesting Grok… ${progress.projectsDone}/${progress.projectsTotal}`,
          current: progress.projectsDone,
          total: progress.projectsTotal,
        });
      },
    );

    // A partial ingest still resolves — the failed inputs kept their retry —
    // so the result is the only record that anything went wrong.
    reportIngestErrors('grok', stats, this.errorSink);
  }

  /** Pure-TS GrokReader path. Handle must be open. */
  private readWithTypeScript(): void {
    const extractVer = this.ingestService.getMeta(GROK_EXTRACT_META_KEY);
    const sessionsDir = this.source.paths.sessionsDir;
    let forceReread = extractVer !== GROK_EXTRACT_VERSION;

    // Missing fingerprint paths (deleted on disk) → full wipe + re-read so
    // sessions/messages do not linger as orphans (mirrors native ClearSourceData).
    if (!forceReread) {
      for (const fp of this.ingestService.getAllFingerprints()) {
        if (fp.path.startsWith(sessionsDir) && !this.fileService.exists(fp.path)) {
          forceReread = true;
          break;
        }
      }
    }

    // Drop prior Grok rows so force re-read does not leave ghosts.
    // Fingerprints are cleared too; onFileSeen repopulates.
    if (forceReread) {
      this.ingestService.clearSourceData();
    }

    const reader = new GrokReader(this.fileService, sessionsDir);
    this.ingestService.beginTransaction();
    try {
      reader.readAll(this.ingestService, {
        shouldReadMessages: (file, mtimeMs) => {
          if (forceReread) return true;
          const fp = this.ingestService.getFingerprint(file);
          return !fp || fp.mtimeMs !== mtimeMs;
        },
        onFileSeen: (file, mtimeMs, size, lastByte) => {
          this.ingestService.upsertFingerprint({ path: file, mtimeMs, size, bytePosition: lastByte });
        },
      });
      if (forceReread || extractVer !== GROK_EXTRACT_VERSION) {
        this.ingestService.setMeta(GROK_EXTRACT_META_KEY, GROK_EXTRACT_VERSION);
      }
      this.ingestService.commitTransaction();
    } catch (error) {
      this.ingestService.rollbackTransaction();
      throw error;
    }
  }

  shutdown(): void {
    this.ready = false;
    void this.liveWatch?.stop();
    this.liveWatch = undefined;
  }

  async shutdownAsync(): Promise<void> {
    this.ready = false;
    if (this.liveWatch) {
      await this.liveWatch.stop();
      this.liveWatch = undefined;
    }
  }

  async rebuild(): Promise<void> {
    await this.initialize();
  }

  async rebuildIndex(): Promise<{ durationMs: number }> {
    const start = Date.now();
    await this.initialize();
    return { durationMs: Date.now() - start };
  }

  isReady(): boolean {
    return this.ready;
  }

  getStore(): AgentDataStore {
    return this.store;
  }

  getLiveWatch(): LiveWatch | undefined {
    return this.liveWatch;
  }
}
