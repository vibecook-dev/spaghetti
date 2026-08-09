/**
 * Read-only probe for retired Spaghetti Claude Code plugin state (RFC 007).
 *
 * This module is the *retained* diagnostic surface. The hooks and channel
 * plugins are gone from Spaghetti, but uninstalling the npm package never
 * removed them from Claude Code — so doctor and `spag uninstall` still need to
 * report what is left and print the commands to remove it.
 *
 * Two rules shape the whole design:
 *
 *   1. Never report clean when it cannot be proven. Every input is tri-state —
 *      `present`, `absent`, or `unknown` — and `unknown` never collapses into
 *      absence. A source mismatch is likewise never treated as "not ours".
 *   2. Never mutate, and never require the Claude executable to answer. The
 *      primary inputs are three JSON files. The structured `claude … --json`
 *      commands are a fallback for unsupported/unreadable formats only, and
 *      human-formatted CLI output is never parsed.
 *
 * The Claude home and the command runner are both injected: nothing here
 * captures `homedir()` in an import-time constant, so tests can point at a fake
 * home without touching the developer's real `~/.claude`.
 */

import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

// ─── Retired identities ──────────────────────────────────────────────────

/** The user-scope marketplace historically created by `spag plugin install`. */
export const MARKETPLACE_NAME = 'spaghetti';

/** The only repository whose `spaghetti` marketplace this tool owns. */
export const CANONICAL_REPO = 'vibecook-dev/spaghetti';

/** Bare names of the two retired plugins, in the normative cleanup order. */
export const RETIRED_PLUGIN_NAMES = ['spaghetti-hooks', 'spaghetti-channel'] as const;

export type RetiredPluginName = (typeof RETIRED_PLUGIN_NAMES)[number];

/** Fully qualified plugin id, e.g. `spaghetti-hooks@spaghetti`. */
export function pluginId(name: string): string {
  return `${name}@${MARKETPLACE_NAME}`;
}

// ─── Tri-state results ───────────────────────────────────────────────────

/** Proven present, with whatever identifying detail the input carried. */
export interface PresentResult<T> {
  status: 'present';
  value: T;
}

/** Proven absent — only ever produced by a missing or well-formed input. */
export interface AbsentResult {
  status: 'absent';
}

/** Indeterminate. Carries the offending path/command and why it failed. */
export interface UnknownResult {
  status: 'unknown';
  /** The file path or exact command that could not be interpreted. */
  input: string;
  reason: string;
}

export type Tri<T> = PresentResult<T> | AbsentResult | UnknownResult;

function present<T>(value: T): PresentResult<T> {
  return { status: 'present', value };
}

const ABSENT: AbsentResult = { status: 'absent' };

function unknown(input: string, reason: string): UnknownResult {
  return { status: 'unknown', input, reason };
}

// ─── Report shape ────────────────────────────────────────────────────────

/** Installation scopes Claude Code recognises. */
export type PluginScope = 'user' | 'project' | 'local';

/** One installation record for a plugin id. */
export interface InstalledRecord {
  id: string;
  scope: PluginScope;
  installPath: string | null;
  /** Whether `installPath` currently resolves on disk. */
  pathExists: boolean;
  version: string | null;
}

/** The `enabledPlugins` entry in a settings file. */
export interface EnabledRecord {
  id: string;
  enabled: boolean;
}

/** How a `spaghetti` marketplace registration is sourced. */
export type MarketplaceSourceKind = 'github' | 'git' | 'directory' | 'unsupported';

/**
 * Ownership verdict. Only `owned` may ever be removed automatically —
 * `source-mismatch` means a registration under our name that we cannot prove
 * is ours, which is reported for manual resolution and never touched.
 */
export type MarketplaceOwnership = 'owned' | 'source-mismatch';

export interface MarketplaceRecord {
  name: string;
  ownership: MarketplaceOwnership;
  sourceKind: MarketplaceSourceKind;
  /** Human-readable rendering of the raw source, for diagnostics. */
  sourceDescription: string;
  installLocation: string | null;
}

