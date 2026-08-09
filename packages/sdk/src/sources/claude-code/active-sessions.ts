/**
 * Active Claude Code sessions — reads `~/.claude/sessions/{pid}.json`.
 *
 * Owned by the Claude Code source, not by an ingest plane: the discovery
 * files are part of Claude Code's on-disk product layout, and doctor /
 * index-live reporting reads them independently of any streaming surface.
 * Types: `ActiveSessionFile` in `types/claude/toplevel-files-data.ts`.
 */

import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

import type { ActiveSessionFile } from '../../types/claude/toplevel-files-data.js';

/**
 * True if the OS still has a process with this pid (signal 0 probe).
 * Does not require ownership of the process.
 */
export function isProcessAlive(pid: number): boolean {
  if (!Number.isFinite(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (err) {
    const code = (err as NodeJS.ErrnoException).code;
    // EPERM: process exists but we can't signal it — treat as alive.
    if (code === 'EPERM') return true;
    return false;
  }
}

function parseActiveSessionFile(raw: string, fallbackPid?: number): ActiveSessionFile | null {
  try {
    const parsed = JSON.parse(raw) as Partial<ActiveSessionFile>;
    if (!parsed || typeof parsed !== 'object') return null;
    const pid = typeof parsed.pid === 'number' ? parsed.pid : fallbackPid;
    const sessionId = typeof parsed.sessionId === 'string' ? parsed.sessionId : null;
    const cwd = typeof parsed.cwd === 'string' ? parsed.cwd : null;
    const startedAt = typeof parsed.startedAt === 'number' ? parsed.startedAt : null;
    if (pid == null || sessionId == null || cwd == null || startedAt == null) return null;
    return {
      pid,
      sessionId,
      cwd,
      startedAt,
      kind: parsed.kind,
      entrypoint: parsed.entrypoint,
      name: parsed.name,
      status: parsed.status,
      updatedAt: parsed.updatedAt,
      statusUpdatedAt: parsed.statusUpdatedAt,
      procStart: parsed.procStart,
      version: parsed.version,
      peerProtocol: parsed.peerProtocol,
      nameSource: parsed.nameSource,
      bridgeSessionId: parsed.bridgeSessionId,
    };
  } catch {
    return null;
  }
}

export interface ListActiveSessionsOptions {
  /**
   * When true (default), drop rows whose pid is not running.
   * Stale discovery files are common after crashes.
   */
  requireAlive?: boolean;
}

/**
 * Read all active-session files under `sessionsDir` (typically
 * `~/.claude/sessions`).
 */
export function listActiveSessionsFromDir(
  sessionsDir: string,
  options?: ListActiveSessionsOptions,
): ActiveSessionFile[] {
  const requireAlive = options?.requireAlive !== false;
  if (!existsSync(sessionsDir)) return [];

  let entries: string[];
  try {
    entries = readdirSync(sessionsDir);
  } catch {
    return [];
  }

  const out: ActiveSessionFile[] = [];
  for (const entry of entries) {
    if (!entry.endsWith('.json')) continue;
    const pidFromName = Number.parseInt(entry.replace(/\.json$/i, ''), 10);
    const filePath = join(sessionsDir, entry);
    let raw: string;
    try {
      raw = readFileSync(filePath, 'utf-8');
    } catch {
      continue;
    }
    const session = parseActiveSessionFile(raw, Number.isFinite(pidFromName) ? pidFromName : undefined);
    if (!session) continue;
    if (requireAlive && !isProcessAlive(session.pid)) continue;
    out.push(session);
  }

  // Newest first
  out.sort((a, b) => (b.updatedAt ?? b.startedAt) - (a.updatedAt ?? a.startedAt));
  return out;
}
