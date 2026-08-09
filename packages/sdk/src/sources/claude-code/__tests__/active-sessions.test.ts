/**
 * Active session registry reader tests.
 */

import assert from 'node:assert/strict';
import { describe, it, before, after } from 'node:test';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';

import { listActiveSessionsFromDir, isProcessAlive } from '../active-sessions.js';

describe('listActiveSessionsFromDir', () => {
  let dir: string;

  before(() => {
    dir = mkdtempSync(join(tmpdir(), 'spag-active-sess-'));
  });

  after(() => {
    try {
      rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
    } catch {
      /* ignore */
    }
  });

  it('isProcessAlive reports current process as alive', () => {
    assert.equal(isProcessAlive(process.pid), true);
    assert.equal(isProcessAlive(-1), false);
  });

  it('parses valid session files and filters dead pids when requireAlive', () => {
    const livePid = process.pid;
    const deadPid = 999_999_999;

    writeFileSync(
      join(dir, `${livePid}.json`),
      JSON.stringify({
        pid: livePid,
        sessionId: 'sess-live',
        cwd: '/tmp/proj',
        startedAt: Date.now() - 1000,
        name: 'live-one',
      }),
      'utf-8',
    );
    writeFileSync(
      join(dir, `${deadPid}.json`),
      JSON.stringify({
        pid: deadPid,
        sessionId: 'sess-dead',
        cwd: '/tmp/other',
        startedAt: Date.now() - 5000,
      }),
      'utf-8',
    );
    writeFileSync(join(dir, 'not-json.txt'), 'nope', 'utf-8');
    writeFileSync(join(dir, 'bad.json'), '{not json', 'utf-8');

    const alive = listActiveSessionsFromDir(dir, { requireAlive: true });
    assert.ok(alive.some((s) => s.sessionId === 'sess-live'));
    assert.ok(!alive.some((s) => s.sessionId === 'sess-dead'));

    const all = listActiveSessionsFromDir(dir, { requireAlive: false });
    assert.ok(all.some((s) => s.sessionId === 'sess-live'));
    assert.ok(all.some((s) => s.sessionId === 'sess-dead'));
  });

  it('returns empty for missing directory', () => {
    assert.deepEqual(listActiveSessionsFromDir(join(dir, 'nope-missing')), []);
  });
});
