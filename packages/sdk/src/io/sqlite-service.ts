/**
 * SqliteService — wrapper around Node's built-in `node:sqlite`.
 *
 * Ported from `better-sqlite3` by RFC 010, which removed the last dependency
 * in the tree needing an install script to work. The public shape of this
 * module is unchanged: `SqliteConfig` keeps `readonly` / `fileMustExist`
 * spelled the way SDK consumers already spell them, and the driver's own
 * naming is an implementation detail handled in `open()`.
 */

import { DatabaseSync, type SQLInputValue } from 'node:sqlite';
import { existsSync, mkdirSync, statSync } from 'fs';
import { dirname } from 'path';

// ═══════════════════════════════════════════════════════════════════════════
// TYPES
// ═══════════════════════════════════════════════════════════════════════════

export interface SqliteConfig {
  path: string;
  /** Open read-only. Mapped to the driver's `readOnly` in {@link SqliteServiceImpl.open}. */
  readonly?: boolean;
  /** Throw instead of creating the file. Enforced here — the driver has no such option. */
  fileMustExist?: boolean;
  timeout?: number;
}

export interface RunResult {
  changes: number;
  lastInsertRowid: number | bigint;
}

export interface PreparedStatement<T = unknown> {
  run(...params: unknown[]): RunResult;
  get(...params: unknown[]): T | undefined;
  all(...params: unknown[]): T[];
  iterate(...params: unknown[]): IterableIterator<T>;
}

export interface TableInfo {
  cid: number;
  name: string;
  type: string;
  notnull: number;
  dflt_value: unknown;
  pk: number;
}

// ═══════════════════════════════════════════════════════════════════════════
// INTERFACE
// ═══════════════════════════════════════════════════════════════════════════

export interface SqliteService {
  open(config: SqliteConfig): void;
  close(): void;
  isOpen(): boolean;
  getDb(): DatabaseSync;

  exec(sql: string): void;
  run(sql: string, ...params: unknown[]): RunResult;
  get<T>(sql: string, ...params: unknown[]): T | undefined;
  all<T>(sql: string, ...params: unknown[]): T[];
  iterate<T>(sql: string, ...params: unknown[]): IterableIterator<T>;

  prepare<T = unknown>(sql: string): PreparedStatement<T>;

  transaction<T>(fn: () => T): T;

  tableExists(tableName: string): boolean;
  getTables(): string[];
  getTableInfo(tableName: string): TableInfo[];
  vacuum(): void;
  getFileSize(): number;
}

// ═══════════════════════════════════════════════════════════════════════════
// DRIVER ADAPTERS
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Adapt this facade's `unknown[]` parameters to the driver's `SQLInputValue[]`,
 * coercing `undefined` to `null`.
 *
 * **`undefined` is the one binding difference between the drivers, and it is
 * everywhere.** `better-sqlite3` accepted an `undefined` parameter and stored
 * `NULL`; `node:sqlite` throws `Provided value cannot be bound to SQLite
 * parameter N`. TypeScript spells every optional field `undefined`, so without
 * this coercion any row with an absent optional column fails to insert — which
 * is what happened to `sessions` (`entry.summary` is optional), taking the
 * session row and everything keyed to it with it.
 *
 * Coercing here rather than at each call site keeps the previous behaviour
 * exactly: absent value in, `NULL` on disk.
 */
function bind(params: unknown[]): SQLInputValue[] {
  return params.map((p) => (p === undefined ? null : p)) as SQLInputValue[];
}

/**
 * Normalise a driver run result to {@link RunResult}.
 *
 * `node:sqlite` types `changes` and `lastInsertRowid` as `number | bigint`,
 * widening for the `readBigInts` option this service does not enable. Both come
 * back as `number` at runtime — matching `better-sqlite3` — but the declared
 * union would otherwise leak into every caller doing arithmetic on `changes`.
 */
function toRunResult(result: { changes: number | bigint; lastInsertRowid: number | bigint }): RunResult {
  return {
    changes: Number(result.changes),
    lastInsertRowid: result.lastInsertRowid,
  };
}

/**
 * Give a driver row `Object.prototype`.
 *
 * `node:sqlite` returns rows with a **null prototype**; `better-sqlite3`
 * returned ordinary objects. The difference is invisible when logging — the two
 * print identically — and shows up as `row.hasOwnProperty` being `undefined`,
 * or `assert.deepStrictEqual` failing against an object literal whose printed
 * form is character-for-character the same. Row shape is part of this service's
 * contract, so it is normalised once here rather than surprising each caller.
 *
 * `undefined` is passed through: a missing row is not an object.
 */
function toPlain<T>(row: T): T {
  return (row === undefined || row === null ? row : { ...row }) as T;
}

/** {@link toPlain} over an iterator, lazily — `iterate()` exists to avoid
 * materialising the result set, so this must not collect it. */
function* plainRows<T>(rows: IterableIterator<T>): IterableIterator<T> {
  for (const row of rows) yield toPlain(row);
}

// ═══════════════════════════════════════════════════════════════════════════
// IMPLEMENTATION
// ═══════════════════════════════════════════════════════════════════════════

export class SqliteServiceImpl implements SqliteService {
  private db: DatabaseSync | null = null;
  private config: SqliteConfig | null = null;
  /** Names savepoints uniquely; see {@link transaction}. Not a nesting depth. */
  private savepointSeq = 0;

