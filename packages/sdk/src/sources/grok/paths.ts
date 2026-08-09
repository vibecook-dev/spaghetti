/**
 * Default filesystem roots for the Grok CLI (xAI) agent source.
 *
 * Only `sessionsDir` is load-bearing (`~/.grok/sessions`). Spaghetti-owned
 * hooks/channel paths come from `stateDir`.
 */

import * as os from 'node:os';
import * as path from 'node:path';

import type { AgentSourcePaths } from '../types.js';

/** Default Grok data directory: `~/.grok`. */
export function defaultGrokDir(): string {
  return path.join(os.homedir(), '.grok');
}

/** Default Spaghetti state directory: `~/.spaghetti`. */
export function defaultSpaghettiStateDir(): string {
  return path.join(os.homedir(), '.spaghetti');
}

/** Build the path map for a Grok installation. */
export function buildGrokPaths(rootDir: string): AgentSourcePaths {
  return {
    // sessions/<url-encoded-abs-cwd>/<session-uuid>/chat_history.jsonl
    sessionsDir: path.join(rootDir, 'sessions'),
    settingsFile: path.join(rootDir, 'user-settings.json'),
    // Spaghetti-owned state (source-independent).
  };
}
