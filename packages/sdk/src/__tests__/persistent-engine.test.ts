/**
 * RFC 011 Phase 1 — real N-API lifecycle contract.
 *
 * Rust unit tests pin the worker internals. This suite pins what SDK callers
 * actually receive: an async opener, a persistent class handle, typed reads,
 * exclusive-owner diagnostics, and deterministic disposal.
 */

import { afterEach, describe, test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

import { loadNativeAddon, openSpaghettiEngine, type SpaghettiEngine } from '../index.js';

const native = loadNativeAddon();
const engines: SpaghettiEngine[] = [];
const tempDirs: string[] = [];

function temporaryDatabase(): string {
  const dir = mkdtempSync(path.join(tmpdir(), 'spaghetti-engine-'));
  tempDirs.push(dir);
  return path.join(dir, 'spaghetti.db');
}

async function openTracked(dbPath: string, ownerLabel: string): Promise<SpaghettiEngine> {
  const engine = await openSpaghettiEngine({ dbPath, ownerLabel, queryWorkers: 2 });
  engines.push(engine);
  return engine;
}

afterEach(async () => {
  for (const engine of engines.splice(0)) {
    await engine.dispose();
  }
  for (const dir of tempDirs.splice(0)) {
    rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 });
  }
});

describe('persistent SpaghettiEngine', { skip: !native }, () => {
  test('opens asynchronously, reports health, queries, and disposes', async () => {
    const engine = await openTracked(temporaryDatabase(), 'sdk-lifecycle-test');

    assert.equal(engine.status.state, 'running');
    assert.equal(engine.status.writerAlive, true);
    assert.equal(engine.status.aliveQueryWorkers, 2);
    assert.equal(engine.status.owner?.ownerLabel, 'sdk-lifecycle-test');

    const health = await engine.health();
    assert.equal(health.healthy, true, health.detail);

    const overview = await engine.overview();
    assert.equal(overview.schemaVersion > 0, true);
    assert.equal(overview.commitSeq, 0);
    assert.deepEqual([overview.projects, overview.sessions, overview.messages], [0, 0, 0]);
    assert.equal(overview.journalMode, 'wal');
    assert.equal(overview.queryOnly, true);
    assert.equal(overview.readOnly, true);

    const firstCancellationEpoch = engine.cancelPendingQueries();
    assert.equal(engine.cancelPendingQueries(), firstCancellationEpoch + 1);
    assert.equal((await engine.overview()).schemaVersion, overview.schemaVersion);

    const stopped = await engine.dispose();
    assert.equal(stopped.state, 'stopped');
    assert.equal(stopped.writerAlive, false);
    assert.equal(stopped.aliveQueryWorkers, 0);
    assert.equal((await engine.health()).healthy, false);
    await assert.rejects(engine.overview(), /shutting down|stopped/i);
  });

  test('rejects a second owner with the current owner metadata', async () => {
    const dbPath = temporaryDatabase();
    await openTracked(dbPath, 'first-owner');

    await assert.rejects(openSpaghettiEngine({ dbPath, ownerLabel: 'second-owner' }), (error: Error) => {
      assert.match(error.message, /already owned/i);
      assert.match(error.message, /first-owner/);
      return true;
    });
  });

  test('reopens after deterministic disposal without leaked locks', async () => {
    const dbPath = temporaryDatabase();
    const first = await openTracked(dbPath, 'first');
    await first.dispose();

    const second = await openTracked(dbPath, 'second');
    assert.equal(second.status.owner?.ownerLabel, 'second');
    assert.equal((await second.health()).healthy, true);
  });
});
