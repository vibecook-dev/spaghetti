/**
 * IngestService — Write layer for the Phase 3 dedicated-table schema
 *
 * Implements ProjectParseSink so it can receive streaming data directly
 * from the parser. All frequent INSERTs use prepared statements.
 */

import type { SqliteService, PreparedStatement } from '../io/index.js';
import type { ProjectParseSink } from './parse-sink.js';
import type {
  SessionsIndex,
  SessionIndexEntry,
  SessionMessage,
  SubagentTranscript,
  PersistedToolResult,
  FileHistorySession,
  TodoFile,
  TaskEntry,
  PlanFile,
  WorkflowRun,
} from '../types/index.js';
import type { Change } from '../live/change-events.js';
import type { ParsedRow, ParsedRowCategory } from '../live/parsed-row.js';
import type { NativeAddon } from '../native.js';
import type { IngestEngine } from '../settings.js';
import type { IngestHooks, MessageExtractor, SessionTokenApi } from '../sources/types.js';
import { claudeCodeMessageExtractor } from '../sources/claude-code/message-extractor.js';
import type { SourceFingerprint } from './segment-types.js';
import { initializeSchema } from './schema.js';

// ═══════════════════════════════════════════════════════════════════════════
// INTERFACE
// ═══════════════════════════════════════════════════════════════════════════

export interface IngestService extends ProjectParseSink {
  open(dbPath: string): void;
  close(): void;
  /** True when the shared SQLite handle is open (another owner may already hold it). */
  isOpen(): boolean;

  // Fingerprints
  getFingerprint(path: string): SourceFingerprint | null;
  getAllFingerprints(): SourceFingerprint[];
  upsertFingerprint(fp: SourceFingerprint): void;
  deleteFingerprint(path: string): void;

  /**
   * Next `msg_index` for a session — `MAX(msg_index) + 1`, or 0 for a
   * session with no rows. Incremental appenders (warm-start grown-file
   * path, live tailer) MUST base their indexes here: the streaming
   * reader's line index restarts at 0 when resuming from a byte
   * position, and messages upsert on `(session_id, msg_index)` — an
   * unbased index overwrites the head of the session.
   */
  getNextMessageIndex(sessionId: string): number;

  /** Replace-on-rewrite sources use this before replaying a truncated session. */
  clearSessionMessages(sessionId: string): void;

  /**
   * Write surface for product sidecars (token attribution, timestamps).
   * Same API passed into {@link IngestHooks}.
   */
  getSessionWriteApi(): SessionTokenApi;

  // Schema meta — small key/value markers (one-shot heals, migrations)
  getMeta(key: string): string | null;
  setMeta(key: string, value: string): void;

  // Transactions
  beginTransaction(): void;
  commitTransaction(): void;
  rollbackTransaction(): void;

  // Bulk ingest optimization
  /** Disable FTS triggers and set aggressive PRAGMAs for bulk ingestion. */
  beginBulkIngest(): void;
  /** Re-enable FTS triggers, rebuild the FTS index, and restore PRAGMAs. */
  endBulkIngest(): void;

  /**
   * Write a batch of rows as a single live-update transaction.
   * Used by LiveUpdates (C2.7) on the hot path after parsing a
   * filesystem delta.
   *
   * Opens a BEGIN IMMEDIATE, dispatches each ParsedRow to the
   * existing per-category `onX()` methods, commits, and returns
   * the set of Change events the caller should emit.
   *
   * Each returned Change is stamped with `ts = Date.now()` and
   * `seq = 0` as a placeholder — the store (`AgentDataStore.emit`)
   * owns the monotonic `seq` counter and overwrites it on emit. See
   * RFC 005 §Event sequence numbering and C3.1 for the rationale.
   *
   * **Not safe to call concurrently on the same instance.** The
   * underlying `better-sqlite3` handle is synchronous and `writeBatch`
   * manages transaction state via a boolean flag on the impl; two
   * overlapping calls would silently nest and the outer one could
   * persist rows outside the transaction opened by the inner. The
   * live-update writer loop in `LiveUpdates` awaits each call
   * serially, which is the only production caller today — external
   * consumers must do the same.
   */
  writeBatch(rows: ParsedRow[]): Promise<WriteResult>;

  // Maintenance
  vacuum(): void;
  rebuildFts(): void;
  deleteAllData(): void;

  /**
   * Delete all durable rows for this service's `source_id` (messages,
   * sessions, projects, source_files). FTS auto-syncs via message DELETE
   * triggers. Used when extract behaviour changes (force re-read) or a
   * non-fast-path re-ingest must drop orphans for sessions removed on disk.
   * Scoped — never touches other agents in a multi-source DB.
   */
  clearSourceData(): void;
}

// ═══════════════════════════════════════════════════════════════════════════
// PUBLIC RESULT TYPE
// ═══════════════════════════════════════════════════════════════════════════

export interface WriteResult {
  changes: Change[];
  durationMs: number;
}

// ═══════════════════════════════════════════════════════════════════════════
// INTERNAL ROW TYPES
// ═══════════════════════════════════════════════════════════════════════════

interface SourceFileRow {
  path: string;
  mtime_ms: number;
  size: number;
  byte_position: number | null;
}

// ═══════════════════════════════════════════════════════════════════════════
// MESSAGE EXTRACTION
// ═══════════════════════════════════════════════════════════════════════════
//
// Relocated to `sources/claude-code/message-extractor.ts` (RFC 006). The stored
// projection (msg_type / text_content / token columns / uuid / timestamp) is now
// produced by `source.messages.extract(record)` — see IngestServiceImpl's
// `messageExtractor` field, which defaults to `claudeCodeMessageExtractor`.

// ═══════════════════════════════════════════════════════════════════════════
// IMPLEMENTATION
// ═══════════════════════════════════════════════════════════════════════════

class IngestServiceImpl implements IngestService {
  private db: SqliteService;
  private opened = false;

