/**
 * config-parser-settings-local.test.ts — settings.local.json promotion.
 *
 * The 2026-07 audit found settings.local.json was read in the live path
 * but never promoted into AgentConfig at cold start, so displayed
 * permissions omitted its entries. These assert the cold-start parser
 * now surfaces it (and leaves it null when absent).
 */

import { test, describe, before, after, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from 'node:fs';

import { createFileService } from '../../../../io/file-service.js';
import { createConfigParser } from '../config-parser.js';
import type { ConfigParser } from '../config-parser.js';

describe('ConfigParser settings.local.json', () => {
  let tempDir: string;
  let parser: ConfigParser;
  let rootDir: string;

  before(() => {
    tempDir = mkdtempSync(path.join(os.tmpdir(), 'spaghetti-slocal-'));
    parser = createConfigParser(createFileService());
  });

  after(() => rmSync(tempDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 }));

  beforeEach((t) => {
    rootDir = path.join(tempDir, t.name.replace(/[^a-zA-Z0-9]/g, '_'));
    mkdirSync(rootDir, { recursive: true });
    writeFileSync(path.join(rootDir, 'settings.json'), JSON.stringify({ permissions: { allow: ['Read'] } }));
  });

  test('settings.local.json is parsed into config.settingsLocal', () => {
    writeFileSync(
      path.join(rootDir, 'settings.local.json'),
      JSON.stringify({ permissions: { allow: ['Bash(git*)'] }, skipAutoPermissionPrompt: true }),
    );

    const config = parser.parseConfig(rootDir);

    assert.deepEqual(config.settings.permissions.allow, ['Read']);
    assert.ok(config.settingsLocal, 'settingsLocal should be present');
    assert.deepEqual(config.settingsLocal?.permissions.allow, ['Bash(git*)']);
    assert.equal(config.settingsLocal?.skipAutoPermissionPrompt, true);
  });

  test('settingsLocal is null when the file is absent', () => {
    const config = parser.parseConfig(rootDir);
    assert.equal(config.settingsLocal, null);
  });

  test('empty() seeds settingsLocal: null', () => {
    assert.equal(parser.empty().settingsLocal, null);
  });

  test('new settings.json keys are typed on SettingsFile', () => {
    writeFileSync(
      path.join(rootDir, 'settings.json'),
      JSON.stringify({
        permissions: { allow: [] },
        tui: 'fullscreen',
        autoCompactEnabled: false,
        agentPushNotifEnabled: true,
        promptSuggestionEnabled: true,
        skipWorkflowUsageWarning: true,
        autoMode: {
          environment: ['FIXTURE_MODE=enabled'],
          allow: ['fixture-allow-pattern'],
          soft_deny: ['fixture-deny-pattern'],
        },
      }),
    );
    const s = parser.parseConfig(rootDir).settings;
    assert.equal(s.tui, 'fullscreen');
    assert.equal(s.autoCompactEnabled, false);
    assert.equal(s.agentPushNotifEnabled, true);
    assert.equal(s.promptSuggestionEnabled, true);
    assert.equal(s.skipWorkflowUsageWarning, true);
    assert.deepEqual(s.autoMode, {
      environment: ['FIXTURE_MODE=enabled'],
      allow: ['fixture-allow-pattern'],
      soft_deny: ['fixture-deny-pattern'],
    });
  });

  test('cache/my-closed-issues.json is retained without assuming an upstream record schema', () => {
    mkdirSync(path.join(rootDir, 'cache'), { recursive: true });
    const issue = { number: 42, repository: 'example/project', futureField: { retained: true } };
    writeFileSync(path.join(rootDir, 'cache', 'my-closed-issues.json'), JSON.stringify([issue]));

    assert.deepEqual(parser.parseConfig(rootDir).cache.myClosedIssues, [issue]);
  });

  test('malformed cache/my-closed-issues.json is ignored as an incomplete cache write', () => {
    mkdirSync(path.join(rootDir, 'cache'), { recursive: true });
    writeFileSync(path.join(rootDir, 'cache', 'my-closed-issues.json'), '{');

    assert.equal(parser.parseConfig(rootDir).cache.myClosedIssues, undefined);
  });

  test('mcp-needs-auth-cache.json is read into config.mcpNeedsAuth', () => {
    writeFileSync(
      path.join(rootDir, 'mcp-needs-auth-cache.json'),
      JSON.stringify({ 'plugin:vercel:vercel': { timestamp: 1779286151774, id: 'auth-xyz' }, gmail: { timestamp: 1 } }),
    );
    const c = parser.parseConfig(rootDir);
    assert.equal(c.mcpNeedsAuth?.['plugin:vercel:vercel'].id, 'auth-xyz');
    assert.equal(c.mcpNeedsAuth?.['gmail'].timestamp, 1);
    assert.equal(c.mcpNeedsAuth?.['gmail'].id, undefined);
  });

  test('plugins/blocklist.json is read into plugins.blocklist', () => {
    mkdirSync(path.join(rootDir, 'plugins'), { recursive: true });
    writeFileSync(
      path.join(rootDir, 'plugins', 'blocklist.json'),
      JSON.stringify({
        fetchedAt: '2026-03-31T22:19:08.632Z',
        plugins: [{ plugin: 'code-review@official', added_at: '2026-02-11T03:16:31.424Z', reason: 'test' }],
      }),
    );
    const b = parser.parseConfig(rootDir).plugins.blocklist;
    assert.equal(b?.plugins.length, 1);
    assert.equal(b?.plugins[0].plugin, 'code-review@official');
  });

  test('mcpNeedsAuth + blocklist are null/absent when files are missing', () => {
    const c = parser.parseConfig(rootDir);
    assert.equal(c.mcpNeedsAuth, null);
    assert.equal(c.plugins.blocklist, null);
    assert.equal(parser.empty().mcpNeedsAuth, null);
    assert.equal(parser.empty().plugins.blocklist, null);
  });
});
