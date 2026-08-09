/**
 * Default filesystem roots for the OpenAI Codex CLI agent source.
 *
 * Only `sessionsDir` is load-bearing for Codex ingest (rollout tree).
 * Spaghetti-owned hooks/channel paths come from `stateDir`.
 */

import * as os from 'node:os';
import * as path from 'node:path';

import type { AgentSourcePaths } from '../types.js';

/** Default Codex data directory: `~/.codex`. */
export function defaultCodexDir(): string {
  return path.join(os.homedir(), '.codex');
}

/** Default Spaghetti state directory: `~/.spaghetti`. */
export function defaultSpaghettiStateDir(): string {
  return path.join(os.homedir(), '.spaghetti');
}

/** Build the path map for a Codex installation. */
export function buildCodexPaths(rootDir: string): AgentSourcePaths {
  return {
    // sessions/YYYY/MM/DD/rollout-*.jsonl
    sessionsDir: path.join(rootDir, 'sessions'),
    settingsFile: path.join(rootDir, 'config.toml'),
    // Spaghetti-owned state (source-independent).
  };
}
