/**
 * AgentDataStore — unit tests (RFC 005 C1.1 + C1.2).
 *
 * Proves the new store works in isolation against a prepared SQLite
 * file, without needing the parser, ingest-service, or any ~/.claude
 * layout. Run with `pnpm --filter @vibecook/spaghetti-sdk test`.
 */

import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { mkdtempSync, rmSync } from 'node:fs';

import { createSqliteService } from '../../io/sqlite-service.js';
import { createQueryService } from '../query-service.js';
import { createAgentDataStore } from '../agent-data-store.js';
import { initializeSchema } from '../schema.js';
import type { SqliteService } from '../../io/index.js';
import type { QueryService } from '../query-service.js';
import type { AgentDataStore } from '../agent-data-store.js';
import type { AgentAnalytic, AgentConfig } from '../../types/index.js';

// ═══════════════════════════════════════════════════════════════════════════
// FIXTURE CONSTANTS
// ═══════════════════════════════════════════════════════════════════════════

const SLUG = 'test-project';
const SESSION_ID = 'session-abc-123';
const MSG_TEXT = 'hello world spaghetti fixture';
const MEMORY_TEXT = '# Fixture memory\nThis project lives in a unit test.';
const TOOL_USE_ID = 'toolu_test_1';
const TOOL_RESULT_TEXT = 'tool result body';

// ═══════════════════════════════════════════════════════════════════════════
// TEST SUITE
// ═══════════════════════════════════════════════════════════════════════════

