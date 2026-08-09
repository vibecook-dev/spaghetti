/**
 * OpenAI Codex CLI AgentSource factory (RFC 006 second source).
 *
 * The paired-adapter data half for Codex: its `source_id`, a `classify` for its
 * path layout, and a `MessageExtractor` for its rollout envelope. Reading is
 * done by {@link CodexReader} (Codex's layout differs enough from Claude's that
 * it needs its own record-production path — RFC 006 §3.1).
 */

import * as path from 'node:path';

import type { AgentSource } from '../types.js';
import type { RouteResult } from '../../live/router.js';
import type { FileService } from '../../io/file-service.js';
import { codexMessageExtractor } from './message-extractor.js';
import { CodexReader } from './reader.js';
import { buildCodexPaths, defaultCodexDir, defaultSpaghettiStateDir } from './paths.js';

export { codexMessageExtractor } from './message-extractor.js';
export { CodexReader } from './reader.js';
export { CodexLifecycleOwner } from './lifecycle-owner.js';
export { createCodexIngestHooks } from './ingest-hooks.js';
export { buildCodexPaths, defaultCodexDir } from './paths.js';
export { parseCodexTokenCount, type CodexTokenUsage, type ParsedCodexTokenCount } from './token-usage.js';
export { countTextTokens, estimateTokensFromMessageRows, type EstimatedMessageTokens } from './estimate-tokens.js';
export {
  isCodexInjectedUserText,
  considerCodexFirstPromptLine,
  codexContentText,
  CODEX_FIRST_PROMPT_MAX,
} from './first-prompt.js';
export { isCodexInternalSessionPayload } from './session-meta.js';

const UUID = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i;

/**
 * Classify a Codex path (product-owned; parallel to Claude's classify.ts).
 * Transcripts live under `sessions/YYYY/MM/DD/rollout-*.jsonl` → `session`;
 * anything else is ignored.
 */
export function classifyCodexPath(absPath: string, rootDir: string): RouteResult {
  const sessionsDir = path.join(rootDir, 'sessions');
  const base = path.basename(absPath);
  if ((absPath === sessionsDir || absPath.startsWith(sessionsDir + path.sep)) && /^rollout-.*\.jsonl$/.test(base)) {
    const m = base.match(UUID);
    return m ? { category: 'session', sessionId: m[0] } : { category: 'session' };
  }
  return { category: 'ignored' };
}

export interface CodexSourceOptions {
  /** Override agent data root (default `~/.codex`). */
  rootDir?: string;
  /** Override Spaghetti state root (default `~/.spaghetti`). */
  stateDir?: string;
}

/** Create the Codex CLI agent source adapter. */
export function createCodexSource(options?: CodexSourceOptions): AgentSource {
  const rootDir = options?.rootDir ?? defaultCodexDir();
  const stateDir = options?.stateDir ?? defaultSpaghettiStateDir();
  return {
    id: 'codex',
    rootDir,
    stateDir,
    paths: buildCodexPaths(rootDir),
    classify: (absPath: string) => classifyCodexPath(absPath, rootDir),
    messages: codexMessageExtractor,
  };
}

/** Create a {@link CodexReader} for a source's sessions directory. */
export function createCodexReader(source: AgentSource, fileService: FileService): CodexReader {
  return new CodexReader(fileService, source.paths.sessionsDir);
}
