/**
 * ClaudeCodeLifecycleOwner — Claude Code's ingest-lifecycle owner.
 *
 * Lives under `sources/claude-code/` (product code). Implements the shared
 * `LifecycleOwner` contract from `data/lifecycle-owner.ts`. Cold/warm start,
 * engine selection (rs/ts), progress events, and live start/stop; reads
 * delegate to the shared `AgentDataStore`.
 *
 * Formerly `data/lifecycle-owner.ts` / `AgentDataServiceImpl`. Compat
 * re-exports remain in `data/agent-data-service.ts` and `data/lifecycle-owner.ts`.
 */

import * as os from 'node:os';
import * as path from 'node:path';
import { existsSync, mkdirSync } from 'node:fs';
import { EventEmitter } from 'events';
import type {
  SegmentType,
  SegmentKey,
  Segment,
  SegmentChangeBatch,
  InitProgress,
  PaginatedSegmentQuery,
  PaginatedSegmentResult,
  SearchQuery,
  SearchResultSet,
  StoreStats,
} from '../../data/segment-types.js';
import type { SessionSummaryData, ProjectSummaryData } from '../../data/summary-types.js';
import type {
  Project,
  Session,
  SessionMessage,
  AgentConfig,
  AgentAnalytic,
  SessionsIndex,
  SessionIndexEntry,
  SubagentTranscript,
  PersistedToolResult,
  FileHistorySession,
  TodoFile,
  TaskEntry,
  PlanFile,
  WorkflowRun,
} from '../../types/index.js';
import type { QueryService } from '../../data/query-service.js';
import type { IngestService } from '../../data/ingest-service.js';
import type { AgentDataStore } from '../../data/agent-data-store.js';
import type { AgentDataService, AgentDataServiceOptions, LifecycleOwner } from '../../data/lifecycle-owner.js';
import type {
  SubagentTimelinePage,
  SubagentTimelinePageRequest,
  TimelineFacets,
  TimelinePage,
  TimelinePageRequest,
} from '../../data/timeline-query.js';
import type { ClaudeCodeParser } from './parser/claude-code-parser.js';
import type { FileService } from '../../io/index.js';
import type { ClaudeCodeLiveUpdates } from './live/live-updates.js';
import type { LiveWatch } from '../../live/live-watch.js';
import { createWorkerPool, isWorkerThreadsAvailable, type WorkerToMainMessage } from '../../workers/index.js';
import { loadNativeAddon } from '../../native.js';
import { defaultDbPathForEngine, resolveEngine, type IngestEngine } from '../../settings.js';

// ═══════════════════════════════════════════════════════════════════════════
// IMPLEMENTATION
// ═══════════════════════════════════════════════════════════════════════════

function getDefaultDbPath(engine: IngestEngine): string {
  // Each ingest engine keeps its own DB file so switching engines
  // doesn't force a re-ingest, and results are comparable side-by-side.
  return defaultDbPathForEngine(engine);
}

/** Point-in-time stat of one fingerprintable source file (see `snapshotSourceStats`). */
interface SourceStatSnapshot {
  mtimeMs: number;
  size: number;
  isJsonl: boolean;
}

export class ClaudeCodeLifecycleOwner extends EventEmitter implements AgentDataService, LifecycleOwner {
  /** RFC 006: the source this owner ingests for. Claude Code today. */
  readonly sourceId = 'claude-code';
  private fileService: FileService;
  private parser: ClaudeCodeParser;
  private queryService: QueryService;
  private ingestService: IngestService;
  private store: AgentDataStore;
  private options: AgentDataServiceOptions;
  /**
   * RFC 005 C2.7: the live-updates orchestrator, composed in `create.ts`
   * only when the caller opted in via `SpaghettiServiceOptions.live`.
   * `undefined` means "no live pipeline" — `initialize()` / `shutdown()`
   * skip the start/stop calls and the service behaves identically to
   * the pre-RFC-005 build.
   */
  private liveUpdates: ClaudeCodeLiveUpdates | undefined;

  private ready = false;
  private dbPath: string;
  private rootDir: string;
  /**
   * Engine selected for this service instance — explicit option if the
   * caller provided one, otherwise the resolution chain in
   * [`resolveEngine`](../settings.ts) (env vars → persisted config →
   * default `rs`). Fixed at construction time so every `initialize()` and
   * `rebuildIndex()` on this instance picks the same path.
   */
  private engine: IngestEngine;

  constructor(
    fileService: FileService,
    parser: ClaudeCodeParser,
    queryService: QueryService,
    ingestService: IngestService,
    store: AgentDataStore,
    options?: AgentDataServiceOptions,
    liveUpdates?: ClaudeCodeLiveUpdates,
  ) {
    super();
    this.fileService = fileService;
    this.parser = parser;
    this.queryService = queryService;
    this.ingestService = ingestService;
    this.store = store;
    this.options = options ?? {};
    this.liveUpdates = liveUpdates;
    this.engine = this.options.engine ?? resolveEngine();
    this.dbPath = this.options.dbPath ?? getDefaultDbPath(this.engine);
    this.rootDir = this.options.rootDir ?? this.options.claudeDir ?? path.join(os.homedir(), '.claude');
  }

  // ─────────────────────────────────────────────────────────────────────────
  // Lifecycle — multi-source exclusive queue phases
  // ─────────────────────────────────────────────────────────────────────────

  /**
   * Solo path: exclusiveIngest → attachShared → startLivePipeline.
   * Multi-source coordinators call the three phases separately so every
   * agent gets exclusive native access in series.
   */
  async initialize(): Promise<void> {
    const startTime = Date.now();
    try {
      await this.exclusiveIngest();
      await this.attachShared();
      await this.startLivePipeline();
      this.ready = true;
      this.emit('ready', { durationMs: Date.now() - startTime });
    } catch (error) {
      this.emit('error', { error: error instanceof Error ? error.message : String(error) });
      throw error;
    }
  }