describe('AgentDataStore (C1.1 skeleton)', () => {
  let tempDir: string;
  let dbPath: string;
  let sqlite: SqliteService;
  let queryService: QueryService;
  let store: AgentDataStore;

  before(() => {
    // Use an on-disk temp DB — `better-sqlite3` supports `:memory:` but
    // `SqliteServiceImpl.open()` calls `mkdirSync(dirname(path))` so an
    // honest file keeps the scaffolding identical to production.
    tempDir = mkdtempSync(path.join(os.tmpdir(), 'spaghetti-store-test-'));
    dbPath = path.join(tempDir, 'store.db');

    sqlite = createSqliteService();
    sqlite.open({ path: dbPath });
    initializeSchema(sqlite);

    // Seed the minimal rows each read path below will hit. All inserts
    // are raw SQL so the test doesn't depend on IngestService plumbing.
    sqlite.run(
      `INSERT INTO projects (slug, original_path, sessions_index, updated_at) VALUES (?, ?, ?, ?)`,
      SLUG,
      '/tmp/fake/original/path',
      JSON.stringify({ sessions: [] }),
      Date.now(),
    );

    sqlite.run(
      `INSERT INTO project_memories (project_slug, content, updated_at) VALUES (?, ?, ?)`,
      SLUG,
      MEMORY_TEXT,
      Date.now(),
    );

    sqlite.run(
      `INSERT INTO sessions (id, project_slug, full_path, first_prompt, summary, git_branch, project_path, is_sidechain, created_at, modified_at, file_mtime, plan_slug, has_task, updated_at)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      SESSION_ID,
      SLUG,
      `/tmp/fake/original/path/${SESSION_ID}.jsonl`,
      'What is spaghetti?',
      'Fixture session',
      'main',
      '/tmp/fake/original/path',
      0,
      '2026-04-20T00:00:00Z',
      '2026-04-20T00:05:00Z',
      Date.now(),
      null,
      0,
      Date.now(),
    );

    const message = {
      type: 'user',
      uuid: 'uuid-msg-0',
      timestamp: '2026-04-20T00:00:01Z',
      message: { role: 'user', content: MSG_TEXT },
    };

    sqlite.run(
      `INSERT INTO messages (project_slug, session_id, msg_index, msg_type, uuid, timestamp, data, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, text_content, byte_offset)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      SLUG,
      SESSION_ID,
      0,
      'user',
      'uuid-msg-0',
      '2026-04-20T00:00:01Z',
      JSON.stringify(message),
      0,
      0,
      0,
      0,
      MSG_TEXT,
      0,
    );

    sqlite.run(
      `INSERT INTO subagents
         (source_id, project_slug, session_id, agent_id, agent_type, file_name, message_count, workflow_id, updated_at)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      'claude-code',
      SLUG,
      SESSION_ID,
      'agent-0001',
      'general',
      'agent-0001.jsonl',
      1,
      '',
      Date.now(),
    );
    sqlite.run(
      `INSERT INTO subagent_messages
         (source_id, project_slug, session_id, workflow_id, agent_id, msg_index, timestamp, data)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
      'claude-code',
      SLUG,
      SESSION_ID,
      '',
      'agent-0001',
      0,
      message.timestamp,
      JSON.stringify(message),
    );

    sqlite.run(
      `INSERT INTO tool_results (project_slug, session_id, tool_use_id, content, updated_at) VALUES (?, ?, ?, ?, ?)`,
      SLUG,
      SESSION_ID,
      TOOL_USE_ID,
      TOOL_RESULT_TEXT,
      Date.now(),
    );

    sqlite.run(
      `INSERT INTO todos (session_id, agent_id, items, updated_at) VALUES (?, ?, ?, ?)`,
      SESSION_ID,
      'agent-0001',
      JSON.stringify([{ content: 'fixture todo', status: 'pending' }]),
      Date.now(),
    );

    // The store's query path for `getSessionMessages` counts from `messages`
    // + joins, but search goes through the FTS5 triggers — which our INSERT
    // above already fires. No extra seeding needed for search.

    queryService = createQueryService(() => sqlite);
    // QueryService.open() is a no-op when the shared SqliteService is
    // already open; it just runs `initializeSchema` again (idempotent).
    queryService.open(dbPath);
    // Production boot materializes ingestion-owned summaries before exposing
    // query APIs; this raw-SQL fixture mirrors that lifecycle boundary.
    queryService.prepareTokenActivity();

    store = createAgentDataStore(queryService);
  });

  after(() => {
    try {
      sqlite.close();
    } catch {
      /* ignore */
    }
    rmSync(tempDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  });

  test('getProjectSlugs returns the seeded slug', () => {
    const slugs = store.getProjectSlugs();
    assert.deepStrictEqual(slugs, [SLUG]);
  });

  test('getProjectSummaries returns a row for the seeded project', () => {
    const summaries = store.getProjectSummaries();
    assert.strictEqual(summaries.length, 1);
    assert.strictEqual(summaries[0].slug, SLUG);
    assert.strictEqual(summaries[0].sessionCount, 1);
    assert.strictEqual(summaries[0].messageCount, 1);
    assert.strictEqual(summaries[0].hasMemory, true);
  });

  test('getSessionSummaries returns the seeded session', () => {
    const sessions = store.getSessionSummaries(SLUG);
    assert.strictEqual(sessions.length, 1);
    assert.strictEqual(sessions[0].sessionId, SESSION_ID);
    assert.strictEqual(sessions[0].messageCount, 1);
    assert.strictEqual(sessions[0].firstPrompt, 'What is spaghetti?');
  });

  test('getSessionMessages returns the seeded message row', () => {
    const page = store.getSessionMessages(SLUG, SESSION_ID, 10, 0);
    assert.strictEqual(page.total, 1);
    assert.strictEqual(page.offset, 0);
    assert.strictEqual(page.hasMore, false);
    assert.strictEqual(page.messages.length, 1);

    const msg = page.messages[0] as Record<string, unknown>;
    assert.strictEqual(msg.type, 'user');
    assert.strictEqual(msg.uuid, 'uuid-msg-0');
  });

  test('getSessionSubagents returns the seeded agent metadata', () => {
    const agents = store.getSessionSubagents(SLUG, SESSION_ID);
    assert.strictEqual(agents.length, 1);
    assert.deepStrictEqual(agents[0], {
      sourceId: 'claude-code',
      agentId: 'agent-0001',
      agentType: 'general',
      messageCount: 1,
      workflowId: '',
      spawnToolId: null,
      linkMethod: 'unlinked',
    });
  });

  test('getSubagentMessages returns the inlined message list', () => {
    const page = store.getSubagentMessages(SLUG, SESSION_ID, 'agent-0001', 10, 0);
    assert.strictEqual(page.total, 1);
    assert.strictEqual(page.messages.length, 1);
    const msg = page.messages[0] as Record<string, unknown>;
    assert.strictEqual(msg.uuid, 'uuid-msg-0');
  });

  test('getProjectMemory returns the seeded memory body', () => {
    assert.strictEqual(store.getProjectMemory(SLUG), MEMORY_TEXT);
  });

  test('getSessionTodos returns the seeded todo payload', () => {
    const todos = store.getSessionTodos(SLUG, SESSION_ID);
    assert.strictEqual(todos.length, 1);
  });

  test('getSessionPlan returns null when no plan is seeded', () => {
    assert.strictEqual(store.getSessionPlan(SLUG, SESSION_ID), null);
  });

  test('getSessionTask returns null when no task row exists', () => {
    assert.strictEqual(store.getSessionTask(SLUG, SESSION_ID), null);
  });

  test('getToolResult returns the seeded tool-result content', () => {
    assert.strictEqual(store.getToolResult(SLUG, SESSION_ID, TOOL_USE_ID), TOOL_RESULT_TEXT);
  });

  test('search finds the fixture message text via FTS', () => {
    const result = store.search({ text: 'spaghetti' });
    assert.ok(result.total > 0, `Expected at least one FTS hit, got total=${result.total}`);
    const first = result.results[0];
    assert.strictEqual(first.projectSlug, SLUG);
    assert.strictEqual(first.sessionId, SESSION_ID);
  });

  // ── C1.2: config + analytics cache ─────────────────────────────────────

  test('setConfig → getConfig roundtrip preserves the snapshot', () => {
    // The shapes are big aggregate types; casting a minimal object
    // through `unknown` keeps the test focused on store semantics
    // rather than on whether AgentConfig's sub-shapes are perfectly
    // filled in.
    const fakeConfig = { __fixture: 'config-v1' } as unknown as AgentConfig;
    store.setConfig(fakeConfig);
    assert.strictEqual(store.hasConfig(), true);
    assert.strictEqual(store.getConfig(), fakeConfig);
  });

  test('setAnalytics → getAnalytics roundtrip preserves the snapshot', () => {
    const fakeAnalytics = { __fixture: 'analytics-v1' } as unknown as AgentAnalytic;
    store.setAnalytics(fakeAnalytics);
    assert.strictEqual(store.hasAnalytics(), true);
    assert.strictEqual(store.getAnalytics(), fakeAnalytics);
  });

  test('setConfig overwrites a previous value', () => {
    const v1 = { __fixture: 'config-v1' } as unknown as AgentConfig;
    const v2 = { __fixture: 'config-v2' } as unknown as AgentConfig;
    store.setConfig(v1);
    store.setConfig(v2);
    assert.strictEqual(store.getConfig(), v2);
  });

  test('getConfig throws when no config has been set (fresh store)', () => {
    // Build a second store that never had `setConfig` called so we can
    // verify the "empty" contract without disturbing the shared fixture
    // store used by the other tests.
    const freshStore = createAgentDataStore(queryService);
    assert.strictEqual(freshStore.hasConfig(), false);
    assert.throws(() => freshStore.getConfig(), /config not set/);
    assert.strictEqual(freshStore.hasAnalytics(), false);
    assert.throws(() => freshStore.getAnalytics(), /analytics not set/);
  });

  // ─────────────────────────────────────────────────────────────────────────
  // Subscriber registry (RFC 005 C3.1)
  // ─────────────────────────────────────────────────────────────────────────
  //
  // These cover the store's fan-out contract: topic matching at three
  // granularities, dispose idempotency, listener-error isolation,
  // monotonic `lastEmittedSeq` regardless of listener count, and
  // throttle `{ latest: true }` / `{ latest: false }` behaviours.
  //
  // Fresh stores are constructed per test so registry state from one
  // case can't leak into the next.

  function mkMessageChange(slug: string, sessionId: string, tag: string): Parameters<typeof store.emit>[0] {
    return {
      type: 'session.message.added',
      seq: 0,
      ts: Date.now(),
      slug,
      sessionId,
      message: { __tag: tag } as unknown,
      byteOffset: 0,
    } as Parameters<typeof store.emit>[0];
  }

  test('firehose subscriber receives every emitted Change', () => {
    const freshStore = createAgentDataStore(queryService);
    const received: Array<Parameters<typeof store.emit>[0]> = [];
    const dispose = freshStore.subscribe(undefined, (e) => received.push(e));

    freshStore.emit(mkMessageChange('s1', 'x', 'a'));
    freshStore.emit(mkMessageChange('s2', 'y', 'b'));
    freshStore.emit({
      type: 'todo.updated',
      seq: 0,
      ts: Date.now(),
      sessionId: 'z',
      agentId: 'ag',
      items: [],
    } as Parameters<typeof store.emit>[0]);

    assert.equal(received.length, 3);
    dispose();
  });

  test('subscribe({kind:"session", slug:"s1"}) scopes to that slug only', () => {
    const freshStore = createAgentDataStore(queryService);
    const received: string[] = [];
    freshStore.subscribe({ kind: 'session', slug: 's1' }, (e) => {
      if (e.type === 'session.message.added') received.push(e.slug + '/' + e.sessionId);
    });

    freshStore.emit(mkMessageChange('s1', 'A', 'a'));
    freshStore.emit(mkMessageChange('s1', 'B', 'b'));
    freshStore.emit(mkMessageChange('s2', 'C', 'c')); // wrong slug
    freshStore.emit({
      type: 'todo.updated',
      seq: 0,
      ts: Date.now(),
      sessionId: 's1',
      agentId: 'z',
      items: [],
    } as Parameters<typeof store.emit>[0]); // wrong kind

    assert.deepEqual(received, ['s1/A', 's1/B']);
  });

  test('subscribe with sessionId filters to that specific session', () => {
    const freshStore = createAgentDataStore(queryService);
    const received: string[] = [];
    freshStore.subscribe({ kind: 'session', slug: 's1', sessionId: 'A' }, (e) => {
      if (e.type === 'session.message.added') received.push(e.sessionId);
    });

    freshStore.emit(mkMessageChange('s1', 'A', 'a1'));
    freshStore.emit(mkMessageChange('s1', 'A', 'a2'));
    freshStore.emit(mkMessageChange('s1', 'B', 'b')); // sibling session
    freshStore.emit(mkMessageChange('s2', 'A', 'a-wrong-slug')); // different slug

    assert.deepEqual(received, ['A', 'A']);
  });

  test('dispose returned from subscribe actually unsubscribes', () => {
    const freshStore = createAgentDataStore(queryService);
    let hits = 0;
    const dispose = freshStore.subscribe(undefined, () => {
      hits++;
    });
    freshStore.emit(mkMessageChange('s1', 'A', 'a'));
    assert.equal(hits, 1);

    dispose();
    freshStore.emit(mkMessageChange('s1', 'A', 'b'));
    assert.equal(hits, 1, 'dispose should stop further deliveries');

    // Idempotent.
    assert.doesNotThrow(() => dispose());
  });

  test('lastEmittedSeq() increments monotonically regardless of listener presence', () => {
    const freshStore = createAgentDataStore(queryService);
    assert.equal(freshStore.lastEmittedSeq(), 0);
    freshStore.emit(mkMessageChange('s1', 'A', 'a'));
    assert.equal(freshStore.lastEmittedSeq(), 1);
    // No listener attached — still bumps.
    freshStore.emit(mkMessageChange('s1', 'A', 'b'));
    assert.equal(freshStore.lastEmittedSeq(), 2);

    // With a listener now.
    freshStore.subscribe(undefined, () => {});
    freshStore.emit(mkMessageChange('s1', 'A', 'c'));
    assert.equal(freshStore.lastEmittedSeq(), 3);
  });

  test('emit stamps seq onto the delivered Change', () => {
    const freshStore = createAgentDataStore(queryService);
    const seen: number[] = [];
    freshStore.subscribe(undefined, (e) => seen.push(e.seq));
    freshStore.emit(mkMessageChange('s1', 'A', 'a'));
    freshStore.emit(mkMessageChange('s1', 'A', 'b'));
    freshStore.emit(mkMessageChange('s1', 'A', 'c'));
    assert.deepEqual(seen, [1, 2, 3]);
  });

  test('listener throwing does not kill subsequent listeners', () => {
    const freshStore = createAgentDataStore(queryService);
    let tail = 0;
    freshStore.subscribe(undefined, () => {
      throw new Error('intentional');
    });
    freshStore.subscribe(undefined, () => {
      tail++;
    });
    // Both listeners share the firehose; the first throws but the
    // second must still fire.
    assert.doesNotThrow(() => freshStore.emit(mkMessageChange('s1', 'A', 'a')));
    assert.equal(tail, 1);
  });

  test('throttleMs + latest:true delivers at most one event per window', async () => {
    const freshStore = createAgentDataStore(queryService);
    const seen: string[] = [];
    freshStore.subscribe(
      undefined,
      (e) => {
        if (e.type === 'session.message.added') {
          const msg = e.message as unknown as { __tag: string };
          seen.push(msg.__tag);
        }
      },
      { throttleMs: 30 },
    );

    freshStore.emit(mkMessageChange('s1', 'A', 'a'));
    freshStore.emit(mkMessageChange('s1', 'A', 'b'));
    freshStore.emit(mkMessageChange('s1', 'A', 'c'));

    // Give the throttle timer enough runway to fire. Two windows (3×
    // throttleMs) is comfortably deterministic on CI.
    await new Promise((r) => setTimeout(r, 90));

    // Only the most recent pending change ('c') should have landed.
    assert.deepEqual(seen, ['c']);
  });

  test('throttleMs + latest:false coalesces events into an array', async () => {
    const freshStore = createAgentDataStore(queryService);
    const batches: string[][] = [];
    // The overloaded subscribe pins the listener to (e: Change[]) => void
    // when `{ throttleMs, latest: false }` is passed — no cast needed.
    freshStore.subscribe(
      undefined,
      (batch) => {
        const tags = batch.map((e) => {
          if (e.type === 'session.message.added') {
            return (e.message as unknown as { __tag: string }).__tag;
          }
          return '?';
        });
        batches.push(tags);
      },
      { throttleMs: 30, latest: false },
    );

    freshStore.emit(mkMessageChange('s1', 'A', 'a'));
    freshStore.emit(mkMessageChange('s1', 'A', 'b'));
    freshStore.emit(mkMessageChange('s1', 'A', 'c'));

    await new Promise((r) => setTimeout(r, 90));

    // All three events should arrive in one batched invocation.
    assert.equal(batches.length, 1);
    assert.deepEqual(batches[0], ['a', 'b', 'c']);
  });
});
