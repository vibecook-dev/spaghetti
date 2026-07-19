/**
 * Segment Store — SQLite persistence for segments, fingerprints, and FTS5 search
 */

import type { SqliteService } from '../io/index.js';
import type {
  SegmentKey,
  SegmentType,
  Segment,
  SourceFingerprint,
  PaginatedSegmentQuery,
  PaginatedSegmentResult,
  SearchQuery,
  SearchResultSet,
} from './segment-types.js';
import type { SearchIndexEntry } from './search-indexer.js';

export interface SegmentStore {
  open(dbPath: string): void;
  close(): void;
  isOpen(): boolean;

  getSegment<T>(key: SegmentKey): Segment<T> | null;
  getSegments<T>(keys: SegmentKey[]): Segment<T>[];
  getSegmentsByType<T>(type: SegmentType): Segment<T>[];
  getSegmentsByPrefix<T>(type: SegmentType, keyPrefix: string): Segment<T>[];
  getSegmentsPaginated<T>(query: PaginatedSegmentQuery): PaginatedSegmentResult<T>;
  upsertSegment<T>(key: SegmentKey, type: SegmentType, data: T): void;
  upsertSegmentsBatch(segments: Array<{ key: SegmentKey; type: SegmentType; data: unknown }>): void;
  deleteSegment(key: SegmentKey): void;
  deleteSegmentsByPrefix(keyPrefix: string): void;
  countByPrefix(type: SegmentType, keyPrefix: string): number;

  getFingerprint(path: string): SourceFingerprint | null;
  getAllFingerprints(): SourceFingerprint[];
  upsertFingerprint(fp: SourceFingerprint, segmentKeys: SegmentKey[]): void;
  deleteFingerprint(path: string): void;
  getSegmentKeysForFile(path: string): SegmentKey[];

  indexSegment(key: SegmentKey, type: SegmentType, entry: SearchIndexEntry): void;
  indexSegmentsBatch(
    entries: Array<{ key: SegmentKey; type: SegmentType; entry: SearchIndexEntry }>,
    options?: { skipDelete?: boolean },
  ): void;
  removeFromIndex(key: SegmentKey): void;
  search(query: SearchQuery): SearchResultSet;

  vacuum(): void;
  getDbSizeBytes(): number;
  getSegmentCount(): number;
  getSegmentCountByType(): Record<string, number>;
  getSearchIndexCount(): number;
}

const CURRENT_SCHEMA_VERSION = 2;

const SCHEMA_VERSION_SQL = `
CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);
`;

const SCHEMA_SQL = `
CREATE TABLE IF NOT EXISTS segments (
  key TEXT PRIMARY KEY,
  type TEXT NOT NULL,
  data TEXT NOT NULL,
  version INTEGER NOT NULL DEFAULT 1,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_segments_type ON segments(type);
CREATE INDEX IF NOT EXISTS idx_segments_type_key ON segments(type, key);

CREATE TABLE IF NOT EXISTS source_files (
  path TEXT PRIMARY KEY,
  mtime_ms REAL NOT NULL,
  size INTEGER NOT NULL,
  byte_position INTEGER,
  segment_keys TEXT NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS search_index USING fts5(
  key,
  type,
  project_slug,
  session_id,
  text_content,
  tags
);
`;

interface SegmentRow {
  key: string;
  type: string;
  data: string;
  version: number;
  updated_at: number;
}
interface SourceFileRow {
  path: string;
  mtime_ms: number;
  size: number;
  byte_position: number | null;
  segment_keys: string;
}
interface CountRow {
  count: number;
}
interface TypeCountRow {
  type: string;
  count: number;
}
interface SearchRow {
  key: string;
  type: string;
  project_slug: string;
  session_id: string;
  snippet: string;
  rank: number;
}

class SegmentStoreImpl implements SegmentStore {
  private db: SqliteService;
  private opened = false;

  constructor(sqliteServiceFactory: () => SqliteService) {
    this.db = sqliteServiceFactory();
  }

  open(dbPath: string): void {
    this.db.open({ path: dbPath });
    this.migrateIfNeeded();
    this.opened = true;
  }