  /**
   * Phase 1 — exclusive cold/warm. Leaves shared better-sqlite3 **closed**
   * so a subsequent owner can also run native bulk (`journal_mode=MEMORY`).
   */
  async exclusiveIngest(): Promise<void> {
    this.ready = false;
    const dbDir = path.dirname(this.dbPath);
    if (!existsSync(dbDir)) {
      mkdirSync(dbDir, { recursive: true });
    }

    // Ensure we do not hold the file open across exclusive native turns.
    this.releaseShared();

    const native = this.engine === 'rs' ? loadNativeAddon() : null;
    if (native) {
      await this.initializeWithNative(native);
      // Native owns its own connection and closes it — shared handle stays closed.
    } else {
      // TS path needs the handle during ingest, then must release it.
      this.emitProgress('parsing', 'Opening database...');
      this.queryService.open(this.dbPath);
      this.ingestService.open(this.dbPath);
      try {
        await this.initializeWithTypeScript();
      } finally {
        this.releaseShared();
      }
    }
  }

  /**
   * Phase 2 — open shared handle + config/analytics (not covered by native).
   */
  async attachShared(): Promise<void> {
    this.queryService.open(this.dbPath);
    this.ingestService.open(this.dbPath);

    this.emitProgress('parsing', 'Parsing config and analytics...');
    const fullData = this.parser.parseSync({
      rootDir: this.rootDir,
      skipProjects: true,
      skipSessionMessages: true,
    });
    this.store.setConfig(fullData.config);
    this.store.setAnalytics(fullData.analytics);
  }

  /** Phase 3 — Plane 2 live (opt-in). Marks the owner ready for multi-source. */
  async startLivePipeline(): Promise<void> {
    if (this.liveUpdates) {
      try {
        await this.liveUpdates.start();
      } catch (err) {
        this.emit('error', { error: err instanceof Error ? err.message : String(err) });
      }
    }
    this.ready = true;
  }

  releaseShared(): void {
    try {
      this.ingestService.close();
    } catch {
      /* ignore */
    }
    try {
      this.queryService.close();
    } catch {
      /* ignore */
    }
  }

  /** Delete the shared cache file so the next exclusiveIngest is a full cold start. */
  wipeCache(): void {
    this.releaseShared();
    for (const suffix of ['', '-wal', '-shm', '-journal']) {
      const p = this.dbPath + suffix;
      if (existsSync(p)) {
        try {
          // eslint-disable-next-line @typescript-eslint/no-require-imports
          const fs = require('node:fs') as typeof import('node:fs');
          fs.rmSync(p, { force: true });
        } catch {
          /* best-effort */
        }
      }
    }
  }

  getCacheDbPath(): string {
    return this.dbPath;
  }

  /**
   * Native-ingest path: exclusive Rust connection only — does **not** open
   * better-sqlite3. {@link attachShared} opens the shared handle afterwards.
   */
  private async initializeWithNative(native: NonNullable<ReturnType<typeof loadNativeAddon>>): Promise<void> {
    this.emitProgress('parsing', `Running native ingest (${native.nativeVersion()})...`);

    await native.ingest(
      {
        agentDir: this.rootDir,
        dbPath: this.dbPath,
        mode: 'warm',
        sourceId: 'claude-code',
        ...(this.options.safeBulk ? { safeBulk: true } : {}),
      },
      (progress) => {
        switch (progress.phase) {
          case 'scanning':
            this.emitProgress(
              'parsing',
              `Scanning ${progress.projectsTotal} projects...`,
              progress.projectsDone,
              progress.projectsTotal,
            );
            break;
          case 'parsing':
            this.emitProgress(
              'parsing',
              `Parsing projects... ${progress.projectsDone}/${progress.projectsTotal}`,
              progress.projectsDone,
              progress.projectsTotal,
            );
            break;
          case 'finalizing':
            this.emitProgress('storing', 'Writing fingerprints...', progress.projectsDone, progress.projectsTotal);
            break;
        }
      },
    );
  }

  /**
   * schema_meta marker for the one-shot msg_index heal. Present on every
   * DB born or repaired after the incremental-index fix landed.
   */
  private static readonly MSG_INDEX_HEAL_KEY = 'heal_msg_index_v1';

  /**
   * Fallback TS-ingest path. Caller must open the DB first and is responsible
   * for closing it after exclusiveIngest when multi-source needs a free file.
   */
  private async initializeWithTypeScript(): Promise<void> {
    const fingerprints = this.ingestService.getAllFingerprints();
    const isColdStart = fingerprints.length === 0;

    if (isColdStart) {
      await this.performColdStart();
      this.ingestService.setMeta(ClaudeCodeLifecycleOwner.MSG_INDEX_HEAL_KEY, '1');
    } else if (this.ingestService.getMeta(ClaudeCodeLifecycleOwner.MSG_INDEX_HEAL_KEY) === null) {
      // One-shot heal: releases before the incremental msg_index fix
      // overwrote the head of active sessions with appended messages.
      await this.warmStartFullReparse('Healing message indexes from a previous version...');
      this.ingestService.setMeta(ClaudeCodeLifecycleOwner.MSG_INDEX_HEAL_KEY, '1');
    } else {
      await this.performWarmStart(fingerprints);
    }
  }

  /**
   * Force a full cold rebuild (solo). Multi-source rebuild uses the
   * coordinator wipe + exclusive queue so every agent re-ingests with rs.
   */
  async rebuildIndex(): Promise<{ durationMs: number }> {
    const start = Date.now();
    this.ready = false;
    this.wipeCache();
    await this.initialize();
    return { durationMs: Date.now() - start };
  }

