import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { createSqliteService } from '../../io/sqlite-service.js';
import { initializeSchema, SCHEMA_SQL, SCHEMA_VERSION } from '../schema.js';

test('current schema rebuilds stale stores with bounded unknown evidence', () => {
  const directory = mkdtempSync(join(tmpdir(), 'spaghetti-schema-migration-'));
  const sqlite = createSqliteService();
  try {
    sqlite.open({ path: join(directory, 'cache.db') });
    sqlite.exec(SCHEMA_SQL);
    sqlite.run(`INSERT INTO schema_meta (key, value) VALUES ('version', ?)`, String(SCHEMA_VERSION - 1));
    sqlite.run(
      `INSERT INTO projects (slug, original_path, sessions_index, updated_at) VALUES (?, ?, ?, ?)`,
      'preserved',
      '/tmp/preserved',
      '[]',
      456,
    );

    initializeSchema(sqlite);

    assert.equal(
      sqlite.get<{ value: string }>(`SELECT value FROM schema_meta WHERE key = 'version'`)?.value,
      String(SCHEMA_VERSION),
    );
    assert.equal(
      sqlite.get<{ count: number }>(`SELECT COUNT(*) AS count FROM projects WHERE slug = 'preserved'`)?.count,
      0,
    );
    assert.equal(
      sqlite.get<{ count: number }>(
        `SELECT COUNT(*) AS count FROM pragma_table_info('run_evidence') WHERE name IN ('evidence_count', 'last_activity_at')`,
      )?.count,
      2,
    );
    assert.equal(
      sqlite.get<{ count: number }>(
        `SELECT COUNT(*) AS count FROM sqlite_master WHERE type = 'table' AND name = 'unknown_native_evidence'`,
      )?.count,
      1,
    );
    assert.equal(
      sqlite.get<{ count: number }>(
        `SELECT COUNT(*) AS count FROM sqlite_master WHERE type = 'index' AND name = 'idx_unknown_native_evidence_object_generation'`,
      )?.count,
      1,
    );
  } finally {
    if (sqlite.isOpen()) sqlite.close();
    rmSync(directory, { recursive: true, force: true });
  }
});
