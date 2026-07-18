/**
 * CodexLifecycleOwner — Codex's ingest-lifecycle owner (RFC 006 multi-source).
 *
 * Lives under `sources/codex/` (product code). Implements the shared
 * `LifecycleOwner` contract: ingests Codex rollouts into the SHARED store under
 * `sourceId: 'codex'`.
 *
 * ## Multi-source exclusive queue
 *
 * Participates in the three-phase protocol so **native rs is always used when
 * available** — even when Claude already warm-started the same cache:
 *
 * 1. `exclusiveIngest` — `native.ingest({ sourceId: 'codex' })` with the shared
 *    better-sqlite3 handle **closed** (MEMORY journal is safe).
 * 2. `attachShared` — open shared handle + stamp extract meta.
 * 3. `startLivePipeline` — TS live tail for Change events.
 *
 * Failures in exclusive/attach are non-fatal (emit `error`, leave Claude up).
 */

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
import { CodexReader } from './reader.js';
import { createCodexLiveWatch, type CodexLiveWatch } from './live-watch.js';

/**
 * Bump when Codex message/token extraction changes in a way that requires
 * re-reading rollouts even if mtime is unchanged. Absent or mismatched →
 * force a full Codex re-read once, then stamp the new version.
 *
 * History:
 * - token_count_v2_estimate — token attribution / tiktoken fallback
 * - first_prompt_v2_skip_injected — session list title skips environment_context /
 *   AGENTS.md / plugins; prefers event_msg user_message
 * - root_sessions_v3_human_timeline — hides child rollouts and excludes Codex
 *   developer/injected-user context from the materialized display timeline
 * - rich_records_v4_tools_reasoning — retains canonical tool call/result and
 *   readable reasoning response_items for full-session timeline projection
 */
const CODEX_EXTRACT_VERSION = 'rich_records_v4_tools_reasoning';
const CODEX_EXTRACT_META_KEY = 'codex_extract_version';

export class CodexLifecycleOwner extends EventEmitter implements LifecycleOwner {
  readonly sourceId = 'codex';
  private ready = false;
  private liveWatch: CodexLiveWatch | undefined;
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
    this.emit('progress', { phase: 'parsing', message: 'Ingesting Codex sessions…' });

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
      await this.initializeWithTypeScript();
    } finally {
      this.releaseShared();
    }
  }

  /** Phase 2 — open shared handle + stamp extract version meta. */
  async attachShared(): Promise<void> {
    this.ingestService.open(this.dbPath);
    // Native path stamps meta here (after open). TS path may have stamped already;
    // re-stamp is idempotent.
    this.ingestService.setMeta(CODEX_EXTRACT_META_KEY, CODEX_EXTRACT_VERSION);
  }

  /** Phase 3 — live tail (TS writer for Change events + token attribution). */
  async startLivePipeline(): Promise<void> {
    if (this.live) {
      if (!this.liveWatch) {
        this.liveWatch = createCodexLiveWatch({
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
   * True when extract/token behaviour changed and a full Codex re-read is
   * required even if file mtimes match. Brief open/close keeps the exclusive
   * native queue free for peers.
   */
  private needsExtractForceReread(): boolean {
    try {
      this.ingestService.open(this.dbPath);
      return this.ingestService.getMeta(CODEX_EXTRACT_META_KEY) !== CODEX_EXTRACT_VERSION;
    } catch {
      return false;
    } finally {
      this.releaseShared();
    }
  }

  /** Native Codex cold/warm (exclusive connection inside the addon). */
  private async initializeWithNative(native: NonNullable<ReturnType<typeof loadNativeAddon>>): Promise<void> {
    // Native warm-skip is fingerprint-only. Wipe this source on extract meta
    // mismatch so warm cannot skip and attachShared does not stamp a new
    // version over stale projections.
    if (this.needsExtractForceReread()) {
      this.emit('progress', {
        phase: 'parsing',
        message: 'Codex extract version changed — clearing Codex index for full re-read…',
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
      message: `Running native Codex ingest (${native.nativeVersion()})…`,
    });
    await native.ingest(
      {
        agentDir: this.source.rootDir,
        dbPath: this.dbPath,
        mode: 'warm',
        sourceId: 'codex',
        ...(this.safeBulk ? { safeBulk: true } : {}),
      },
      (progress) => {
        this.emit('progress', {
          phase: progress.phase === 'finalizing' ? 'storing' : 'parsing',
          message:
            progress.phase === 'scanning'
              ? 'Scanning Codex sessions…'
              : progress.phase === 'finalizing'
                ? 'Finalizing Codex index…'
                : `Ingesting Codex… ${progress.projectsDone}/${progress.projectsTotal}`,
          current: progress.projectsDone,
          total: progress.projectsTotal,
        });
      },
    );
  }

  /** TypeScript CodexReader path (when native unavailable). Handle must be open. */
  private async initializeWithTypeScript(): Promise<void> {
    const extractVer = this.ingestService.getMeta(CODEX_EXTRACT_META_KEY);
    const sessionsDir = this.source.paths.sessionsDir;
    let forceReread = extractVer !== CODEX_EXTRACT_VERSION;

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

    if (forceReread) {
      this.ingestService.clearSourceData();
    }

    const reader = new CodexReader(this.fileService, sessionsDir);
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
      if (forceReread || extractVer !== CODEX_EXTRACT_VERSION) {
        this.ingestService.setMeta(CODEX_EXTRACT_META_KEY, CODEX_EXTRACT_VERSION);
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