  private async performColdStart(): Promise<void> {
    // Snapshot source-file stats BEFORE parsing. Fingerprints must
    // record at most what the parser could have consumed — stat-ing at
    // save time instead would stamp bytes appended mid-parse as
    // "already ingested" and silently skip them on every later warm
    // start (TOCTOU).
    const snapshot = this.snapshotSourceStats();

    // Discover project slugs to decide on parallel vs sequential
    const slugs = this.discoverProjectSlugs();

    // Enable bulk ingest optimizations: disable FTS triggers and use
    // aggressive SQLite PRAGMAs for maximum write throughput.
    this.ingestService.beginBulkIngest();

    try {
      if (slugs.length >= 4 && isWorkerThreadsAvailable()) {
        try {
          await this.coldStartParallel(slugs);
        } catch {
          // Worker threads may fail in bundled environments (e.g., tsup inlines
          // the worker script as a data URL which isn't a valid worker path).
          // Fall back to sequential parsing gracefully.
          this.emitProgress('parsing', 'Workers unavailable, falling back to sequential...');
          await this.coldStartSequential();
        }
      } else {
        await this.coldStartSequential();
      }
    } finally {
      // Restore FTS triggers, rebuild FTS index, restore safe PRAGMAs
      this.ingestService.endBulkIngest();
    }

    // Save fingerprints for all session JSONL files we can find
    this.emitProgress('storing', 'Saving file fingerprints...');
    this.saveAllFingerprints(snapshot);

    this.emitProgress('indexing', 'Cold start complete.');
  }

  private async coldStartSequential(): Promise<void> {
    // Discover slugs to report progress count
    const slugs = this.discoverProjectSlugs();
    const totalProjects = slugs.length;

    this.emitProgress('parsing', `Parsing ${totalProjects} projects...`, 0, totalProjects);

    // Parse project by project, yielding the event loop between each so
    // consumers (e.g. Ink TUI) can re-render progress updates.
    // Previously this was a single blocking parseStreaming() call that
    // starved the event loop for the entire duration.
    this.ingestService.beginTransaction();
    try {
      for (let i = 0; i < slugs.length; i++) {
        const slug = slugs[i];
        this.parser.parseProjectStreaming(this.rootDir, slug, this.ingestService);
        this.ingestService.onProjectComplete(slug);
        this.emitProgress('parsing', `Parsed ${slug}`, i + 1, totalProjects);

        // Yield to the event loop so UI can render progress updates
        if (i < slugs.length - 1) {
          await new Promise<void>((resolve) => setImmediate(resolve));
        }
      }
      this.ingestService.commitTransaction();
    } catch (error) {
      this.ingestService.rollbackTransaction();
      throw error;
    }
  }

  private async coldStartParallel(slugs: string[]): Promise<void> {
    let completedProjects = 0;
    const totalProjects = slugs.length;
    this.emitProgress('parsing', `Parsing ${totalProjects} projects...`, 0, totalProjects);

    const pool = createWorkerPool();

    this.ingestService.beginTransaction();

    try {
      await pool.parseProjects(this.rootDir, slugs, (msg: WorkerToMainMessage) => {
        // Route each message type to the appropriate IngestService method.
        // Workers send pre-serialized JSON strings — we parse them on the main thread
        // and call the existing sink methods to reuse all extraction logic.
        switch (msg.type) {
          case 'project-result': {
            const sessionsIndex = JSON.parse(msg.sessionsIndexJson) as SessionsIndex;
            this.ingestService.onProject(msg.slug, msg.originalPath, sessionsIndex);
            break;
          }
          case 'project-memory': {
            this.ingestService.onProjectMemory(msg.slug, msg.content);
            break;
          }
          case 'session-result': {
            const entry = JSON.parse(msg.indexEntryJson) as SessionIndexEntry;
            this.ingestService.onSession(msg.slug, entry);
            break;
          }
          case 'message-batch': {
            // Each message in the batch is a JSON string — parse and insert
            for (let i = 0; i < msg.messages.length; i++) {
              const message = JSON.parse(msg.messages[i]) as SessionMessage;
              const index = msg.startIndex + i;
              const byteOffset = msg.byteOffsets[i];
              this.ingestService.onMessage(msg.slug, msg.sessionId, message, index, byteOffset);
            }
            break;
          }
          case 'subagent-result': {
            const messages = JSON.parse(msg.messagesJson) as SessionMessage[];
            const transcript: SubagentTranscript = {
              agentId: msg.agentId,
              agentType: msg.agentType as SubagentTranscript['agentType'],
              fileName: msg.fileName,
              messages,
              workflowId: msg.workflowId,
            };
            this.ingestService.onSubagent(msg.slug, msg.sessionId, transcript);
            break;
          }
          case 'workflow-result': {
            const workflow = JSON.parse(msg.workflowJson) as WorkflowRun;
            this.ingestService.onWorkflow(msg.slug, msg.sessionId, workflow);
            break;
          }
          case 'tool-result': {
            const toolResult: PersistedToolResult = {
              toolUseId: msg.toolUseId,
              content: msg.content,
            };
            this.ingestService.onToolResult(msg.slug, msg.sessionId, toolResult);
            break;
          }
          case 'file-history': {
            const history = JSON.parse(msg.dataJson) as FileHistorySession;
            this.ingestService.onFileHistory(msg.sessionId, history);
            break;
          }
          case 'todo-result': {
            const items = JSON.parse(msg.itemsJson) as TodoFile['items'];
            const todo: TodoFile = {
              sessionId: msg.sessionId,
              agentId: msg.agentId,
              items,
            };
            this.ingestService.onTodo(msg.sessionId, todo);
            break;
          }
          case 'task-result': {
            const task = JSON.parse(msg.taskJson) as TaskEntry;
            this.ingestService.onTask(msg.sessionId, task);
            break;
          }
          case 'plan-result': {
            const plan: PlanFile = {
              slug: msg.slug,
              title: msg.title,
              content: msg.content,
              size: msg.size,
            };
            this.ingestService.onPlan(msg.slug, plan);
            break;
          }
          case 'session-complete': {
            this.ingestService.onSessionComplete(msg.slug, msg.sessionId, msg.messageCount, msg.lastBytePosition);
            break;
          }
          case 'project-complete': {
            this.ingestService.onProjectComplete(msg.slug);
            completedProjects++;
            this.emitProgress('parsing', `Parsed ${msg.slug}`, completedProjects, totalProjects);
            break;
          }
          case 'worker-error': {
            console.error(`[cold-start] Worker error for project "${msg.slug}": ${msg.error}`);
            break;
          }
        }
      });

      this.ingestService.commitTransaction();
    } catch (error) {
      this.ingestService.rollbackTransaction();
      throw error;
    } finally {
      pool.shutdown();
    }
  }