  private migrateIfNeeded(): void {
    // Create the schema_version table if it doesn't exist
    this.db.exec(SCHEMA_VERSION_SQL);

    // Check current version
    const row = this.db.get<{ version: number }>('SELECT version FROM schema_version LIMIT 1');
    const currentVersion = row?.version ?? 0;

    if (currentVersion !== CURRENT_SCHEMA_VERSION) {
      // Drop all existing tables and recreate with new schema
      // Use try/catch for each DROP since the tables may not exist
      try {
        this.db.exec('DROP TABLE IF EXISTS segments');
      } catch {
        /* ignore */
      }
      try {
        this.db.exec('DROP TABLE IF EXISTS source_files');
      } catch {
        /* ignore */
      }
      try {
        this.db.exec('DROP TABLE IF EXISTS search_index');
      } catch {
        /* ignore */
      }
      try {
        this.db.exec('DROP TABLE IF EXISTS schema_version');
      } catch {
        /* ignore */
      }

      // Recreate schema_version and set version
      this.db.exec(SCHEMA_VERSION_SQL);
      if (currentVersion === 0) {
        this.db.run('INSERT INTO schema_version (version) VALUES (?)', CURRENT_SCHEMA_VERSION);
      } else {
        this.db.run('DELETE FROM schema_version');
        this.db.run('INSERT INTO schema_version (version) VALUES (?)', CURRENT_SCHEMA_VERSION);
      }
    }

    // Create tables (IF NOT EXISTS makes this safe to always run)
    this.db.exec(SCHEMA_SQL);
  }

  close(): void {
    if (this.opened) {
      this.db.close();
      this.opened = false;
    }
  }

  isOpen(): boolean {
    return this.opened;
  }

  getSegment<T>(key: SegmentKey): Segment<T> | null {
    const row = this.db.get<SegmentRow>('SELECT key, type, data, version, updated_at FROM segments WHERE key = ?', key);
    if (!row) return null;
    return this.rowToSegment<T>(row);
  }

  getSegments<T>(keys: SegmentKey[]): Segment<T>[] {
    if (keys.length === 0) return [];
    const placeholders = keys.map(() => '?').join(',');
    const rows = this.db.all<SegmentRow>(
      `SELECT key, type, data, version, updated_at FROM segments WHERE key IN (${placeholders})`,
      ...keys,
    );
    return rows.map((row) => this.rowToSegment<T>(row));
  }

  getSegmentsByType<T>(type: SegmentType): Segment<T>[] {
    const rows = this.db.all<SegmentRow>(
      'SELECT key, type, data, version, updated_at FROM segments WHERE type = ?',
      type,
    );
    return rows.map((row) => this.rowToSegment<T>(row));
  }

  getSegmentsByPrefix<T>(type: SegmentType, keyPrefix: string): Segment<T>[] {
    const rows = this.db.all<SegmentRow>(
      'SELECT key, type, data, version, updated_at FROM segments WHERE type = ? AND key LIKE ?',
      type,
      `${keyPrefix}%`,
    );
    return rows.map((row) => this.rowToSegment<T>(row));
  }

  getSegmentsPaginated<T>(query: PaginatedSegmentQuery): PaginatedSegmentResult<T> {
    const countRow = this.db.get<CountRow>(
      'SELECT COUNT(*) as count FROM segments WHERE type = ? AND key LIKE ?',
      query.type,
      `${query.keyPrefix}%`,
    );
    const total = countRow?.count ?? 0;
    const rows = this.db.all<SegmentRow>(
      'SELECT key, type, data, version, updated_at FROM segments WHERE type = ? AND key LIKE ? ORDER BY key LIMIT ? OFFSET ?',
      query.type,
      `${query.keyPrefix}%`,
      query.limit,
      query.offset,
    );
    return {
      segments: rows.map((row) => this.rowToSegment<T>(row)),
      total,
      offset: query.offset,
      hasMore: query.offset + rows.length < total,
    };
  }

