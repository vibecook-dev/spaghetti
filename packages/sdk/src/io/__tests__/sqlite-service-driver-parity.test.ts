/**
 * Driver-parity regressions for the `node:sqlite` port (RFC 010).
 *
 * Each case below is a difference from `better-sqlite3` that this service has
 * to absorb so callers cannot see it. Every one was found by something else
 * breaking rather than by reading the API — three of the four produce no error
 * at the point of the mistake, which is why they are pinned here.
 */

import { test, describe, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert';
import { mkdtempSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { createSqliteService, type SqliteService } from '../sqlite-service.js';

describe('sqlite-service driver parity', () => {
  let dir: string;
  let db: SqliteService;

  beforeEach(() => {
    dir = mkdtempSync(join(tmpdir(), 'sqlite-parity-'));
    db = createSqliteService();
    db.open({ path: join(dir, 'test.db') });
    db.exec('CREATE TABLE t (a TEXT, b TEXT)');
  });

  afterEach(() => {
    try {
      db.close();
    } catch {
      /* already closed */
    }
    rmSync(dir, { recursive: true, force: true });
  });

  // ── undefined parameters ────────────────────────────────────────────────
  //
  // `better-sqlite3` bound `undefined` as NULL; `node:sqlite` throws. Every
  // optional TS field is `undefined`, so without coercion an absent column
  // fails the whole insert — which is exactly how the `sessions` row (and
  // everything keyed to it) went missing during the port.

  test('an undefined parameter binds as NULL rather than throwing', () => {
    db.run('INSERT INTO t (a, b) VALUES (?, ?)', 'x', undefined);
    assert.deepStrictEqual(db.get<{ b: string | null }>('SELECT b FROM t'), { b: null });
  });

  test('undefined binds as NULL through a prepared statement too', () => {
    const stmt = db.prepare('INSERT INTO t (a, b) VALUES (?, ?)');
    stmt.run('y', undefined);
    assert.deepStrictEqual(db.get<{ b: string | null }>('SELECT b FROM t'), { b: null });
  });

  // ── row prototype ───────────────────────────────────────────────────────
  //
  // `node:sqlite` returns null-prototype rows. They print identically to plain
  // objects, so the difference surfaces as `deepStrictEqual` failing against a
  // literal that looks the same, or `row.hasOwnProperty` being undefined.

  test('rows are ordinary objects, not null-prototype', () => {
    db.run('INSERT INTO t (a, b) VALUES (?, ?)', 'p', 'q');

    const one = db.get<Record<string, unknown>>('SELECT a FROM t');
    assert.strictEqual(Object.getPrototypeOf(one), Object.prototype);
    assert.strictEqual(typeof one?.hasOwnProperty, 'function');

    for (const row of db.all<Record<string, unknown>>('SELECT a FROM t')) {
      assert.strictEqual(Object.getPrototypeOf(row), Object.prototype);
    }
    for (const row of db.iterate<Record<string, unknown>>('SELECT a FROM t')) {
      assert.strictEqual(Object.getPrototypeOf(row), Object.prototype);
    }
  });

  test('a missing row is still undefined, not an empty object', () => {
    assert.strictEqual(db.get('SELECT a FROM t WHERE a = ?', 'nope'), undefined);
  });

  // ── transaction nesting ─────────────────────────────────────────────────
  //
  // `better-sqlite3` nested its transaction helper as a SAVEPOINT, and
  // `ingest-service` relies on that for live batches. This service always
  // issues a SAVEPOINT — SQLite treats an outermost one as BEGIN DEFERRED — so
  // it nests correctly even inside a transaction opened by a raw `exec`.

  const count = () => db.get<{ c: number }>('SELECT COUNT(*) c FROM t')?.c ?? -1;

  test('a top-level transaction commits', () => {
    db.transaction(() => db.run('INSERT INTO t (a) VALUES (?)', '1'));
    assert.strictEqual(count(), 1);
  });

  test('a throwing top-level transaction rolls back', () => {
    assert.throws(() =>
      db.transaction(() => {
        db.run('INSERT INTO t (a) VALUES (?)', '1');
        throw new Error('boom');
      }),
    );
    assert.strictEqual(count(), 0);
  });

  test('transactions nest, and the inner one commits with the outer', () => {
    db.transaction(() => {
      db.run('INSERT INTO t (a) VALUES (?)', '1');
      db.transaction(() => db.run('INSERT INTO t (a) VALUES (?)', '2'));
    });
    assert.strictEqual(count(), 2);
  });

  test('an inner failure rolls back only the inner work', () => {
    db.transaction(() => {
      db.run('INSERT INTO t (a) VALUES (?)', '1');
      try {
        db.transaction(() => {
          db.run('INSERT INTO t (a) VALUES (?)', '2');
          throw new Error('inner');
        });
      } catch {
        /* swallowed on purpose — the outer must still commit */
      }
    });
    assert.strictEqual(count(), 1);
  });

  test('nests inside a transaction opened by a raw exec — the live-ingest shape', () => {
    // `IngestService` opens its outermost transaction this way, so the service
    // cannot rely on having seen a `BEGIN` itself.
    db.exec('BEGIN');
    db.run('INSERT INTO t (a) VALUES (?)', '1');
    db.transaction(() => db.run('INSERT INTO t (a) VALUES (?)', '2'));
    db.exec('COMMIT');
    assert.strictEqual(count(), 2);
  });

  test('an inner failure inside a raw transaction leaves the outer intact', () => {
    db.exec('BEGIN');
    db.run('INSERT INTO t (a) VALUES (?)', '1');
    try {
      db.transaction(() => {
        db.run('INSERT INTO t (a) VALUES (?)', '2');
        throw new Error('inner');
      });
    } catch {
      /* expected */
    }
    db.exec('COMMIT');
    assert.strictEqual(count(), 1);
  });
});

// ── open options ──────────────────────────────────────────────────────────
//
// The driver does not validate its options bag, so a wrong key is ignored
// rather than rejected. `better-sqlite3` spelled it `readonly`; the driver
// wants `readOnly`, and it has no `fileMustExist` at all.

describe('sqlite-service open options are honoured, not silently dropped', () => {
  let dir: string;

  beforeEach(() => {
    dir = mkdtempSync(join(tmpdir(), 'sqlite-open-'));
  });
  afterEach(() => rmSync(dir, { recursive: true, force: true }));

  test('fileMustExist throws on a missing file instead of creating one', () => {
    const missing = join(dir, 'absent.db');
    const db = createSqliteService();
    assert.throws(() => db.open({ path: missing, fileMustExist: true }), /does not exist/);
    assert.strictEqual(existsSync(missing), false, 'must not have created the file');
  });

  test('readonly actually prevents writes', () => {
    const path = join(dir, 'ro.db');
    const writer = createSqliteService();
    writer.open({ path });
    writer.exec('CREATE TABLE t (a TEXT)');
    writer.close();

    const reader = createSqliteService();
    reader.open({ path, readonly: true });
    try {
      assert.throws(
        () => reader.run('INSERT INTO t (a) VALUES (?)', 'nope'),
        /readonly|read-only/i,
        'a read-only handle must reject writes — passing the wrong option name silently allows them',
      );
    } finally {
      reader.close();
    }
  });

  test('a read-only handle can still be read from', () => {
    const path = join(dir, 'ro2.db');
    const writer = createSqliteService();
    writer.open({ path });
    writer.exec('CREATE TABLE t (a TEXT)');
    writer.run('INSERT INTO t (a) VALUES (?)', 'v');
    writer.close();

    const reader = createSqliteService();
    reader.open({ path, readonly: true });
    try {
      assert.deepStrictEqual(reader.get<{ a: string }>('SELECT a FROM t'), { a: 'v' });
    } finally {
      reader.close();
    }
  });
});
