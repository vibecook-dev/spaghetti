/**
 * CodexLiveWatch — Plane 2 (live disk ingest) for Codex (RFC 006).
 *
 * Claude's `LiveUpdates` pipeline is taxonomy-specific (settings/todos/scopes)
 * and derives a project slug from the file PATH — which Codex can't, since its
 * slug lives inside `session_meta` in the file. So Codex gets a focused live
 * watcher instead of reusing that pipeline:
 *
 *   watcher(sessionsDir) → debounce → for each changed rollout:
 *     incremental read from the last byte offset → writeBatch(codexIngest)
 *     → store.emit(change)
 *
 * `msg_index` is the ABSOLUTE file line index (same convention the cold reader
 * uses), tracked per file in memory. This keeps live-appended rows aligned with
 * what a cold re-ingest would write — critical, because messages upsert on
 * `(session_id, msg_index)`; a divergent live index would leave duplicates
 * after a restart. The in-memory map is rebuilt lazily: a file not yet seen in
 * this watch session is read in full on its first event, establishing the
 * offset + line count, then incrementally thereafter.
 */

import * as path from 'node:path';

import type { FileService } from '../../io/file-service.js';
import type { ErrorSink } from '../../io/error-sink.js';
import type { IngestService } from '../../data/ingest-service.js';
import type { AgentDataStore } from '../../data/agent-data-store.js';
import type { ParsedRow } from '../../live/parsed-row.js';
import type { SessionIndexEntry, SessionsIndex } from '../../types/index.js';
import { createParcelWatcher, createChokidarWatcher, type Watcher, type Unsubscribe } from '../../live/watcher.js';
import { considerCodexFirstPromptLine } from './first-prompt.js';
import { isCodexInternalSessionPayload } from './session-meta.js';
import type { LiveWatch } from '../../live/live-watch.js';

const ROLLOUT_FILE = /rollout-.*\.jsonl$/;
const UUID = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i;
const DEBOUNCE_MS = 50;
const STOP_PEEK = Symbol('codex-live-peek-stop');

interface FileState {
  byteOffset: number;
  nextIndex: number;
  slug: string;
  sessionId: string;
}

export interface CodexLiveWatchDeps {
  fileService: FileService;
  sessionsDir: string;
  /** Codex-configured IngestService (sourceId='codex' + codex extractor). */
  ingestService: IngestService;
  /** Shared store — emits reach `api.live`. */
  store: AgentDataStore;
  errorSink: ErrorSink;
}

/** Codex's {@link LiveWatch} — a plain whole-tree watcher (no prewarm scopes). */
export interface CodexLiveWatch extends LiveWatch {
  readonly sourceId: 'codex';
}