  /**
   * Discover all project slugs from the claude directory.
   */
  private discoverProjectSlugs(): string[] {
    const projectsDir = path.join(this.rootDir, 'projects');
    try {
      const projectPaths = this.fileService.scanDirectorySync(projectsDir, {
        directoriesOnly: true,
      });
      return projectPaths.map((p) => path.basename(p));
    } catch {
      return [];
    }
  }

  private async performWarmStart(
    existingFingerprints: Array<{ path: string; mtimeMs: number; size: number; bytePosition?: number }>,
  ): Promise<void> {
    this.emitProgress('reconciling', 'Warm start: checking for changes...');

    // Stat snapshot BEFORE any parsing — fingerprints saved at the end
    // of this pass stamp these values, never fresher ones (see
    // performColdStart for the TOCTOU rationale).
    const snapshot = this.snapshotSourceStats();

    // Build a lookup map from path → fingerprint for efficient access
    const fpMap = new Map<string, { path: string; mtimeMs: number; size: number; bytePosition?: number }>();
    for (const fp of existingFingerprints) {
      fpMap.set(fp.path, fp);
    }

    // Check which files have changed since last parse
    // Skip recovery:// fingerprints — those track imported legacy data
    const changedFiles: string[] = [];
    const removedFiles: string[] = [];
    // Track JSONL files that only grew (appended) — eligible for incremental parse
    const grownFiles: Array<{ path: string; oldSize: number; oldBytePosition: number }> = [];

    for (const fp of existingFingerprints) {
      if (fp.path.startsWith('recovery://')) continue;
      const stats = this.fileService.getStats(fp.path);
      if (!stats) {
        removedFiles.push(fp.path);
      } else if (stats.mtimeMs !== fp.mtimeMs || stats.size !== fp.size) {
        // Detect append-only growth: mtime changed, size grew, and we have a byte position
        if (
          fp.path.endsWith('.jsonl') &&
          stats.size > fp.size &&
          fp.bytePosition !== undefined &&
          fp.bytePosition > 0
        ) {
          grownFiles.push({ path: fp.path, oldSize: fp.size, oldBytePosition: fp.bytePosition });
        } else {
          changedFiles.push(fp.path);
        }
      }
    }

    // Also detect new JSONL files on disk that we don't have fingerprints for
    const newFiles: string[] = [];
    const projectsDir = path.join(this.rootDir, 'projects');
    try {
      const projectPaths = this.fileService.scanDirectorySync(projectsDir, { directoriesOnly: true });
      for (const projectPath of projectPaths) {
        try {
          const files = this.fileService.scanDirectorySync(projectPath, { pattern: '*.jsonl' });
          for (const filePath of files) {
            if (!fpMap.has(filePath)) {
              newFiles.push(filePath);
            }
          }
        } catch {
          // skip bad project directory
        }
      }
    } catch {
      // projects dir doesn't exist
    }

    // Recovery check: detect projects that have sessions in the DB but 0
    // messages.  This happens when a previous cold start silently failed
    // to parse JSONL files (e.g. stale sessions-index.json).  If we find
    // any, force a full re-parse to recover the lost data.
    //
    // Separate recovery check: detect ORPHANED message rows — messages
    // whose `project_slug` has no matching row in `projects`. That
    // shape was created by the pre-fix `fullParseNewJsonl` (now
    // removed) which emitted `onMessage` without `onProject` /
    // `onSession`. Those orphans need a targeted `parseProjectStreaming`
    // per slug to materialise the missing parent rows. We treat them
    // like `newFiles` so they flow through the incremental path
    // alongside legitimately new projects.
    const orphanedSlugs = this.detectOrphanedProjectSlugs();
    let needsRecovery = false;
    const hasNoChanges =
      changedFiles.length === 0 &&
      removedFiles.length === 0 &&
      grownFiles.length === 0 &&
      newFiles.length === 0 &&
      orphanedSlugs.length === 0;
    if (hasNoChanges) {
      needsRecovery = this.hasProjectsWithMissingMessages();
      if (!needsRecovery) {
        this.emitProgress('reconciling', 'No changes detected, using cached data.');
        return;
      }
      this.emitProgress('reconciling', 'Detected projects with 0 messages — triggering recovery re-parse...');
    }

    if (needsRecovery) {
      // Full re-parse needed for recovery
      await this.warmStartFullReparse('Recovery re-parse: fixing projects with missing messages...', snapshot);
      return;
    }

    if (orphanedSlugs.length > 0) {
      this.emitProgress(
        'reconciling',
        `Recovering ${orphanedSlugs.length} project(s) with orphaned messages (no parent project row)...`,
      );
    }

    // If only JSONL files grew (most common warm-start scenario: active session
    // appended new messages), do incremental parsing instead of full re-parse.
    // We also handle new files by doing a full parse of just those sessions.
    if (changedFiles.length === 0 && removedFiles.length === 0) {
      // Dedupe new JSONL files by project slug — `parseProjectStreaming`
      // walks a whole project slug at once (sessions-index + all
      // sessions + memory + subagents + tool-results), so if a new
      // project has three new JSONL files we only need ONE call per
      // slug, not three. The writer is upsert-by-PK so running it
      // against a partially-populated project (e.g. existing project,
      // one new session) is idempotent. Orphaned slugs (messages with
      // no parent `projects` row) ride the same path — one pass
      // materialises their missing parent + session rows.
      const newProjectSlugs = [...new Set([...this.collectNewProjectSlugs(newFiles), ...orphanedSlugs])];
      const totalFiles = grownFiles.length + newProjectSlugs.length;
      this.emitProgress(
        'parsing',
        `Incremental update: ${grownFiles.length} grown files, ${newProjectSlugs.length} new projects...`,
        0,
        totalFiles,
      );

      this.ingestService.beginTransaction();
      try {
        let processed = 0;

        // Incrementally parse appended data from grown files
        for (const gf of grownFiles) {
          this.incrementalParseJsonl(gf.path, gf.oldBytePosition);
          processed++;
          this.emitProgress('parsing', `Incremental: ${path.basename(gf.path)}`, processed, totalFiles);
        }

        // New JSONL files get the full parser pass per project slug so
        // `onProject` + `onSession` + `onMessage` all land. The previous
        // path (`fullParseNewJsonl`) only emitted `onMessage`, leaving
        // orphaned rows with no `projects`/`sessions` parent — projects
        // never appeared in the UI even though their messages were
        // ingested.
        for (const slug of newProjectSlugs) {
          this.parser.parseProjectStreaming(this.rootDir, slug, this.ingestService);
          this.ingestService.onProjectComplete(slug);
          processed++;
          this.emitProgress('parsing', `New: ${slug}`, processed, totalFiles);
        }

        this.ingestService.commitTransaction();
      } catch (error) {
        this.ingestService.rollbackTransaction();
        throw error;
      }

      // Update fingerprints for changed files
      this.saveAllFingerprints(snapshot);
      this.emitProgress('indexing', 'Incremental warm start complete.');
      return;
    }

    // Files were modified in a non-append way or removed — determine which
    // projects are affected and only re-parse those.
    const affectedSlugs = this.getAffectedProjectSlugs(changedFiles, removedFiles);
    if (affectedSlugs.length === 0) {
      // Edge case: changed files couldn't be mapped to projects. Do full re-parse.
      await this.warmStartFullReparse(
        `Re-parsing: ${changedFiles.length} changed, ${removedFiles.length} removed files...`,
        snapshot,
      );
      return;
    }

    this.emitProgress(
      'parsing',
      `Re-parsing ${affectedSlugs.length} affected projects (${changedFiles.length} changed, ${removedFiles.length} removed)...`,
    );

    this.ingestService.beginTransaction();
    try {
      for (let i = 0; i < affectedSlugs.length; i++) {
        const slug = affectedSlugs[i];
        this.parser.parseProjectStreaming(this.rootDir, slug, this.ingestService);
        this.ingestService.onProjectComplete(slug);
        this.emitProgress('parsing', `Parsed ${slug}`, i + 1, affectedSlugs.length);

        if (i < affectedSlugs.length - 1) {
          await new Promise<void>((resolve) => setImmediate(resolve));
        }
      }

      // Also handle grown/new files from other projects
      for (const gf of grownFiles) {
        this.incrementalParseJsonl(gf.path, gf.oldBytePosition);
      }
      // New JSONL files + orphaned slugs: parse per unique project
      // slug via the full parser so new projects get their `projects`
      // + `sessions` rows (`fullParseNewJsonl` only wrote `messages`
      // and left the parent rows missing). Skip slugs already covered
      // by `affectedSlugs` above — those just got a full re-parse.
      const affected = new Set(affectedSlugs);
      const newProjectSlugs = [...new Set([...this.collectNewProjectSlugs(newFiles), ...orphanedSlugs])].filter(
        (s) => !affected.has(s),
      );
      for (const slug of newProjectSlugs) {
        this.parser.parseProjectStreaming(this.rootDir, slug, this.ingestService);
        this.ingestService.onProjectComplete(slug);
      }

      this.ingestService.commitTransaction();
    } catch (error) {
      this.ingestService.rollbackTransaction();
      throw error;
    }

    this.saveAllFingerprints(snapshot);
    this.emitProgress('indexing', 'Warm start complete.');
  }

