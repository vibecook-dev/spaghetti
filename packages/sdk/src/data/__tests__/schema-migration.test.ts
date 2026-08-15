import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { createSqliteService } from '../../io/sqlite-service.js';
import { initializeSchema, SCHEMA_SQL, SCHEMA_VERSION } from '../schema.js';

test('v41 adds the message content codec without discarding the cache', () => {
  const directory = mkdtempSync(join(tmpdir(), 'spaghetti-schema-migration-'));
  const sqlite = createSqliteService();
  try {
    sqlite.open({ path: join(directory, 'cache.db') });
    const v41Schema = SCHEMA_SQL.replace("  content_json_codec TEXT NOT NULL DEFAULT 'identity',\n", '');
    assert.notEqual(v41Schema, SCHEMA_SQL, 'v41 fixture did not remove the v42 column');
    sqlite.exec(v41Schema);
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
      1,
    );
    const codec = sqlite.getTableInfo('canonical_messages').find((column) => column.name === 'content_json_codec');
    assert.equal(codec?.dflt_value, "'identity'");
  } finally {
    if (sqlite.isOpen()) sqlite.close();
    rmSync(directory, { recursive: true, force: true });
  }
});