function encodeSlug(cwd: string): string {
  return cwd.replace(/\//g, '-');
}

export function createCodexLiveWatch(deps: CodexLiveWatchDeps): CodexLiveWatch {
  const state = new Map<string, FileState>();
  const pending = new Set<string>();
  let watcher: Watcher | null = null;
  let unsubscribe: Unsubscribe | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;

  function schedule(file: string): void {
    pending.add(file);
    if (timer) return;
    timer = setTimeout(() => {
      timer = null;
      const batch = [...pending];
      pending.clear();
      for (const f of batch) void ingestChanged(f);
    }, DEBOUNCE_MS);
  }

  /**
   * Read a rollout's session_meta + first human prompt → slug + sessionId + entry.
   * Same first-prompt rules as cold {@link CodexReader} (skip injected context).
   */
  function peekMeta(file: string): { slug: string; sessionId: string; entry: SessionIndexEntry; cwd: string } | null {
    let cwd: string | null = null;
    let sid: string | null = null;
    let ts: string | null = null;
    let firstPrompt = '';
    let internalSession = false;
    try {
      deps.fileService.readJsonlStreaming<Record<string, unknown>>(file, (line, idx) => {
        const payload = line.payload as Record<string, unknown> | undefined;
        if (line.type === 'session_meta' && payload) {
          if (isCodexInternalSessionPayload(payload)) {
            internalSession = true;
            throw STOP_PEEK;
          }
          if (typeof payload.cwd === 'string') cwd = payload.cwd;
          if (typeof payload.id === 'string') sid = payload.id;
          if (typeof line.timestamp === 'string') ts = line.timestamp;
        } else {
          firstPrompt = considerCodexFirstPromptLine(firstPrompt, line);
        }
        // Need cwd always; keep scanning until we have a real prompt or hit the cap.
        if ((cwd && firstPrompt) || idx >= 100) throw STOP_PEEK;
      });
    } catch (e) {
      if (e !== STOP_PEEK) return null;
    }
    if (internalSession || !cwd) return null;
    const sessionId = sid ?? path.basename(file).match(UUID)?.[0] ?? path.basename(file);
    const stats = deps.fileService.getStats(file);
    const iso = stats ? new Date(stats.mtimeMs).toISOString() : (ts ?? '');
    const entry: SessionIndexEntry = {
      sessionId,
      fullPath: file,
      fileMtime: stats?.mtimeMs ?? 0,
      firstPrompt: firstPrompt || 'No prompt',
      summary: '',
      messageCount: 0,
      created: ts ?? iso,
      modified: iso,
      gitBranch: '',
      projectPath: cwd,
      isSidechain: false,
    };
    return { slug: encodeSlug(cwd), sessionId, entry, cwd };
  }

  async function ingestChanged(file: string): Promise<void> {
    if (!ROLLOUT_FILE.test(path.basename(file))) return;
    try {
      let st = state.get(file);
      if (!st) {
        const meta = peekMeta(file);
        if (!meta) return;
        // Was this session already in the DB (cold-ingested)? If not, announce it.
        const existed = deps.store.getSessionMessages(meta.slug, meta.sessionId, 1, 0).total > 0;
        const sessionsIndex: SessionsIndex = { version: 1, originalPath: meta.cwd, entries: [meta.entry] };
        deps.ingestService.onProject(meta.slug, meta.cwd, sessionsIndex);
        deps.ingestService.onSession(meta.slug, meta.entry);
        st = { byteOffset: 0, nextIndex: 0, slug: meta.slug, sessionId: meta.sessionId };
        state.set(file, st);
        if (!existed) {
          deps.store.emit({
            type: 'session.created',
            seq: 0,
            ts: Date.now(),
            slug: meta.slug,
            sessionId: meta.sessionId,
            entry: meta.entry,
          });
        }
      }

      // Rewrite / truncate detection: if the file shrank past our offset,
      // re-read from 0 so we don't permanently miss compacted history.
      const stats = deps.fileService.getStats(file);
      if (stats && stats.size < st.byteOffset) {
        st.byteOffset = 0;
        st.nextIndex = 0;
      }

      // Incremental read from the last offset; msg_index = absolute file line.
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

      if (rows.length > 0) {
        const result = await deps.ingestService.writeBatch(rows);
        for (const change of result.changes) deps.store.emit(change);
      }
    } catch (err) {
      deps.errorSink.error(err instanceof Error ? err : new Error(String(err)));
    }
  }

  return {
    sourceId: 'codex',
    async start(): Promise<void> {
      if (watcher) return;
      watcher = createParcelWatcher();
      try {
        unsubscribe = await watcher.subscribe(
          deps.sessionsDir,
          (events) => {
            for (const e of events) {
              if ((e.type === 'create' || e.type === 'update') && ROLLOUT_FILE.test(path.basename(e.path))) {
                schedule(e.path);
              }
            }
          },
          { ignore: [], recursive: true },
        );
      } catch {
        // Fall back to chokidar if parcel's native watcher is unavailable.
        watcher = createChokidarWatcher();
        unsubscribe = await watcher.subscribe(
          deps.sessionsDir,
          (events) => {
            for (const e of events) {
              if ((e.type === 'create' || e.type === 'update') && ROLLOUT_FILE.test(path.basename(e.path))) {
                schedule(e.path);
              }
            }
          },
          { ignore: [], recursive: true },
        );
      }
    },

    async stop(): Promise<void> {
      if (timer) {
        clearTimeout(timer);
        timer = null;
      }
      pending.clear();
      if (unsubscribe) {
        await unsubscribe();
        unsubscribe = null;
      }
      watcher = null;
      state.clear();
    },
  };
}