  upsertSegment<T>(key: SegmentKey, type: SegmentType, data: T): void {
    const encoded = JSON.stringify(data);
    const now = Date.now();
    this.db.run(
      `INSERT INTO segments (key, type, data, version, updated_at) VALUES (?, ?, ?, 1, ?) ON CONFLICT(key) DO UPDATE SET type = excluded.type, data = excluded.data, version = segments.version + 1, updated_at = excluded.updated_at`,
      key,
      type,
      encoded,
      now,
    );
  }

  upsertSegmentsBatch(segments: Array<{ key: SegmentKey; type: SegmentType; data: unknown }>): void {
    if (segments.length === 0) return;
    const stmt = this.db.prepare(
      `INSERT INTO segments (key, type, data, version, updated_at) VALUES (?, ?, ?, 1, ?) ON CONFLICT(key) DO UPDATE SET type = excluded.type, data = excluded.data, version = segments.version + 1, updated_at = excluded.updated_at`,
    );
    const now = Date.now();
    this.db.transaction(() => {
      for (const seg of segments) {
        const encoded = JSON.stringify(seg.data);
        stmt.run(seg.key, seg.type, encoded, now);
      }
    });
  }

  deleteSegment(key: SegmentKey): void {
    this.db.run('DELETE FROM segments WHERE key = ?', key);
  }
  deleteSegmentsByPrefix(keyPrefix: string): void {
    this.db.run('DELETE FROM segments WHERE key LIKE ?', `${keyPrefix}%`);
  }

  countByPrefix(type: SegmentType, keyPrefix: string): number {
    const row = this.db.get<CountRow>(
      'SELECT COUNT(*) as count FROM segments WHERE type = ? AND key LIKE ?',
      type,
      `${keyPrefix}%`,
    );
    return row?.count ?? 0;
  }

  getFingerprint(filePath: string): SourceFingerprint | null {
    const row = this.db.get<SourceFileRow>(
      'SELECT path, mtime_ms, size, byte_position FROM source_files WHERE path = ?',
      filePath,
    );
    if (!row) return null;
    return this.rowToFingerprint(row);
  }

  getAllFingerprints(): SourceFingerprint[] {
    const rows = this.db.all<SourceFileRow>('SELECT path, mtime_ms, size, byte_position FROM source_files');
    return rows.map((row) => this.rowToFingerprint(row));
  }

  upsertFingerprint(fp: SourceFingerprint, segmentKeys: SegmentKey[]): void {
    this.db.run(
      `INSERT INTO source_files (path, mtime_ms, size, byte_position, segment_keys) VALUES (?, ?, ?, ?, ?) ON CONFLICT(path) DO UPDATE SET mtime_ms = excluded.mtime_ms, size = excluded.size, byte_position = excluded.byte_position, segment_keys = excluded.segment_keys`,
      fp.path,
      fp.mtimeMs,
      fp.size,
      fp.bytePosition ?? null,
      JSON.stringify(segmentKeys),
    );
  }

  deleteFingerprint(filePath: string): void {
    this.db.run('DELETE FROM source_files WHERE path = ?', filePath);
  }

  getSegmentKeysForFile(filePath: string): SegmentKey[] {
    const row = this.db.get<SourceFileRow>('SELECT segment_keys FROM source_files WHERE path = ?', filePath);
    if (!row) return [];
    try {
      return JSON.parse(row.segment_keys) as SegmentKey[];
    } catch {
      return [];
    }
  }

  indexSegment(key: SegmentKey, type: SegmentType, entry: SearchIndexEntry): void {
    this.removeFromIndex(key);
    this.db.run(
      `INSERT INTO search_index (key, type, project_slug, session_id, text_content, tags) VALUES (?, ?, ?, ?, ?, ?)`,
      key,
      type,
      entry.projectSlug ?? '',
      entry.sessionId ?? '',
      entry.textContent,
      entry.tags.join(' '),
    );
  }