  /**
   * Full re-parse of all projects — used as fallback for recovery or when
   * changes can't be handled incrementally. Uses parallel workers when available.
   *
   * `snapshot` is the pre-parse stat snapshot to stamp fingerprints from;
   * callers that haven't taken one yet (e.g. the one-shot heal) get a
   * fresh snapshot taken before any parsing starts.
   */
  private async warmStartFullReparse(
    message: string,
    snapshot: Map<string, SourceStatSnapshot> = this.snapshotSourceStats(),
  ): Promise<void> {
    this.emitProgress('parsing', message);

    const slugs = this.discoverProjectSlugs();
    const totalProjects = slugs.length;

    // Enable bulk ingest optimizations for full re-parse
    this.ingestService.beginBulkIngest();

    try {
      // Use parallel parsing for full re-parse when beneficial
      if (slugs.length >= 4 && isWorkerThreadsAvailable()) {
        try {
          await this.coldStartParallel(slugs);
          this.saveAllFingerprints(snapshot);
          this.emitProgress('indexing', 'Warm start full re-parse complete.');
          return;
        } catch {
          this.emitProgress('parsing', 'Workers unavailable, falling back to sequential...');
        }
      }

      this.ingestService.beginTransaction();
      try {
        for (let i = 0; i < slugs.length; i++) {
          const slug = slugs[i];
          this.parser.parseProjectStreaming(this.rootDir, slug, this.ingestService);
          this.ingestService.onProjectComplete(slug);
          this.emitProgress('parsing', `Parsed ${slug}`, i + 1, totalProjects);

          if (i < slugs.length - 1) {
            await new Promise<void>((resolve) => setImmediate(resolve));
          }
        }
        this.ingestService.commitTransaction();
      } catch (error) {
        this.ingestService.rollbackTransaction();
        throw error;
      }
    } finally {
      this.ingestService.endBulkIngest();
    }

    this.saveAllFingerprints(snapshot);
  }

  /**
   * Extract project slug and session ID from a JSONL file path.
   * Path format: <rootDir>/projects/<slug>/<sessionId>.jsonl
   */
  private extractProjectInfo(filePath: string): { slug: string; sessionId: string } | null {
    const parts = filePath.split(path.sep);
    const fileName = parts[parts.length - 1];
    const slug = parts[parts.length - 2];
    const sessionId = fileName.replace('.jsonl', '');
    if (!slug || !sessionId) return null;
    return { slug, sessionId };
  }

