/**
 * config-parser-teams.test.ts — Unit tests for the `~/.claude/teams/` parser.
 *
 * Exercises `ConfigParserImpl.parseTeams` against real files in a tmp
 * directory (no mocking, same approach as incremental-parser.test.ts).
 * Shapes mirror what Claude Code writes in the wild as of 2026-07:
 * teams/{name}/config.json + teams/{name}/inboxes/{agent}.json, where
 * config.json may be missing entirely (orphaned inbox-only dirs exist),
 * `description`/`model` are optional, and inbox `text` may itself hold
 * embedded JSON that must survive as a raw string.
 *
 * Run with `pnpm --filter @vibecook/spaghetti-sdk test`.
 */

import { test, describe, before, after, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from 'node:fs';

import { createFileService } from '../../../../io/file-service.js';
import { createConfigParser } from '../config-parser.js';
import type { ConfigParser } from '../config-parser.js';
import type { TeamConfig, InboxMessage } from '../../../../types/index.js';

// ═══════════════════════════════════════════════════════════════════════════
// FIXTURE HELPERS
// ═══════════════════════════════════════════════════════════════════════════

function teamConfig(name: string, overrides: Partial<TeamConfig> = {}): TeamConfig {
  return {
    name,
    createdAt: 1772943657441,
    leadAgentId: `team-lead@${name}`,
    leadSessionId: '2a4f6d12-2a3b-41fa-a70f-ff9ccaa83bed',
    members: [
      {
        agentId: `team-lead@${name}`,
        name: 'team-lead',
        agentType: 'team-lead',
        joinedAt: 1772943657441,
        tmuxPaneId: 'leader',
        cwd: '/tmp/somewhere',
        subscriptions: [],
        backendType: 'in-process',
      },
    ],
    ...overrides,
  };
}

function inboxMessage(from: string, overrides: Partial<InboxMessage> = {}): InboxMessage {
  return {
    from,
    text: 'Review complete, all tests pass.',
    timestamp: '2026-03-08T04:31:45.408Z',
    read: true,
    ...overrides,
  };
}

describe('ConfigParser teams/', () => {
  let tempDir: string;
  let parser: ConfigParser;

  before(() => {
    tempDir = mkdtempSync(path.join(os.tmpdir(), 'spaghetti-teams-'));
    parser = createConfigParser(createFileService());
  });

  after(() => {
    rmSync(tempDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  });

  // Each test gets a fresh fake ~/.claude dir so cases stay independent.
  let rootDir: string;
  let teamsDir: string;
  beforeEach((t) => {
    const safe = t.name.replace(/[^a-zA-Z0-9]/g, '_');
    rootDir = path.join(tempDir, safe);
    teamsDir = path.join(rootDir, 'teams');
    mkdirSync(teamsDir, { recursive: true });
  });

  function writeTeam(teamId: string, config: TeamConfig | string | null, inboxes: Record<string, unknown> = {}) {
    const teamDir = path.join(teamsDir, teamId);
    mkdirSync(teamDir, { recursive: true });
    if (config !== null) {
      const body = typeof config === 'string' ? config : JSON.stringify(config, null, 2);
      writeFileSync(path.join(teamDir, 'config.json'), body);
    }
    const names = Object.keys(inboxes);
    if (names.length > 0) {
      mkdirSync(path.join(teamDir, 'inboxes'), { recursive: true });
      for (const name of names) {
        const value = inboxes[name];
        const body = typeof value === 'string' ? value : JSON.stringify(value, null, 2);
        writeFileSync(path.join(teamDir, 'inboxes', `${name}.json`), body);
      }
    }
  }

  test('parses config and per-agent inboxes for a full team', () => {
    const config = teamConfig('rust-rewrite', { description: 'Rewrite it in Rust' });
    config.members.push({
      agentId: 'implementer@rust-rewrite',
      name: 'implementer',
      model: 'claude-opus-4-6',
      prompt: 'You are the implementer.',
      color: 'blue',
      planModeRequired: false,
      joinedAt: 1772943715186,
      tmuxPaneId: 'in-process',
      cwd: '/tmp/somewhere',
      subscriptions: [],
      backendType: 'in-process',
    });
    const embedded = JSON.stringify({ type: 'idle_notification', from: 'architect', idleReason: 'available' });
    writeTeam('rust-rewrite', config, {
      'team-lead': [
        inboxMessage('architect', {
          summary: 'review done',
          color: 'green',
          msg_id: 'native-message-1',
          msgV: 1,
          type: 'message',
        }),
        inboxMessage('architect', { text: embedded }),
      ],
      implementer: [inboxMessage('team-lead')],
    });

    const teams = parser.parseConfig(rootDir).teams;

    assert.equal(teams.length, 1);
    const team = teams[0];
    assert.equal(team.teamId, 'rust-rewrite');
    assert.equal(team.config?.name, 'rust-rewrite');
    assert.equal(team.config?.description, 'Rewrite it in Rust');
    assert.equal(team.config?.members.length, 2);
    // Lead has no `model`; spawned member does — both shapes occur in the wild.
    assert.equal(team.config?.members[0].model, undefined);
    assert.equal(team.config?.members[1].model, 'claude-opus-4-6');

    assert.deepEqual(Object.keys(team.inboxes).sort(), ['implementer', 'team-lead']);
    assert.equal(team.inboxes['team-lead'].length, 2);
    assert.equal(team.inboxes['team-lead'][0].summary, 'review done');
    assert.equal(team.inboxes['team-lead'][0].msg_id, 'native-message-1');
    assert.equal(team.inboxes['team-lead'][0].msgV, 1);
    assert.equal(team.inboxes['team-lead'][0].type, 'message');
    assert.equal(team.inboxes['team-lead'][1].summary, undefined);
    // Embedded JSON payloads stay raw strings.
    assert.equal(team.inboxes['team-lead'][1].text, embedded);
    assert.equal(team.inboxes['implementer'][0].from, 'team-lead');
  });

  test('team without inboxes dir gets empty inboxes', () => {
    writeTeam('config-only', teamConfig('config-only'));

    const teams = parser.parseConfig(rootDir).teams;

    assert.equal(teams.length, 1);
    assert.equal(teams[0].config?.name, 'config-only');
    assert.deepEqual(teams[0].inboxes, {});
  });

  test('orphaned inbox-only team dir surfaces with null config', () => {
    writeTeam('orphan', null, { architect: [inboxMessage('team-lead')] });

    const teams = parser.parseConfig(rootDir).teams;

    assert.equal(teams.length, 1);
    assert.equal(teams[0].teamId, 'orphan');
    assert.equal(teams[0].config, null);
    assert.equal(teams[0].inboxes['architect'].length, 1);
  });

  test('stray files like .DS_Store are not teams', () => {
    writeFileSync(path.join(teamsDir, '.DS_Store'), 'not json');
    writeTeam('real-team', teamConfig('real-team'));

    const teams = parser.parseConfig(rootDir).teams;

    assert.deepEqual(
      teams.map((t) => t.teamId),
      ['real-team'],
    );
  });

  test('corrupt config.json yields a team with null config', () => {
    writeTeam('broken', '{ not valid json');

    const teams = parser.parseConfig(rootDir).teams;

    assert.equal(teams.length, 1);
    assert.equal(teams[0].teamId, 'broken');
    assert.equal(teams[0].config, null);
  });

  test('corrupt or non-array inbox files are skipped, valid siblings kept', () => {
    writeTeam('mixed', teamConfig('mixed'), {
      good: [inboxMessage('team-lead')],
      corrupt: '[ not valid json',
      'not-array': { from: 'x' },
    });

    const teams = parser.parseConfig(rootDir).teams;

    assert.deepEqual(Object.keys(teams[0].inboxes), ['good']);
  });

  test('missing teams dir parses to an empty list', () => {
    rmSync(teamsDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });

    assert.deepEqual(parser.parseConfig(rootDir).teams, []);
  });

  test('teams are sorted by teamId for deterministic output', () => {
    writeTeam('zeta', teamConfig('zeta'));
    writeTeam('alpha', teamConfig('alpha'));
    writeTeam('mid', teamConfig('mid'));

    const teams = parser.parseConfig(rootDir).teams;

    assert.deepEqual(
      teams.map((t) => t.teamId),
      ['alpha', 'mid', 'zeta'],
    );
  });

  test('empty() includes a teams field', () => {
    assert.deepEqual(parser.empty().teams, []);
  });
});
