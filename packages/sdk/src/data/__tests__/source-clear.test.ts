/**
 * source-clear.test.ts — what `clearSourceData` may and may not delete.
 *
 * Seven artifact tables carry no `source_id`: project_memories, workflows,
 * tool_results, todos, tasks, plans, file_history. They cannot be cleared
 * selectively, so the rule is all-or-nothing *and only for their sole writer*,
 * Claude Code.
 *
 * That rule rests on a property of the readers, not of the schema — the Codex
 * and Grok readers emit only project/session/message/fingerprint events and
 * never the artifact events these tables are written from. If that ever stops
 * being true, these tests are what should fail: clearing them for Codex would
 * delete Claude's artifacts, and skipping them for a second artifact writer
 * would strand rows forever.
 *
 * Run with `pnpm --filter @vibecook/spaghetti-sdk test`.
 */

import { test, describe, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { mkdtempSync, rmSync } from 'node:fs';

import { createSqliteService } from '../../io/sqlite-service.js';
import { createIngestService } from '../ingest-service.js';
import { initializeSchema } from '../schema.js';
import { isSourceContractCurrent, markSourceContractCurrent } from '../ingest-contract.js';
import type { SqliteService } from '../../io/index.js';
import type { IngestService } from '../ingest-service.js';

/** The seven tables with no `source_id` column. */
const ARTIFACT_TABLES = [
  'project_memories',
  'workflows',
  'tool_results',
  'todos',
  'tasks',
  'plans',
  'file_history',
] as const;

let tmpDir: string;
let dbPath: string;
let db: SqliteService;

function serviceFor(sourceId: string): IngestService {
  const svc = createIngestService(() => db, { sourceId });
  svc.open(dbPath);
  return svc;
}

/** Seed one row into every artifact table, plus a scoped row per source. */
function seed(): void {
  db.run('INSERT INTO project_memories (project_slug, content, updated_at) VALUES (?, ?, ?)', 'p', 'mem', 1);
  db.run(
    'INSERT INTO workflows (project_slug, session_id, workflow_id, name, status, agent_count, total_tokens, total_tool_calls, duration_ms, subagent_count, data, journal, updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)',
    'p',
    's',
    'wf_1',
    'n',
    'done',
    0,
    0,
    0,
    0,
    0,
    '{}',
    '[]',
    1,
  );
  db.run(
    'INSERT INTO tool_results (project_slug, session_id, tool_use_id, content, updated_at) VALUES (?,?,?,?,?)',
    'p',
    's',
    't1',
    'out',
    1,
  );
  db.run('INSERT INTO todos (session_id, agent_id, items, updated_at) VALUES (?,?,?,?)', 's', 'a', '[]', 1);
  db.run(
    'INSERT INTO tasks (session_id, has_highwatermark, highwatermark, lock_exists, updated_at) VALUES (?,?,?,?,?)',
    's',
    0,
    '',
    0,
    1,
  );
  db.run('INSERT INTO plans (slug, title, content, size, updated_at) VALUES (?,?,?,?,?)', 'pl', 'T', 'C', 1, 1);
  db.run('INSERT INTO file_history (session_id, data, updated_at) VALUES (?,?,?)', 's', '{}', 1);

  for (const sourceId of ['claude-code', 'codex', 'grok']) {
    db.run(
      'INSERT INTO projects (slug, source_id, original_path, sessions_index, updated_at) VALUES (?,?,?,?,?)',
      `proj-${sourceId}`,
      sourceId,
      '/x',
      '{}',
      1,
    );
  }
}

function artifactCounts(): Record<string, number> {
  const out: Record<string, number> = {};
  for (const t of ARTIFACT_TABLES) {
    out[t] = db.get<{ count: number }>(`SELECT COUNT(*) AS count FROM ${t}`)?.count ?? -1;
  }
  return out;
}

function projectCount(sourceId: string): number {
  return db.get<{ count: number }>('SELECT COUNT(*) AS count FROM projects WHERE source_id = ?', sourceId)?.count ?? -1;
}

beforeEach(() => {
  tmpDir = mkdtempSync(path.join(os.tmpdir(), 'spag-clear-'));
  dbPath = path.join(tmpDir, 'index.db');
  db = createSqliteService();
  db.open({ path: dbPath });
  initializeSchema(db);
  seed();
});

afterEach(() => {
  try {
    db.close();
  } catch {
    /* already closed */
  }
  rmSync(tmpDir, { recursive: true, force: true });
});

describe('clearSourceData — artifact table ownership', () => {
  test('clearing claude-code empties every artifact table', () => {
    serviceFor('claude-code').clearSourceData();

    for (const [table, count] of Object.entries(artifactCounts())) {
      assert.equal(count, 0, `${table} should be empty after a claude-code clear`);
    }
  });

  test('clearing codex leaves every artifact table untouched', () => {
    const before = artifactCounts();

    serviceFor('codex').clearSourceData();

    assert.deepEqual(artifactCounts(), before, "codex must not delete Claude's artifacts");
  });

  test('clearing grok leaves every artifact table untouched', () => {
    const before = artifactCounts();

    serviceFor('grok').clearSourceData();

    assert.deepEqual(artifactCounts(), before, "grok must not delete Claude's artifacts");
  });

  test('every artifact table was actually seeded — otherwise the tests above prove nothing', () => {
    for (const [table, count] of Object.entries(artifactCounts())) {
      assert.equal(count, 1, `${table} seed missing; the ownership assertions would pass vacuously`);
    }
  });
});

describe('clearSourceData — scoped rows', () => {
  test('a clear drops only the calling source rows', () => {
    serviceFor('codex').clearSourceData();

    assert.equal(projectCount('codex'), 0);
    assert.equal(projectCount('claude-code'), 1);
    assert.equal(projectCount('grok'), 1);
  });
});

describe('clearSourceData — contract marker', () => {
  test('a clear invalidates the marker for that source only', () => {
    markSourceContractCurrent(db, 'claude-code');
    markSourceContractCurrent(db, 'codex');

    serviceFor('claude-code').clearSourceData();

    assert.equal(isSourceContractCurrent(db, 'claude-code'), false, 'a cleared source is not repaired');
    assert.equal(isSourceContractCurrent(db, 'codex'), true, "codex's marker survives");
  });

  test('the marker is dropped in the same transaction as the rows', () => {
    // Not a rollback test — SqliteService.transaction owns that — but it does
    // pin that the invalidation is inside clearSourceData rather than a
    // separate call a caller could forget.
    markSourceContractCurrent(db, 'grok');

    serviceFor('grok').clearSourceData();

    assert.equal(isSourceContractCurrent(db, 'grok'), false);
    assert.equal(projectCount('grok'), 0);
  });
});
