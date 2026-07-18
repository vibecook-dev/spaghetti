/**
 * GrokReader — drives a {@link ProjectParseSink} from Grok's on-disk layout.
 *
 * The RFC 006 §3.1 "record production" seam for Grok. Grok's layout differs from
 * both Claude (projects/<slug>/<session>.jsonl) and Codex (one flat rollout with
 * a `session_meta` first line): Grok keeps a DIRECTORY per session,
 *
 *   sessions/<url-encoded-abs-cwd>/<session-uuid>/chat_history.jsonl
 *                                                 summary.json   (metadata)
 *                                                 events.jsonl   (timeline)
 *                                                 signals.json   (aggregate stats)
 *
 * so the project cwd and session id do NOT live inside chat_history.jsonl — they
 * live in the sibling `summary.json` (`info.cwd`, `info.id`, `created_at`,
 * `generated_title`, `head_branch`). That is cleaner than Codex's peek: no
 * line-scan is needed for metadata. The reader:
 *   1. discovers `chat_history.jsonl` files (recursive scan),
 *   2. reads each session dir's `summary.json` for cwd / id / title / times
 *      (falling back to the URL-encoded dir name + uuid dir if it is missing),
 *   3. groups sessions by project (cwd-derived slug),
 *   4. streams every chat_history line through `onMessage`; all known Grok
 *      transcript and tool records are retained losslessly.
 *
 * Emits the same sink-event shape as the Claude/Codex cold starts
 * (`onProject → onSession → onMessage* → onSessionComplete → onProjectComplete`),
 * so `IngestService` stores Grok rows unchanged — only `source_id` and the raw
 * record shape differ.
 *
 * NOTE: like Codex, the project slug is the cwd with `/`→`-`. Schema v6+ uses
 * composite PK `(source_id, slug)` so Claude/Codex/Grok can share a cwd without
 * merging project rows. Sessions/messages still key primarily by session id
 * (product UUIDs are expected unique across agents).
 */

import * as path from 'node:path';

import type { FileService } from '../../io/file-service.js';
import type { ProjectParseSink } from '../../data/parse-sink.js';
import type { IngestService } from '../../data/ingest-service.js';
import type { SessionIndexEntry, SessionsIndex } from '../../types/index.js';
import { applyGrokSidecars } from './sidecars.js';

const CHAT_HISTORY_FILE = 'chat_history.jsonl';
const SUMMARY_FILE = 'summary.json';
const UUID = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i;
const FIRST_PROMPT_MAX = 200;

export interface GrokSessionMeta {
  cwd: string;
  sessionId: string;
  created: string | null;
  updated: string | null;
  title: string;
  summary: string;
  gitBranch: string;
}

/** Encode a project cwd into an opaque slug (mirrors Claude/Codex `/`→`-`). */
export function encodeGrokSlug(cwd: string): string {
  // Windows cwds separate with `\`, so both separators are folded.
  return cwd.replace(/[/\\]/g, '-');
}

/**
 * Read a Grok session's metadata from the sibling `summary.json`. Falls back to
 * the URL-encoded parent-of-parent directory name (the cwd) and the session-uuid
 * directory name when `summary.json` is missing or unreadable. Shared by the
 * cold reader and the live watcher. Returns null when no cwd can be determined.
 */
export function readGrokSessionMeta(fileService: FileService, chatHistoryFile: string): GrokSessionMeta | null {
  const sessionDir = path.dirname(chatHistoryFile);
  const uuidDir = path.basename(sessionDir);
  const encodedCwdDir = path.basename(path.dirname(sessionDir));

  let cwd: string | null = null;
  let sessionId: string | null = null;
  let created: string | null = null;
  let updated: string | null = null;
  let title = '';
  let summary = '';
  let gitBranch = '';

  try {
    const parsed = JSON.parse(fileService.readFileSync(path.join(sessionDir, SUMMARY_FILE))) as {
      info?: { id?: unknown; cwd?: unknown };
      git_root_dir?: unknown;
      created_at?: unknown;
      updated_at?: unknown;
      last_active_at?: unknown;
      generated_title?: unknown;
      session_summary?: unknown;
      head_branch?: unknown;
    };
    if (typeof parsed.info?.cwd === 'string') cwd = parsed.info.cwd;
    else if (typeof parsed.git_root_dir === 'string') cwd = parsed.git_root_dir.replace(/\/$/, '');
    if (typeof parsed.info?.id === 'string') sessionId = parsed.info.id;
    if (typeof parsed.created_at === 'string') created = parsed.created_at;
    if (typeof parsed.updated_at === 'string') updated = parsed.updated_at;
    else if (typeof parsed.last_active_at === 'string') updated = parsed.last_active_at;
    if (typeof parsed.generated_title === 'string') title = parsed.generated_title;
    if (typeof parsed.session_summary === 'string') {
      summary = parsed.session_summary;
      if (!title) title = parsed.session_summary;
    }
    if (typeof parsed.head_branch === 'string') gitBranch = parsed.head_branch;
  } catch {
    // no/invalid summary.json — fall back to the directory names below.
  }

  // Fallbacks: the encoded cwd dir name decodes straight to the abs cwd.
  if (!cwd) {
    try {
      cwd = decodeURIComponent(encodedCwdDir);
    } catch {
      cwd = null;
    }
  }
  if (!cwd) return null;

  if (!sessionId) {
    const m = uuidDir.match(UUID);
    sessionId = m ? m[0] : uuidDir;
  }

  return {
    cwd,
    sessionId,
    created,
    updated,
    title: title.slice(0, FIRST_PROMPT_MAX),
    summary,
    gitBranch,
  };
}