  /**
   * Incrementally parse new lines appended to a JSONL file from a given byte position.
   *
   * The streaming reader's line index restarts at 0 when resuming from a
   * byte position, and `messages` upserts on `(session_id, msg_index)` —
   * so appended rows MUST be based at the session's next index or they
   * overwrite the head of the session. Fingerprints are stamped by the
   * caller's end-of-pass `saveAllFingerprints(snapshot)`, never here.
   */
  private incrementalParseJsonl(filePath: string, fromBytePosition: number): void {
    const info = this.extractProjectInfo(filePath);
    if (!info) return;
    const { slug, sessionId } = info;

    const baseIndex = this.ingestService.getNextMessageIndex(sessionId);
    try {
      this.fileService.readJsonlStreaming<SessionMessage>(
        filePath,
        (message, index, byteOffset) => {
          this.ingestService.onMessage(slug, sessionId, message, baseIndex + index, byteOffset);
        },
        { fromBytePosition },
      );
    } catch {
      // File read failed — skip
    }
  }

  /**
   * Return project slugs that have rows in `messages` but no row in
   * `projects`. This shape was left behind by the pre-fix
   * `fullParseNewJsonl` path (which emitted `onMessage` without
   * `onProject`/`onSession`) — the messages exist but the parent row
   * needed by `getProjectSummaries()` is missing, so the project is
   * invisible in the UI. The warm-start path treats each orphan slug
   * like a new project and re-runs `parseProjectStreaming` to
   * materialise the missing parent rows (upsert on hit, create on
   * miss, idempotent either way).
   *
   * Cheap probe via a single LEFT JOIN on the indexed slug columns —
   * runs every warm-start, returns `[]` once historical orphans have
   * been healed.
   */
  private detectOrphanedProjectSlugs(): string[] {
    try {
      return this.queryService.getOrphanedMessageProjectSlugs();
    } catch {
      return [];
    }
  }

  /**
   * Collect unique project slugs from a list of new-JSONL paths.
   *
   * Used by the incremental warm-start path to route new files through
   * `parser.parseProjectStreaming(slug)` once per project instead of
   * per file — that full-project pass emits `onProject` + `onSession`
   * + `onMessage` in one go, which is what brand-new projects need
   * (and is a no-op upsert for existing ones). The previous per-file
   * path (`fullParseNewJsonl`) only emitted `onMessage`, leaving the
   * `projects` and `sessions` rows missing — invisible in the UI.
   */
  private collectNewProjectSlugs(newFiles: string[]): string[] {
    const slugs = new Set<string>();
    for (const filePath of newFiles) {
      const info = this.extractProjectInfo(filePath);
      if (info) slugs.add(info.slug);
    }
    return [...slugs];
  }

  /**
   * Determine which project slugs are affected by the changed/removed files.
   */
  private getAffectedProjectSlugs(changedFiles: string[], removedFiles: string[]): string[] {
    const affected = new Set<string>();
    const projectsDir = path.join(this.rootDir, 'projects');

    for (const filePath of [...changedFiles, ...removedFiles]) {
      // Extract slug from path: <rootDir>/projects/<slug>/...
      if (filePath.startsWith(projectsDir)) {
        const relative = filePath.substring(projectsDir.length + 1);
        const slug = relative.split(path.sep)[0];
        if (slug) affected.add(slug);
      }
    }

    return [...affected];
  }

  /**
   * Check whether any project in the DB has sessions but zero messages.
   * This indicates a previous cold start failed to parse JSONL files and
   * the data needs recovery.  We also verify that the project actually has
   * JSONL files on disk — projects with no JSONL files are legitimately
   * empty and don't need re-parsing.
   */
  private hasProjectsWithMissingMessages(): boolean {
    try {
      const summaries = this.store.getProjectSummaries();
      for (const summary of summaries) {
        if (summary.sessionCount > 0 && summary.messageCount === 0) {
          // Verify there are actually JSONL files on disk for this project
          const projectDir = path.join(this.rootDir, 'projects', summary.slug);
          try {
            const files = this.fileService.scanDirectorySync(projectDir, { pattern: '*.jsonl' });
            if (files.length > 0) {
              return true;
            }
          } catch {
            // can't read project dir — skip
          }
        }
      }
    } catch {
      // query service not ready — skip
    }
    return false;
  }

  /**
   * Stat every fingerprintable source file (top-level session `*.jsonl`
   * plus `sessions-index.json` per project). Taken BEFORE a parse pass;
   * `saveAllFingerprints` stamps these values so a fingerprint never
   * claims bytes the parser could not have consumed. Files that appear
   * after the snapshot stay unfingerprintd and are picked up as "new"
   * on the next warm start.
   */
  private snapshotSourceStats(): Map<string, SourceStatSnapshot> {
    const snapshot = new Map<string, SourceStatSnapshot>();
    const projectsDir = path.join(this.rootDir, 'projects');

    try {
      const projectPaths = this.fileService.scanDirectorySync(projectsDir, { directoriesOnly: true });

      for (const projectPath of projectPaths) {
        try {
          const files = this.fileService.scanDirectorySync(projectPath, { pattern: '*.jsonl' });
          for (const filePath of files) {
            const stats = this.fileService.getStats(filePath);
            if (stats) {
              snapshot.set(filePath, { mtimeMs: stats.mtimeMs, size: stats.size, isJsonl: true });
            }
          }

          const indexPath = path.join(projectPath, 'sessions-index.json');
          const indexStats = this.fileService.getStats(indexPath);
          if (indexStats) {
            snapshot.set(indexPath, { mtimeMs: indexStats.mtimeMs, size: indexStats.size, isJsonl: false });
          }
        } catch {
          // skip bad project directory
        }
      }
    } catch {
      // projects dir doesn't exist
    }

    return snapshot;
  }

