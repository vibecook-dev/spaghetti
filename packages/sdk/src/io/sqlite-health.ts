/**
 * SQLite cache health — detect corruption and wipe rebuildable indexes.
 *
 * The on-disk DB is a pure function of agent roots on disk. When
 * `PRAGMA quick_check` fails (or the file cannot be opened), deleting the
 * cache and re-ingesting is always safe and matches the intent documented
 * on the native bulk writer (`writer.rs`: crash mid-ingest leaves a
 * half-written cache that the next start should recover from).
 *
 * Note: schema version bumps already wipe via `initializeSchema`; this
 * covers structural damage (e.g. `SQLITE_CORRUPT`, freelist/page errors)
 * that leaves `schema_meta` looking valid.
 *
 * Runtime recovery: when ingest throws a corruption error (including native
 * `writer error: sqlite error: database disk image is malformed`), callers
 * should {@link wipeSqliteCacheFiles} and re-run cold ingest once.
 */

import Database from 'better-sqlite3';
import { existsSync, rmSync } from 'node:fs';

export interface CacheHealthResult {
  /** True when the cache files were deleted and must be re-ingested. */
  wiped: boolean;
  /** Human-readable reason when wiped (for progress / logs). */
  detail?: string;
}

/**
 * True when an error (from better-sqlite3, rusqlite via NAPI, or nested
 * messages) indicates a corrupt / unreadable SQLite database file.
 */
export function isSqliteCorruptError(err: unknown): boolean {
  const parts: string[] = [];
  let cur: unknown = err;
  for (let i = 0; i < 4 && cur; i++) {
    if (cur instanceof Error) {
      parts.push(cur.message);
      parts.push(String((cur as NodeJS.ErrnoException).code ?? ''));
      cur = (cur as Error & { cause?: unknown }).cause;
    } else {
      parts.push(String(cur));
      break;
    }
  }
  const text = parts.join(' ').toLowerCase();
  return (
    text.includes('malformed') ||
    text.includes('sqlite_corrupt') ||
    text.includes('database disk image is malformed') ||
    text.includes('file is not a database') ||
    text.includes('not a database') ||
    /\bcorrupt(ed|ion)?\b/.test(text)
  );
}

/**
 * Delete the SQLite main file and common sidecar suffixes (`-wal`, `-shm`,
 * `-journal`). Best-effort; ignores missing files.
 */
export function wipeSqliteCacheFiles(dbPath: string): void {
  for (const suffix of ['', '-wal', '-shm', '-journal']) {
    const p = dbPath + suffix;
    if (!existsSync(p)) continue;
    try {
      rmSync(p, { force: true });
    } catch {
      /* best-effort */
    }
  }
}

function isQuickCheckOk(result: unknown): boolean {
  // better-sqlite3 returns `[{ quick_check: 'ok' }]` for healthy DBs.
  // On failure it may return an array of error strings / objects.
  if (Array.isArray(result)) {
    if (result.length === 0) return false;
    const first = result[0];
    if (typeof first === 'string') return first === 'ok';
    if (first && typeof first === 'object' && 'quick_check' in first) {
      return (first as { quick_check: string }).quick_check === 'ok';
    }
    return false;
  }
  if (typeof result === 'string') return result === 'ok';
  return false;
}

/**
 * If `dbPath` exists, open it read-only and run `PRAGMA quick_check`.
 * On any open/check failure, wipe the cache so the next exclusive ingest
 * is a clean cold start.
 *
 * No-op when the file is missing (first run).
 */
export function ensureSqliteCacheHealthy(dbPath: string): CacheHealthResult {
  if (!existsSync(dbPath)) {
    return { wiped: false };
  }

  let db: Database.Database | null = null;
  try {
    db = new Database(dbPath, { readonly: true, fileMustExist: true });
    const check = db.pragma('quick_check');
    if (!isQuickCheckOk(check)) {
      const detail =
        typeof check === 'string'
          ? check
          : Array.isArray(check)
            ? JSON.stringify(check).slice(0, 200)
            : 'quick_check failed';
      try {
        db.close();
      } catch {
        /* ignore */
      }
      db = null;
      wipeSqliteCacheFiles(dbPath);
      return { wiped: true, detail: `quick_check: ${detail}` };
    }
    return { wiped: false };
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    try {
      db?.close();
    } catch {
      /* ignore */
    }
    wipeSqliteCacheFiles(dbPath);
    return { wiped: true, detail: message };
  } finally {
    try {
      db?.close();
    } catch {
      /* ignore */
    }
  }
}