/** Everything the probe can say about one retired plugin. */
export interface PluginLeftover {
  name: RetiredPluginName;
  id: string;
  /** The user-scope installation record. */
  userInstall: Tri<InstalledRecord>;
  /** The user-scope `enabledPlugins` entry. Dangling entries count. */
  userEnabled: Tri<EnabledRecord>;
  /**
   * Installations in project/local scope that happened to be visible. The
   * probe does not scan arbitrary project trees to find them; this reports
   * only what the inspected inputs disclosed.
   */
  nonUserInstalls: Tri<InstalledRecord[]>;
}

export interface PluginLeftoverReport {
  /** The Claude configuration root this report describes. */
  claudeHome: string;
  plugins: PluginLeftover[];
  /** The user-scope marketplace named `spaghetti`. */
  userMarketplace: Tri<MarketplaceRecord>;
  /**
   * True only when every retired id is absent-and-not-enabled in every visible
   * state, the expected marketplace is absent, and nothing is `unknown`.
   */
  clean: boolean;
  /** Every `unknown` result in the report, for diagnostics. */
  unknowns: UnknownResult[];
}

// ─── Injected inputs ─────────────────────────────────────────────────────

export interface CommandResult {
  /** True only when the process ran and exited zero. */
  ok: boolean;
  stdout: string;
  /** Null when the process could not be spawned at all. */
  exitCode: number | null;
  /** Why the command was unusable, when `ok` is false. */
  errorMessage?: string;
}

/**
 * Runs a read-only Claude command as an argument vector. Never interpolated
 * into a shell string. Returning `ok: false` (or omitting the runner entirely)
 * leaves the affected result `unknown` rather than assuming absence.
 */
export type ReadOnlyCommandRunner = (argv: string[]) => CommandResult;

export interface ProbeOptions {
  /** Claude configuration root. Required — no implicit home lookup. */
  claudeHome: string;
  /** Fallback runner for unsupported/unreadable primary inputs. */
  runCommand?: ReadOnlyCommandRunner;
}

/** Resolved at call time, never captured in a module constant. */
export function defaultClaudeHome(): string {
  return process.env.CLAUDE_CONFIG_DIR?.trim() || join(homedir(), '.claude');
}

/** The three primary read-only inputs, derived from an explicit home. */
export function claudePaths(claudeHome: string): {
  pluginsDir: string;
  installedPlugins: string;
  settings: string;
  knownMarketplaces: string;
} {
  const pluginsDir = join(claudeHome, 'plugins');
  return {
    pluginsDir,
    installedPlugins: join(pluginsDir, 'installed_plugins.json'),
    settings: join(claudeHome, 'settings.json'),
    knownMarketplaces: join(pluginsDir, 'known_marketplaces.json'),
  };
}

/**
 * Default runner around the real `claude` executable. Constructed on demand so
 * importing this module never probes the environment.
 */
export function createDefaultCommandRunner(executable = 'claude'): ReadOnlyCommandRunner {
  return (argv) => {
    try {
      const stdout = execFileSync(executable, argv, {
        encoding: 'utf-8',
        stdio: ['ignore', 'pipe', 'ignore'],
      });
      return { ok: true, stdout, exitCode: 0 };
    } catch (err) {
      const e = err as NodeJS.ErrnoException & { status?: number; stdout?: string };
      return {
        ok: false,
        stdout: typeof e.stdout === 'string' ? e.stdout : '',
        exitCode: typeof e.status === 'number' ? e.status : null,
        errorMessage: e.code === 'ENOENT' ? 'claude executable not found on PATH' : e.message,
      };
    }
  };
}

// ─── JSON reading ────────────────────────────────────────────────────────

type JsonRead =
  | { kind: 'missing' }
  | { kind: 'ok'; value: unknown }
  | { kind: 'unreadable'; reason: string }
  | { kind: 'malformed'; reason: string };

