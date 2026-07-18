import { after, before, describe, test } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { mkdtempSync, rmSync } from 'node:fs';

import { createSqliteService } from '../../io/sqlite-service.js';
import type { SqliteService } from '../../io/index.js';
import { createQueryService, type QueryService } from '../query-service.js';
import { initializeSchema } from '../schema.js';

const SLUG = 'timeline-project';
const SESSION = 'timeline-session';

describe('normalized timeline DB queries', () => {
  let dir: string;
  let dbPath: string;
  let sqlite: SqliteService;
  let query: QueryService;

  before(() => {
    dir = mkdtempSync(path.join(os.tmpdir(), 'spaghetti-timeline-test-'));
    dbPath = path.join(dir, 'timeline.db');
    sqlite = createSqliteService();
    sqlite.open({ path: dbPath });
    initializeSchema(sqlite);
    sqlite.run(
      'INSERT INTO projects (slug, source_id, original_path) VALUES (?, ?, ?)',
      SLUG,
      'claude-code',
      '/tmp/timeline-project',
    );
    sqlite.run('INSERT INTO sessions (id, source_id, project_slug) VALUES (?, ?, ?)', SESSION, 'claude-code', SLUG);

    const rows = [
      {
        type: 'user',
        uuid: 'user-1',
        timestamp: '2026-01-01T00:00:00Z',
        sessionId: SESSION,
        message: { role: 'user', content: 'the old user row' },
      },
      {
        type: 'assistant',
        uuid: 'assistant-1',
        timestamp: '2026-01-01T00:00:01Z',
        sessionId: SESSION,
        message: {
          role: 'assistant',
          content: [
            { type: 'thinking', thinking: 'consider the fixture' },
            { type: 'text', text: 'first assistant response' },
            { type: 'tool_use', id: 'tool-1', name: 'Bash', input: { command: 'pwd' } },
          ],
        },
      },
      {
        type: 'user',
        uuid: 'result-1',
        timestamp: '2026-01-01T00:00:02Z',
        sessionId: SESSION,
        message: {
          role: 'user',
          content: [{ type: 'tool_result', tool_use_id: 'tool-1', content: '/tmp/timeline-project' }],
        },
      },
      {
        type: 'assistant',
        uuid: 'assistant-2',
        timestamp: '2026-01-01T00:00:03Z',
        sessionId: SESSION,
        message: { role: 'assistant', content: [{ type: 'text', text: 'newest assistant response' }] },
      },
    ];
    rows.forEach((data, index) => {
      sqlite.run(
        `INSERT INTO messages
           (source_id, project_slug, session_id, msg_index, msg_type, data)
         VALUES (?, ?, ?, ?, ?, ?)`,
        'claude-code',
        SLUG,
        SESSION,
        index,
        data.type,
        JSON.stringify(data),
      );
    });

    query = createQueryService(() => sqlite);
    query.open(dbPath);
  });

  after(() => {
    sqlite.close();
    rmSync(dir, { recursive: true, force: true });
  });

  test('facets count the full normalized session, including split assistant parts', () => {
    const facets = query.getSessionTimelineFacets(SLUG, SESSION, { sourceId: 'claude-code' });
    assert.equal(facets.total, 5);
    assert.deepEqual(facets.messageCounts, { assistant: 2, thinking: 1, tool_use: 1, user: 1 });
    assert.deepEqual(facets.toolCounts, { Bash: 1 });
  });

  test('solo query finds an old type absent from the loaded newest page', () => {
    const newest = query.getSessionTimeline(SLUG, SESSION, { sourceId: 'claude-code', limit: 2 });
    assert.equal(
      newest.messages.some((message) => message.type === 'user'),
      false,
    );

    const users = query.getSessionTimeline(SLUG, SESSION, {
      sourceId: 'claude-code',
      includeTypes: ['user'],
      limit: 2,
    });
    assert.equal(users.total, 1);
    assert.equal(users.messages[0]?.type, 'user');
    assert.equal(users.messages[0]?.content, 'the old user row');
  });

  test('tool filters return the normalized call with its merged result', () => {
    const tools = query.getSessionTimeline(SLUG, SESSION, {
      sourceId: 'claude-code',
      includeTools: ['Bash'],
    });
    assert.equal(tools.total, 1);
    assert.equal(tools.messages[0]?.toolUse?.result?.content, '/tmp/timeline-project');
  });

  test('search and cursor pagination operate on normalized display rows', () => {
    const search = query.getSessionTimeline(SLUG, SESSION, { search: 'NEWEST ASSISTANT' });
    assert.equal(search.total, 1);
    assert.equal(search.messages[0]?.uuid, 'assistant-2');

    const first = query.getSessionTimeline(SLUG, SESSION, { limit: 2 });
    assert.equal(first.messages.length, 2);
    assert.equal(first.hasMore, true);
    assert.notEqual(first.nextCursor, undefined);
    const second = query.getSessionTimeline(SLUG, SESSION, { limit: 2, before: first.nextCursor });
    assert.equal(second.messages.length, 2);
    assert.equal(second.hasMore, true);
    assert.equal(new Set([...first.messages, ...second.messages].map((message) => message.uuid)).size, 4);
  });

  test('raw updates mark the projection dirty and refresh the next query', () => {
    const updated = {
      type: 'assistant',
      uuid: 'assistant-2',
      sessionId: SESSION,
      message: { role: 'assistant', content: [{ type: 'text', text: 'rewritten tail' }] },
    };
    sqlite.run('UPDATE messages SET data = ? WHERE session_id = ? AND msg_index = 3', JSON.stringify(updated), SESSION);

    assert.equal(query.getSessionTimeline(SLUG, SESSION, { search: 'newest assistant' }).total, 0);
    assert.equal(query.getSessionTimeline(SLUG, SESSION, { search: 'rewritten tail' }).total, 1);
  });
});
