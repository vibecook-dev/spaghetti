/**
 * Default filesystem roots for the Claude Code agent source.
 */

import * as os from 'node:os';
import * as path from 'node:path';

import type { ClaudeCodePaths } from '../types.js';

/** Default Claude Code data directory: `~/.claude`. */
export function defaultClaudeDir(): string {
  return path.join(os.homedir(), '.claude');
}

/** Default Spaghetti state directory: `~/.spaghetti`. */
export function defaultSpaghettiStateDir(): string {
  return path.join(os.homedir(), '.spaghetti');
}

/** Build the full path map for a Claude Code installation. */
export function buildClaudeCodePaths(rootDir: string): ClaudeCodePaths {
  return {
    projectsDir: path.join(rootDir, 'projects'),
    todosDir: path.join(rootDir, 'todos'),
    plansDir: path.join(rootDir, 'plans'),
    tasksDir: path.join(rootDir, 'tasks'),
    fileHistoryDir: path.join(rootDir, 'file-history'),
    sessionsDir: path.join(rootDir, 'sessions'),
    settingsFile: path.join(rootDir, 'settings.json'),
  };
}