function readJsonFile(path: string): JsonRead {
  if (!existsSync(path)) return { kind: 'missing' };
  let raw: string;
  try {
    raw = readFileSync(path, 'utf-8');
  } catch (err) {
    return { kind: 'unreadable', reason: err instanceof Error ? err.message : String(err) };
  }
  try {
    return { kind: 'ok', value: JSON.parse(raw) };
  } catch (err) {
    return { kind: 'malformed', reason: err instanceof Error ? err.message : String(err) };
  }
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

function asScope(v: unknown): PluginScope | null {
  return v === 'user' || v === 'project' || v === 'local' ? v : null;
}

// ─── Installed-plugin state ──────────────────────────────────────────────

/**
 * Parsed view of `installed_plugins.json`, keyed by plugin id.
 * `null` means the file could not be interpreted at all.
 */
type InstalledIndex = Map<string, InstalledRecord[]>;

interface InstalledParse {
  index: InstalledIndex | null;
  /** Set when `index` is null. */
  failure?: { input: string; reason: string };
}

function recordsFromEntries(id: string, entries: unknown, onBadScope: () => void): InstalledRecord[] {
  if (!Array.isArray(entries)) return [];
  const out: InstalledRecord[] = [];
  for (const entry of entries) {
    if (!isRecord(entry)) continue;
    const scope = asScope(entry.scope);
    if (scope === null) {
      // An installation we cannot attribute to a scope must not be silently
      // treated as user-scope (we would mutate someone else's state) nor as
      // non-user (cleanup would never converge). Force the caller to unknown.
      onBadScope();
      continue;
    }
    const installPath = typeof entry.installPath === 'string' ? entry.installPath : null;
    out.push({
      id,
      scope,
      installPath,
      pathExists: installPath ? existsSync(installPath) : false,
      version: typeof entry.version === 'string' ? entry.version : null,
    });
  }
  return out;
}

function parseInstalledFile(path: string): InstalledParse {
  const read = readJsonFile(path);
  if (read.kind === 'missing') return { index: new Map() };
  if (read.kind === 'unreadable' || read.kind === 'malformed') {
    return { index: null, failure: { input: path, reason: `${read.kind}: ${read.reason}` } };
  }
  const root = read.value;
  if (!isRecord(root) || !isRecord(root.plugins)) {
    return { index: null, failure: { input: path, reason: 'unsupported shape: expected { plugins: { … } }' } };
  }
  if (root.version !== undefined && root.version !== 2) {
    return {
      index: null,
      failure: { input: path, reason: `unsupported installed_plugins.json version: ${String(root.version)}` },
    };
  }

  const index: InstalledIndex = new Map();
  let badScope = false;
  for (const [id, entries] of Object.entries(root.plugins)) {
    const records = recordsFromEntries(id, entries, () => {
      badScope = true;
    });
    if (records.length > 0) index.set(id, records);
  }
  if (badScope) {
    return {
      index: null,
      failure: { input: path, reason: 'an installation record has no recognised scope' },
    };
  }
  return { index };
}

/** `claude plugin list --json` — the only permitted installed-state fallback. */
function parseInstalledFallback(runner: ReadOnlyCommandRunner | undefined, command: string): InstalledParse {
  if (!runner) {
    return { index: null, failure: { input: command, reason: 'no command runner available for fallback' } };
  }
  const result = runner(['plugin', 'list', '--json']);
  if (!result.ok) {
    return {
      index: null,
      failure: {
        input: command,
        reason: result.errorMessage ?? `command exited ${result.exitCode ?? 'without status'}`,
      },
    };
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(result.stdout);
  } catch (err) {
    return {
      index: null,
      failure: { input: command, reason: `unsupported output: ${err instanceof Error ? err.message : String(err)}` },
    };
  }
  if (!Array.isArray(parsed)) {
    return { index: null, failure: { input: command, reason: 'unsupported output: expected a JSON array' } };
  }

  const index: InstalledIndex = new Map();
  for (const entry of parsed) {
    if (!isRecord(entry) || typeof entry.id !== 'string') continue;
    const scope = asScope(entry.scope);
    if (scope === null) {
      return { index: null, failure: { input: command, reason: `entry "${entry.id}" has no recognised scope` } };
    }
    const installPath = typeof entry.installPath === 'string' ? entry.installPath : null;
    const record: InstalledRecord = {
      id: entry.id,
      scope,
      installPath,
      pathExists: installPath ? existsSync(installPath) : false,
      version: typeof entry.version === 'string' ? entry.version : null,
    };
    const list = index.get(entry.id);
    if (list) list.push(record);
    else index.set(entry.id, [record]);
  }
  return { index };
}

// ─── Enabled state ───────────────────────────────────────────────────────

interface EnabledParse {
  map: Map<string, boolean> | null;
  failure?: { input: string; reason: string };
}

function parseSettingsFile(path: string): EnabledParse {
  const read = readJsonFile(path);
  if (read.kind === 'missing') return { map: new Map() };
  if (read.kind === 'unreadable' || read.kind === 'malformed') {
    return { map: null, failure: { input: path, reason: `${read.kind}: ${read.reason}` } };
  }
  const root = read.value;
  if (!isRecord(root)) {
    return { map: null, failure: { input: path, reason: 'unsupported shape: expected a JSON object' } };
  }
  // A settings file with no `enabledPlugins` key proves no entry exists.
  if (root.enabledPlugins === undefined) return { map: new Map() };
  if (!isRecord(root.enabledPlugins)) {
    return { map: null, failure: { input: path, reason: 'unsupported shape: enabledPlugins is not an object' } };
  }

  const map = new Map<string, boolean>();
  for (const [id, value] of Object.entries(root.enabledPlugins)) {
    if (typeof value !== 'boolean') {
      return { map: null, failure: { input: path, reason: `enabledPlugins["${id}"] is not a boolean` } };
    }
    map.set(id, value);
  }
  return { map };
}

// ─── Marketplace state ───────────────────────────────────────────────────

/**
 * Normalise a marketplace source to `owner/repo`, or null when the shape is
 * not a Git repository we can attribute. Trailing `.git` is ignored; directory
 * sources never normalise.
 */
export function normalizeRepoSource(source: unknown): {
  kind: MarketplaceSourceKind;
  repo: string | null;
  description: string;
} {
  if (typeof source === 'string') {
    // Older/flattened shapes may carry a bare URL or `owner/repo` string.
    return fromUrlLike(source);
  }
  if (!isRecord(source)) {
    return { kind: 'unsupported', repo: null, description: JSON.stringify(source ?? null) };
  }
  const kind = source.source;
  if (kind === 'github' && typeof source.repo === 'string') {
    const repo = stripGitSuffix(source.repo.trim());
    return { kind: 'github', repo: isOwnerRepo(repo) ? repo : null, description: `github:${source.repo}` };
  }
  if (kind === 'git' && typeof source.url === 'string') {
    const parsed = fromUrlLike(source.url);
    return { kind: 'git', repo: parsed.repo, description: `git:${source.url}` };
  }
  if (kind === 'directory' || kind === 'local' || typeof source.path === 'string') {
    const path = typeof source.path === 'string' ? source.path : '(unspecified)';
    return { kind: 'directory', repo: null, description: `directory:${path}` };
  }
  return { kind: 'unsupported', repo: null, description: JSON.stringify(source) };
}

function stripGitSuffix(s: string): string {
  return s.replace(/\.git$/i, '');
}

function isOwnerRepo(s: string): boolean {
  return /^[^/\s]+\/[^/\s]+$/.test(s);
}

function fromUrlLike(raw: string): { kind: MarketplaceSourceKind; repo: string | null; description: string } {
  const value = raw.trim();
  const description = value;

  // SSH: git@github.com:owner/repo(.git)
  const ssh = /^(?:ssh:\/\/)?[^@/\s]+@([^:/\s]+)[:/]([^/\s]+\/[^/\s]+?)(?:\.git)?$/i.exec(value);
  if (ssh) {
    return { kind: 'git', repo: ssh[1].toLowerCase() === 'github.com' ? ssh[2] : null, description };
  }

  // HTTPS: https://github.com/owner/repo(.git)
  if (/^https?:\/\//i.test(value)) {
    try {
      const url = new URL(value);
      const segments = url.pathname.split('/').filter(Boolean);
      if (url.hostname.toLowerCase() === 'github.com' && segments.length >= 2) {
        return { kind: 'git', repo: `${segments[0]}/${stripGitSuffix(segments[1])}`, description };
      }
    } catch {
      /* fall through to unsupported */
    }
    return { kind: 'git', repo: null, description };
  }

  if (isOwnerRepo(value)) {
    return { kind: 'github', repo: stripGitSuffix(value), description };
  }
  return { kind: 'unsupported', repo: null, description };
}

function marketplaceFromSource(name: string, source: unknown, installLocation: unknown): MarketplaceRecord {
  const normalized = normalizeRepoSource(source);
  const owned = normalized.repo !== null && normalized.repo.toLowerCase() === CANONICAL_REPO.toLowerCase();
  return {
    name,
    ownership: owned ? 'owned' : 'source-mismatch',
    sourceKind: normalized.kind,
    sourceDescription: normalized.description,
    installLocation: typeof installLocation === 'string' ? installLocation : null,
  };
}

function probeMarketplaceFile(path: string): Tri<MarketplaceRecord> | { retry: { input: string; reason: string } } {
  const read = readJsonFile(path);
  if (read.kind === 'missing') return ABSENT;
  if (read.kind === 'unreadable' || read.kind === 'malformed') {
    return { retry: { input: path, reason: `${read.kind}: ${read.reason}` } };
  }
  const root = read.value;
  if (!isRecord(root)) {
    return { retry: { input: path, reason: 'unsupported shape: expected a JSON object' } };
  }
  const entry = root[MARKETPLACE_NAME];
  if (entry === undefined) return ABSENT;
  if (!isRecord(entry)) {
    return { retry: { input: path, reason: `unsupported shape: "${MARKETPLACE_NAME}" entry is not an object` } };
  }
  return present(marketplaceFromSource(MARKETPLACE_NAME, entry.source, entry.installLocation));
}

function probeMarketplaceFallback(runner: ReadOnlyCommandRunner | undefined, command: string): Tri<MarketplaceRecord> {
  if (!runner) return unknown(command, 'no command runner available for fallback');
  const result = runner(['plugin', 'marketplace', 'list', '--json']);
  if (!result.ok) {
    return unknown(command, result.errorMessage ?? `command exited ${result.exitCode ?? 'without status'}`);
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(result.stdout);
  } catch (err) {
    return unknown(command, `unsupported output: ${err instanceof Error ? err.message : String(err)}`);
  }
  if (!Array.isArray(parsed)) return unknown(command, 'unsupported output: expected a JSON array');

  for (const entry of parsed) {
    if (!isRecord(entry) || entry.name !== MARKETPLACE_NAME) continue;
    // The flattened CLI shape hoists `repo`/`url` beside `source`.
    const source = isRecord(entry.source) ? entry.source : { source: entry.source, repo: entry.repo, url: entry.url };
    return present(marketplaceFromSource(MARKETPLACE_NAME, source, entry.installLocation));
  }
  return ABSENT;
}

// ─── The probe ───────────────────────────────────────────────────────────

/**
 * Inspect a Claude configuration root for retired Spaghetti plugin state.
 * Purely read-only: it never writes, and it never runs a mutating command.
 */
export function probePluginLeftovers(options: ProbeOptions): PluginLeftoverReport {
  const { claudeHome, runCommand } = options;
  const paths = claudePaths(claudeHome);

  const listCommand = 'claude plugin list --json';

  // Installed state: primary file, then the structured command, then unknown.
  let installed = parseInstalledFile(paths.installedPlugins);
  if (installed.index === null) {
    const viaCommand = parseInstalledFallback(runCommand, listCommand);
    installed =
      viaCommand.index !== null
        ? viaCommand
        : {
            index: null,
            failure: {
              input: `${installed.failure?.input ?? paths.installedPlugins} → ${listCommand}`,
              reason: `${installed.failure?.reason ?? 'unreadable'}; fallback failed: ${viaCommand.failure?.reason ?? 'unknown'}`,
            },
          };
  }

  // Enabled state: settings.json, then the same structured command.
  let enabled = parseSettingsFile(paths.settings);
  if (enabled.map === null) {
    const viaCommand = enabledFromCommand(runCommand, listCommand);
    enabled =
      viaCommand.map !== null
        ? viaCommand
        : {
            map: null,
            failure: {
              input: `${enabled.failure?.input ?? paths.settings} → ${listCommand}`,
              reason: `${enabled.failure?.reason ?? 'unreadable'}; fallback failed: ${viaCommand.failure?.reason ?? 'unknown'}`,
            },
          };
  }

  const plugins: PluginLeftover[] = RETIRED_PLUGIN_NAMES.map((name) => {
    const id = pluginId(name);
    const records = installed.index?.get(id) ?? [];

    let userInstall: Tri<InstalledRecord>;
    let nonUserInstalls: Tri<InstalledRecord[]>;
    if (installed.index === null) {
      const f = installed.failure!;
      userInstall = unknown(f.input, f.reason);
      nonUserInstalls = unknown(f.input, f.reason);
    } else {
      const user = records.find((r) => r.scope === 'user');
      userInstall = user ? present(user) : ABSENT;
      const others = records.filter((r) => r.scope !== 'user');
      nonUserInstalls = others.length > 0 ? present(others) : ABSENT;
    }

    let userEnabled: Tri<EnabledRecord>;
    if (enabled.map === null) {
      const f = enabled.failure!;
      userEnabled = unknown(f.input, f.reason);
    } else {
      const value = enabled.map.get(id);
      userEnabled = value === undefined ? ABSENT : present({ id, enabled: value });
    }

    return { name, id, userInstall, userEnabled, nonUserInstalls };
  });

  // Marketplace: known_marketplaces.json, then the structured command.
  const fileResult = probeMarketplaceFile(paths.knownMarketplaces);
  let userMarketplace: Tri<MarketplaceRecord>;
  if ('retry' in fileResult) {
    const command = 'claude plugin marketplace list --json';
    const viaCommand = probeMarketplaceFallback(runCommand, command);
    userMarketplace =
      viaCommand.status === 'unknown'
        ? unknown(
            `${fileResult.retry.input} → ${command}`,
            `${fileResult.retry.reason}; fallback failed: ${viaCommand.reason}`,
          )
        : viaCommand;
  } else {
    userMarketplace = fileResult;
  }

  const unknowns: UnknownResult[] = [];
  for (const p of plugins) {
    for (const r of [p.userInstall, p.userEnabled, p.nonUserInstalls]) {
      if (r.status === 'unknown') unknowns.push(r);
    }
  }
  if (userMarketplace.status === 'unknown') unknowns.push(userMarketplace);

  return {
    claudeHome,
    plugins,
    userMarketplace,
    clean: isClean(plugins, userMarketplace),
    unknowns,
  };
}

function enabledFromCommand(runner: ReadOnlyCommandRunner | undefined, command: string): EnabledParse {
  if (!runner) return { map: null, failure: { input: command, reason: 'no command runner available for fallback' } };
  const result = runner(['plugin', 'list', '--json']);
  if (!result.ok) {
    return {
      map: null,
      failure: {
        input: command,
        reason: result.errorMessage ?? `command exited ${result.exitCode ?? 'without status'}`,
      },
    };
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(result.stdout);
  } catch (err) {
    return {
      map: null,
      failure: { input: command, reason: `unsupported output: ${err instanceof Error ? err.message : String(err)}` },
    };
  }
  if (!Array.isArray(parsed)) {
    return { map: null, failure: { input: command, reason: 'unsupported output: expected a JSON array' } };
  }
  const map = new Map<string, boolean>();
  for (const entry of parsed) {
    if (!isRecord(entry) || typeof entry.id !== 'string') continue;
    if (typeof entry.enabled !== 'boolean') {
      return { map: null, failure: { input: command, reason: `entry "${entry.id}" has no boolean enabled field` } };
    }
    map.set(entry.id, entry.enabled);
  }
  return { map };
}

/**
 * Clean means every retired id is provably absent and not enabled in every
 * visible state, the expected marketplace is provably absent, and nothing
 * relevant is unknown. A source mismatch is never clean.
 */
function isClean(plugins: PluginLeftover[], marketplace: Tri<MarketplaceRecord>): boolean {
  for (const p of plugins) {
    if (p.userInstall.status !== 'absent') return false;
    if (p.nonUserInstalls.status !== 'absent') return false;
    if (p.userEnabled.status === 'unknown') return false;
    if (p.userEnabled.status === 'present' && p.userEnabled.value.enabled) return false;
  }
  return marketplace.status === 'absent';
}

// ─── Derived helpers for renderers ───────────────────────────────────────

/** True when this plugin has user-scope state the guided cleanup owns. */
export function hasOwnedUserState(leftover: PluginLeftover): boolean {
  if (leftover.userInstall.status === 'present') return true;
  return leftover.userEnabled.status === 'present' && leftover.userEnabled.value.enabled;
}

/** Non-user installations the probe could see, across both plugins. */
export function visibleNonUserInstalls(report: PluginLeftoverReport): InstalledRecord[] {
  return report.plugins.flatMap((p) => (p.nonUserInstalls.status === 'present' ? p.nonUserInstalls.value : []));
}

/** True when a `spaghetti` marketplace exists that we cannot prove is ours. */
export function hasMarketplaceSourceMismatch(report: PluginLeftoverReport): boolean {
  return report.userMarketplace.status === 'present' && report.userMarketplace.value.ownership === 'source-mismatch';
}
