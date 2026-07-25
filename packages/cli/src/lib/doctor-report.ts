/**
 * Doctor report — pure data collection for spaghetti's health check.
 *
 * Both the `doctor` CLI command (text) and the `DoctorView` TUI view (Ink)
 * call `collectDoctorReport(version)` and render the same shape differently.
 * Keeping the collection pure means adding a new field anywhere shows up in
 * both surfaces at once.
 */

import { execSync } from 'node:child_process';
import { existsSync, readdirSync, statSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import {
  createClaudeCodeSource,
  createHookEventWatcher,
  createRuntimeBridge,
  defaultDbPathForEngine,
  getChannelSessionsDir,
  isNativeIngestEnabled,
  resolveActiveEngine,
  resolveEngine,
  type IngestEngine,
} from '@vibecook/spaghetti-sdk';
import { PLUGINS, PLUGINS_DIR, SETTINGS_PATH, getPluginState, type PluginState } from './plugins.js';

export const CLAUDE_DIR = join(homedir(), '.claude');
export const CODEX_DIR = join(homedir(), '.codex');
export const GROK_DIR = join(homedir(), '.grok');

export interface PathStatus {
  path: string;
  exists: boolean;
}

export interface AgentRootReport {
  id: string;
  label: string;
  path: string;
  exists: boolean;
  bin: string | null;
}

export interface EnvironmentReport {
  node: string;
  platform: NodeJS.Platform;
  arch: string;
  claudeBin: string | null;
  claudeDir: PathStatus;
  /** Multi-agent data roots (claude / codex / grok). */
  agentRoots: AgentRootReport[];
  settings: PathStatus;
  pluginsDir: PathStatus;
}

export interface PluginReport {
  name: string;
  description: string;
  state: PluginState;
}

export type HookEventsReport =
  | { kind: 'ok'; path: string; count: number; mtimeMs: number }
  | { kind: 'missing'; path: string }
  | { kind: 'error'; message: string };

export type ChannelSessionsReport =
  | { kind: 'ok'; path: string; activeCount: number }
  | { kind: 'absent'; path: string };

/** Index + Plane 2/3 defaults (follow-up: "doctor shows live status"). */
export interface IndexLiveReport {
  /** Configured engine preference (env / settings / default). */
  preferredEngine: IngestEngine;
  /** Effective engine after native availability. */
  effectiveEngine: IngestEngine;
  nativeAvailable: boolean;
  nativeVersion: string | null;
  dbPath: string;
  dbExists: boolean;
  dbSizeBytes: number | null;
  /** Long-lived TUI / playground default. */
  liveDefaultLongLived: boolean;
  /** One-shot CLI commands default. */
  liveDefaultOneShot: boolean;
  activeSessionsDir: string;
  activeSessionsOnDisk: number;
  activeSessionsAlive: number;
}

export interface DoctorReport {
  version: string;
  environment: EnvironmentReport;
  indexLive: IndexLiveReport;
  plugins: PluginReport[];
  hookEvents: HookEventsReport;
  channelSessions: ChannelSessionsReport;
}

/**
 * Locate an agent CLI on PATH.
 *
 * `command -v` is a POSIX shell builtin — on Windows it runs under
 * cmd.exe, which has no such command, so every lookup threw and doctor
 * reported "not in PATH" even for installed binaries. `where` is the
 * Windows equivalent and prints one match per line; take the first.
 */
function findBin(name: string): string | null {
  const cmd = process.platform === 'win32' ? `where ${name}` : `command -v ${name}`;
  try {
    const out = execSync(cmd, {
      encoding: 'utf-8',
      stdio: ['ignore', 'pipe', 'ignore'],
    }).trim();
    return out.split(/\r?\n/)[0]?.trim() || null;
  } catch {
    return null;
  }
}

function findClaudeBinary(): string | null {
  return findBin('claude');
}

function collectAgentRoots(): AgentRootReport[] {
  return [
    {
      id: 'claude-code',
      label: '~/.claude',
      path: CLAUDE_DIR,
      exists: existsSync(CLAUDE_DIR),
      bin: findClaudeBinary(),
    },
    {
      id: 'codex',
      label: '~/.codex',
      path: CODEX_DIR,
      exists: existsSync(CODEX_DIR),
      bin: findBin('codex'),
    },
    {
      id: 'grok',
      label: '~/.grok',
      path: GROK_DIR,
      exists: existsSync(GROK_DIR),
      bin: findBin('grok'),
    },
  ];
}

function collectEnvironment(): EnvironmentReport {
  return {
    node: process.version,
    platform: process.platform,
    arch: process.arch,
    claudeBin: findClaudeBinary(),
    claudeDir: { path: CLAUDE_DIR, exists: existsSync(CLAUDE_DIR) },
    agentRoots: collectAgentRoots(),
    settings: { path: SETTINGS_PATH, exists: existsSync(SETTINGS_PATH) },
    pluginsDir: { path: PLUGINS_DIR, exists: existsSync(PLUGINS_DIR) },
  };
}

function collectHookEvents(): HookEventsReport {
  try {
    const watcher = createHookEventWatcher();
    const path = watcher.getEventsPath();
    if (!existsSync(path)) return { kind: 'missing', path };
    const stats = statSync(path);
    const history = watcher.getHistory();
    return { kind: 'ok', path, count: history.length, mtimeMs: stats.mtimeMs };
  } catch (err) {
    return { kind: 'error', message: err instanceof Error ? err.message : String(err) };
  }
}

function collectChannelSessions(): ChannelSessionsReport {
  const path = getChannelSessionsDir();
  if (!existsSync(path)) return { kind: 'absent', path };
  let activeCount = 0;
  try {
    activeCount = readdirSync(path).filter((f) => f.endsWith('.json')).length;
  } catch {
    /* swallow — shown as 0 */
  }
  return { kind: 'ok', path, activeCount };
}

function collectIndexLive(): IndexLiveReport {
  const preferredEngine = resolveEngine();
  const active = resolveActiveEngine();
  const dbPath = defaultDbPathForEngine(active.engine);
  let dbExists = false;
  let dbSizeBytes: number | null = null;
  try {
    if (existsSync(dbPath)) {
      dbExists = true;
      dbSizeBytes = statSync(dbPath).size;
    }
  } catch {
    dbExists = existsSync(dbPath);
  }

  const source = createClaudeCodeSource();
  const bridge = createRuntimeBridge(source);
  const alive = bridge.listActiveSessions({ requireAlive: true });
  const onDisk = bridge.listActiveSessions({ requireAlive: false });

  return {
    preferredEngine,
    effectiveEngine: active.engine,
    nativeAvailable: isNativeIngestEnabled() || active.nativeAvailable,
    nativeVersion: active.nativeVersion,
    dbPath,
    dbExists,
    dbSizeBytes,
    liveDefaultLongLived: true,
    liveDefaultOneShot: false,
    activeSessionsDir: bridge.activeSessionsDir(),
    activeSessionsOnDisk: onDisk.length,
    activeSessionsAlive: alive.length,
  };
}

export function collectDoctorReport(version: string): DoctorReport {
  return {
    version,
    environment: collectEnvironment(),
    indexLive: collectIndexLive(),
    plugins: PLUGINS.map((p) => ({
      name: p.name,
      description: p.description,
      state: getPluginState(p.name),
    })),
    hookEvents: collectHookEvents(),
    channelSessions: collectChannelSessions(),
  };
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

// ─── Shared helpers for rendering (used by CLI text + TUI Ink) ──────────

export function tildify(p: string): string {
  const home = homedir();
  return p.startsWith(home) ? '~' + p.slice(home.length) : p;
}

export function formatRelative(ts: number): string {
  const diff = Date.now() - ts;
  if (diff < 60_000) return 'just now';
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`;
  return `${Math.floor(diff / 86_400_000)}d ago`;
}

export type PluginStatusKind = 'ok' | 'disabled' | 'path-missing' | 'not-installed';

export function pluginStatusKind(state: PluginState): PluginStatusKind {
  if (!state.installed) return 'not-installed';
  if (!state.pathExists) return 'path-missing';
  if (!state.enabled) return 'disabled';
  return 'ok';
}

export const PLUGIN_STATUS_LABEL: Record<PluginStatusKind, string> = {
  ok: 'installed & enabled',
  disabled: 'installed, disabled',
  'path-missing': 'registered, path missing',
  'not-installed': 'not installed',
};
