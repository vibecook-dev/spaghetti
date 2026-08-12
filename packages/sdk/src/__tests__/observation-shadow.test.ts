import assert from 'node:assert/strict';
import { afterEach, describe, test } from 'node:test';
import { appendFileSync, existsSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

import {
  compareClaudeObservationHistory,
  defaultClaudeObservationShadowDbPath,
  loadNativeAddon,
  openClaudeObservationShadow,
  type ClaudeObservationShadow,
  type SpaghettiEngineOverview,
} from '../index.js';

const native = loadNativeAddon();
const SESSION_ID = '11111111-2222-3333-4444-555555555555';
const shadows: ClaudeObservationShadow[] = [];
const tempDirs: string[] = [];

function fixture(): { directory: string; productionDb: string; root: string; transcript: string } {
  const directory = mkdtempSync(path.join(tmpdir(), 'spaghetti-shadow-'));
  tempDirs.push(directory);
  const root = path.join(directory, 'claude');
  const project = path.join(root, 'projects', '-tmp-shadow-project');
  mkdirSync(project, { recursive: true });
  return {
    directory,
    productionDb: path.join(directory, 'production.db'),
    root,
    transcript: path.join(project, `${SESSION_ID}.jsonl`),
  };
}

function message(uuid: string, role: 'user' | 'assistant', content: string): string {
  return `${JSON.stringify({
    type: role,
    uuid,
    parentUuid: null,
    timestamp: '2026-08-12T00:00:00.000Z',
    sessionId: SESSION_ID,
    cwd: '/tmp/shadow-project',
    gitBranch: 'main',
    message: { role, content },
  })}\n`;
}

afterEach(async () => {
  for (const shadow of shadows.splice(0)) await shadow.dispose();
  for (const directory of tempDirs.splice(0)) {
    rmSync(directory, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 });
  }
});

describe('Claude observation shadow', () => {
  test('derives a sibling database path without replacing the production extension', () => {
    assert.equal(
      defaultClaudeObservationShadowDbPath('/tmp/cache/spaghetti-rs.db'),
      '/tmp/cache/spaghetti-rs.observation-shadow.db',
    );
    assert.equal(
      defaultClaudeObservationShadowDbPath('/tmp/cache/spaghetti'),
      '/tmp/cache/spaghetti.observation-shadow.db',
    );
  });

  test('compares only explicitly scoped Claude compatibility counts', () => {
    const overview = {
      commitSeq: 8,
      canonicalSessions: 2,
      canonicalMessages: 7,
    } as SpaghettiEngineOverview;
    assert.deepEqual(compareClaudeObservationHistory(overview, { sessions: 2, messages: 5, subagentMessages: 2 }), {
      atCommitSeq: 8,
      exact: true,
      sessions: { legacy: 2, canonical: 2, delta: 0, exact: true },
      messages: {
        legacyParent: 5,
        legacySubagent: 2,
        legacyTotal: 7,
        canonical: 7,
        delta: 0,
        exact: true,
      },
    });
    assert.throws(
      () => compareClaudeObservationHistory(overview, { sessions: -1, messages: 5, subagentMessages: 2 }),
      /non-negative safe integer/,
    );
  });
});

describe('Claude observation shadow native lifecycle', { skip: !native }, () => {
  test('rejects the production database and its SQLite sidecars before opening an engine', async () => {
    const { productionDb, root } = fixture();
    for (const shadowDbPath of [productionDb, `${productionDb}-wal`, `${productionDb}.owner.json`]) {
      await assert.rejects(
        openClaudeObservationShadow({ productionDbPath: productionDb, shadowDbPath, roots: [root] }),
        /must be isolated/,
      );
    }
    assert.equal(existsSync(productionDb), false);
  });

  test(
    'rejects a symlink alias of the production database before owner acquisition',
    {
      skip: process.platform === 'win32',
    },
    async () => {
      const { directory, productionDb, root } = fixture();
      writeFileSync(productionDb, 'production sentinel');
      const alias = path.join(directory, 'shadow-alias.db');
      symlinkSync(productionDb, alias);

      await assert.rejects(
        openClaudeObservationShadow({ productionDbPath: productionDb, shadowDbPath: alias, roots: [root] }),
        /must be isolated/,
      );
      assert.equal(existsSync(`${productionDb}.owner.json`), false);
    },
  );

  test(
    'rejects a dangling symlink aimed at a not-yet-created production database',
    {
      skip: process.platform === 'win32',
    },
    async () => {
      const { directory, productionDb, root } = fixture();
      const alias = path.join(directory, 'dangling-shadow-alias.db');
      symlinkSync(productionDb, alias);

      await assert.rejects(
        openClaudeObservationShadow({ productionDbPath: productionDb, shadowDbPath: alias, roots: [root] }),
        /must be isolated/,
      );
      assert.equal(existsSync(productionDb), false);
    },
  );

  test('owns an isolated database, observes live history, and exposes typed parity evidence', async () => {
    const { productionDb, root, transcript } = fixture();
    writeFileSync(transcript, message('aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee', 'user', 'first'));
    const shadow = await openClaudeObservationShadow({
      productionDbPath: productionDb,
      roots: [root],
      ownerLabel: 'sdk-shadow-test',
    });
    shadows.push(shadow);

    assert.equal(existsSync(productionDb), false, 'shadow startup must not touch production storage');
    assert.equal(shadow.status.observation.supervisorsRunning, 1);
    assert.equal(path.basename(shadow.databasePath), 'production.observation-shadow.db');

    const initial = await shadow.snapshot();
    assert.equal(initial.mode, 'shadow');
    assert.equal(initial.health.healthy, true, initial.health.detail);
    assert.deepEqual([initial.overview.canonicalSessions, initial.overview.canonicalMessages], [1, 1]);
    assert.equal(initial.overview.messages, 0, 'compatibility tables remain distinct');

    appendFileSync(transcript, message('bbbbbbbb-cccc-dddd-eeee-ffffffffffff', 'assistant', 'second'));
    await shadow.refresh();
    const parity = await shadow.compareHistory({ sessions: 1, messages: 2, subagentMessages: 0 });
    assert.equal(parity.exact, true);
    assert.equal(parity.messages.canonical, 2);

    const [stopped, stoppedAgain] = await Promise.all([shadow.dispose(), shadow.dispose()]);
    assert.equal(stopped.state, 'stopped');
    assert.equal(stoppedAgain.state, 'stopped');
    assert.equal(stopped.observation.supervisorsRunning, 0);

    const reopened = await openClaudeObservationShadow({
      productionDbPath: productionDb,
      shadowDbPath: shadow.databasePath,
      roots: [root],
      ownerLabel: 'sdk-shadow-restart-test',
    });
    shadows.push(reopened);
    const resumed = await reopened.snapshot();
    assert.deepEqual([resumed.overview.canonicalSessions, resumed.overview.canonicalMessages], [1, 2]);
    assert.equal(resumed.status.owner?.ownerLabel, 'sdk-shadow-restart-test');
    assert.equal((await reopened.dispose()).state, 'stopped');
  });
});
