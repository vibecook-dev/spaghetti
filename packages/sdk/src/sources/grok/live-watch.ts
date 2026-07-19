/**
 * GrokLiveWatch — Plane 2 (live disk ingest) for Grok (RFC 006 / M6 A6).
 *
 * Sibling of `CodexLiveWatch`. Claude's `LiveUpdates` pipeline derives a project
 * slug from the file PATH; Grok can't (its cwd lives in `summary.json`, not the
 * chat_history path shape we key on), so Grok gets a focused watcher:
 *
 *   watcher(sessionsDir) → debounce → for each changed chat_history.jsonl:
 *     incremental read from the last byte offset → writeBatch(grokIngest)
 *     → store.emit(change) → apply sidecars
 *
 *   events.jsonl / signals.json changes re-apply sidecars only (timestamps +
 *   session tokens) for the sibling chat_history in the same session dir.
 *
 * `msg_index` is the ABSOLUTE file line index (same convention the cold reader
 * uses), tracked per file in memory — critical because messages upsert on
 * `(session_id, msg_index)`.
 */

import * as path from 'node:path';

import type { FileService } from '../../io/file-service.js';
import type { ErrorSink } from '../../io/error-sink.js';
import type { IngestService } from '../../data/ingest-service.js';
import type { AgentDataStore } from '../../data/agent-data-store.js';
import type { ParsedRow } from '../../live/parsed-row.js';
import type { SessionIndexEntry, SessionsIndex } from '../../types/index.js';
import { createResilientWatcher, type Watcher, type Unsubscribe } from '../../live/watcher.js';
import type { LiveWatch } from '../../live/live-watch.js';
import { readGrokSessionMeta, encodeGrokSlug } from './reader.js';
import { applyGrokSidecars } from './sidecars.js';

const CHAT_HISTORY_FILE = 'chat_history.jsonl';
const EVENTS_FILE = 'events.jsonl';
const SIGNALS_FILE = 'signals.json';
const DEBOUNCE_MS = 50;

interface FileState {
  byteOffset: number;
  nextIndex: number;
  slug: string;
  sessionId: string;
}

/** Work unit after debounce — chat delta and/or sidecar re-apply. */
interface PendingWork {
  chatHistory?: string;
  sidecarOnly?: boolean;
}

export interface GrokLiveWatchDeps {
  fileService: FileService;
  sessionsDir: string;
  /** Grok-configured IngestService (sourceId='grok' + grok extractor). */
  ingestService: IngestService;
  /** Shared store — emits reach `api.live`. */
  store: AgentDataStore;
  errorSink: ErrorSink;
}

/** Grok's {@link LiveWatch} — a plain whole-tree watcher (no prewarm scopes). */
export interface GrokLiveWatch extends LiveWatch {
  readonly sourceId: 'grok';
}

function isChatHistory(file: string): boolean {
  return path.basename(file) === CHAT_HISTORY_FILE;
}

function isSidecar(file: string): boolean {
  const base = path.basename(file);
  return base === EVENTS_FILE || base === SIGNALS_FILE;
}

/** Resolve chat_history.jsonl next to an events/signals path. */
function chatHistoryForSidecar(file: string): string {
  return path.join(path.dirname(file), CHAT_HISTORY_FILE);
}

