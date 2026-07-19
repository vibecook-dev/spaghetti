import { after, before, describe, test } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { mkdtempSync, rmSync } from 'node:fs';

import { createSqliteService } from '../../io/sqlite-service.js';
import type { SqliteService } from '../../io/index.js';
import { createQueryService, type QueryService } from '../query-service.js';
import { initializeSchema } from '../schema.js';

const SOURCE = 'claude-code';
const SLUG = 'branch-project';
const SESSION = 'branch-session';
const AGENT = 'agent-branch-001';
const SPAWN_TOOL = 'task-tool-001';

describe('normalized subagent transcript queries', () => {
  let dir: string;
  let sqlite: SqliteService;
  let query: QueryService;

  before(() => {
    dir = mkdtempSync(path.join(os.tmpdir(), 'spaghetti-subagent-test-'));
    const dbPath = path.join(dir, 'subagents.db');
    sqlite = createSqliteService();
    sqlite.open({ path: dbPath });
    initializeSchema(sqlite);
    sqlite.run('INSERT INTO projects (slug, source_id, original_path) VALUES (?, ?, ?)', SLUG, SOURCE, '/tmp/branch');
    sqlite.run('INSERT INTO sessions (id, source_id, project_slug) VALUES (?, ?, ?)', SESSION, SOURCE, SLUG);

    const parentRows = [
      {
        type: 'assistant',
        uuid: 'parent-task',
        timestamp: '2026-07-01T00:00:00Z',
        sessionId: SESSION,
        message: {
          role: 'assistant',
          content: [
            {
              type: 'tool_use',
              id: SPAWN_TOOL,
              name: 'Task',
              input: { description: 'Research branch', prompt: 'Inspect the implementation' },
            },
          ],
        },
      },
      {
        type: 'user',
        uuid: 'parent-task-result',
        timestamp: '2026-07-01T00:00:01Z',
        sessionId: SESSION,
        message: {
          role: 'user',
          content: [
            {
              type: 'tool_result',
              tool_use_id: SPAWN_TOOL,
              content: `Agent completed successfully: ${AGENT}`,
            },
          ],
        },
      },
    ];
    for (let index = 0; index < parentRows.length; index++) {
      sqlite.run(
        `INSERT INTO messages (source_id, project_slug, session_id, msg_index, msg_type, data)
         VALUES (?, ?, ?, ?, ?, ?)`,
        SOURCE,
        SLUG,
        SESSION,
        index,
        parentRows[index]!.type,
        JSON.stringify(parentRows[index]),
      );
    }

    sqlite.run(
      `INSERT INTO subagents
         (source_id, project_slug, session_id, agent_id, agent_type, file_name, message_count, workflow_id)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
      SOURCE,
      SLUG,
      SESSION,
      AGENT,
      'Explore',
      `agent-${AGENT}.jsonl`,
      3,
      '',
    );
    const branchRows = [
      {
        type: 'user',
        uuid: 'injected-prompt',
        timestamp: '2026-07-01T00:00:00Z',
        sessionId: SESSION,
        isSidechain: true,
        message: { role: 'user', content: 'Inspect the implementation' },
      },
      {
        type: 'assistant',
        uuid: 'branch-assistant',
        timestamp: '2026-07-01T00:00:02Z',
        sessionId: SESSION,
        isSidechain: true,
        message: {
          role: 'assistant',
          content: [
            { type: 'text', text: 'A uniquely searchable branch finding' },
            { type: 'tool_use', id: 'branch-bash', name: 'Bash', input: { command: 'pwd' } },
          ],
        },
      },
      {
        type: 'user',
        uuid: 'branch-result',
        timestamp: '2026-07-01T00:00:03Z',
        sessionId: SESSION,
        isSidechain: true,
        message: {
          role: 'user',
          content: [{ type: 'tool_result', tool_use_id: 'branch-bash', content: '/tmp/branch' }],
        },
      },
    ];
    for (let index = 0; index < branchRows.length; index++) {
      sqlite.run(
        `INSERT INTO subagent_messages
           (source_id, project_slug, session_id, workflow_id, agent_id, msg_index, timestamp, data)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
        SOURCE,
        SLUG,
        SESSION,
        '',
        AGENT,
        index,
        branchRows[index]!.timestamp,
        JSON.stringify(branchRows[index]),
      );
    }

    query = createQueryService(() => sqlite);
    query.open(dbPath);
  });

  after(() => {
    sqlite.close();
    rmSync(dir, { recursive: true, force: true });
  });

  test('links the sidecar to its Task call without relying on file order', () => {
    const threads = query.getSessionSubagents(SLUG, SESSION, { sourceId: SOURCE });
    assert.equal(threads.length, 1);
    assert.equal(threads[0]?.spawnToolId, SPAWN_TOOL);
    assert.equal(threads[0]?.linkMethod, 'tool_result');
  });

  test('normalizes branch rows and merges their tool results', () => {
    const page = query.getSubagentTimeline(SLUG, SESSION, AGENT, { sourceId: SOURCE, limit: 20 });
    assert.equal(page.total, 2, 'the injected sidechain prompt is not a displayed user message');
    assert.deepEqual(
      page.messages.map((message) => message.type),
      ['assistant', 'tool_use'],
    );
    assert.equal(page.messages[1]?.toolUse?.result?.content, '/tmp/branch');
    assert.equal(
      page.messages.every((message) => message.agentId === AGENT && message.isSidechain),
      true,
    );
    assert.equal(new Set(page.messages.map((message) => message.timelineId)).size, 2);
  });

  test('facets include normalized branch types and tools', () => {
    const facets = query.getSessionTimelineFacets(SLUG, SESSION, { sourceId: SOURCE });
    assert.equal(facets.total, 3);
    assert.deepEqual(facets.messageCounts, { assistant: 1, tool_use: 2 });
    assert.deepEqual(facets.toolCounts, { Bash: 1, Task: 1 });
  });

  test('DB filters retain the parent anchor when only its branch matches', () => {
    const page = query.getSessionTimeline(SLUG, SESSION, {
      sourceId: SOURCE,
      includeTypes: ['assistant'],
      search: 'uniquely searchable',
    });
    assert.equal(page.total, 1);
    assert.equal(page.messages[0]?.toolUse?.toolId, SPAWN_TOOL);

    const branch = query.getSubagentTimeline(SLUG, SESSION, AGENT, {
      sourceId: SOURCE,
      includeTypes: ['assistant'],
      search: 'uniquely searchable',
    });
    assert.equal(branch.total, 1);
    assert.match(branch.messages[0]?.content ?? '', /uniquely searchable/);
  });

  test('global FTS returns navigable normalized subagent hits', () => {
    const result = query.search({ text: 'uniquely searchable' });
    assert.equal(result.total, 1);
    assert.equal(result.results[0]?.type, 'subagent');
    assert.equal(result.results[0]?.agentId, AGENT);
    assert.equal(result.results[0]?.spawnToolId, undefined);
    assert.equal(result.results[0]?.agentTimelineIndex, 0);
  });

  test('raw transcript API remains available for lossless inspection', () => {
    const page = query.getSubagentMessages(SLUG, SESSION, AGENT, 10, 0, '', { sourceId: SOURCE });
    assert.equal(page.total, 3);
    assert.equal((page.messages[0] as Record<string, unknown>).uuid, 'injected-prompt');
  });
});
