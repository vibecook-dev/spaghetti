import { after, before, describe, test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { createSqliteService, type SqliteService } from '../../io/index.js';
import { initializeSchema } from '../schema.js';
import { readTokenActivity, rebuildDirtyTokenActivity } from '../token-activity.js';

const SLUG = 'activity-project';

describe('daily token activity', () => {
  let directory: string;
  let db: SqliteService;

  before(() => {
    directory = mkdtempSync(path.join(os.tmpdir(), 'spaghetti-token-activity-'));
    db = createSqliteService();
    db.open({ path: path.join(directory, 'activity.db') });
    initializeSchema(db);

    for (const [sourceId, sessionId, estimated] of [
      ['claude-code', 'claude-session', 0],
      ['codex', 'codex-session', 0],
      ['grok', 'grok-session', 1],
    ] as const) {
      db.run(
        'INSERT INTO sessions(id, source_id, project_slug, tokens_estimated) VALUES (?, ?, ?, ?)',
        sessionId,
        sourceId,
        SLUG,
        estimated,
      );
    }

    db.run(
      `INSERT INTO messages
        (source_id, project_slug, session_id, msg_index, timestamp, input_tokens, output_tokens,
         cache_creation_tokens, cache_read_tokens, data)
       VALUES ('claude-code', ?, 'claude-session', 0, '2026-07-18T12:00:00Z', 10, 2, 3, 5, '{}')`,
      SLUG,
    );
    db.run(
      `INSERT INTO subagent_messages
        (source_id, project_slug, session_id, workflow_id, agent_id, msg_index, timestamp,
         input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, data)
       VALUES ('claude-code', ?, 'claude-session', '', 'agent-1', 0, '2026-07-18T12:01:00Z', 4, 6, 1, 2, '{}')`,
      SLUG,
    );
    db.run(
      `INSERT INTO messages
        (source_id, project_slug, session_id, msg_index, timestamp, input_tokens, output_tokens,
         cache_creation_tokens, cache_read_tokens, data)
       VALUES ('codex', ?, 'codex-session', 0, '2026-07-18T13:00:00Z', 100, 25, 0, 40, '{}')`,
      SLUG,
    );
    db.run(
      `INSERT INTO messages
        (source_id, project_slug, session_id, msg_index, timestamp, input_tokens, data)
       VALUES ('grok', ?, 'grok-session', 0, '2026-07-19T13:00:00Z', 50, '{}')`,
      SLUG,
    );
  });

  after(() => {
    db.close();
    rmSync(directory, { recursive: true, force: true });
  });

  test('normalizes Codex cache semantics and includes subagent tokens', () => {
    rebuildDirtyTokenActivity(db);
    const rows = readTokenActivity(db, SLUG, { from: '2026-07-18', to: '2026-07-19' });
    const claude = rows.find((row) => row.sourceId === 'claude-code');
    const codex = rows.find((row) => row.sourceId === 'codex');
    const grok = rows.find((row) => row.sourceId === 'grok');

    assert.equal(claude?.tokenUsage.totalTokens, 33);
    assert.equal(claude?.messageCount, 2);
    assert.equal(codex?.tokenUsage.totalTokens, 125, 'cached Codex input is a subset, not an additive bucket');
    assert.equal(codex?.tokenUsage.cacheReadTokens, 40, 'the cache breakdown remains available');
    assert.equal(grok?.estimatedTokens, 50);
    assert.equal(grok?.exactTokens, 0);
  });

  test('year-range reads use the compact rollup index instead of canonical messages', () => {
    const plan = db.all<{ detail: string }>(
      `EXPLAIN QUERY PLAN
       SELECT * FROM token_activity_daily
        WHERE project_slug = ? AND activity_day >= ? AND activity_day <= ?`,
      SLUG,
      '2025-07-18',
      '2026-07-19',
    );
    assert.ok(plan.some((step) => step.detail.includes('idx_token_activity_project_day')));
    assert.ok(plan.every((step) => !step.detail.includes('messages')));
  });

  test('a project read leaves unrelated live-update buckets deferred', () => {
    db.run(
      `INSERT INTO sessions(id, source_id, project_slug) VALUES ('other-session', 'claude-code', 'other-project')`,
    );
    db.run(
      `INSERT INTO messages
        (source_id, project_slug, session_id, msg_index, timestamp, input_tokens, data)
       VALUES ('claude-code', 'other-project', 'other-session', 0, '2026-07-18T14:00:00Z', 9, '{}')`,
    );

    readTokenActivity(db, SLUG, { from: '2026-07-18', to: '2026-07-19' });

    assert.equal(
      db.get<{ count: number }>(
        `SELECT COUNT(*) AS count FROM token_activity_dirty WHERE project_slug = 'other-project'`,
      )?.count,
      1,
    );
  });

  test('token and timestamp changes rebuild both affected days without cumulative drift', () => {
    db.run(
      `UPDATE messages SET input_tokens = 20, timestamp = '2026-07-19T12:00:00Z'
        WHERE source_id = 'claude-code' AND session_id = 'claude-session' AND msg_index = 0`,
    );
    const rows = readTokenActivity(db, SLUG, {
      sourceId: 'claude-code',
      from: '2026-07-18',
      to: '2026-07-19',
    });
    assert.deepEqual(
      rows.map((row) => [row.date, row.tokenUsage.totalTokens]),
      [
        ['2026-07-18', 13],
        ['2026-07-19', 30],
      ],
    );
    assert.equal(
      db.get<{ count: number }>('SELECT COUNT(*) AS count FROM token_activity_dirty WHERE project_slug = ?', SLUG)
        ?.count,
      0,
    );
  });

  test('self-heals an empty derived table even when no dirty markers remain', () => {
    db.run('DELETE FROM token_activity_daily');
    db.run('DELETE FROM token_activity_dirty');

    const rows = readTokenActivity(db, SLUG, { from: '2026-07-18', to: '2026-07-19' });

    assert.ok(rows.length > 0);
    assert.ok((db.get<{ count: number }>('SELECT COUNT(*) AS count FROM token_activity_daily')?.count ?? 0) > 0);
  });
});
