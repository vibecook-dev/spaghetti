/**
 * CodexReader — drives a {@link ProjectParseSink} from Codex's on-disk layout.
 *
 * This is the RFC 006 §3.1 "record production" seam for Codex. Where Claude
 * Code's `ProjectParserImpl` walks `projects/<slug>/<session>.jsonl` plus a
 * whole taxonomy (subagents/todos/plans/…), Codex keeps one rollout file per
 * session under `sessions/YYYY/MM/DD/rollout-<ts>-<uuid7>.jsonl`, with the
 * project identity (`cwd`) and session id living in the file's first line
 * (`type: session_meta`). So the reader:
 *   1. discovers rollout files (recursive scan),
 *   2. peeks each file's `session_meta` → project `cwd` + session id,
 *   3. groups sessions by project and emits `onProject` / `onSession`,
 *   4. streams every line through `onMessage` — the Codex `MessageExtractor`
 *      returns `null` for non-chat lines; IngestService still sees
 *      `event_msg`/`token_count` and attributes turn tokens onto the previous
 *      assistant row (ccusage style).
 *
 * It emits the same sink-event shape the claude cold-start uses
 * (`onProject → onSession → onMessage* → onSessionComplete → onProjectComplete`),
 * so the existing `IngestService` sink stores Codex rows unchanged — only the
 * `source_id` (bound by the sink) and the raw record shape differ.
 *
 * NOTE: the project slug here is the cwd with path separators replaced by `-`,
 * mirroring Claude's scheme. Until the `(source_id, slug)` composite PK lands
 * (RFC 006 §8), a Codex and a Claude session sharing a cwd would collide on
 * `projects.slug` in ONE shared DB — fine for a codex-only index, but that PR
 * must precede running both sources into the same store.
 */

import * as path from 'node:path';

import type { FileService } from '../../io/file-service.js';
import type { ProjectParseSink } from '../../data/parse-sink.js';
import type { SessionIndexEntry, SessionsIndex } from '../../types/index.js';
import { considerCodexFirstPromptLine } from './first-prompt.js';

const ROLLOUT_FILE = /^rollout-.*\.jsonl$/;
const UUID = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i;
// Bound the metadata peek so we never read a large rollout fully just to find
// the first *real* user prompt (injected context often occupies the first N lines).
const PEEK_LINE_LIMIT = 100;
const STOP_PEEK = Symbol('codex-peek-stop');

interface CodexSessionMeta {
  cwd: string;
  sessionId: string;
  timestamp: string | null;
  firstPrompt: string;
}

/**
 * Encode a project cwd into an opaque slug (mirrors Claude's `/`→`-`).
 * Windows cwds separate with `\`, so both separators are folded.
 */
function encodeSlug(cwd: string): string {
  return cwd.replace(/[/\\]/g, '-');
}

/** Warm-start hooks (RFC 006). Let a caller skip unchanged files + track them. */
export interface CodexReadOptions {
  /**
   * Return false to SKIP streaming a rollout's messages (warm-start: the file
   * is unchanged and already ingested). `onProject`/`onSession` still fire, so
   * project/session metadata stays current. Default: read everything.
   */
  shouldReadMessages?(file: string, mtimeMs: number): boolean;
  /** Called after each discovered file with its stats — for fingerprint upserts. */
  onFileSeen?(file: string, mtimeMs: number, size: number, lastByte: number): void;
}

export class CodexReader {
  constructor(
    private readonly fileService: FileService,
    private readonly sessionsDir: string,
  ) {}

  /** Discover, group, and stream all rollout files into the sink. */
  readAll(sink: ProjectParseSink, opts?: CodexReadOptions): void {
    const files = this.discover();

    // Group sessions by project (cwd-derived slug); one rollout file = one session.
    const projects = new Map<
      string,
      { originalPath: string; sessions: { file: string; entry: SessionIndexEntry; mtimeMs: number; size: number }[] }
    >();

    for (const file of files) {
      const meta = this.peek(file);
      if (!meta) continue;
      const slug = encodeSlug(meta.cwd);
      const stats = this.fileService.getStats(file);
      const mtimeMs = stats?.mtimeMs ?? 0;
      const modifiedIso = stats ? new Date(mtimeMs).toISOString() : (meta.timestamp ?? '');
      const entry: SessionIndexEntry = {
        sessionId: meta.sessionId,
        fullPath: file,
        fileMtime: mtimeMs,
        firstPrompt: meta.firstPrompt || 'No prompt',
        summary: '',
        messageCount: 0,
        created: meta.timestamp ?? modifiedIso,
        modified: modifiedIso,
        gitBranch: '',
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
              // Every line goes to onMessage; the Codex extractor skips the
              // non-message lines (session_meta / event_msg / non-message
              // response_items) by returning null.
              sink.onMessage(slug, entry.sessionId, line as never, index, byteOffset);
              lineCount++;
              lastByte = byteOffset;
            });
            lastByte = res.finalBytePosition;
          } catch {
            // unreadable rollout file — skip
          }
        }
        sink.onSessionComplete(slug, entry.sessionId, lineCount, lastByte);
        opts?.onFileSeen?.(file, mtimeMs, size, lastByte);
      }

      sink.onProjectComplete(slug);
    }
  }

  private discover(): string[] {
    try {
      return this.fileService
        .scanDirectorySync(this.sessionsDir, { pattern: 'rollout-*.jsonl', recursive: true })
        .filter((f) => ROLLOUT_FILE.test(basename(f)));
    } catch {
      return [];
    }
  }

  /**
   * Read a rollout's `session_meta` + first *human* user prompt without a full scan.
   *
   * Skips Codex-injected user turns (`environment_context`, AGENTS.md, plugins, …)
   * and prefers `event_msg`/`user_message` when present — see `first-prompt.ts`.
   */
  private peek(file: string): CodexSessionMeta | null {
    let cwd: string | null = null;
    let sessionId: string | null = null;
    let timestamp: string | null = null;
    let firstPrompt = '';

    try {
      this.fileService.readJsonlStreaming<Record<string, unknown>>(file, (line, index) => {
        const type = line.type;
        const payload = line.payload as Record<string, unknown> | undefined;
        if (type === 'session_meta' && payload) {
          if (typeof payload.cwd === 'string') cwd = payload.cwd;
          if (typeof payload.id === 'string') sessionId = payload.id;
          if (typeof line.timestamp === 'string') timestamp = line.timestamp;
        } else {
          firstPrompt = considerCodexFirstPromptLine(firstPrompt, line);
        }
        // Stop once we have the project cwd and a real user prompt, or hit the cap.
        if ((cwd && firstPrompt) || index >= PEEK_LINE_LIMIT) throw STOP_PEEK;
      });
    } catch (e) {
      if (e !== STOP_PEEK) return null;
    }

    if (!cwd) return null;
    return {
      cwd,
      sessionId: sessionId ?? uuidFromFilename(file) ?? basename(file),
      timestamp,
      firstPrompt,
    };
  }
}

function basename(p: string): string {
  // path.basename, NOT lastIndexOf('/'): scanDirectorySync returns
  // native separators, so the slash-only version returned the WHOLE
  // path on Windows and the ROLLOUT_FILE filter discovered nothing.
  return path.basename(p);
}

function uuidFromFilename(file: string): string | null {
  const m = basename(file).match(UUID);
  return m ? m[0] : null;
}
