/**
 * Grok CLI (xAI) AgentSource factory (RFC 006 third source).
 *
 * The paired-adapter data half for Grok: its `source_id`, a `classify` for its
 * path layout, and a `MessageExtractor` for its chat_history records. Reading is
 * done by {@link GrokReader} — Grok's directory-per-session layout differs from
 * both Claude and Codex, so it needs its own record-production path.
 */

import * as path from 'node:path';

import type { AgentSource } from '../types.js';
import type { RouteResult } from '../../live/router.js';
import type { FileService } from '../../io/file-service.js';
import { grokMessageExtractor } from './message-extractor.js';
import { GrokReader } from './reader.js';
import { buildGrokPaths, defaultGrokDir, defaultSpaghettiStateDir } from './paths.js';

export { grokMessageExtractor } from './message-extractor.js';
export {
  GrokReader,
  readGrokSessionMeta,
  encodeGrokSlug,
  type GrokReadOptions,
  type GrokSessionMeta,
} from './reader.js';
export { GrokLifecycleOwner } from './lifecycle-owner.js';
export { createGrokLiveWatch, type GrokLiveWatch, type GrokLiveWatchDeps } from './live-watch.js';
export { buildGrokPaths, defaultGrokDir } from './paths.js';
export {
  applyGrokSidecars,
  buildTimestampMap,
  parseGrokEvents,
  parseGrokSignals,
  collectChatLineTypes,
  type GrokEventLine,
  type GrokSignals,
} from './sidecars.js';

const UUID = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i;

/**
 * Classify a Grok path (product-owned; parallel to Claude's classify.ts).
 * Transcripts live at `sessions/<encoded-cwd>/<session-uuid>/chat_history.jsonl`
 * → `session` (session id = the uuid directory); anything else is ignored.
 */
export function classifyGrokPath(absPath: string, rootDir: string): RouteResult {
  const sessionsDir = path.join(rootDir, 'sessions');
  const under = absPath === sessionsDir || absPath.startsWith(sessionsDir + path.sep);
  if (under && path.basename(absPath) === 'chat_history.jsonl') {
    const uuidDir = path.basename(path.dirname(absPath));
    const m = uuidDir.match(UUID);
    return m ? { category: 'session', sessionId: m[0] } : { category: 'session' };
  }
  return { category: 'ignored' };
}

export interface GrokSourceOptions {
  /** Override agent data root (default `~/.grok`). */
  rootDir?: string;
  /** Override Spaghetti state root (default `~/.spaghetti`). */
  stateDir?: string;
}

/** Create the Grok CLI agent source adapter. */
export function createGrokSource(options?: GrokSourceOptions): AgentSource {
  const rootDir = options?.rootDir ?? defaultGrokDir();
  const stateDir = options?.stateDir ?? defaultSpaghettiStateDir();
  return {
    id: 'grok',
    rootDir,
    stateDir,
    paths: buildGrokPaths(rootDir),
    classify: (absPath: string) => classifyGrokPath(absPath, rootDir),
    messages: grokMessageExtractor,
  };
}

/** Create a {@link GrokReader} for a source's sessions directory. */
export function createGrokReader(source: AgentSource, fileService: FileService): GrokReader {
  return new GrokReader(fileService, source.paths.sessionsDir);
}