/** Warm-start hooks (RFC 006). Let a caller skip unchanged files + track them. */
export interface GrokReadOptions {
  /**
   * Return false to SKIP streaming a session's messages (warm-start: the file is
   * unchanged and already ingested). `onProject`/`onSession` still fire, so
   * project/session metadata stays current. Default: read everything.
   */
  shouldReadMessages?(file: string, mtimeMs: number): boolean;
  /** Called after each discovered file with its stats — for fingerprint upserts. */
  onFileSeen?(file: string, mtimeMs: number, size: number, lastByte: number): void;
}

export class GrokReader {
  constructor(
    private readonly fileService: FileService,
    private readonly sessionsDir: string,
  ) {}

  /** Discover, group, and stream all Grok sessions into the sink. */
  readAll(sink: ProjectParseSink, opts?: GrokReadOptions): void {
    const files = this.discover();

    // Group sessions by project (cwd-derived slug); one session dir = one session.
    const projects = new Map<
      string,
      { originalPath: string; sessions: { file: string; entry: SessionIndexEntry; mtimeMs: number; size: number }[] }
    >();

    for (const file of files) {
      const meta = readGrokSessionMeta(this.fileService, file);
      if (!meta) continue;
      const slug = encodeGrokSlug(meta.cwd);
      const stats = this.fileService.getStats(file);
      const mtimeMs = stats?.mtimeMs ?? 0;
      const modifiedIso = meta.updated ?? (stats ? new Date(mtimeMs).toISOString() : '');
      const entry: SessionIndexEntry = {
        sessionId: meta.sessionId,
        fullPath: file,
        fileMtime: mtimeMs,
        firstPrompt: meta.title || 'No prompt',
        summary: meta.summary,
        messageCount: 0,
        created: meta.created ?? modifiedIso,
        modified: modifiedIso,
        gitBranch: meta.gitBranch,
        projectPath: meta.cwd,
        isSidechain: false,
      };
      let proj = projects.get(slug);
      if (!proj) {
        proj = { originalPath: meta.cwd, sessions: [] };
        projects.set(slug, proj);
      }
      proj.sessions.push({ file, entry, mtimeMs, size: stats?.size ?? 0 });
    }

    for (const [slug, proj] of projects) {
      const sessionsIndex: SessionsIndex = {
        version: 1,
        originalPath: proj.originalPath,
        entries: proj.sessions.map((s) => s.entry),
      };
      sink.onProject(slug, proj.originalPath, sessionsIndex);

      for (const { file, entry, mtimeMs, size } of proj.sessions) {
        sink.onSession(slug, entry);
        let lineCount = 0;
        let lastByte = 0;
        const read = opts?.shouldReadMessages ? opts.shouldReadMessages(file, mtimeMs) : true;
        if (read) {
          try {
            const res = this.fileService.readJsonlStreaming<unknown>(file, (line, index, byteOffset) => {
              // Every line goes to onMessage; the extractor retains all known
              // Grok transcript and tool record shapes.
              sink.onMessage(slug, entry.sessionId, line as never, index, byteOffset);
              lineCount++;
              lastByte = byteOffset;
            });
            lastByte = res.finalBytePosition;
          } catch {
            // unreadable chat_history file — skip
          }
        }
        sink.onSessionComplete(slug, entry.sessionId, lineCount, lastByte);

        // events.jsonl timestamps + signals.json session tokens (best-effort).
        if (isIngestService(sink)) {
          applyGrokSidecars(this.fileService, file, entry.sessionId, sink.getSessionWriteApi(), {
            fallbackCreated: entry.created || null,
          });
        }

        opts?.onFileSeen?.(file, mtimeMs, size, lastByte);
      }

      sink.onProjectComplete(slug);
    }
  }

  private discover(): string[] {
    try {
      return this.fileService
        .scanDirectorySync(this.sessionsDir, { pattern: CHAT_HISTORY_FILE, recursive: true })
        .filter((f) => path.basename(f) === CHAT_HISTORY_FILE);
    } catch {
      return [];
    }
  }
}

function isIngestService(sink: ProjectParseSink): sink is IngestService {
  return typeof (sink as IngestService).getSessionWriteApi === 'function';
}
