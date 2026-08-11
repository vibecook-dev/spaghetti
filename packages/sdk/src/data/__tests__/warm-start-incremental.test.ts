/**
 * warm-start-incremental.test.ts — Regression tests for the TS engine's
 * warm-start correctness fixes (2026-07 engine-flow audit).
 *
 * The big one: the streaming JSONL reader restarts its line index at 0
 * when resuming from a byte position, and `messages` upserts on
 * `(session_id, msg_index)` — so the warm-start grown-file path used to
 * write appended messages over the HEAD of the session (index 0..N),
 * silently destroying history in the DB. These tests run the real
 * service (TS engine) against a temp rootDir and assert appends
 * extend the tail, stray files aren't projects, and the one-shot
 * msg_index heal repairs previously clobbered rows.
 *
 * Run with `pnpm --filter @vibecook/spaghetti-sdk test`.
 */

import { test, describe, beforeEach, after } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import * as path from 'node:path';
import { mkdtempSync, rmSync, writeFileSync, appendFileSync, mkdirSync } from 'node:fs';
import { createRequire } from 'node:module';

import { createSpaghettiService } from '../../create.js';
import type { SpaghettiAPI } from '../../api.js';

const require = createRequire(import.meta.url);

// ═══════════════════════════════════════════════════════════════════════════
// FIXTURE HELPERS
// ═══════════════════════════════════════════════════════════════════════════

const SLUG = '-Users-demo-warm';
const SESSION_ID = 'aaaa0000-1111-2222-3333-444455556666';

function messageLine(i: number): string {
  const type = i % 2 === 0 ? 'user' : 'assistant';
  const content = type === 'user' ? `prompt ${i}` : [{ type: 'text', text: `reply ${i}` }];
  return (
    JSON.stringify({
      type,
      uuid: `uuid-${i}`,
      parentUuid: i === 0 ? null : `uuid-${i - 1}`,
      sessionId: SESSION_ID,
      timestamp: new Date(Date.UTC(2026, 0, 1, 8, 0, i)).toISOString(),
      message: { role: type, content },
    }) + '\n'
  );
}

describe('TS warm start — incremental correctness', () => {
  let tempDir: string;
  let rootDir: string;
  let dbPath: string;
  let sessionFile: string;

  beforeEach((t) => {
    const safe = t.name.replace(/[^a-zA-Z0-9]/g, '_');
    tempDir = mkdtempSync(path.join(os.tmpdir(), `spaghetti-warm-${safe}-`));
    rootDir = path.join(tempDir, '.claude');
    dbPath = path.join(tempDir, 'test.db');
    const projectDir = path.join(rootDir, 'projects', SLUG);
    mkdirSync(projectDir, { recursive: true });
    sessionFile = path.join(projectDir, `${SESSION_ID}.jsonl`);
    writeFileSync(sessionFile, [0, 1, 2].map(messageLine).join(''));
  });

  after(() => {
    // beforeEach re-creates tempDir per test; clean the last one.
    rmSync(tempDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  });

  async function boot(): Promise<SpaghettiAPI> {
    const svc = createSpaghettiService({ engine: 'ts', rootDir, dbPath });
    await svc.initialize();
    return svc;
  }

  function messageUuids(svc: SpaghettiAPI): string[] {
    const page = svc.getSessionMessages(SLUG, SESSION_ID, 100, 0);
    return page.messages.map((m) => (m as unknown as { uuid: string }).uuid);
  }

  test('appended messages extend the tail — head stays intact', async () => {
    const cold = await boot();
    assert.deepEqual(messageUuids(cold), ['uuid-0', 'uuid-1', 'uuid-2']);
    await cold.dispose();

    appendFileSync(sessionFile, messageLine(3) + messageLine(4));

    const warm = await boot();
    // Pre-fix behavior: total stays 3 and uuid-3/uuid-4 OVERWRITE uuid-0/uuid-1.
    assert.deepEqual(messageUuids(warm), ['uuid-0', 'uuid-1', 'uuid-2', 'uuid-3', 'uuid-4']);
    await warm.dispose();

    rmSync(tempDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  });

  test('stray files under projects/ are not ingested as projects', async () => {
    writeFileSync(path.join(rootDir, 'projects', '.DS_Store'), 'junk');

    const svc = await boot();
    assert.deepEqual(
      svc.getProjectList().map((p) => p.slug),
      [SLUG],
    );
    await svc.dispose();

    rmSync(tempDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  });

  test('one-shot heal restores rows clobbered by the old incremental path', async () => {
    const cold = await boot();
    await cold.dispose();

    // Simulate the historical damage: head row overwritten by an
    // appended message, and no heal marker (old-version DB).
    const { DatabaseSync } = require('node:sqlite');
    const db = new DatabaseSync(dbPath);
    db.prepare('UPDATE messages SET uuid = ?, data = ? WHERE session_id = ? AND msg_index = 0').run(
      'uuid-999',
      JSON.stringify({ type: 'user', uuid: 'uuid-999' }),
      SESSION_ID,
    );
    db.prepare('DELETE FROM schema_meta WHERE key = ?').run('heal_msg_index_v1');
    db.close();

    const healed = await boot();
    assert.deepEqual(messageUuids(healed), ['uuid-0', 'uuid-1', 'uuid-2']);
    await healed.dispose();

    // Second boot must NOT re-heal (marker present) and stays correct.
    const warm = await boot();
    assert.deepEqual(messageUuids(warm), ['uuid-0', 'uuid-1', 'uuid-2']);
    await warm.dispose();

    rmSync(tempDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  });

  test('repeated warm starts with no changes stay stable', async () => {
    const cold = await boot();
    await cold.dispose();

    for (let i = 0; i < 2; i++) {
      const warm = await boot();
      assert.deepEqual(messageUuids(warm), ['uuid-0', 'uuid-1', 'uuid-2']);
      await warm.dispose();
    }

    rmSync(tempDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  });
});