  // Prepared statements (created once on open, reused for all inserts)
  private stmtInsertProject!: PreparedStatement;
  private stmtInsertMemory!: PreparedStatement;
  private stmtInsertSession!: PreparedStatement;
  private stmtInsertMessage!: PreparedStatement;
  private stmtInsertSubagent!: PreparedStatement;
  private stmtDeleteSubagentMessages!: PreparedStatement;
  private stmtInsertSubagentMessage!: PreparedStatement;
  private stmtInsertWorkflow!: PreparedStatement;
  private stmtInsertToolResult!: PreparedStatement;
  private stmtInsertFileHistory!: PreparedStatement;
  private stmtInsertTodo!: PreparedStatement;
  private stmtInsertTask!: PreparedStatement;
  private stmtInsertPlan!: PreparedStatement;
  private stmtUpsertFingerprint!: PreparedStatement;
  private stmtUpdateMessageTokens!: PreparedStatement;
  private stmtUpdateMessageTimestamp!: PreparedStatement;

  private inTransaction = false;

  // RFC 005 C4.3: engine pin + native addon handle for the live-ingest
  // native route. When `engine === 'rs'` and `native` is loaded,
  // `writeBatch` dispatches through `native.liveIngestBatch(dbPath,
  // rows)`; otherwise it stays on the TS path. `dbPath` is captured
  // on `open()` so the native call can re-open its own short-lived
  // connection against the same file.
  private readonly engine: IngestEngine;
  private readonly native: NativeAddon | null;
  private readonly messageExtractor: MessageExtractor;
  private readonly sourceId: string;
  /** Optional product hooks (e.g. Codex token attribution). Default no-op. */
  private readonly hooks: IngestHooks;
  private dbPath: string | null = null;

  /**
   * Process-lifetime flag: after the first native live-ingest failure we
   * log a one-shot warning and silently fall back to the TS path for
   * subsequent batches. Keeps live-updates resilient to transient
   * rusqlite hiccups without spamming the console.
   */
  private nativeFallbackLogged = false;

  // NOTE(RFC 005 C3.1): the seq counter used to live here. It now
  // belongs to `AgentDataStore` — the store owns fan-out and stamps
  // every emitted Change on its way through `emit()`. `writeBatch`
  // returns Changes with `seq: 0` as a placeholder; the live-updates
  // writer loop passes them to `store.emit()`, which overwrites.

  private readonly safeBulk: boolean;

  constructor(sqliteServiceFactory: () => SqliteService, options?: CreateIngestServiceOptions) {
    this.db = sqliteServiceFactory();
    this.engine = options?.engine ?? 'ts';
    this.native = options?.native ?? null;
    this.messageExtractor = options?.messages ?? claudeCodeMessageExtractor;
    this.sourceId = options?.sourceId ?? 'claude-code';
    this.hooks = options?.hooks ?? {};
    this.safeBulk = options?.safeBulk ?? false;
  }

  /** Token write API passed into {@link IngestHooks} callbacks. */
  private tokenApi(): SessionTokenApi {
    return {
      updateMessageTokens: (sessionId, msgIndex, tokens) => {
        this.stmtUpdateMessageTokens.run(
          tokens.inputTokens,
          tokens.outputTokens,
          tokens.cacheCreationTokens,
          tokens.cacheReadTokens,
          sessionId,
          msgIndex,
        );
      },
      setSessionTokensEstimated: (sessionId, estimated) => {
        this.db.run('UPDATE sessions SET tokens_estimated = ? WHERE id = ?', estimated ? 1 : 0, sessionId);
      },
      listSessionMessageTexts: (sessionId) => {
        const rows = this.db.all<{ msg_index: number; msg_type: string; text_content: string | null }>(
          'SELECT msg_index, msg_type, text_content FROM messages WHERE session_id = ? ORDER BY msg_index',
          sessionId,
        );
        return rows.map((r) => ({
          msgIndex: r.msg_index,
          msgType: r.msg_type,
          text: r.text_content,
        }));
      },
      updateMessageTimestamp: (sessionId, msgIndex, timestamp) => {
        this.stmtUpdateMessageTimestamp.run(timestamp, sessionId, msgIndex);
      },
    };
  }

  open(dbPath: string): void {
    // If the underlying SqliteService is already open (shared connection),
    // skip opening again to avoid "Database already open" errors.
    if (!this.db.isOpen()) {
      this.db.open({ path: dbPath });
    }
    initializeSchema(this.db);
    this.prepareStatements();
    this.opened = true;
    this.dbPath = dbPath;
  }

  isOpen(): boolean {
    return this.opened && this.db.isOpen();
  }

  close(): void {
    if (this.opened) {
      if (this.inTransaction) {
        // Rollback on close rather than commit — if we're closing with an
        // open transaction, something went wrong and we should not persist
        // potentially partial/corrupt data.
        this.rollbackTransaction();
      }
      this.db.close();
      this.opened = false;
    }
  }

