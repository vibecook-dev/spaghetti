/**
 * sqlite-health — wipe on corrupt / unreadable cache.
 */

import { describe, test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, existsSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import Database from 'better-sqlite3';
import { ensureSqliteCacheHealthy, isSqliteCorruptError, wipeSqliteCacheFiles } from '../sqlite-health.js';

describe('ensureSqliteCacheHealthy', () => {
  let dir: string;

  before(() => {
    dir = mkdtempSync(join(tmpdir(), 'spag-health-'));
  });

  after(() => {
    rmSync(dir, { recursive: true, force: true });
  });

  test('missing file is a no-op', () => {
    const path = join(dir, 'missing.db');
    const r = ensureSqliteCacheHealthy(path);
    assert.equal(r.wiped, false);
    assert.equal(existsSync(path), false);
  });

  test('healthy db is left alone', () => {
    const path = join(dir, 'healthy.db');
    const db = new Database(path);
    db.exec('CREATE TABLE t (id INTEGER PRIMARY KEY); INSERT INTO t VALUES (1);');
    db.close();
    const r = ensureSqliteCacheHealthy(path);
    assert.equal(r.wiped, false);
    assert.equal(existsSync(path), true);
  });

  test('garbage file is wiped', () => {
    const path = join(dir, 'garbage.db');
    writeFileSync(path, 'this is not a sqlite database at all!!!!');
    writeFileSync(path + '-wal', 'x');
    const r = ensureSqliteCacheHealthy(path);
    assert.equal(r.wiped, true);
    assert.ok(r.detail);
    assert.equal(existsSync(path), false);
    assert.equal(existsSync(path + '-wal'), false);
  });

  test('wipeSqliteCacheFiles removes sidecars', () => {
    const path = join(dir, 'sidecars.db');
    writeFileSync(path, 'x');
    writeFileSync(path + '-shm', 'y');
    writeFileSync(path + '-journal', 'z');
    wipeSqliteCacheFiles(path);
    assert.equal(existsSync(path), false);
    assert.equal(existsSync(path + '-shm'), false);
    assert.equal(existsSync(path + '-journal'), false);
  });
});

describe('isSqliteCorruptError', () => {
  test('matches native writer malformed message', () => {
    assert.equal(isSqliteCorruptError(new Error('writer error: sqlite error: database disk image is malformed')), true);
  });

  test('matches SQLITE_CORRUPT code-like text', () => {
    const err = new Error('SqliteError: database disk image is malformed');
    (err as NodeJS.ErrnoException).code = 'SQLITE_CORRUPT';
    assert.equal(isSqliteCorruptError(err), true);
  });

  test('rejects unrelated errors', () => {
    assert.equal(isSqliteCorruptError(new Error('database is locked')), false);
    assert.equal(isSqliteCorruptError(new Error('ENOENT')), false);
  });
});