  private saveAllFingerprints(snapshot: Map<string, SourceStatSnapshot>): void {
    for (const [filePath, stat] of snapshot) {
      // For JSONL files the size doubles as bytePosition so incremental
      // parsing can resume where this pass left off on the next warm start.
      this.ingestService.upsertFingerprint({
        path: filePath,
        mtimeMs: stat.mtimeMs,
        size: stat.size,
        bytePosition: stat.isJsonl ? stat.size : undefined,
      });
    }
  }

  /**
   * Awaitable teardown. Used by `api[Symbol.asyncDispose]` (C3.4) so
   * callers on `await using` semantics can flush in-flight live
   * writes and close the subscriber registry before returning.
   *
   * Sequence (RFC 005 §4): stop watchers first (no new events),
   * await the writer loop (in-flight writeBatch completes), flush
   * checkpoints, dispose the registry, close SQLite.
   */
  async shutdownAsync(): Promise<void> {
    this.ready = false;

    if (this.liveUpdates) {
      try {
        await this.liveUpdates.stop();
      } catch {
        /* best-effort teardown */
      }
    }

    // Tear down any straggling subscribers now that no more events
    // can be emitted. The concrete store exposes `disposeRegistry`
    // via the class, not the interface — cast locally.
    const store = this.store as AgentDataStore & { disposeRegistry?: () => void };
    if (typeof store.disposeRegistry === 'function') {
      try {
        store.disposeRegistry();
      } catch {
        /* ignore */
      }
    }

    try {
      this.ingestService.close();
    } catch {
      /* ignore */
    }
    try {
      this.queryService.close();
    } catch {
      /* ignore */
    }
  }

  shutdown(): void {
    this.ready = false;
    // Config/analytics caches now live on `AgentDataStore`. The store
    // outlives `shutdown()` in the current wiring (both are owned by
    // the same lifecycle), so we let its cached snapshots remain — the
    // next `initialize()` will overwrite them via `setConfig/Analytics`.

    // RFC 005 C2.7: stop the live-updates pipeline BEFORE closing the
    // SQLite connections so no in-flight `writeBatch` hits a closed
    // handle. The service's `shutdown()` contract is sync, but the
    // orchestrator's `stop()` is async (watcher unsubscribe + writer-
    // loop drain + final checkpoint flush). We fire-and-forget here —
    // callers that need a fully awaited teardown (subscriber-flush,
    // checkpoint persistence, SQLite close ordering) call
    // `shutdownAsync()` instead, which lands the same sequence as a
    // single awaitable.
    if (this.liveUpdates) {
      try {
        void this.liveUpdates.stop();
      } catch {
        /* best-effort teardown */
      }
    }

    try {
      this.ingestService.close();
    } catch {
      /* ignore */
    }
    try {
      this.queryService.close();
    } catch {
      /* ignore */
    }
  }

  isReady(): boolean {
    return this.ready;
  }

  // ─────────────────────────────────────────────────────────────────────────
  // Legacy segment methods (minimal implementations for backward compat)
  // ─────────────────────────────────────────────────────────────────────────

  getSegment<T>(_key: SegmentKey): Segment<T> | null {
    // Phase 3 no longer uses the generic segment abstraction.
    // Return null — callers should migrate to dedicated methods.
    return null;
  }

  getSegmentsByType<T>(_type: SegmentType): Segment<T>[] {
    return [];
  }

  getSegmentsPaginated<T>(query: PaginatedSegmentQuery): PaginatedSegmentResult<T> {
    return { segments: [], total: 0, offset: query.offset, hasMore: false };
  }

  // ─────────────────────────────────────────────────────────────────────────
  // Project queries (delegate to AgentDataStore)
  // ─────────────────────────────────────────────────────────────────────────

  getProjectSlugs(): string[] {
    return this.store.getProjectSlugs();
  }

  getProject(_slug: string): Segment<Project> | null {
    // Legacy segment-based project retrieval — not supported in Phase 3.
    // Callers should use getProjectSummaries() instead.
    return null;
  }

  getProjectSessions(_slug: string): Segment<Session>[] {
    // Legacy — callers should use getSessionSummaries() instead.
    return [];
  }

  getSessionMessages(
    slug: string,
    sessionId: string,
    limit: number,
    offset: number,
    options?: { sourceId?: string },
  ): PaginatedSegmentResult<SessionMessage> {
    const result = this.store.getSessionMessages(slug, sessionId, limit, offset, options);

    // Wrap in Segment<SessionMessage> for backward compat with app-service.
    // The store returns the raw `{ messages, total, ... }` shape; the
    // segment wrapper lives here because it's a presentation concern
    // tied to the public `PaginatedSegmentResult<SessionMessage>`
    // contract, not to how data is fetched.
    const segments: Segment<SessionMessage>[] = result.messages.map((msg, i) => ({
      key: `message:${slug}/${sessionId}/${offset + i}`,
      type: 'message' as SegmentType,
      data: msg as SessionMessage,
      version: 1,
      updatedAt: Date.now(),
    }));

    return {
      segments,
      total: result.total,
      offset: result.offset,
      hasMore: result.hasMore,
    };
  }

  getSessionTimelineFacets(slug: string, sessionId: string, options?: { sourceId?: string }): TimelineFacets {
    return this.store.getSessionTimelineFacets(slug, sessionId, options);
  }

  getSessionTimeline(slug: string, sessionId: string, request?: TimelinePageRequest): TimelinePage {
    return this.store.getSessionTimeline(slug, sessionId, request);
  }

  getConfig(): AgentConfig {
    if (this.store.hasConfig()) return this.store.getConfig();
    // Fallback: parse config if not cached yet (rare — initialize()
    // populates the cache for normal flows).
    const data = this.parser.parseSync({
      rootDir: this.rootDir,
      skipProjects: true,
      skipAnalytics: true,
    });
    this.store.setConfig(data.config);
    return data.config;
  }

