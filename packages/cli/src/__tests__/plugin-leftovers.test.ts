/**
 * Leftover-probe tests (RFC 007 Phase 0).
 *
 * Every case runs against a temporary fake Claude home and a fake command
 * runner. Nothing here reads or writes the developer's real `~/.claude`; the
 * final suite asserts that explicitly with a sentinel file.
 */

import assert from 'node:assert/strict';
import { after, describe, it } from 'node:test';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { join } from 'node:path';

import {
  CANONICAL_REPO,
  MARKETPLACE_NAME,
  claudePaths,
  defaultClaudeHome,
  normalizeRepoSource,
  pluginId,
  probePluginLeftovers,
  type CommandResult,
  type PluginLeftoverReport,
  type ReadOnlyCommandRunner,
} from '../lib/plugin-leftovers.js';

const HOOKS_ID = pluginId('spaghetti-hooks');
const CHANNEL_ID = pluginId('spaghetti-channel');

// ─── Fake home helpers ───────────────────────────────────────────────────

let roots: string[] = [];

function makeHome(): string {
  const dir = mkdtempSync(join(tmpdir(), 'spag-leftovers-'));
  roots.push(dir);
  mkdirSync(join(dir, 'plugins'), { recursive: true });
  return dir;
}

function writeInstalled(home: string, value: unknown): void {
  writeFileSync(claudePaths(home).installedPlugins, JSON.stringify(value), 'utf-8');
}

function writeSettings(home: string, value: unknown): void {
  writeFileSync(claudePaths(home).settings, JSON.stringify(value), 'utf-8');
}

function writeMarketplaces(home: string, value: unknown): void {
  writeFileSync(claudePaths(home).knownMarketplaces, JSON.stringify(value), 'utf-8');
}

function userRecord(id: string, extra: Record<string, unknown> = {}): unknown {
  return { scope: 'user', installPath: join('/cache', id), version: '1.0.0', ...extra };
}

/** A runner that fails every call — proves the probe degrades to `unknown`. */
const failingRunner: ReadOnlyCommandRunner = () => ({
  ok: false,
  stdout: '',
  exitCode: 1,
  errorMessage: 'claude executable not found on PATH',
});

function jsonRunner(byCommand: Record<string, CommandResult>): ReadOnlyCommandRunner {
  return (argv) => byCommand[argv.join(' ')] ?? { ok: false, stdout: '', exitCode: 127, errorMessage: 'no fixture' };
}

function hooks(report: PluginLeftoverReport) {
  return report.plugins.find((p) => p.name === 'spaghetti-hooks')!;
}

function channel(report: PluginLeftoverReport) {
  return report.plugins.find((p) => p.name === 'spaghetti-channel')!;
}

after(() => {
  for (const dir of roots) rmSync(dir, { recursive: true, force: true });
  roots = [];
});

// ─── Presence combinations ───────────────────────────────────────────────

