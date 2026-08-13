/**
 * Doctor report — pure data collection for spaghetti's health check.
 *
 * Both the `doctor` CLI command (text) and the `DoctorView` TUI view (Ink)
 * call `collectDoctorReport(version)` and render the same shape differently.
 * Keeping the collection pure means adding a new field anywhere shows up in
 * both surfaces at once.
 */

import { execSync } from 'node:child_process';
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { defaultDbPathForEngine, resolveActiveEngine, resolveEngine, type IngestEngine } from '@vibecook/spaghetti-sdk';
import {
  claudePaths,
  createDefaultCommandRunner,
  defaultClaudeHome,
  probePluginLeftovers,
  type PluginLeftoverReport,
} from './plugin-leftovers.js';

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

/**
 * Retired-plugin leftover state (RFC 007). Doctor renders this instead of a
 * plugin health check: the plugins are gone from Spaghetti, so the only question
 * is whether a trace of them is left in Claude Code, and whether we can prove it
 * either way.
 */
export type PluginLeftoversReport = PluginLeftoverReport;

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
  /** RFC 007 — retiring-plugin leftovers, read-only. */
  leftovers: PluginLeftoversReport;
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
  const paths = claudePaths(defaultClaudeHome());
  return {
    node: process.version,
    platform: process.platform,
    arch: process.arch,
    claudeBin: findClaudeBinary(),
    claudeDir: { path: CLAUDE_DIR, exists: existsSync(CLAUDE_DIR) },
    agentRoots: collectAgentRoots(),
    settings: { path: paths.settings, exists: existsSync(paths.settings) },
    pluginsDir: { path: paths.pluginsDir, exists: existsSync(paths.pluginsDir) },
  };
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

  // This is a transient process-health probe, not an ingest path. It reads
  // only PID fields from Claude's discovery files and never writes history.
  const sessionsDir = join(CLAUDE_DIR, 'sessions');
  const onDisk = activeSessionPids(sessionsDir);
  const alive = onDisk.filter(isProcessAlive);

  return {
    preferredEngine,
    effectiveEngine: active.engine,
    nativeAvailable: active.nativeAvailable,
    nativeVersion: active.nativeVersion,
    dbPath,
    dbExists,
    dbSizeBytes,
    liveDefaultLongLived: true,
    liveDefaultOneShot: false,
    activeSessionsDir: sessionsDir,
    activeSessionsOnDisk: onDisk.length,
    activeSessionsAlive: alive.length,
  };
}

function activeSessionPids(sessionsDir: string): number[] {
  if (!existsSync(sessionsDir)) return [];
  try {
    return readdirSync(sessionsDir)
      .filter((entry) => entry.endsWith('.json'))
      .map((entry) => {
        const fallback = Number.parseInt(entry.slice(0, -5), 10);
        try {
          const parsed = JSON.parse(readFileSync(join(sessionsDir, entry), 'utf8')) as { pid?: unknown };
          return typeof parsed.pid === 'number' ? parsed.pid : fallback;
        } catch {
          return Number.NaN;
        }
      })
      .filter((pid) => Number.isFinite(pid) && pid > 0);
  } catch {
    return [];
  }
}

function isProcessAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === 'EPERM';
  }
}

export function collectDoctorReport(version: string): DoctorReport {
  const claudeHome = defaultClaudeHome();
  return {
    version,
    environment: collectEnvironment(),
    indexLive: collectIndexLive(),
    leftovers: probePluginLeftovers({ claudeHome, runCommand: createDefaultCommandRunner() }),
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

// ─── Retired-plugin leftover presentation (RFC 007) ─────────────────────

export type LeftoverKind = 'clean' | 'leftover' | 'source-mismatch' | 'unknown';

export interface LeftoverLine {
  kind: LeftoverKind;
  label: string;
  detail: string;
}

/**
 * Flatten a leftover report into renderable lines shared by the CLI text
 * output and the Ink view. There are no install/enable calls to action — the
 * only offered direction is removal, and only for state proven to be ours.
 */
export function leftoverLines(report: PluginLeftoversReport): LeftoverLine[] {
  const lines: LeftoverLine[] = [];

  for (const plugin of report.plugins) {
    if (plugin.userInstall.status === 'unknown') {
      lines.push({ kind: 'unknown', label: plugin.name, detail: unknownDetail(plugin.userInstall) });
    } else if (plugin.userInstall.status === 'present') {
      const record = plugin.userInstall.value;
      lines.push({
        kind: 'leftover',
        label: plugin.name,
        detail: `installed (user scope)${record.version ? ` v${record.version}` : ''}`,
      });
    }

    if (plugin.userEnabled.status === 'unknown') {
      lines.push({ kind: 'unknown', label: plugin.name, detail: unknownDetail(plugin.userEnabled) });
    } else if (plugin.userEnabled.status === 'present' && plugin.userEnabled.value.enabled) {
      lines.push({ kind: 'leftover', label: plugin.name, detail: 'enabled in user settings' });
    }

    if (plugin.nonUserInstalls.status === 'present') {
      for (const record of plugin.nonUserInstalls.value) {
        lines.push({
          kind: 'leftover',
          label: plugin.name,
          detail: `installed in ${record.scope} scope — resolve manually`,
        });
      }
    }
  }

  const marketplace = report.userMarketplace;
  if (marketplace.status === 'unknown') {
    lines.push({ kind: 'unknown', label: 'marketplace', detail: unknownDetail(marketplace) });
  } else if (marketplace.status === 'present') {
    lines.push(
      marketplace.value.ownership === 'owned'
        ? { kind: 'leftover', label: 'marketplace', detail: 'registered (user scope)' }
        : {
            kind: 'source-mismatch',
            label: 'marketplace',
            detail: `source ${marketplace.value.sourceDescription} is not this repository — resolve manually`,
          },
    );
  }

  if (lines.length === 0) {
    lines.push({ kind: 'clean', label: 'plugins', detail: 'no leftovers' });
  }
  return lines;
}

function unknownDetail(result: { status: 'unknown'; input: string; reason: string } | unknown): string {
  const r = result as { input?: string; reason?: string };
  return `unknown — ${r.reason ?? 'could not determine'} (${r.input ?? 'unknown input'})`;
}

/** The manual commands doctor offers, for proven-ours state only. */
export function leftoverManualCommands(report: PluginLeftoversReport): string[] {
  const commands: string[] = [];
  for (const plugin of report.plugins) {
    if (plugin.userEnabled.status === 'present' && plugin.userEnabled.value.enabled) {
      commands.push(`claude plugin disable --scope user ${plugin.id}`);
    }
    if (plugin.userInstall.status === 'present') {
      commands.push(`claude plugin uninstall --scope user --keep-data ${plugin.id}`);
    }
  }
  if (report.userMarketplace.status === 'present' && report.userMarketplace.value.ownership === 'owned') {
    commands.push('claude plugin marketplace remove --scope user spaghetti');
  }
  return commands;
}