  getAnalytics(): AgentAnalytic {
    if (this.store.hasAnalytics()) return this.store.getAnalytics();
    // Fallback: parse analytics if not cached yet.
    const data = this.parser.parseSync({
      rootDir: this.rootDir,
      skipProjects: true,
      skipConfig: true,
    });
    this.store.setAnalytics(data.analytics);
    return data.analytics;
  }

  // ─────────────────────────────────────────────────────────────────────────
  // Summaries (delegate to AgentDataStore — SQL aggregation underneath)
  // ─────────────────────────────────────────────────────────────────────────

  getSourceIds(): string[] {
    return this.store.getSourceIds();
  }

  getProjectSummaries(options?: { sourceId?: string }): ProjectSummaryData[] {
    return this.store.getProjectSummaries(options);
  }

  getSessionSummaries(projectSlug: string, options?: { sourceId?: string }): SessionSummaryData[] {
    return this.store.getSessionSummaries(projectSlug, options);
  }

  // ─────────────────────────────────────────────────────────────────────────
  // Detail queries (delegate to AgentDataStore)
  // ─────────────────────────────────────────────────────────────────────────

  getProjectMemory(slug: string, options?: { sourceId?: string }): string | null {
    return this.store.getProjectMemory(slug, options);
  }

  getSessionTodos(slug: string, sessionId: string): unknown[] {
    return this.store.getSessionTodos(slug, sessionId);
  }

  getSessionPlan(slug: string, sessionId: string): unknown | null {
    return this.store.getSessionPlan(slug, sessionId);
  }

  getSessionTask(slug: string, sessionId: string): unknown | null {
    return this.store.getSessionTask(slug, sessionId);
  }

  getPersistedToolResult(slug: string, sessionId: string, toolUseId: string): string | null {
    return this.store.getToolResult(slug, sessionId, toolUseId);
  }

  getSessionSubagents(
    slug: string,
    sessionId: string,
    options?: { sourceId?: string; includeNested?: boolean },
  ): ReturnType<AgentDataStore['getSessionSubagents']> {
    return this.store.getSessionSubagents(slug, sessionId, options);
  }

  getSessionWorkflows(slug: string, sessionId: string): ReturnType<AgentDataStore['getSessionWorkflows']> {
    return this.store.getSessionWorkflows(slug, sessionId);
  }

  getWorkflowSubagents(
    slug: string,
    sessionId: string,
    workflowId: string,
    options?: { sourceId?: string },
  ): ReturnType<AgentDataStore['getWorkflowSubagents']> {
    return this.store.getWorkflowSubagents(slug, sessionId, workflowId, options);
  }

  getSubagentMessages(
    slug: string,
    sessionId: string,
    agentId: string,
    limit: number,
    offset: number,
    workflowId?: string,
    options?: { sourceId?: string },
  ): PaginatedSegmentResult<SessionMessage> {
    const result = this.store.getSubagentMessages(slug, sessionId, agentId, limit, offset, workflowId, options);

    const segments: Segment<SessionMessage>[] = result.messages.map((msg, i) => ({
      key: `subagent:${slug}/${sessionId}/${agentId}/${offset + i}`,
      type: 'subagent' as SegmentType,
      data: msg as SessionMessage,
      version: 1,
      updatedAt: Date.now(),
    }));

    return {
      segments,
      total: result.total,
      offset: result.offset,
      hasMore: result.hasMore,
    };
  }

  getSubagentTimeline(
    slug: string,
    sessionId: string,
    agentId: string,
    request: SubagentTimelinePageRequest,
  ): SubagentTimelinePage {
    return this.store.getSubagentTimeline(slug, sessionId, agentId, request);
  }

  // ─────────────────────────────────────────────────────────────────────────
  // Search (delegate to AgentDataStore)
  // ─────────────────────────────────────────────────────────────────────────

  search(query: SearchQuery): SearchResultSet {
    return this.store.search(query);
  }

  // ─────────────────────────────────────────────────────────────────────────
  // Rebuild & stats
  // ─────────────────────────────────────────────────────────────────────────

  async rebuild(): Promise<void> {
    this.ready = false;

    // Snapshot before parsing — see performColdStart for the TOCTOU rationale.
    const snapshot = this.snapshotSourceStats();

    // Delete all data and re-parse from scratch
    this.ingestService.deleteAllData();

    this.ingestService.beginTransaction();
    try {
      this.parser.parseStreaming(this.ingestService, {
        rootDir: this.rootDir,
      });
      this.ingestService.commitTransaction();
    } catch (error) {
      this.ingestService.rollbackTransaction();
      throw error;
    }

    this.saveAllFingerprints(snapshot);

    // Re-parse config & analytics
    const fullData = this.parser.parseSync({
      rootDir: this.rootDir,
      skipProjects: true,
    });
    this.store.setConfig(fullData.config);
    this.store.setAnalytics(fullData.analytics);

    this.ready = true;
    this.emit('change', { changes: [], timestamp: Date.now() } satisfies SegmentChangeBatch);
  }

  getStoreStats(): StoreStats {
    return this.store.getStats();
  }

  /**
   * Expose the underlying store. Used by `SpaghettiAppService` to wire
   * `api.live.onChange` (C3.4) — the public surface composes
   * `liveUpdates.prewarm(topic)` with `store.subscribe(topic, ...)`,
   * so the app-service needs both references.
   */
  getStore(): AgentDataStore {
    return this.store;
  }

  /**
   * Expose the optional live-updates orchestrator. `undefined` when
   * the service was constructed with `{ live: false }`.
   */
  getLiveWatch(): LiveWatch | undefined {
    return this.liveUpdates;
  }

  // ─────────────────────────────────────────────────────────────────────────
  // Private helpers
  // ─────────────────────────────────────────────────────────────────────────

  private emitProgress(phase: InitProgress['phase'], message: string, current?: number, total?: number): void {
    const progress: InitProgress = { phase, message };
    if (current !== undefined) progress.current = current;
    if (total !== undefined) progress.total = total;
    this.emit('progress', progress);
  }
}