describe('probePluginLeftovers — presence combinations', () => {
  it('reports clean when nothing exists at all', () => {
    const home = makeHome();
    const report = probePluginLeftovers({ claudeHome: home });

    assert.equal(report.clean, true);
    assert.deepEqual(report.unknowns, []);
    assert.equal(hooks(report).userInstall.status, 'absent');
    assert.equal(hooks(report).userEnabled.status, 'absent');
    assert.equal(hooks(report).nonUserInstalls.status, 'absent');
    assert.equal(report.userMarketplace.status, 'absent');
  });

  it('reports a user installation with scope, path and version', () => {
    const home = makeHome();
    writeInstalled(home, { version: 2, plugins: { [HOOKS_ID]: [userRecord(HOOKS_ID)] } });

    const report = probePluginLeftovers({ claudeHome: home });
    const install = hooks(report).userInstall;

    assert.equal(install.status, 'present');
    assert.equal(install.status === 'present' && install.value.scope, 'user');
    assert.equal(install.status === 'present' && install.value.version, '1.0.0');
    assert.equal(install.status === 'present' && install.value.pathExists, false);
    assert.equal(report.clean, false);
    // The other plugin is independently absent.
    assert.equal(channel(report).userInstall.status, 'absent');
  });

  it('treats a dangling enabled entry as a leftover with no installation', () => {
    const home = makeHome();
    writeSettings(home, { enabledPlugins: { [HOOKS_ID]: true } });

    const report = probePluginLeftovers({ claudeHome: home });
    const enabled = hooks(report).userEnabled;

    assert.equal(hooks(report).userInstall.status, 'absent');
    assert.equal(enabled.status, 'present');
    assert.equal(enabled.status === 'present' && enabled.value.enabled, true);
    assert.equal(report.clean, false);
  });

  it('an explicitly disabled entry is present but still clean', () => {
    const home = makeHome();
    writeSettings(home, { enabledPlugins: { [HOOKS_ID]: false } });

    const report = probePluginLeftovers({ claudeHome: home });

    assert.equal(hooks(report).userEnabled.status, 'present');
    assert.equal(report.clean, true);
  });

  it('reports both plugins installed and enabled', () => {
    const home = makeHome();
    writeInstalled(home, {
      version: 2,
      plugins: { [HOOKS_ID]: [userRecord(HOOKS_ID)], [CHANNEL_ID]: [userRecord(CHANNEL_ID)] },
    });
    writeSettings(home, { enabledPlugins: { [HOOKS_ID]: true, [CHANNEL_ID]: true } });

    const report = probePluginLeftovers({ claudeHome: home });

    assert.equal(hooks(report).userInstall.status, 'present');
    assert.equal(channel(report).userInstall.status, 'present');
    assert.equal(report.clean, false);
  });

  it('separates user and non-user installations of the same id', () => {
    const home = makeHome();
    writeInstalled(home, {
      version: 2,
      plugins: {
        [HOOKS_ID]: [
          userRecord(HOOKS_ID),
          { scope: 'project', installPath: '/repo/.claude/plugins/hooks', version: '1.1.0' },
          { scope: 'local', installPath: '/repo/.local/hooks', version: '1.1.0' },
        ],
      },
    });

    const report = probePluginLeftovers({ claudeHome: home });
    const nonUser = hooks(report).nonUserInstalls;

    assert.equal(hooks(report).userInstall.status, 'present');
    assert.equal(nonUser.status, 'present');
    assert.deepEqual(nonUser.status === 'present' ? nonUser.value.map((r) => r.scope).sort() : [], [
      'local',
      'project',
    ]);
  });

  it('never treats an unattributable scope as user scope', () => {
    const home = makeHome();
    writeInstalled(home, { version: 2, plugins: { [HOOKS_ID]: [{ installPath: '/cache/hooks' }] } });

    const report = probePluginLeftovers({ claudeHome: home });

    assert.equal(hooks(report).userInstall.status, 'unknown');
    assert.equal(report.clean, false);
  });
});

// ─── Marketplace source normalisation ────────────────────────────────────

describe('probePluginLeftovers — marketplace registration', () => {
  it('owns a structured github source for the canonical repo', () => {
    const home = makeHome();
    writeMarketplaces(home, {
      [MARKETPLACE_NAME]: { source: { source: 'github', repo: CANONICAL_REPO }, installLocation: '/mk/spaghetti' },
    });

    const report = probePluginLeftovers({ claudeHome: home });

    assert.equal(report.userMarketplace.status, 'present');
    assert.equal(report.userMarketplace.status === 'present' && report.userMarketplace.value.ownership, 'owned');
    assert.equal(report.clean, false);
  });

  it('owns https and ssh git URLs that normalise to the canonical repo', () => {
    for (const url of [
      `https://github.com/${CANONICAL_REPO}.git`,
      `https://github.com/${CANONICAL_REPO}`,
      `git@github.com:${CANONICAL_REPO}.git`,
    ]) {
      const home = makeHome();
      writeMarketplaces(home, { [MARKETPLACE_NAME]: { source: { source: 'git', url } } });
      const report = probePluginLeftovers({ claudeHome: home });
      assert.equal(
        report.userMarketplace.status === 'present' && report.userMarketplace.value.ownership,
        'owned',
        `expected ${url} to normalise to ${CANONICAL_REPO}`,
      );
    }
  });

  it('flags a directory source as a mismatch, never as owned', () => {
    const home = makeHome();
    writeMarketplaces(home, {
      [MARKETPLACE_NAME]: { source: { source: 'directory', path: '/somewhere/checkout' } },
    });

    const report = probePluginLeftovers({ claudeHome: home });

    assert.equal(
      report.userMarketplace.status === 'present' && report.userMarketplace.value.ownership,
      'source-mismatch',
    );
    assert.equal(report.userMarketplace.status === 'present' && report.userMarketplace.value.sourceKind, 'directory');
    assert.equal(report.clean, false);
  });

  it('flags a different owner/repo as a mismatch', () => {
    const home = makeHome();
    writeMarketplaces(home, {
      [MARKETPLACE_NAME]: { source: { source: 'github', repo: 'someone-else/spaghetti' } },
    });

    const report = probePluginLeftovers({ claudeHome: home });

    assert.equal(
      report.userMarketplace.status === 'present' && report.userMarketplace.value.ownership,
      'source-mismatch',
    );
  });

  it('flags an unsupported source shape as a mismatch, not unknown', () => {
    const home = makeHome();
    writeMarketplaces(home, { [MARKETPLACE_NAME]: { source: { source: 'carrier-pigeon' } } });

    const report = probePluginLeftovers({ claudeHome: home });

    assert.equal(report.userMarketplace.status, 'present');
    assert.equal(
      report.userMarketplace.status === 'present' && report.userMarketplace.value.ownership,
      'source-mismatch',
    );
    assert.deepEqual(report.unknowns, []);
  });

  it('is absent when other marketplaces exist but ours does not', () => {
    const home = makeHome();
    writeMarketplaces(home, {
      'claude-plugins-official': { source: { source: 'github', repo: 'anthropics/claude-plugins-official' } },
    });

    const report = probePluginLeftovers({ claudeHome: home });

    assert.equal(report.userMarketplace.status, 'absent');
    assert.equal(report.clean, true);
  });

  it('normalizeRepoSource ignores a trailing .git and rejects non-GitHub hosts', () => {
    assert.equal(normalizeRepoSource({ source: 'github', repo: `${CANONICAL_REPO}.git` }).repo, CANONICAL_REPO);
    assert.equal(normalizeRepoSource({ source: 'git', url: 'https://gitlab.com/a/b.git' }).repo, null);
    assert.equal(normalizeRepoSource({ source: 'git', url: 'git@gitlab.com:a/b.git' }).repo, null);
  });
});

