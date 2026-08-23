/**
 * Claude Code AgentSource factory.
 */

import type { AgentSource, ClaudeCodePaths } from '../types.js';
import { classifyClaudePath } from './classify.js';
import { claudeCodeMessageExtractor } from './message-extractor.js';
import { buildClaudeCodePaths, defaultClaudeDir, defaultSpaghettiStateDir } from './paths.js';

export { buildClaudeCodePaths, defaultClaudeDir, defaultSpaghettiStateDir } from './paths.js';
export { listActiveSessionsFromDir, isProcessAlive, type ListActiveSessionsOptions } from './active-sessions.js';
export { ClaudeCodeLifecycleOwner } from './lifecycle-owner.js';
export { classifyClaudePath, classify, HARD_IGNORE_SEGMENTS, HARD_IGNORE_SUFFIXES } from './classify.js';
export {
  createClaudeCodeParser,
  createProjectParser,
  createConfigParser,
  createAnalyticsParser,
  type ClaudeCodeParser,
  type ClaudeCodeParserOptions,
  type ProjectParser,
  type ConfigParser,
  type AnalyticsParser,
} from './parser/index.js';
export {
  createClaudeCodeLiveUpdates,
  createClaudeCodeLiveDiskIngest,
  type ClaudeCodeLiveUpdates,
  type ClaudeCodeLiveDiskIngest,
  watchSessionTranscript,
} from './live/index.js';

export interface ClaudeCodeSourceOptions {
  /** Override agent data root (default `~/.claude`). */
  rootDir?: string;
  /** Override Spaghetti state root (default `~/.spaghetti`). */
  stateDir?: string;
}

/** Claude Code source with the full product path map. */
export interface ClaudeCodeAgentSource extends AgentSource {
  readonly id: 'claude-code';
  readonly paths: ClaudeCodePaths;
}

/**
 * Create the Claude Code agent source adapter.
 *
 * Callers that previously passed `claudeDir` into `createSpaghettiService`
 * map to `{ rootDir: claudeDir }`.
 */
export function createClaudeCodeSource(options?: ClaudeCodeSourceOptions): ClaudeCodeAgentSource {
  const rootDir = options?.rootDir ?? defaultClaudeDir();
  const stateDir = options?.stateDir ?? defaultSpaghettiStateDir();
  return {
    id: 'claude-code',
    rootDir,
    stateDir,
    paths: buildClaudeCodePaths(rootDir),
    // Path→category rules live in ./classify.ts (product layout). The live
    // plane calls source.classify — never hardcodes Claude's tree.
    classify: (absPath: string) => classifyClaudePath(absPath, rootDir),
    messages: claudeCodeMessageExtractor,
  };
}