  open(config: SqliteConfig): void {
    if (this.db) {
      throw new Error('Database already open. Close it first.');
    }

    const dir = dirname(config.path);
    if (!existsSync(dir)) {
      mkdirSync(dir, { recursive: true });
    }

    // `node:sqlite` has no `fileMustExist`, and — because it does not validate
    // its options bag — passing one is silently ignored rather than rejected.
    // Enforce it here so the option keeps meaning what callers think it means.
    if (config.fileMustExist && !existsSync(config.path)) {
      throw new Error(`Database file does not exist: ${config.path}`);
    }

    // NOTE the spelling. `better-sqlite3` used `readonly`; the driver wants
    // `readOnly`, and an unknown key is dropped without complaint — so the
    // lowercase form would open the database *writable* while looking correct.
    const dbOptions: { readOnly?: boolean; timeout?: number } = {};
    if (config.readonly !== undefined) dbOptions.readOnly = config.readonly;
    if (config.timeout !== undefined) dbOptions.timeout = config.timeout;

    this.db = new DatabaseSync(config.path, dbOptions);
    // WAL is a write to the database header, so it cannot be set on a
    // read-only handle. Foreign keys are already on: the driver defaults
    // `enableForeignKeyConstraints` to true.
    if (!config.readonly) {
      this.db.exec('PRAGMA journal_mode = WAL');
    }
    this.config = config;
  }

  close(): void {
    if (this.db) {
      this.db.close();
      this.db = null;
      this.config = null;
    }
  }

  isOpen(): boolean {
    return this.db !== null;
  }

  getDb(): DatabaseSync {
    if (!this.db) {
      throw new Error('Database not open. Call open() first.');
    }
    return this.db;
  }

  exec(sql: string): void {
    this.getDb().exec(sql);
  }

  run(sql: string, ...params: unknown[]): RunResult {
    const result = this.getDb()
      .prepare(sql)
      .run(...bind(params));
    return toRunResult(result);
  }

  get<T>(sql: string, ...params: unknown[]): T | undefined {
    return toPlain(
      this.getDb()
        .prepare(sql)
        .get(...bind(params)) as T | undefined,
    );
  }

  all<T>(sql: string, ...params: unknown[]): T[] {
    return (
      this.getDb()
        .prepare(sql)
        .all(...bind(params)) as T[]
    ).map(toPlain);
  }

  iterate<T>(sql: string, ...params: unknown[]): IterableIterator<T> {
    return plainRows(
      this.getDb()
        .prepare(sql)
        .iterate(...bind(params)) as IterableIterator<T>,
    );
  }

  prepare<T = unknown>(sql: string): PreparedStatement<T> {
    const stmt = this.getDb().prepare(sql);
    return {
      run: (...params: unknown[]) => toRunResult(stmt.run(...bind(params))),
      get: (...params: unknown[]) => toPlain(stmt.get(...bind(params)) as T | undefined),
      all: (...params: unknown[]) => (stmt.all(...bind(params)) as T[]).map(toPlain),
      iterate: (...params: unknown[]) => plainRows(stmt.iterate(...bind(params)) as IterableIterator<T>),
    };
  }

  /**
   * Run `fn` atomically, nesting correctly inside an already-open transaction.
   *
   * Always a `SAVEPOINT`, never a `BEGIN`. SQLite treats an outermost savepoint
   * exactly like `BEGIN DEFERRED`, so one code path covers both cases and this
   * needs no knowledge of whether a transaction is already open — which matters,
   * because `IngestService` opens its outer transaction with a raw
   * `exec('BEGIN')` that no counter here would ever see.
   *
   * `better-sqlite3` provided this by nesting its transaction helper as a
   * savepoint; `ingest-service.ts` depends on that for live batches, where an
   * aggregation failure has to roll back the whole batch. A plain
   * `BEGIN`/`COMMIT` would raise "cannot start a transaction within a
   * transaction" on that path.
   */
  transaction<T>(fn: () => T): T {
    const db = this.getDb();
    const name = `sp_${this.savepointSeq++}`;
    db.exec(`SAVEPOINT ${name}`);
    try {
      const result = fn();
      db.exec(`RELEASE ${name}`);
      return result;
    } catch (err) {
      // ROLLBACK TO rewinds to the savepoint but leaves it on the stack;
      // RELEASE then pops it. Both are needed to leave the stack as we found it.
      db.exec(`ROLLBACK TO ${name}`);
      db.exec(`RELEASE ${name}`);
      throw err;
    }
  }

  tableExists(tableName: string): boolean {
    const result = this.get<{ count: number }>(
      `SELECT COUNT(*) as count FROM sqlite_master WHERE type='table' AND name=?`,
      tableName,
    );
    return (result?.count ?? 0) > 0;
  }

  getTables(): string[] {
    const rows = this.all<{ name: string }>(
      `SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name`,
    );
    return rows.map((r) => r.name);
  }

  getTableInfo(tableName: string): TableInfo[] {
    return this.all<TableInfo>(`PRAGMA table_info(${tableName})`);
  }

  vacuum(): void {
    this.getDb().exec('VACUUM');
  }

  getFileSize(): number {
    if (!this.config) return 0;
    try {
      const stats = statSync(this.config.path);
      return stats.size;
    } catch {
      return 0;
    }
  }
}

// ═══════════════════════════════════════════════════════════════════════════
// FACTORY
// ═══════════════════════════════════════════════════════════════════════════

export function createSqliteService(): SqliteService {
  return new SqliteServiceImpl();
}