// ─── Degraded inputs ─────────────────────────────────────────────────────

describe('probePluginLeftovers — degraded inputs', () => {
  it('malformed installed_plugins.json with no runner is unknown, never clean', () => {
    const home = makeHome();
    writeFileSync(claudePaths(home).installedPlugins, '{ not json', 'utf-8');

    const report = probePluginLeftovers({ claudeHome: home });

    assert.equal(hooks(report).userInstall.status, 'unknown');
    assert.equal(report.clean, false);
    assert.ok(report.unknowns.length > 0);
    assert.match(report.unknowns[0].reason, /malformed/);
  });

  it('an unreadable installed_plugins.json is unknown', () => {
    const home = makeHome();
    // A directory at the file's path is unreadable as a file on every platform.
    mkdirSync(claudePaths(home).installedPlugins, { recursive: true });

    const report = probePluginLeftovers({ claudeHome: home });

    assert.equal(hooks(report).userInstall.status, 'unknown');
    assert.equal(report.clean, false);
  });

  it('an unsupported installed_plugins.json version is unknown', () => {
    const home = makeHome();
    writeInstalled(home, { version: 99, plugins: {} });

    const report = probePluginLeftovers({ claudeHome: home });

    assert.equal(hooks(report).userInstall.status, 'unknown');
    assert.match(report.unknowns[0].reason, /unsupported installed_plugins.json version/);
  });

  it('falls back to claude plugin list --json when the file is unsupported', () => {
    const home = makeHome();
    writeInstalled(home, { version: 99, plugins: {} });
    const runner = jsonRunner({
      'plugin list --json': {
        ok: true,
        exitCode: 0,
        stdout: JSON.stringify([
          { id: HOOKS_ID, scope: 'user', enabled: true, version: '1.1.0', installPath: '/cache/hooks' },
        ]),
      },
    });

    const report = probePluginLeftovers({ claudeHome: home, runCommand: runner });
    const install = hooks(report).userInstall;

    assert.equal(install.status, 'present');
    assert.equal(install.status === 'present' && install.value.version, '1.1.0');
    assert.equal(channel(report).userInstall.status, 'absent');
    assert.deepEqual(report.unknowns, []);
  });

  it('a failing fallback command leaves the result unknown and names both inputs', () => {
    const home = makeHome();
    writeFileSync(claudePaths(home).installedPlugins, 'nope', 'utf-8');

    const report = probePluginLeftovers({ claudeHome: home, runCommand: failingRunner });
    const install = hooks(report).userInstall;

    assert.equal(install.status, 'unknown');
    assert.match(
      install.status === 'unknown' ? install.input : '',
      /installed_plugins\.json → claude plugin list --json/,
    );
    assert.match(install.status === 'unknown' ? install.reason : '', /fallback failed/);
    assert.equal(report.clean, false);
  });

  it('unparseable fallback output is unknown, never a silent clean', () => {
    const home = makeHome();
    writeFileSync(claudePaths(home).installedPlugins, 'nope', 'utf-8');
    const runner = jsonRunner({ 'plugin list --json': { ok: true, exitCode: 0, stdout: 'Plugins:\n  none' } });

    const report = probePluginLeftovers({ claudeHome: home, runCommand: runner });

    assert.equal(hooks(report).userInstall.status, 'unknown');
    assert.equal(report.clean, false);
  });

  it('malformed settings.json makes only the enabled state unknown', () => {
    const home = makeHome();
    writeFileSync(claudePaths(home).settings, '{{{', 'utf-8');

    const report = probePluginLeftovers({ claudeHome: home });

    assert.equal(hooks(report).userEnabled.status, 'unknown');
    assert.equal(hooks(report).userInstall.status, 'absent');
    assert.equal(report.clean, false);
  });

  it('settings.json without enabledPlugins proves the entry is absent', () => {
    const home = makeHome();
    writeSettings(home, { theme: 'dark' });

    const report = probePluginLeftovers({ claudeHome: home });

    assert.equal(hooks(report).userEnabled.status, 'absent');
    assert.equal(report.clean, true);
  });

  it('malformed known_marketplaces.json falls back, then reports unknown', () => {
    const home = makeHome();
    writeFileSync(claudePaths(home).knownMarketplaces, '[[[', 'utf-8');

    const report = probePluginLeftovers({ claudeHome: home, runCommand: failingRunner });

    assert.equal(report.userMarketplace.status, 'unknown');
    assert.equal(report.clean, false);
  });

  it('recovers the marketplace from the structured command fallback', () => {
    const home = makeHome();
    writeFileSync(claudePaths(home).knownMarketplaces, 'not json', 'utf-8');
    const runner = jsonRunner({
      'plugin marketplace list --json': {
        ok: true,
        exitCode: 0,
        stdout: JSON.stringify([
          { name: MARKETPLACE_NAME, source: 'github', repo: CANONICAL_REPO, installLocation: '/mk/spaghetti' },
        ]),
      },
    });

    const report = probePluginLeftovers({ claudeHome: home, runCommand: runner });

    assert.equal(report.userMarketplace.status === 'present' && report.userMarketplace.value.ownership, 'owned');
  });
});