  indexSegmentsBatch(
    entries: Array<{ key: SegmentKey; type: SegmentType; entry: SearchIndexEntry }>,
    options?: { skipDelete?: boolean },
  ): void {
    if (entries.length === 0) return;
    const skipDelete = options?.skipDelete ?? false;
    const deleteStmt = skipDelete ? null : this.db.prepare('DELETE FROM search_index WHERE key = ?');
    const insertStmt = this.db.prepare(
      `INSERT INTO search_index (key, type, project_slug, session_id, text_content, tags) VALUES (?, ?, ?, ?, ?, ?)`,
    );
    this.db.transaction(() => {
      for (const { key, type, entry } of entries) {
        if (deleteStmt) deleteStmt.run(key);
        insertStmt.run(
          key,
          type,
          entry.projectSlug ?? '',
          entry.sessionId ?? '',
          entry.textContent,
          entry.tags.join(' '),
        );
      }
    });
  }

  removeFromIndex(key: SegmentKey): void {
    this.db.run('DELETE FROM search_index WHERE key = ?', key);
  }

  search(query: SearchQuery): SearchResultSet {
    const limit = query.limit ?? 50;
    const offset = query.offset ?? 0;
    const matchParts: string[] = [];
    matchParts.push(`text_content : ${escapeFts5(query.text)}`);
    if (query.type) matchParts.push(`type : ${escapeFts5(query.type)}`);
    if (query.projectMembers && query.projectMembers.length > 0) {
      const slugs = [...new Set(query.projectMembers.map((member) => member.slug))];
      matchParts.push(`(${slugs.map((slug) => `project_slug : ${escapeFts5(slug)}`).join(' OR ')})`);
    } else if (query.projectSlug) {
      matchParts.push(`project_slug : ${escapeFts5(query.projectSlug)}`);
    }
    if (query.sessionId) matchParts.push(`session_id : ${escapeFts5(query.sessionId)}`);
    if (query.tags && query.tags.length > 0)
      matchParts.push(`tags : ${query.tags.map((t) => escapeFts5(t)).join(' ')}`);
    const matchExpr = matchParts.join(' AND ');
    const countRow = this.db.get<CountRow>(
      `SELECT COUNT(*) as count FROM search_index WHERE search_index MATCH ?`,
      matchExpr,
    );
    const total = countRow?.count ?? 0;
    const rows = this.db.all<SearchRow>(
      `SELECT key, type, project_slug, session_id, snippet(search_index, 4, '<b>', '</b>', '...', 64) as snippet, rank FROM search_index WHERE search_index MATCH ? ORDER BY rank LIMIT ? OFFSET ?`,
      matchExpr,
      limit,
      offset,
    );
    return {
      results: rows.map((row) => ({
        key: row.key,
        type: row.type as SegmentType,
        snippet: row.snippet,
        rank: row.rank,
        projectSlug: row.project_slug || undefined,
        sessionId: row.session_id || undefined,
      })),
      total,
      hasMore: offset + rows.length < total,
    };
  }

  vacuum(): void {
    this.db.vacuum();
  }
  getDbSizeBytes(): number {
    return this.db.getFileSize();
  }
  getSegmentCount(): number {
    const row = this.db.get<CountRow>('SELECT COUNT(*) as count FROM segments');
    return row?.count ?? 0;
  }
  getSegmentCountByType(): Record<string, number> {
    const rows = this.db.all<TypeCountRow>('SELECT type, COUNT(*) as count FROM segments GROUP BY type');
    const result: Record<string, number> = {};
    for (const row of rows) result[row.type] = row.count;
    return result;
  }
  getSearchIndexCount(): number {
    const row = this.db.get<CountRow>('SELECT COUNT(*) as count FROM search_index');
    return row?.count ?? 0;
  }

  private rowToSegment<T>(row: SegmentRow): Segment<T> {
    return {
      key: row.key,
      type: row.type as SegmentType,
      data: JSON.parse(row.data) as T,
      version: row.version,
      updatedAt: row.updated_at,
    };
  }

  private rowToFingerprint(row: SourceFileRow): SourceFingerprint {
    const fp: SourceFingerprint = { path: row.path, mtimeMs: row.mtime_ms, size: row.size };
    if (row.byte_position != null) fp.bytePosition = row.byte_position;
    return fp;
  }
}

function escapeFts5(text: string): string {
  return `"${text.replace(/"/g, '""')}"`;
}

export function createSegmentStore(sqliteServiceFactory: () => SqliteService): SegmentStore {
  return new SegmentStoreImpl(sqliteServiceFactory);
}