  private prepareStatements(): void {
    this.stmtInsertProject = this.db.prepare(
      `INSERT INTO projects (slug, original_path, sessions_index, updated_at, source_id)
       VALUES (?, ?, ?, ?, ?)
       ON CONFLICT(source_id, slug) DO UPDATE SET
         original_path = excluded.original_path,
         sessions_index = excluded.sessions_index,
         updated_at = excluded.updated_at`,
    );

    this.stmtInsertMemory = this.db.prepare(
      `INSERT INTO project_memories (project_slug, content, updated_at)
       VALUES (?, ?, ?)
       ON CONFLICT(project_slug) DO UPDATE SET
         content = excluded.content,
         updated_at = excluded.updated_at`,
    );

    this.stmtInsertSession = this.db.prepare(
      `INSERT INTO sessions (id, project_slug, full_path, first_prompt, summary, git_branch, project_path, is_sidechain, created_at, modified_at, file_mtime, plan_slug, has_task, updated_at, source_id, tokens_estimated)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
       ON CONFLICT(id) DO UPDATE SET
         project_slug = excluded.project_slug,
         full_path = excluded.full_path,
         first_prompt = excluded.first_prompt,
         summary = excluded.summary,
         git_branch = excluded.git_branch,
         project_path = excluded.project_path,
         is_sidechain = excluded.is_sidechain,
         created_at = excluded.created_at,
         modified_at = excluded.modified_at,
         file_mtime = excluded.file_mtime,
         plan_slug = excluded.plan_slug,
         has_task = excluded.has_task,
         updated_at = excluded.updated_at,
         source_id = excluded.source_id,
         tokens_estimated = excluded.tokens_estimated`,
    );

    this.stmtUpdateMessageTokens = this.db.prepare(
      `UPDATE messages
       SET input_tokens = ?,
           output_tokens = ?,
           cache_creation_tokens = ?,
           cache_read_tokens = ?
       WHERE session_id = ? AND msg_index = ?`,
    );

    this.stmtUpdateMessageTimestamp = this.db.prepare(
      `UPDATE messages SET timestamp = ? WHERE session_id = ? AND msg_index = ?`,
    );

    this.stmtInsertMessage = this.db.prepare(
      `INSERT INTO messages (project_slug, session_id, msg_index, msg_type, uuid, timestamp, data, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, text_content, byte_offset, source_id)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
       ON CONFLICT(session_id, msg_index) DO UPDATE SET
         project_slug = excluded.project_slug,
         msg_type = excluded.msg_type,
         uuid = excluded.uuid,
         timestamp = excluded.timestamp,
         data = excluded.data,
         input_tokens = excluded.input_tokens,
         output_tokens = excluded.output_tokens,
         cache_creation_tokens = excluded.cache_creation_tokens,
         cache_read_tokens = excluded.cache_read_tokens,
         text_content = excluded.text_content,
         byte_offset = excluded.byte_offset,
         source_id = excluded.source_id`,
    );

    this.stmtInsertSubagent = this.db.prepare(
      `INSERT INTO subagents
         (source_id, project_slug, session_id, agent_id, agent_type, file_name, message_count,
          workflow_id, spawn_tool_id, link_method, updated_at)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
       ON CONFLICT(source_id, project_slug, session_id, workflow_id, agent_id) DO UPDATE SET
         agent_type = excluded.agent_type,
         file_name = excluded.file_name,
         message_count = excluded.message_count,
         spawn_tool_id = excluded.spawn_tool_id,
         link_method = excluded.link_method,
         updated_at = excluded.updated_at`,
    );

    this.stmtDeleteSubagentMessages = this.db.prepare(
      `DELETE FROM subagent_messages
        WHERE source_id = ? AND session_id = ? AND workflow_id = ? AND agent_id = ?`,
    );

    this.stmtInsertSubagentMessage = this.db.prepare(
      `INSERT INTO subagent_messages
         (source_id, project_slug, session_id, workflow_id, agent_id, msg_index, timestamp, data)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
    );

    this.stmtInsertWorkflow = this.db.prepare(
      `INSERT INTO workflows (project_slug, session_id, workflow_id, name, status, agent_count, total_tokens, total_tool_calls, duration_ms, subagent_count, data, journal, updated_at)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
       ON CONFLICT(project_slug, session_id, workflow_id) DO UPDATE SET
         name = excluded.name,
         status = excluded.status,
         agent_count = excluded.agent_count,
         total_tokens = excluded.total_tokens,
         total_tool_calls = excluded.total_tool_calls,
         duration_ms = excluded.duration_ms,
         subagent_count = excluded.subagent_count,
         data = excluded.data,
         journal = excluded.journal,
         updated_at = excluded.updated_at`,
    );

    this.stmtInsertToolResult = this.db.prepare(
      `INSERT INTO tool_results (project_slug, session_id, tool_use_id, content, updated_at)
       VALUES (?, ?, ?, ?, ?)
       ON CONFLICT(project_slug, session_id, tool_use_id) DO UPDATE SET
         content = excluded.content,
         updated_at = excluded.updated_at`,
    );

    this.stmtInsertFileHistory = this.db.prepare(
      `INSERT INTO file_history (session_id, data, updated_at)
       VALUES (?, ?, ?)
       ON CONFLICT(session_id) DO UPDATE SET
         data = excluded.data,
         updated_at = excluded.updated_at`,
    );

    this.stmtInsertTodo = this.db.prepare(
      `INSERT INTO todos (session_id, agent_id, items, updated_at)
       VALUES (?, ?, ?, ?)
       ON CONFLICT(session_id, agent_id) DO UPDATE SET
         items = excluded.items,
         updated_at = excluded.updated_at`,
    );

    this.stmtInsertTask = this.db.prepare(
      `INSERT INTO tasks (session_id, has_highwatermark, highwatermark, lock_exists, updated_at)
       VALUES (?, ?, ?, ?, ?)
       ON CONFLICT(session_id) DO UPDATE SET
         has_highwatermark = excluded.has_highwatermark,
         highwatermark = excluded.highwatermark,
         lock_exists = excluded.lock_exists,
         updated_at = excluded.updated_at`,
    );

    this.stmtInsertPlan = this.db.prepare(
      `INSERT INTO plans (slug, title, content, size, updated_at)
       VALUES (?, ?, ?, ?, ?)
       ON CONFLICT(slug) DO UPDATE SET
         title = excluded.title,
         content = excluded.content,
         size = excluded.size,
         updated_at = excluded.updated_at`,
    );

    this.stmtUpsertFingerprint = this.db.prepare(
      `INSERT INTO source_files (path, mtime_ms, size, byte_position, source_id)
       VALUES (?, ?, ?, ?, ?)
       ON CONFLICT(path) DO UPDATE SET
         mtime_ms = excluded.mtime_ms,
         size = excluded.size,
         byte_position = excluded.byte_position`,
    );
  }

  // ─────────────────────────────────────────────────────────────────────────
  // ProjectParseSink implementation
  // ─────────────────────────────────────────────────────────────────────────

  onProject(slug: string, originalPath: string, sessionsIndex: SessionsIndex): void {
    const now = Date.now();
    this.stmtInsertProject.run(slug, originalPath, JSON.stringify(sessionsIndex), now, this.sourceId);
  }

  onProjectMemory(slug: string, content: string): void {
    const now = Date.now();
    this.stmtInsertMemory.run(slug, content, now);
  }

  onSession(slug: string, entry: SessionIndexEntry): void {
    const now = Date.now();
    this.hooks.onSessionStart?.(entry.sessionId);
    this.stmtInsertSession.run(
      entry.sessionId,
      slug,
      entry.fullPath,
      entry.firstPrompt,
      entry.summary,
      entry.gitBranch,
      entry.projectPath,
      entry.isSidechain ? 1 : 0,
      entry.created,
      entry.modified,
      entry.fileMtime,
      null, // plan_slug — set later if found
      0, // has_task — set later if found
      now,
      this.sourceId,
      0, // tokens_estimated — set on session complete if we estimate
    );
  }

  onMessage(slug: string, sessionId: string, message: SessionMessage, index: number, byteOffset: number): void {
    const extracted = this.messageExtractor.extract(message);
    // null = the source's extractor declared this record a non-message row.
    // Product hooks (e.g. Codex token_count) handle skipped records.
    if (!extracted) {
      this.hooks.onSkippedRecord?.(message, { slug, sessionId }, this.tokenApi());
      return;
    }
    const data = JSON.stringify(message);

    this.stmtInsertMessage.run(
      slug,
      sessionId,
      index,
      extracted.msgType,
      extracted.uuid,
      extracted.timestamp,
      data,
      extracted.tokens.inputTokens,
      extracted.tokens.outputTokens,
      extracted.tokens.cacheCreationTokens,
      extracted.tokens.cacheReadTokens,
      extracted.text,
      byteOffset,
      this.sourceId,
    );

    this.hooks.onMessageWritten?.(extracted, { slug, sessionId, msgIndex: index });
  }

  onSubagent(slug: string, sessionId: string, transcript: SubagentTranscript): void {
    const now = Date.now();
    // Prefer the sidecar's real agent type (general-purpose, Explore, …)
    // over the filename-inferred kind (task/prompt_suggestion/compact).
    const agentType = transcript.meta?.agentType ?? transcript.agentType;
    const spawnToolId = this.resolveSubagentSpawnToolId(sessionId, transcript.agentId);
    this.stmtInsertSubagent.run(
      this.sourceId,
      slug,
      sessionId,
      transcript.agentId,
      agentType,
      transcript.fileName,
      transcript.messages.length,
      transcript.workflowId,
      spawnToolId,
      spawnToolId ? 'tool_result' : 'unlinked',
      now,
    );
    this.stmtDeleteSubagentMessages.run(this.sourceId, sessionId, transcript.workflowId, transcript.agentId);
    for (let index = 0; index < transcript.messages.length; index++) {
      const message = transcript.messages[index] as unknown as Record<string, unknown>;
      this.stmtInsertSubagentMessage.run(
        this.sourceId,
        slug,
        sessionId,
        transcript.workflowId,
        transcript.agentId,
        index,
        typeof message.timestamp === 'string' ? message.timestamp : null,
        JSON.stringify(message),
      );
    }
    // Empty transcripts still need an empty materialized projection.
    if (transcript.messages.length === 0) {
      this.db.run(
        `INSERT INTO subagent_dirty_threads(source_id, project_slug, session_id, workflow_id, agent_id)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(source_id, session_id, workflow_id, agent_id) DO UPDATE SET project_slug = excluded.project_slug`,
        this.sourceId,
        slug,
        sessionId,
        transcript.workflowId,
        transcript.agentId,
      );
    }
  }

  onWorkflow(slug: string, sessionId: string, workflow: WorkflowRun): void {
    const now = Date.now();
    this.stmtInsertWorkflow.run(
      slug,
      sessionId,
      workflow.workflowId,
      workflow.name,
      workflow.status,
      workflow.agentCount,
      workflow.totalTokens,
      workflow.totalToolCalls,
      workflow.durationMs,
      workflow.subagentCount,
      JSON.stringify(workflow.data),
      JSON.stringify(workflow.journal),
      now,
    );
  }

  onToolResult(slug: string, sessionId: string, toolResult: PersistedToolResult): void {
    const now = Date.now();
    this.stmtInsertToolResult.run(slug, sessionId, toolResult.toolUseId, toolResult.content, now);
  }

  onFileHistory(sessionId: string, history: FileHistorySession): void {
    const now = Date.now();
    this.stmtInsertFileHistory.run(sessionId, JSON.stringify(history), now);
  }

  onTodo(sessionId: string, todo: TodoFile): void {
    const now = Date.now();
    this.stmtInsertTodo.run(sessionId, todo.agentId, JSON.stringify(todo.items), now);
  }

  onTask(sessionId: string, task: TaskEntry): void {
    const now = Date.now();
    this.stmtInsertTask.run(sessionId, task.hasHighwatermark ? 1 : 0, task.highwatermark, task.lockExists ? 1 : 0, now);

    // Also update the session's has_task flag
    this.db.run('UPDATE sessions SET has_task = 1 WHERE id = ?', sessionId);
  }

  onPlan(slug: string, plan: PlanFile): void {
    const now = Date.now();
    this.stmtInsertPlan.run(slug, plan.title, plan.content, plan.size, now);
  }

  onSessionComplete(_slug: string, sessionId: string, _messageCount: number, _lastBytePosition: number): void {
    this.hooks.onSessionComplete?.(sessionId, this.tokenApi());
  }

  onProjectComplete(_slug: string): void {
    // No-op for now. Could be used for summary recomputation.
  }

  // ─────────────────────────────────────────────────────────────────────────
  // Fingerprints
  // ─────────────────────────────────────────────────────────────────────────

  getFingerprint(filePath: string): SourceFingerprint | null {
    const row = this.db.get<SourceFileRow>(
      'SELECT path, mtime_ms, size, byte_position FROM source_files WHERE path = ?',
      filePath,
    );
    if (!row) return null;
    return this.rowToFingerprint(row);
  }

  getAllFingerprints(): SourceFingerprint[] {
    const rows = this.db.all<SourceFileRow>('SELECT path, mtime_ms, size, byte_position FROM source_files');
    return rows.map((row) => this.rowToFingerprint(row));
  }

  upsertFingerprint(fp: SourceFingerprint): void {
    this.stmtUpsertFingerprint.run(fp.path, fp.mtimeMs, fp.size, fp.bytePosition ?? null, this.sourceId);
  }

  deleteFingerprint(filePath: string): void {
    this.db.run('DELETE FROM source_files WHERE path = ?', filePath);
  }

  getNextMessageIndex(sessionId: string): number {
    const row = this.db.get<{ next: number }>(
      'SELECT COALESCE(MAX(msg_index) + 1, 0) AS next FROM messages WHERE session_id = ?',
      sessionId,
    );
    return row?.next ?? 0;
  }

  getSessionWriteApi(): SessionTokenApi {
    return this.tokenApi();
  }

  getMeta(key: string): string | null {
    const row = this.db.get<{ value: string }>('SELECT value FROM schema_meta WHERE key = ?', key);
    return row?.value ?? null;
  }

  setMeta(key: string, value: string): void {
    this.db.run(
      'INSERT INTO schema_meta (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value',
      key,
      value,
    );
  }

  // ─────────────────────────────────────────────────────────────────────────
  // Transactions
  // ─────────────────────────────────────────────────────────────────────────

  beginTransaction(): void {
    if (!this.inTransaction) {
      this.db.exec('BEGIN TRANSACTION');
      this.inTransaction = true;
    }
  }

  commitTransaction(): void {
    if (this.inTransaction) {
      this.db.exec('COMMIT');
      this.inTransaction = false;
    }
  }

  rollbackTransaction(): void {
    if (this.inTransaction) {
      try {
        this.db.exec('ROLLBACK');
      } catch {
        // Ignore errors during rollback — the transaction may already be
        // rolled back (e.g., if the DB connection was lost).
      }
      this.inTransaction = false;
    }
  }

  // ─────────────────────────────────────────────────────────────────────────
  // Bulk ingest optimization
  // ─────────────────────────────────────────────────────────────────────────

  private inBulkMode = false;

  beginBulkIngest(): void {
    if (this.inBulkMode) return;
    this.inBulkMode = true;

    // Drop FTS auto-sync triggers to avoid per-row overhead during bulk insert
    try {
      this.db.exec('DROP TRIGGER IF EXISTS messages_ai');
    } catch {
      /* ignore */
    }
    try {
      this.db.exec('DROP TRIGGER IF EXISTS messages_ad');
    } catch {
      /* ignore */
    }
    try {
      this.db.exec('DROP TRIGGER IF EXISTS messages_au');
    } catch {
      /* ignore */
    }

    // Fast path: aggressive PRAGMAs. Safe path (desktop / live:true): keep
    // durable defaults so a kill mid-ingest is less likely to corrupt the cache.
    try {
      if (!this.safeBulk) {
        this.db.exec('PRAGMA synchronous = OFF');
        this.db.exec('PRAGMA cache_size = -64000'); // 64MB cache
      } else {
        this.db.exec('PRAGMA synchronous = NORMAL');
        this.db.exec('PRAGMA journal_mode = WAL');
        this.db.exec('PRAGMA cache_size = -64000');
      }
    } catch {
      /* ignore */
    }
  }

  endBulkIngest(): void {
    if (!this.inBulkMode) return;
    this.inBulkMode = false;

    // Recreate FTS triggers
    try {
      this.db.exec(`
        CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
          INSERT INTO search_fts(rowid, text_content) VALUES (new.id, new.text_content);
        END;
        CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
          INSERT INTO search_fts(search_fts, rowid, text_content) VALUES ('delete', old.id, old.text_content);
        END;
        CREATE TRIGGER IF NOT EXISTS messages_au AFTER UPDATE ON messages BEGIN
          INSERT INTO search_fts(search_fts, rowid, text_content) VALUES ('delete', old.id, old.text_content);
          INSERT INTO search_fts(rowid, text_content) VALUES (new.id, new.text_content);
        END;
      `);
    } catch {
      /* ignore — triggers may already exist */
    }

    // Rebuild the FTS index in one shot (much faster than per-row trigger inserts)
    try {
      this.rebuildFts();
    } catch {
      /* ignore */
    }

    // Restore safe PRAGMAs
    try {
      this.db.exec('PRAGMA synchronous = NORMAL');
      this.db.exec('PRAGMA cache_size = -2000'); // default ~2MB
    } catch {
      /* ignore */
    }
  }

  // ─────────────────────────────────────────────────────────────────────────
  // Live-updates write path (RFC 005 C2.6)
  // ─────────────────────────────────────────────────────────────────────────

  /**
   * Write a batch of `ParsedRow`s as a single live-update transaction.
   *
   * Atomicity: wraps the whole batch in `BEGIN IMMEDIATE` (via the
   * existing `beginTransaction`/`commitTransaction`/`rollbackTransaction`
   * helpers) so either all rows land or none do. A throw mid-batch
   * rolls back and rethrows — the checkpoint is not advanced by the
   * caller so the orchestrator will retry.
   *
   * Dispatch: each row's `category` discriminates the union and
   * routes to the matching `on*` method. TS narrows the variant so
   * payload fields are read directly without any `as` casts.
   *
   * Change events: after commit we walk the rows again and translate
   * each into the matching `Change` variant (see `live/change-events.ts`).
   * `project_memory` + `session_index` rows mutate SQLite but emit no
   * `Change` — the union has no matching variants (see RFC 005 §2.9).
   * Each returned `Change` is stamped `ts = Date.now()` and `seq = 0`;
   * the real monotonic `seq` is assigned inside `AgentDataStore.emit()`
   * when the writer loop fans the change out. See RFC 005 §Event
   * sequence numbering (counter is not persisted).
   */
  async writeBatch(rows: ParsedRow[]): Promise<WriteResult> {
    const startedAt = Date.now();

    // Empty batch: no-op. Do NOT open a transaction; callers hit this
    // when the coalescing queue drains with nothing to write.
    if (rows.length === 0) {
      return { changes: [], durationMs: Date.now() - startedAt };
    }

    // RFC 005 C4.3: when this instance is pinned to the `rs` engine and
    // the native addon loaded, dispatch through
    // `native.liveIngestBatch` so the live path writes via the same
    // Rust writer the cold-start engine uses. On any failure — native
    // addon throws, DB locked, etc. — fall back to the TS path for
    // *this* batch (same process, subsequent batches try native again
    // if they were transient). We log once per process to keep the
    // fallback visible without spamming.
    if (this.engine === 'rs' && this.native && this.dbPath) {
      try {
        // Skip message rows the extractor rejects (null) — same contract as
        // onMessage / the TS path. Without this, Grok tool_result lines and
        // Codex non-message rollouts land as msgType "unknown".
        const nativeRows = rows
          .map((r) => parsedRowToNativeLiveRow(r, this.messageExtractor))
          .filter((r): r is NonNullable<typeof r> => r !== null);
        if (nativeRows.length > 0) {
          this.native.liveIngestBatch(this.dbPath, nativeRows, this.sourceId);
        }
        return {
          changes: buildChangesFromRows(rows, this.messageExtractor),
          durationMs: Date.now() - startedAt,
        };
      } catch (err) {
        if (!this.nativeFallbackLogged) {
          console.warn(
            '[spaghetti-sdk] native live-ingest failed; falling back to TS writer. ' +
              `Further native failures this session will be silent. Error: ${
                err instanceof Error ? err.message : String(err)
              }`,
          );
          this.nativeFallbackLogged = true;
        }
        // Fall through to the TS path.
      }
    }

    // `BEGIN IMMEDIATE` equivalent: the shared `beginTransaction` uses
    // `BEGIN TRANSACTION` (deferred by default in SQLite). For live-
    // update semantics we want the write lock acquired up front so
    // concurrent readers can't block the commit indefinitely. Use
    // `BEGIN IMMEDIATE` directly when we're the first to open the tx.
    const weOpenedTx = !this.inTransaction;
    if (weOpenedTx) {
      this.db.exec('BEGIN IMMEDIATE');
      this.inTransaction = true;
    }

    try {
      // Pass `this` directly as the RowWriteContext — IngestServiceImpl
      // implements the context surface structurally, so no alias needed.
      for (const row of rows) {
        applyRowHandler(row, this);
      }

      if (weOpenedTx) {
        this.db.exec('COMMIT');
        this.inTransaction = false;
      }
    } catch (err) {
      if (weOpenedTx && this.inTransaction) {
        try {
          this.db.exec('ROLLBACK');
        } catch {
          // Ignore — the tx may already be rolled back.
        }
        this.inTransaction = false;
      }
      throw err;
    }

    return {
      changes: buildChangesFromRows(rows, this.messageExtractor),
      durationMs: Date.now() - startedAt,
    };
  }

  // ─────────────────────────────────────────────────────────────────────────
  // Maintenance
  // ─────────────────────────────────────────────────────────────────────────

  vacuum(): void {
    this.db.vacuum();
  }

  rebuildFts(): void {
    this.db.exec(`INSERT INTO search_fts(search_fts) VALUES('rebuild')`);
    this.db.exec(`INSERT INTO subagent_search_fts(subagent_search_fts) VALUES('rebuild')`);
  }

  clearSourceData(): void {
    // Order: messages first so FTS DELETE triggers run while the connection
    // is healthy; then sessions/projects; fingerprints last.
    this.db.run('DELETE FROM messages WHERE source_id = ?', this.sourceId);
    this.db.run('DELETE FROM timeline_messages WHERE source_id = ?', this.sourceId);
    this.db.run('DELETE FROM timeline_dirty_sessions WHERE source_id = ?', this.sourceId);
    this.db.run('DELETE FROM subagent_timeline_messages WHERE source_id = ?', this.sourceId);
    this.db.run('DELETE FROM subagent_messages WHERE source_id = ?', this.sourceId);
    this.db.run('DELETE FROM subagent_dirty_threads WHERE source_id = ?', this.sourceId);
    this.db.run('DELETE FROM subagents WHERE source_id = ?', this.sourceId);
    this.db.run('DELETE FROM sessions WHERE source_id = ?', this.sourceId);
    this.db.run('DELETE FROM projects WHERE source_id = ?', this.sourceId);
    this.db.run('DELETE FROM source_files WHERE source_id = ?', this.sourceId);
  }

  clearSessionMessages(sessionId: string): void {
    this.db.run('DELETE FROM timeline_messages WHERE session_id = ? AND source_id = ?', sessionId, this.sourceId);
    this.db.run('DELETE FROM messages WHERE session_id = ? AND source_id = ?', sessionId, this.sourceId);
    this.db.run('UPDATE sessions SET tokens_estimated = 0 WHERE id = ? AND source_id = ?', sessionId, this.sourceId);
  }

  deleteAllData(): void {
    const tables = [
      'messages',
      'timeline_messages',
      'timeline_dirty_sessions',
      'subagents',
      'subagent_messages',
      'subagent_timeline_messages',
      'subagent_dirty_threads',
      'workflows',
      'tool_results',
      'todos',
      'tasks',
      'plans',
      'sessions',
      'project_memories',
      'projects',
      'file_history',
      'config',
      'analytics',
      'source_files',
    ];
    for (const table of tables) {
      this.db.exec(`DELETE FROM ${table}`);
    }
    // Rebuild FTS after deleting all content
    this.rebuildFts();
  }

  // ─────────────────────────────────────────────────────────────────────────
  // Private
  // ─────────────────────────────────────────────────────────────────────────

  /** Link a Claude Task/Agent result to the transcript id it returned. */
  private resolveSubagentSpawnToolId(sessionId: string, agentId: string): string | null {
    const rows = this.db.all<{ data: string }>(
      'SELECT data FROM messages WHERE session_id = ? AND source_id = ? ORDER BY msg_index',
      sessionId,
      this.sourceId,
    );
    for (const row of rows) {
      try {
        const raw = JSON.parse(row.data) as Record<string, unknown>;
        const message = raw.message as Record<string, unknown> | undefined;
        if (!Array.isArray(message?.content)) continue;
        for (const value of message.content) {
          const block = value as Record<string, unknown>;
          if (block.type !== 'tool_result' || typeof block.tool_use_id !== 'string') continue;
          const content = typeof block.content === 'string' ? block.content : JSON.stringify(block.content ?? '');
          if (content.includes(agentId)) return block.tool_use_id;
        }
      } catch {
        /* malformed raw row */
      }
    }
    return null;
  }

  private rowToFingerprint(row: SourceFileRow): SourceFingerprint {
    const fp: SourceFingerprint = { path: row.path, mtimeMs: row.mtime_ms, size: row.size };
    if (row.byte_position != null) fp.bytePosition = row.byte_position;
    return fp;
  }
}

// ═══════════════════════════════════════════════════════════════════════════
// FACTORY
// ═══════════════════════════════════════════════════════════════════════════

export function createIngestService(
  sqliteServiceFactory: () => SqliteService,
  options?: CreateIngestServiceOptions,
): IngestService {
  return new IngestServiceImpl(sqliteServiceFactory, options);
}

/**
 * Options accepted by {@link createIngestService}.
 *
 * Introduced in RFC 005 Phase 4 C4.3 to thread the engine pin + native
 * addon handle into `IngestServiceImpl`. Both default to "no native
 * routing" (`engine: 'ts'`, `native: null`) so call sites that don't
 * opt in — tests, non-live paths — keep the existing TS-only behaviour.
 */
export interface CreateIngestServiceOptions {
  /**
   * Which engine this service was built for. Only `'rs'` enables the
   * native live-ingest route in {@link IngestService.writeBatch}; any
   * other value keeps the TS path.
   */
  engine?: IngestEngine;
  /**
   * The loaded native addon, or `null` when unavailable. When `engine
   * === 'rs'` but `native === null` (addon missing on this platform),
   * `writeBatch` stays on the TS path.
   */
  native?: NativeAddon | null;
  /**
   * The source's message extractor (RFC 006). Defaults to
   * {@link claudeCodeMessageExtractor}. A second `AgentSource` passes its own so
   * the ingest writer never learns that source's message envelope.
   */
  messages?: MessageExtractor;
  /**
   * The `AgentSource.id` this service writes for. Bound into the `source_id`
   * column of every row (RFC 006 §5.1 — one index, source_id column). Defaults
   * to `'claude-code'`, matching the schema DEFAULT, so the claude-code path and
   * the Rust writer (which still relies on the DEFAULT) stay byte-identical.
   */
  sourceId?: string;
  /**
   * Prefer crash-safer bulk PRAGMAs (WAL + synchronous=NORMAL) during
   * `beginBulkIngest`. Defaults to false.
   */
  safeBulk?: boolean;
  /**
   * Optional product hooks (token attribution, etc.). Defaults to no-op.
   * Codex passes {@link createCodexIngestHooks}.
   */
  hooks?: IngestHooks;
}

// ═══════════════════════════════════════════════════════════════════════════
// LIVE-PATH HELPERS (shared by TS + native write paths)
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Serialize a {@link ParsedRow} for the Rust live-ingest entry
 * (`liveIngestBatch`).
 *
 * The wire contract is defined in `crates/spaghetti-napi/src/orchestrate/live_ingest.rs`
 * — each `category` carries a `payload_json` whose shape matches the
 * corresponding `IngestEvent` variant fields. For `message` we flatten a
 * handful of projections (msgType / uuid / timestamp / token counters /
 * ftsText) that the Rust side would otherwise have to re-derive from the
 * raw JSONL — pre-extracting on the TS side keeps the Rust path a pure
 * parameter bind.
 */
function parsedRowToNativeLiveRow(
  row: ParsedRow,
  extractor: MessageExtractor,
): {
  category: string;
  slug?: string;
  sessionId?: string;
  payloadJson: string;
} | null {
  switch (row.category) {
    case 'message': {
      // Mirrors `onMessage`: null extract means "not a message row" (Grok
      // tool I/O, Codex non-message envelopes). Do not invent msgType
      // "unknown" — that polluted live native writes for multi-source.
      const extracted = extractor.extract(row.message);
      if (!extracted) return null;
      const payload = {
        msgIndex: row.msgIndex,
        byteOffset: row.byteOffset,
        // Raw JSONL line isn't available on ParsedRow; the Rust writer
        // stores `JSON.stringify(message)` into `messages.data`, matching
        // what the TS writer does via `data = JSON.stringify(message)` in
        // `onMessage`. Keeping the same stringifier means round-tripping
        // produces identical bytes.
        rawJson: JSON.stringify(row.message),
        msgType: extracted.msgType,
        uuid: extracted.uuid,
        timestamp: extracted.timestamp,
        inputTokens: extracted.tokens.inputTokens,
        outputTokens: extracted.tokens.outputTokens,
        cacheCreationTokens: extracted.tokens.cacheCreationTokens,
        cacheReadTokens: extracted.tokens.cacheReadTokens,
        ftsText: extracted.text,
      };
      return {
        category: 'message',
        slug: row.slug,
        sessionId: row.sessionId,
        payloadJson: JSON.stringify(payload),
      };
    }
    case 'subagent':
      return {
        category: 'subagent',
        slug: row.slug,
        sessionId: row.sessionId,
        payloadJson: JSON.stringify(row.transcript),
      };
    case 'tool_result':
      return {
        category: 'tool_result',
        slug: row.slug,
        sessionId: row.sessionId,
        payloadJson: JSON.stringify(row.result),
      };
    case 'file_history':
      return {
        category: 'file_history',
        sessionId: row.sessionId,
        payloadJson: JSON.stringify(row.history),
      };
    case 'todo':
      return {
        category: 'todo',
        sessionId: row.sessionId,
        payloadJson: JSON.stringify(row.todo),
      };
    case 'task':
      return {
        category: 'task',
        sessionId: row.sessionId,
        payloadJson: JSON.stringify(row.task),
      };
    case 'plan':
      return {
        category: 'plan',
        slug: row.slug,
        payloadJson: JSON.stringify(row.plan),
      };
    case 'project_memory':
      return {
        category: 'project_memory',
        slug: row.slug,
        payloadJson: JSON.stringify({ content: row.content }),
      };
    case 'session_index':
      return {
        category: 'session_index',
        slug: row.slug,
        payloadJson: JSON.stringify({
          originalPath: row.originalPath,
          sessionsIndex: row.sessionsIndex,
        }),
      };
  }
}

// ═══════════════════════════════════════════════════════════════════════════
// ROW HANDLER TABLE
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Subset of `IngestService` the row handlers need to write a row.
 * Keeping this structural rather than passing the full implementation
 * lets the table stay outside the class (no `this` capture) while the
 * impl class still satisfies the shape via duck-typing.
 *
 * `session_index` reuses `onProject(slug, originalPath, sessionsIndex)`
 * — that signature is identical to the `applySessionIndex` helper
 * the live path used to call.
 */
interface RowWriteContext {
  onMessage(slug: string, sessionId: string, message: SessionMessage, index: number, byteOffset: number): void;
  onSubagent(slug: string, sessionId: string, transcript: SubagentTranscript): void;
  onToolResult(slug: string, sessionId: string, toolResult: PersistedToolResult): void;
  onFileHistory(sessionId: string, history: FileHistorySession): void;
  onTodo(sessionId: string, todo: TodoFile): void;
  onTask(sessionId: string, task: TaskEntry): void;
  onPlan(slug: string, plan: PlanFile): void;
  onProjectMemory(slug: string, content: string): void;
  onProject(slug: string, originalPath: string, sessionsIndex: SessionsIndex): void;
}

/** Narrow a `ParsedRow` to the variant matching its `category`. */
type RowOf<C extends ParsedRowCategory> = Extract<ParsedRow, { category: C }>;

/**
 * One entry per `ParsedRow.category`. `apply` drives the SQLite write
 * via `RowWriteContext`; `toChange` builds the matching `Change`
 * variant or returns `null` for SQLite-only rows that have no
 * corresponding event (`project_memory`, `session_index`, plus
 * `file_history` when the snapshot list is empty).
 *
 * Adding a new category means adding ONE entry here — the dispatch
 * loop in `writeBatch` and the change-fan-out loop in
 * `buildChangesFromRows` consult this table directly so neither needs
 * a parallel switch.
 */
interface RowHandler<C extends ParsedRowCategory> {
  apply(row: RowOf<C>, ctx: RowWriteContext): void;
  toChange(row: RowOf<C>, ts: number): Change | null;
}

type RowHandlers = { [C in ParsedRowCategory]: RowHandler<C> };

/**
 * Identity helper that pins the per-category entry to its narrowed
 * `RowHandler<C>` type. Without the helper, TypeScript widens each
 * record value to the union over every category and the per-row
 * field reads (`r.slug`, `r.sessionId`, …) lose their narrowing.
 */
function handler<C extends ParsedRowCategory>(h: RowHandler<C>): RowHandler<C> {
  return h;
}

const ROW_HANDLERS: RowHandlers = {
  message: handler<'message'>({
    apply: (r, c) => c.onMessage(r.slug, r.sessionId, r.message, r.msgIndex, r.byteOffset),
    toChange: (r, ts) => ({
      type: 'session.message.added',
      seq: 0,
      ts,
      slug: r.slug,
      sessionId: r.sessionId,
      message: r.message,
      byteOffset: r.byteOffset,
    }),
  }),
  subagent: handler<'subagent'>({
    apply: (r, c) => c.onSubagent(r.slug, r.sessionId, r.transcript),
    toChange: (r, ts) => ({
      type: 'subagent.updated',
      seq: 0,
      ts,
      slug: r.slug,
      sessionId: r.sessionId,
      agentId: r.transcript.agentId,
      transcript: r.transcript,
    }),
  }),
  tool_result: handler<'tool_result'>({
    apply: (r, c) => c.onToolResult(r.slug, r.sessionId, r.result),
    toChange: (r, ts) => ({
      type: 'tool-result.added',
      seq: 0,
      ts,
      slug: r.slug,
      sessionId: r.sessionId,
      toolUseId: r.result.toolUseId,
    }),
  }),
  file_history: handler<'file_history'>({
    apply: (r, c) => c.onFileHistory(r.sessionId, r.history),
    toChange: (r, ts) => {
      // `apply` persists every snapshot in the ParsedRow to SQLite,
      // but this `toChange` emits only ONE `file-history.added` event
      // — for `snapshots[0]`. Multi-snapshot rows (produced by
      // cold-start / rewrite re-ingest) therefore surface a single
      // event referencing the first snapshot; the other snapshots
      // are persisted silently. This matches pre-dispatch-table
      // behavior and the common case from live-tail (one snapshot
      // per ParsedRow). Fanning out one event per snapshot is a
      // follow-up when consumers need per-snapshot granularity.
      const snap = r.history.snapshots[0];
      if (!snap) return null;
      return {
        type: 'file-history.added',
        seq: 0,
        ts,
        sessionId: r.sessionId,
        hash: snap.hash,
        version: snap.version,
      };
    },
  }),
  todo: handler<'todo'>({
    apply: (r, c) => c.onTodo(r.sessionId, r.todo),
    toChange: (r, ts) => ({
      type: 'todo.updated',
      seq: 0,
      ts,
      sessionId: r.sessionId,
      agentId: r.todo.agentId,
      items: r.todo.items,
    }),
  }),
  task: handler<'task'>({
    apply: (r, c) => c.onTask(r.sessionId, r.task),
    toChange: (r, ts) => ({
      type: 'task.updated',
      seq: 0,
      ts,
      sessionId: r.sessionId,
      task: r.task,
    }),
  }),
  plan: handler<'plan'>({
    apply: (r, c) => c.onPlan(r.slug, r.plan),
    toChange: (r, ts) => ({
      type: 'plan.upserted',
      seq: 0,
      ts,
      slug: r.slug,
      plan: r.plan,
    }),
  }),
  project_memory: handler<'project_memory'>({
    apply: (r, c) => c.onProjectMemory(r.slug, r.content),
    // SQLite-only write, no Change emission (no matching union
    // variant — see RFC 005 §2.9).
    toChange: () => null,
  }),
  session_index: handler<'session_index'>({
    // No public `onSessionIndex(slug, originalPath, sessionsIndex)` on
    // `ProjectParseSink` — cold-start uses `onProject(slug,
    // originalPath, sessionsIndex)` with the same signature, which
    // is exactly what the live path needs too.
    apply: (r, c) => c.onProject(r.slug, r.originalPath, r.sessionsIndex),
    // SQLite-only write (ditto).
    toChange: () => null,
  }),
};

/**
 * Dispatch one row to its handler. Index access into `ROW_HANDLERS`
 * loses the discriminated-union → variant correspondence, so the
 * `as never` widens the row to satisfy each variant's `apply`
 * signature. Soundness comes from the `RowHandlers` type ensuring
 * every category has a matching apply.
 */
function applyRowHandler(row: ParsedRow, ctx: RowWriteContext): void {
  (ROW_HANDLERS[row.category] as RowHandler<typeof row.category>).apply(row as never, ctx);
}

/**
 * Build the `Change[]` the subscriber registry should fan out after a
 * successful batch. Shared between the TS and native paths so that
 * subscribers see the exact same events regardless of engine.
 *
 * Each returned Change carries `seq: 0` — the store's `emit()` stamps
 * the real monotonic counter on the way through fan-out. Doing it
 * here would divorce the counter from fan-out order; see C3.1 for the
 * history.
 */
function buildChangesFromRows(rows: ParsedRow[], extractor: MessageExtractor): Change[] {
  const changes: Change[] = [];
  for (const row of rows) {
    // Only emit message.added for rows that were (or would be) written.
    // Skipped extracts must not produce phantom live events.
    if (row.category === 'message' && extractor.extract(row.message) === null) {
      continue;
    }
    const ts = Date.now();
    const change = (ROW_HANDLERS[row.category] as RowHandler<typeof row.category>).toChange(row as never, ts);
    if (change !== null) changes.push(change);
  }
  return changes;
}