// ─── Isolation ───────────────────────────────────────────────────────────

describe('probePluginLeftovers — isolation from the real Claude home', () => {
  const realHome = join(homedir(), '.claude');
  const sentinel = join(realHome, '.spag-rfc007-probe-sentinel');

  it('probing a fake home leaves the real ~/.claude byte-identical', () => {
    const existed = existsSync(sentinel);
    const before = existed ? readFileSync(sentinel, 'utf-8') : null;
    const home = makeHome();
    writeInstalled(home, { version: 2, plugins: { [HOOKS_ID]: [userRecord(HOOKS_ID)] } });
    writeMarketplaces(home, { [MARKETPLACE_NAME]: { source: { source: 'github', repo: CANONICAL_REPO } } });

    const report = probePluginLeftovers({ claudeHome: home, runCommand: failingRunner });

    assert.equal(report.claudeHome, home);
    assert.notEqual(report.claudeHome, realHome);
    assert.equal(existsSync(sentinel), existed, 'probe must not create files in the real Claude home');
    if (before !== null) assert.equal(readFileSync(sentinel, 'utf-8'), before);
  });

  it('defaultClaudeHome honours CLAUDE_CONFIG_DIR at call time, not import time', () => {
    const original = process.env.CLAUDE_CONFIG_DIR;
    try {
      process.env.CLAUDE_CONFIG_DIR = join(tmpdir(), 'some-isolated-claude');
      assert.equal(defaultClaudeHome(), join(tmpdir(), 'some-isolated-claude'));
      delete process.env.CLAUDE_CONFIG_DIR;
      assert.equal(defaultClaudeHome(), join(homedir(), '.claude'));
    } finally {
      if (original === undefined) delete process.env.CLAUDE_CONFIG_DIR;
      else process.env.CLAUDE_CONFIG_DIR = original;
    }
  });
});
