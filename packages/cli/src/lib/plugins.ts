/**
 * Shared metadata and state helpers for spaghetti's Claude Code plugins.
 *
 * Both the `plugin` and `doctor` commands read this module so they stay
 * aligned on which plugins ship with spaghetti and how their install state
 * is probed on disk (installed_plugins.json + settings.json).
 */

import { existsSync, readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

export const MARKETPLACE_NAME = 'spaghetti';
export const REPO = 'vibecook-dev/spaghetti';

export interface PluginMeta {
  /** Bare plugin name (e.g. "spaghetti-hooks"). */
  name: string;
  /** Short human description surfaced in status/doctor output. */
  description: string;
}

/**
 * The set of plugins shipped by this repo's marketplace. Must stay in sync
 * with `.claude-plugin/marketplace.json` at the repo root.
 */
export const PLUGINS: readonly PluginMeta[] = [
  {
    name: 'spaghetti-hooks',
    description: 'Captures all Claude Code hook events for the hooks monitor',
  },
  {
    name: 'spaghetti-channel',
    description: 'WebSocket chat bridge for interactive Claude Code sessions',
  },
];

export function pluginId(name: string): string {
  return `${name}@${MARKETPLACE_NAME}`;
}

const CLAUDE_DIR = join(homedir(), '.claude');
export const PLUGINS_DIR = join(CLAUDE_DIR, 'plugins');
export const INSTALLED_PLUGINS_PATH = join(PLUGINS_DIR, 'installed_plugins.json');
export const SETTINGS_PATH = join(CLAUDE_DIR, 'settings.json');

function readJson(path: string): Record<string, unknown> {
  if (!existsSync(path)) return {};
  try {
    return JSON.parse(readFileSync(path, 'utf-8'));
  } catch {
    return {};
  }
}

export interface PluginState {
  /** Fully qualified id, e.g. "spaghetti-hooks@spaghetti". */
  id: string;
  /** Registered in ~/.claude/plugins/installed_plugins.json. */
  installed: boolean;
  /** Enabled via ~/.claude/settings.json → enabledPlugins. */
  enabled: boolean;
  /** Filesystem path where the plugin is checked out (if any). */
  installPath: string | null;
  /** Whether the installPath currently exists on disk. */
  pathExists: boolean;
  /** Version recorded in installed_plugins.json. */
  version: string | null;
}

export function getPluginState(name: string): PluginState {
  const id = pluginId(name);
  const installed = readJson(INSTALLED_PLUGINS_PATH) as {
    plugins?: Record<string, Array<{ installPath?: string; version?: string }>>;
  };
  const entry = installed.plugins?.[id]?.[0];
  const settings = readJson(SETTINGS_PATH) as { enabledPlugins?: Record<string, boolean> };

  const installPath = entry?.installPath ?? null;
  return {
    id,
    installed: Boolean(entry),
    enabled: settings.enabledPlugins?.[id] === true,
    installPath,
    pathExists: installPath ? existsSync(installPath) : false,
    version: entry?.version ?? null,
  };
}