export function createGrokLiveWatch(deps: GrokLiveWatchDeps): GrokLiveWatch {
  const state = new Map<string, FileState>();
  /** Keyed by chat_history path so chat + sidecar events coalesce. */
  const pending = new Map<string, PendingWork>();
  let watcher: Watcher | null = null;
  let unsubscribe: Unsubscribe | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let workTail: Promise<void> = Promise.resolve();
  let stopping = false;

  function scheduleChat(chatFile: string): void {
    if (stopping) return;
    const cur = pending.get(chatFile) ?? {};
    cur.chatHistory = chatFile;
    cur.sidecarOnly = false;
    pending.set(chatFile, cur);
    armTimer();
  }

  function scheduleSidecar(sidecarFile: string): void {
    if (stopping) return;
    const chatFile = chatHistoryForSidecar(sidecarFile);
    const cur = pending.get(chatFile) ?? {};
    // If chat is already scheduled, sidecar runs after ingestChanged.
    // If only sidecar, mark sidecar-only re-apply.
    if (!cur.chatHistory) {
      cur.sidecarOnly = true;
    }
    pending.set(chatFile, cur);
    armTimer();
  }

  function armTimer(): void {
    if (timer) return;
    timer = setTimeout(() => {
      timer = null;
      const batch = [...pending.entries()];
      pending.clear();
      for (const [chatFile, work] of batch) {
        // Keep every shared-connection mutation in one ordered tail. This
        // also lets stop() await sidecar work before SQLite is closed.
        workTail = workTail
          .then(async () => {
            if (work.chatHistory) {
              await ingestChanged(chatFile);
            } else if (work.sidecarOnly) {
              reapplySidecars(chatFile);
            }
          })
          .catch((err: unknown) => {
            deps.errorSink.error(err instanceof Error ? err : new Error(String(err)));
          });
      }
    }, DEBOUNCE_MS);
  }

  function buildEntry(meta: NonNullable<ReturnType<typeof readGrokSessionMeta>>, file: string): SessionIndexEntry {
    const stats = deps.fileService.getStats(file);
    const iso = stats ? new Date(stats.mtimeMs).toISOString() : (meta.updated ?? '');
    return {
      sessionId: meta.sessionId,
      fullPath: file,
      fileMtime: stats?.mtimeMs ?? 0,
      firstPrompt: meta.title || 'No prompt',
      summary: meta.summary,
      messageCount: 0,
      created: meta.created ?? iso,
      modified: meta.updated ?? iso,
      gitBranch: meta.gitBranch,
      projectPath: meta.cwd,
      isSidechain: false,
    };
  }

  function reapplySidecars(chatFile: string): void {
    try {
      if (!deps.fileService.exists(chatFile)) return;
      const meta = readGrokSessionMeta(deps.fileService, chatFile);
      if (!meta) return;
      applyGrokSidecars(deps.fileService, chatFile, meta.sessionId, deps.ingestService.getSessionWriteApi(), {
        fallbackCreated: meta.created ?? null,
      });
    } catch (err) {
      deps.errorSink.error(err instanceof Error ? err : new Error(String(err)));
    }
  }

  async function ingestChanged(file: string): Promise<void> {
    if (!isChatHistory(file)) return;
    try {
      let st = state.get(file);
      if (!st) {
        const meta = readGrokSessionMeta(deps.fileService, file);
        if (!meta) return;
        const slug = encodeGrokSlug(meta.cwd);
        const entry = buildEntry(meta, file);
        const existed = deps.store.getSessionMessages(slug, meta.sessionId, 1, 0).total > 0;
        const sessionsIndex: SessionsIndex = { version: 1, originalPath: meta.cwd, entries: [entry] };
        deps.ingestService.onProject(slug, meta.cwd, sessionsIndex);
        deps.ingestService.onSession(slug, entry);

        const fingerprint = deps.ingestService.getFingerprint(file);
        st = {
          byteOffset: fingerprint?.bytePosition ?? 0,
          nextIndex: deps.ingestService.getNextMessageIndex(meta.sessionId),
          slug,
          sessionId: meta.sessionId,
        };
        state.set(file, st);
        if (!existed) {
          deps.store.emit({
            type: 'session.created',
            sourceId: 'grok',
            seq: 0,
            ts: Date.now(),
            slug,
            sessionId: meta.sessionId,
            entry,
          });
        }
      }

      // Rewrite / truncate detection: if the file shrank past our offset,
      // re-read from 0 so we don't permanently miss compacted history.
      const stats = deps.fileService.getStats(file);
      let rewritten = false;
      if (stats && stats.size < st.byteOffset) {
        rewritten = true;
        st.byteOffset = 0;
        st.nextIndex = 0;
      }

      // Only newline-terminated lines are consumed: an unterminated tail is
      // typically a row mid-write — emitting it (or advancing past it) would
      // permanently drop the completed message, so we resume from
      // lastTerminatedPosition and pick it up on the next event.
      const rows: ParsedRow[] = [];
      const res = deps.fileService.readJsonlStreaming<unknown>(
        file,
        (line, idx, byteOffset, _endByteOffset, terminated) => {
          if (!terminated) return;
          rows.push({
            category: 'message',
            slug: st!.slug,
            sessionId: st!.sessionId,
            message: line as never,
            msgIndex: st!.nextIndex + idx,
            byteOffset,
          });
        },
        { fromBytePosition: st.byteOffset },
      );
      st.byteOffset = res.lastTerminatedPosition;
      st.nextIndex += res.terminatedLineCount;

      if (rewritten) {
        // Replace atomically: queries must never observe new compacted rows
        // alongside a stale tail from the pre-compaction transcript.
        deps.ingestService.beginTransaction();
        try {
          deps.ingestService.clearSessionMessages(st.sessionId);
          const result = await deps.ingestService.writeBatch(rows);
          deps.ingestService.commitTransaction();
          for (const change of result.changes) deps.store.emit(change);
        } catch (error) {
          deps.ingestService.rollbackTransaction();
          throw error;
        }
      } else if (rows.length > 0) {
        const result = await deps.ingestService.writeBatch(rows);
        for (const change of result.changes) deps.store.emit(change);
      }

      try {
        const meta = readGrokSessionMeta(deps.fileService, file);
        applyGrokSidecars(deps.fileService, file, st.sessionId, deps.ingestService.getSessionWriteApi(), {
          fallbackCreated: meta?.created ?? null,
        });
      } catch {
        /* sidecar best-effort */
      }
      const currentStats = deps.fileService.getStats(file);
      if (currentStats) {
        deps.ingestService.upsertFingerprint({
          path: file,
          mtimeMs: currentStats.mtimeMs,
          size: currentStats.size,
          bytePosition: st.byteOffset,
        });
      }
    } catch (err) {
      deps.errorSink.error(err instanceof Error ? err : new Error(String(err)));
    }
  }

  const onEvents = (events: { type: string; path: string }[]): void => {
    for (const e of events) {
      if (e.type !== 'create' && e.type !== 'update') continue;
      if (isChatHistory(e.path)) {
        scheduleChat(e.path);
      } else if (isSidecar(e.path)) {
        scheduleSidecar(e.path);
      }
    }
  };

  return {
    sourceId: 'grok',
    async start(): Promise<void> {
      if (watcher) return;
      stopping = false;
      watcher = createResilientWatcher({
        shouldInclude: (filePath) => isChatHistory(filePath) || isSidecar(filePath),
        // sessions/<encoded-project>/<session>/<tracked-file>
        quickDiscoveryDepth: 2,
        onError: (error) => deps.errorSink.error(error, { component: 'GrokPollingWatcher' }),
        onPrimaryUnavailable: (error) =>
          deps.errorSink.error(error, { component: 'GrokParcelWatcher', fallback: 'polling' }),
      });
      unsubscribe = await watcher.subscribe(deps.sessionsDir, onEvents, { ignore: [], recursive: true });
    },

    async stop(): Promise<void> {
      stopping = true;
      if (timer) {
        clearTimeout(timer);
        timer = null;
      }
      pending.clear();
      if (unsubscribe) {
        await unsubscribe();
        unsubscribe = null;
      }
      await workTail;
      workTail = Promise.resolve();
      watcher = null;
      state.clear();
    },
  };
}
