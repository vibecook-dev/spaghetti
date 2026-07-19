/**
 * composite-source-pk.test.ts — the v6 `(source_id, slug)` projects PK.
 *
 * A project slug is the encoded cwd, so two sources that worked the same
 * directory derive the SAME slug. Before v6 (`slug` PK) the second source's
 * `onProject` would UPDATE the first source's row — merging two agents' projects
 * into one. This test drives two `IngestService`s (one per source) at the SAME
 * slug into one DB and asserts they stay two distinct projects with per-source
 * counts, not one merged row.
 *
 * Run with `pnpm --filter @vibecook/spaghetti-sdk test`.
 */

import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { mkdtempSync, rmSync } from 'node:fs';

import { createSqliteService } from '../../io/sqlite-service.js';
import { createIngestService } from '../ingest-service.js';
import { createQueryService } from '../query-service.js';
import { initializeSchema } from '../schema.js';
import type { SqliteService } from '../../io/index.js';
import type { IngestService } from '../ingest-service.js';
import type { SessionIndexEntry, SessionsIndex, SessionMessage } from '../../types/index.js';

const SLUG = '-repo-shared';

function sessionsIndex(sessionId: string): SessionsIndex {
  return { version: 1, originalPath: '/repo/shared', entries: [entry(sessionId)] };
}

function entry(sessionId: string): SessionIndexEntry {
  return {
    sessionId,
    fullPath: `/repo/shared/${sessionId}.jsonl`,
    fileMtime: 0,
    firstPrompt: 'hi',
    summary: '',
    messageCount: 0,
    created: '2026-07-13T00:00:00Z',
    modified: '2026-07-13T00:00:00Z',
    gitBranch: 'main',
    projectPath: '/repo/shared',
    isSidechain: false,
  };
}

function userMsg(i: number): SessionMessage {
  return {
    type: 'user',
    uuid: `u-${i}`,
    timestamp: '2026-07-13T00:00:00Z',
    message: { role: 'user', content: `message ${i}` },
  } as unknown as SessionMessage;
}

/** Ingest one source's project (same SLUG) with `msgCount` messages. */
function ingestSource(ingest: IngestService, sourceId: string, sessionId: string, msgCount: number): void {
  ingest.onProject(SLUG, '/repo/shared', sessionsIndex(sessionId));
  ingest.onSession(SLUG, entry(sessionId));
  for (let i = 0; i < msgCount; i++) {
    ingest.onMessage(SLUG, sessionId, userMsg(i), i, i * 100);
  }
  ingest.onSessionComplete(SLUG, sessionId, msgCount, msgCount * 100);
  ingest.onProjectComplete(SLUG);
}

describe('composite (source_id, slug) projects PK (schema v6)', () => {
  let tempDir: string;
  let sqlite: SqliteService;

  before(() => {
    tempDir = mkdtempSync(path.join(os.tmpdir(), 'spaghetti-composite-pk-'));
    const dbPath = path.join(tempDir, 'db.sqlite');
    sqlite = createSqliteService();
    sqlite.open({ path: dbPath });
    initializeSchema(sqlite);

    const claude = createIngestService(() => sqlite, { sourceId: 'claude-code' });
    claude.open(dbPath);
    ingestSource(claude, 'claude-code', 'claude-sess', 3);

    const codex = createIngestService(() => sqlite, { sourceId: 'codex' });
    codex.open(dbPath);
    ingestSource(codex, 'codex', 'codex-sess', 1);

    // Production boot drains dirty aggregate buckets before enabling reads.
    createQueryService(() => sqlite).prepareTokenActivity();
  });

  after(() => {
    try {
      if (sqlite.isOpen()) sqlite.close();
    } catch {
      /* ignore */
    }
    rmSync(tempDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  });

  test('the same slug from two sources is two project rows, not one merged row', () => {
    const rows = sqlite.all<{ slug: string; source_id: string }>(
      'SELECT slug, source_id FROM projects ORDER BY source_id',
    );
    assert.deepEqual(rows, [
      { slug: SLUG, source_id: 'claude-code' },
      { slug: SLUG, source_id: 'codex' },
    ]);
  });

  test('getProjectSummaries reports per-source counts, not merged', () => {
    const query = createQueryService(() => sqlite);
    const summaries = query.getProjectSummaries().filter((p) => p.slug === SLUG);
    const bySource = new Map(summaries.map((s) => [s.sourceId, s]));

    assert.equal(bySource.get('claude-code')?.sessionCount, 1);
    assert.equal(bySource.get('claude-code')?.messageCount, 3);
    assert.equal(bySource.get('codex')?.sessionCount, 1);
    assert.equal(bySource.get('codex')?.messageCount, 1);
  });

  test('getProjectSummaries filters to a single source', () => {
    const query = createQueryService(() => sqlite);
    const codexOnly = query.getProjectSummaries({ sourceId: 'codex' }).filter((p) => p.slug === SLUG);
    assert.equal(codexOnly.length, 1);
    assert.equal(codexOnly[0].messageCount, 1);
  });

  test('getSessionSummaries without sourceId unions both agents on a shared slug', () => {
    const query = createQueryService(() => sqlite);
    const all = query.getSessionSummaries(SLUG);
    assert.equal(all.length, 2);
    assert.deepEqual(all.map((s) => s.sourceId).sort(), ['claude-code', 'codex']);
  });

  test('getSessionSummaries with sourceId returns only that agent', () => {
    const query = createQueryService(() => sqlite);
    const codex = query.getSessionSummaries(SLUG, { sourceId: 'codex' });
    assert.equal(codex.length, 1);
    assert.equal(codex[0].sessionId, 'codex-sess');
    assert.equal(codex[0].sourceId, 'codex');
    assert.equal(codex[0].messageCount, 1);

    const claude = query.getSessionSummaries(SLUG, { sourceId: 'claude-code' });
    assert.equal(claude.length, 1);
    assert.equal(claude[0].sessionId, 'claude-sess');
    assert.equal(claude[0].messageCount, 3);
  });

  test('getProjectMemory is null for non-claude sources on a shared slug', () => {
    const query = createQueryService(() => sqlite);
    assert.equal(query.getProjectMemory(SLUG, { sourceId: 'codex' }), null);
  });
});
