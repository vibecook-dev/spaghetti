/**
 * Ingest-contract marker (RFC 008 Phase 0, item 4).
 *
 * The marker's whole value is that it is only ever true when a source really
 * did complete under the current contract. These tests pin the three ways that
 * could quietly stop being true: an absent marker reading as current, a stale
 * version reading as current, and one source's marker answering for another.
 */

import assert from 'node:assert/strict';
import { describe, it, after } from 'node:test';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { createSqliteService } from '../../io/sqlite-service.js';
import { initializeSchema } from '../schema.js';
import {
  RUST_INGEST_CONTRACT,
  RUST_INGEST_CONTRACT_VERSION,
  invalidateSourceContract,
  isSourceContractCurrent,
  markSourceContractCurrent,
} from '../ingest-contract.js';

let roots: string[] = [];
let opened: ReturnType<typeof createSqliteService>[] = [];

after(() => {
  // Close first: Windows refuses to remove a directory holding an open SQLite
  // handle, so a leaked service turns cleanup into EPERM.
  for (const db of opened) {
    try {
      db.close();
    } catch {
      /* already closed */
    }
  }
  opened = [];
  for (const dir of roots) rmSync(dir, { recursive: true, force: true });
  roots = [];
});

function openDb(): ReturnType<typeof createSqliteService> {
  const dir = mkdtempSync(join(tmpdir(), 'spag-contract-'));
  roots.push(dir);
  const db = createSqliteService();
  db.open({ path: join(dir, 'index.db') });
  initializeSchema(db);
  opened.push(db);
  return db;
}

describe('ingest-contract marker', () => {
  it('a fresh database reports no source as current', () => {
    const db = openDb();

    assert.equal(isSourceContractCurrent(db, 'claude-code'), false);
    assert.equal(isSourceContractCurrent(db, 'codex'), false);
    assert.equal(isSourceContractCurrent(db, 'grok'), false);
  });

  it('marks and reads back a single source', () => {
    const db = openDb();
    markSourceContractCurrent(db, 'claude-code');

    assert.equal(isSourceContractCurrent(db, 'claude-code'), true);
  });

  it('is per source — repairing one must not repair another', () => {
    const db = openDb();
    markSourceContractCurrent(db, 'claude-code');

    assert.equal(isSourceContractCurrent(db, 'codex'), false);
    assert.equal(isSourceContractCurrent(db, 'grok'), false);
  });

  it('a stale version reads as not current, so a bump forces the repair', () => {
    const db = openDb();
    db.run(
      `INSERT INTO source_materializations(source_id, projection, version, completed_at)
       VALUES (?, ?, ?, ?)`,
      'claude-code',
      RUST_INGEST_CONTRACT,
      RUST_INGEST_CONTRACT_VERSION - 1,
      Date.now(),
    );

    assert.equal(isSourceContractCurrent(db, 'claude-code'), false);
  });

  it('invalidation drops only the named source', () => {
    const db = openDb();
    markSourceContractCurrent(db, 'claude-code');
    markSourceContractCurrent(db, 'codex');

    invalidateSourceContract(db, 'claude-code');

    assert.equal(isSourceContractCurrent(db, 'claude-code'), false);
    assert.equal(isSourceContractCurrent(db, 'codex'), true);
  });

  it('re-marking is idempotent — one row per source, not one per run', () => {
    const db = openDb();
    markSourceContractCurrent(db, 'claude-code');
    markSourceContractCurrent(db, 'claude-code');
    markSourceContractCurrent(db, 'claude-code');

    const row = db.get<{ count: number }>(
      `SELECT COUNT(*) AS count FROM source_materializations
        WHERE source_id = ? AND projection = ?`,
      'claude-code',
      RUST_INGEST_CONTRACT,
    );
    assert.equal(row?.count, 1);
  });

  it('does not collide with the token-activity projection in the same table', () => {
    const db = openDb();
    db.run(
      `INSERT INTO source_materializations(source_id, projection, version, completed_at)
       VALUES (?, ?, ?, ?)`,
      'claude-code',
      'token-activity',
      1,
      Date.now(),
    );

    // A token-activity row must not make the ingest contract look current.
    assert.equal(isSourceContractCurrent(db, 'claude-code'), false);

    markSourceContractCurrent(db, 'claude-code');
    const rows = db.all<{ projection: string }>(
      'SELECT projection FROM source_materializations WHERE source_id = ? ORDER BY projection',
      'claude-code',
    );
    assert.deepEqual(
      rows.map((r) => r.projection),
      [RUST_INGEST_CONTRACT, 'token-activity'],
    );
  });

  it('Phase 0 ships the representation only — a normal ingest marks nothing', async () => {
    // The exit gate says no production behavior changed. If some code path
    // starts publishing the marker before Phase 1's success-last ordering
    // exists, a failed repair would look complete and never retry.
    const src = await import('../ingest-service.js');
    const surface = Object.keys(src).join(' ');
    assert.ok(
      !surface.includes('markSourceContractCurrent'),
      'ingest-service must not publish the contract marker in Phase 0',
    );
  });
});
