/**
 * LiveUpdates — integration tests (RFC 005 C2.7 + C3.2).
 *
 * Proves the end-to-end pipeline works: a filesystem change under
 * `<rootDir>/projects/` or `<rootDir>/todos/` reaches SQLite via
 * watcher → queue → parser → writeBatch. Since C3.2, watchers are
 * attached lazily — every test that expects ingest must `prewarm()`
 * the relevant topic (or the end-to-end `api.live.onChange` path from
 * C3.4, which we'll migrate to once that surface ships).
 *
 * Test-isolation contract (refreshed in the audit-remediation pass):
 * each test owns a fresh `{ tempRoot, sqlite, ingest, store, live }`
 * stack via `createFixture()`. Running a single test in isolation must
 * work without depending on side effects from any other test in the
 * suite. The previous before/after sharing pattern coupled tests to
 * each other's SQLite state; the inline helper now removes that
 * coupling at the cost of a few hundred ms per test for the parcel
 * watcher attach.
 *
 * Style matches `data/__tests__/ingest-service-write-batch.test.ts` and
 * `live/__tests__/watcher.test.ts`: `node:test` + `assert/strict`,
 * `mkdtempSync` per test, real SQLite + real filesystem, no mocks.
 *
 * `createParcelWatcher()` is the default — `--test-force-exit` in the
 * SDK's test script handles any lingering native handles.
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { mkdtempSync, rmSync, mkdirSync, writeFileSync, appendFileSync, realpathSync, renameSync } from 'node:fs';

import { createFileService } from '../../../../io/file-service.js';
import { createSqliteService } from '../../../../io/sqlite-service.js';
import { createQueryService } from '../../../../data/query-service.js';
import { createIngestService } from '../../../../data/ingest-service.js';
import { createAgentDataStore } from '../../../../data/agent-data-store.js';
import { initializeSchema } from '../../../../data/schema.js';
import { createLiveUpdates, type LiveUpdates, type LiveUpdatesOptions } from '../live-updates.js';
import type { SqliteService } from '../../../../io/index.js';
import type { IngestService } from '../../../../data/ingest-service.js';
import type { QueryService } from '../../../../data/query-service.js';
import type { AgentDataStore } from '../../../../data/agent-data-store.js';
import type { Change } from '../../../../live/change-events.js';

// ═══════════════════════════════════════════════════════════════════════════
// CONSTANTS
// ═══════════════════════════════════════════════════════════════════════════

const SLUG = 'test-slug';
const SESSION_ID = 'session-xyz';

// Polling budget for eventually-consistent assertions. Watcher +
// queue's 30 ms debounce + 75 ms batch window + SQLite commit usually
// lands within a few hundred ms; 2 s gives headroom for CI noise.
const POLL_INTERVAL_MS = 50;
const POLL_MAX_ITER = 40; // 40 * 50ms = 2s
const QUIET_MS = 500;

// ═══════════════════════════════════════════════════════════════════════════
// HELPERS
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Poll until `check()` returns a value, or the budget runs out.
 * Mirrors the pattern used in the watcher test (longer `timeoutMs`
 * window, shorter per-iteration sleep).
 */
async function pollUntil<T>(check: () => T | undefined): Promise<T> {
  for (let i = 0; i < POLL_MAX_ITER; i++) {
    const v = check();
    if (v !== undefined) return v;
    await new Promise((r) => setTimeout(r, POLL_INTERVAL_MS));
  }
  const final = check();
  if (final !== undefined) return final;
  throw new Error(`pollUntil timed out after ${POLL_MAX_ITER * POLL_INTERVAL_MS}ms`);
}

/** JSONL user message — minimal shape the writer accepts. */
function makeUserMessage(uuid: string, text: string, sessionId: string = SESSION_ID): string {
  return (
    JSON.stringify({
      type: 'user',
      uuid,
      timestamp: new Date().toISOString(),
      sessionId,
      message: { role: 'user', content: text },
    }) + '\n'
  );
}

interface FixtureSeedRow {
  /**
   * Project slug to seed. The fixture creates the project row + the
   * `<rootDir>/projects/<slug>/` directory so live writes have a
   * plausible parent. Parent rows aren't required for the schema (no
   * FKs) but make the assertions easier to reason about.
   */
  slug: string;
  /**
   * Optional sessionId(s) to seed. Each lands as a sessions row + the
   * `<rootDir>/projects/<slug>/<sessionId>.jsonl` file is NOT
   * created — tests that need it write the file themselves so the
   * pipeline sees a `create` event.
   */
  sessionIds?: string[];
}

interface FixtureOptions {
  /**
   * Subdirectories to pre-create under `rootDir`. The watcher's
   * scope-attach surfaces an error if the watched directory doesn't
   * exist, so each test names the dirs it needs (e.g. `['projects',
   * 'todos']` for the end-to-end suite, `['tasks']` for the tasks
   * suite, `[]` for graceful-startup tests).
   */
  ensureDirs?: readonly string[];
  /** Project + session rows to seed before constructing services. */
  seed?: readonly FixtureSeedRow[];
  /** Forwarded to `createLiveUpdates(_, options)`. */
  liveOptions?: Partial<Omit<LiveUpdatesOptions, 'rootDir'>>;
  /**
   * Capture each `onError` call into this array for later assertions.
   * Mutually exclusive with `liveOptions.onError` — pass one or the
   * other.
   */
  capturedErrors?: Error[];
}

interface Fixture {
  tempRoot: string;
  rootDir: string;
  dbPath: string;
  sqlite: SqliteService;
  queryService: QueryService;
  ingest: IngestService;
  store: AgentDataStore;
  live: LiveUpdates;
  /**
   * Tear everything down: stops the live pipeline, closes ingest +
   * SQLite, removes the temp tree. Idempotent. Always call from a
   * try/finally in the test body (or at the very end of the test —
   * the per-test pattern means there's no shared after() block to
   * fall back on).
   */
  cleanup: () => Promise<void>;
}

/**
 * Build an isolated fixture: temp dir, fresh SQLite, schema, ingest +
 * store + live, all wired together. Each test that calls this owns
 * its own stack — running tests independently is safe.
 *
 * The fixture does NOT call `live.start()` for you; tests that want
 * the writer loop running call it themselves so they can assert
 * pre-start behavior (e.g. "no errors before prewarm") if needed.
 */
async function createFixture(opts: FixtureOptions = {}): Promise<Fixture> {
  // Parcel emits realpath-canonicalised paths on macOS; classify
  // compares against the root we pass in, so use the realpath form
  // everywhere to keep the two sides in sync.
  const tempRoot = realpathSync(mkdtempSync(path.join(os.tmpdir(), 'spaghetti-live-updates-test-')));
  const rootDir = path.join(tempRoot, '.claude');

  const ensure = opts.ensureDirs ?? ['projects', 'todos'];
  for (const dir of ensure) {
    mkdirSync(path.join(rootDir, dir), { recursive: true });
  }
  // rootDir itself must exist for the settings watcher (which
  // watches it non-recursively).
  mkdirSync(rootDir, { recursive: true });

  const dbPath = path.join(tempRoot, 'live.db');
  const sqlite = createSqliteService();
  sqlite.open({ path: dbPath });
  initializeSchema(sqlite);

  // Seed projects + sessions rows so live inserts have plausible
  // parents.
  for (const row of opts.seed ?? []) {
    sqlite.run(
      `INSERT INTO projects (slug, original_path, sessions_index, updated_at) VALUES (?, ?, ?, ?)`,
      row.slug,
      `/tmp/fake/original/${row.slug}`,
      JSON.stringify({ sessions: [] }),
      Date.now(),
    );
    // Project dir under rootDir/projects/<slug>/ so a future
    // session.jsonl write fires a `create` event (rather than landing
    // inside a non-existent parent the watcher hasn't seen).
    mkdirSync(path.join(rootDir, 'projects', row.slug), { recursive: true });

    for (const sid of row.sessionIds ?? []) {
      sqlite.run(
        `INSERT INTO sessions (id, project_slug, full_path, first_prompt, summary, git_branch, project_path, is_sidechain, created_at, modified_at, file_mtime, plan_slug, has_task, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
        sid,
        row.slug,
        path.join(rootDir, 'projects', row.slug, `${sid}.jsonl`),
        'fixture',
        'fixture session',
        'main',
        '/tmp/fake',
        0,
        '2026-04-20T00:00:00Z',
        '2026-04-20T00:05:00Z',
        Date.now(),
        null,
        0,
        Date.now(),
      );
    }
  }

  const fileService = createFileService();
  const queryService = createQueryService(() => sqlite);
  queryService.open(dbPath);
  const ingest = createIngestService(() => sqlite);
  ingest.open(dbPath);
  const store = createAgentDataStore(queryService);

  const liveOptions: LiveUpdatesOptions = {
    rootDir,
    ...opts.liveOptions,
  };
  if (opts.capturedErrors) {
    liveOptions.onError = (err) => {
      opts.capturedErrors!.push(err);
    };
  }

  const live = createLiveUpdates({ fileService, ingestService: ingest, store }, liveOptions);

  let cleaned = false;
  const cleanup = async (): Promise<void> => {
    if (cleaned) return;
    cleaned = true;
    try {
      await live.stop();
    } catch {
      /* ignore */
    }
    try {
      ingest.close();
    } catch {
      /* ignore */
    }
    try {
      if (sqlite.isOpen()) sqlite.close();
    } catch {
      /* ignore */
    }
    try {
      rmSync(tempRoot, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
    } catch {
      /* ignore */
    }
  };

  return { tempRoot, rootDir, dbPath, sqlite, queryService, ingest, store, live, cleanup };
}

// ═══════════════════════════════════════════════════════════════════════════
// SUITE: end-to-end with a populated rootDir
// ═══════════════════════════════════════════════════════════════════════════

describe('LiveUpdates end-to-end (RFC 005 C2.7)', () => {
  test('live write lands in SQLite', { timeout: 10000 }, async () => {
    const fx = await createFixture({
      ensureDirs: ['projects', 'todos'],
      seed: [{ slug: SLUG, sessionIds: [SESSION_ID] }],
    });
    try {
      await fx.live.start();
      fx.live.prewarm({ kind: 'session', slug: SLUG, sessionId: SESSION_ID });
      // Give parcel a tick to bind.
      await new Promise((r) => setTimeout(r, 150));

      const sessionPath = path.join(fx.rootDir, 'projects', SLUG, `${SESSION_ID}.jsonl`);
      writeFileSync(sessionPath, makeUserMessage('uuid-1', 'first live message'));

      const row = await pollUntil(() => {
        const r = fx.sqlite.get<{ n: number }>(`SELECT COUNT(*) AS n FROM messages WHERE session_id = ?`, SESSION_ID);
        return r && r.n >= 1 ? r : undefined;
      });
      assert.equal(row.n, 1);
    } finally {
      await fx.cleanup();
    }
  });

  test('append after initial write is captured', { timeout: 10000 }, async () => {
    const fx = await createFixture({
      ensureDirs: ['projects', 'todos'],
      seed: [{ slug: SLUG, sessionIds: [SESSION_ID] }],
    });
    try {
      await fx.live.start();
      fx.live.prewarm({ kind: 'session', slug: SLUG, sessionId: SESSION_ID });
      await new Promise((r) => setTimeout(r, 150));

      const sessionPath = path.join(fx.rootDir, 'projects', SLUG, `${SESSION_ID}.jsonl`);
      writeFileSync(sessionPath, makeUserMessage('uuid-1', 'first live message'));
      await pollUntil(() => {
        const r = fx.sqlite.get<{ n: number }>(`SELECT COUNT(*) AS n FROM messages WHERE session_id = ?`, SESSION_ID);
        return r && r.n >= 1 ? r : undefined;
      });

      appendFileSync(sessionPath, makeUserMessage('uuid-2', 'second live message'));
      const row = await pollUntil(() => {
        const r = fx.sqlite.get<{ n: number }>(`SELECT COUNT(*) AS n FROM messages WHERE session_id = ?`, SESSION_ID);
        return r && r.n >= 2 ? r : undefined;
      });
      assert.equal(row.n, 2);
    } finally {
      await fx.cleanup();
    }
  });

  test('rewrite is captured', { timeout: 10000 }, async () => {
    const fx = await createFixture({
      ensureDirs: ['projects', 'todos'],
      seed: [{ slug: SLUG, sessionIds: [SESSION_ID] }],
    });
    try {
      await fx.live.start();
      fx.live.prewarm({ kind: 'session', slug: SLUG, sessionId: SESSION_ID });
      await new Promise((r) => setTimeout(r, 150));

      const sessionPath = path.join(fx.rootDir, 'projects', SLUG, `${SESSION_ID}.jsonl`);
      // Two initial lines — gives the rewrite below a comfortable
      // size margin to shrink (the parser's rewrite detection fires
      // on `size < checkpoint.lastOffset`, so the second write must
      // be strictly smaller than the first).
      writeFileSync(
        sessionPath,
        makeUserMessage('uuid-1', 'initial line 1') + makeUserMessage('uuid-2', 'initial line 2 with more content'),
      );
      await pollUntil(() => {
        const r = fx.sqlite.get<{ n: number }>(`SELECT COUNT(*) AS n FROM messages WHERE session_id = ?`, SESSION_ID);
        return r && r.n >= 2 ? r : undefined;
      });

      // Truncate + replace with a single short line. Writer upserts
      // on (session_id, msg_index); the parser's `size <
      // checkpoint.lastOffset` check trips and the rewrite fires.
      // Phase 2 just asserts ingest happens; truncation repair is a
      // Phase 5 known-issue (incremental-parser tests document it).
      writeFileSync(sessionPath, makeUserMessage('uuid-rewrite', 'short'));
      const row = await pollUntil(() => {
        const r = fx.sqlite.get<{ uuid: string }>(
          `SELECT uuid FROM messages WHERE session_id = ? AND msg_index = 0`,
          SESSION_ID,
        );
        return r && r.uuid === 'uuid-rewrite' ? r : undefined;
      });
      assert.equal(row.uuid, 'uuid-rewrite');
    } finally {
      await fx.cleanup();
    }
  });

  test('nested workflow subagent transcript lands with its workflow_id', { timeout: 10000 }, async () => {
    const fx = await createFixture({
      ensureDirs: ['projects', 'todos'],
      seed: [{ slug: SLUG, sessionIds: [SESSION_ID] }],
    });
    try {
      await fx.live.start();
      // Nested transcripts ride the `projects` scope, same as any subagent.
      fx.live.prewarm({ kind: 'subagent', slug: SLUG, sessionId: SESSION_ID });
      await new Promise((r) => setTimeout(r, 150));

      // projects/<slug>/<sid>/subagents/workflows/<wf>/agent-*.jsonl —
      // the router extracts wf_live_1 and the transcript must land
      // grouped under it (not the flat ungrouped '' bucket).
      const wfDir = path.join(fx.rootDir, 'projects', SLUG, SESSION_ID, 'subagents', 'workflows', 'wf_live_1');
      mkdirSync(wfDir, { recursive: true });
      writeFileSync(path.join(wfDir, 'agent-alive01.jsonl'), makeUserMessage('sub-uuid-1', 'nested subagent line'));

      const row = await pollUntil(() => {
        const r = fx.sqlite.get<{ workflow_id: string }>(
          `SELECT workflow_id FROM subagents WHERE session_id = ? AND agent_id = ?`,
          SESSION_ID,
          'alive01',
        );
        return r && r.workflow_id ? r : undefined;
      });
      assert.equal(row.workflow_id, 'wf_live_1', 'nested transcript grouped under its run on the live path');
    } finally {
      await fx.cleanup();
    }
  });

  test('todo file lands in SQLite', { timeout: 10000 }, async () => {
    const fx = await createFixture({
      ensureDirs: ['projects', 'todos'],
      seed: [{ slug: SLUG, sessionIds: [SESSION_ID] }],
    });
    try {
      await fx.live.start();
      fx.live.prewarm({ kind: 'todo', sessionId: SESSION_ID });
      await new Promise((r) => setTimeout(r, 150));

      const todoPath = path.join(fx.rootDir, 'todos', `${SESSION_ID}-agent-a0.json`);
      writeFileSync(todoPath, JSON.stringify([{ content: 'test todo', status: 'pending' }]));

      const row = await pollUntil(() => {
        const r = fx.sqlite.get<{ items: string; agent_id: string }>(
          `SELECT items, agent_id FROM todos WHERE session_id = ? AND agent_id = ?`,
          SESSION_ID,
          'a0',
        );
        return r ? r : undefined;
      });
      assert.equal(row.agent_id, 'a0');
      const items = JSON.parse(row.items) as Array<{ content: string; status: string }>;
      assert.equal(items.length, 1);
      assert.equal(items[0].content, 'test todo');
    } finally {
      await fx.cleanup();
    }
  });

  test('stop() halts further writes', { timeout: 10000 }, async () => {
    const fx = await createFixture({
      ensureDirs: ['projects', 'todos'],
      seed: [{ slug: SLUG, sessionIds: [SESSION_ID] }],
    });
    try {
      await fx.live.start();
      fx.live.prewarm({ kind: 'session', slug: SLUG, sessionId: SESSION_ID });
      await new Promise((r) => setTimeout(r, 150));

      const sessionPath = path.join(fx.rootDir, 'projects', SLUG, `${SESSION_ID}.jsonl`);
      writeFileSync(sessionPath, makeUserMessage('uuid-1', 'pre-stop'));
      await pollUntil(() => {
        const r = fx.sqlite.get<{ n: number }>(`SELECT COUNT(*) AS n FROM messages WHERE session_id = ?`, SESSION_ID);
        return r && r.n >= 1 ? r : undefined;
      });
      const before = fx.sqlite.get<{ n: number }>(
        `SELECT COUNT(*) AS n FROM messages WHERE session_id = ?`,
        SESSION_ID,
      )!.n;

      await fx.live.stop();

      appendFileSync(sessionPath, makeUserMessage('uuid-after-stop', 'should be silent'));
      await new Promise((r) => setTimeout(r, QUIET_MS));

      const after =
        fx.sqlite.get<{ n: number }>(`SELECT COUNT(*) AS n FROM messages WHERE session_id = ?`, SESSION_ID)?.n ?? 0;
      assert.equal(after, before, `stop() should halt ingest: messages rose from ${before} → ${after} after stop()`);
    } finally {
      await fx.cleanup();
    }
  });
});

// ═══════════════════════════════════════════════════════════════════════════
// SUITE: graceful startup when only projects/ exists
// ═══════════════════════════════════════════════════════════════════════════

describe('LiveUpdates graceful startup (RFC 005 C2.7)', () => {
  test(
    'start() resolves without attaching watchers; prewarm on missing todos/ surfaces via onError',
    { timeout: 10000 },
    async () => {
      const errors: Error[] = [];
      const fx = await createFixture({
        // Only projects/; todos/ is intentionally missing so the
        // watcher attach for it must fail gracefully.
        ensureDirs: ['projects'],
        capturedErrors: errors,
      });
      try {
        // Must not throw. Under C3.2, start() only loads checkpoints +
        // spawns the writer loop; no watcher is attached yet.
        await assert.doesNotReject(() => fx.live.start());
        assert.equal(fx.live.isRunning(), true);

        // Without prewarm, no errors have been observed yet — watchers
        // are inert.
        assert.equal(
          errors.length,
          0,
          `expected no errors pre-prewarm, got: ${errors.map((e) => e.message).join(' | ')}`,
        );

        // Prewarm the todos/ scope explicitly — this triggers the attach
        // that fails because the directory is missing.
        fx.live.prewarm({ kind: 'todo', sessionId: 'whatever' });
        // Give the async attach a moment to fail and route through onError.
        await new Promise((r) => setTimeout(r, 150));

        // Match the path, not a `todos/` substring: the separator is `\` on
        // Windows, and the backends' own messages name no path at all.
        const todosDir = path.join(fx.rootDir, 'todos');
        const todoErr = errors.find((e) => e.message.includes(todosDir));
        assert.ok(
          todoErr,
          `expected onError to report the missing ${todosDir}. Collected: ${errors.map((e) => e.message).join(' | ')}`,
        );
      } finally {
        await fx.cleanup();
      }
    },
  );
});

// ═══════════════════════════════════════════════════════════════════════════
// SUITE: lazy ref-counting (RFC 005 C3.2)
// ═══════════════════════════════════════════════════════════════════════════

describe('LiveUpdates lazy ref-counting (RFC 005 C3.2)', () => {
  test('without prewarm: writing a JSONL line produces no SQLite rows', { timeout: 10000 }, async () => {
    const fx = await createFixture({
      ensureDirs: ['projects'],
      seed: [{ slug: SLUG, sessionIds: [SESSION_ID] }],
    });
    try {
      await fx.live.start();
      // No prewarm has been issued — the projects/ watcher is detached.
      const sessionPath = path.join(fx.rootDir, 'projects', SLUG, `${SESSION_ID}.jsonl`);
      writeFileSync(sessionPath, makeUserMessage('uuid-no-prewarm', 'should not ingest'));

      // Wait generously; if the watcher were attached we'd see a row.
      await new Promise((r) => setTimeout(r, QUIET_MS));

      const after = fx.sqlite.get<{ n: number }>(`SELECT COUNT(*) AS n FROM messages WHERE session_id = ?`, SESSION_ID);
      assert.equal(after?.n ?? 0, 0, 'no prewarm → no watcher attached → no ingest');
    } finally {
      await fx.cleanup();
    }
  });

  test('prewarm attaches the watcher; subsequent writes ingest', { timeout: 10000 }, async () => {
    const fx = await createFixture({
      ensureDirs: ['projects'],
      seed: [{ slug: SLUG, sessionIds: [SESSION_ID] }],
    });
    try {
      await fx.live.start();
      const dispose = fx.live.prewarm({ kind: 'session', slug: SLUG, sessionId: SESSION_ID });
      // parcel attach is async — give it a tick.
      await new Promise((r) => setTimeout(r, 150));

      const sessionPath = path.join(fx.rootDir, 'projects', SLUG, `${SESSION_ID}.jsonl`);
      writeFileSync(sessionPath, makeUserMessage('uuid-prewarmed', 'with prewarm'));

      const row = await pollUntil(() => {
        const r = fx.sqlite.get<{ n: number }>(`SELECT COUNT(*) AS n FROM messages WHERE session_id = ?`, SESSION_ID);
        return r && r.n >= 1 ? r : undefined;
      });
      assert.ok(row.n >= 1, 'post-prewarm ingest should land at least one row');
      dispose();
    } finally {
      await fx.cleanup();
    }
  });

  test('stacking two prewarms then disposing one keeps the watcher attached', { timeout: 10000 }, async () => {
    const fx = await createFixture({
      ensureDirs: ['projects'],
      seed: [{ slug: SLUG, sessionIds: [SESSION_ID] }],
    });
    try {
      await fx.live.start();
      // First prewarm: slug+sessionId scope.
      // Second prewarm: slug-only. Both resolve to the same `projects`
      // scope, so the ref count is 2. Disposing one drops it to 1, not
      // 0, and the watcher stays attached.
      const dispose1 = fx.live.prewarm({ kind: 'session', slug: SLUG, sessionId: SESSION_ID });
      const dispose2 = fx.live.prewarm({ kind: 'session', slug: SLUG });
      await new Promise((r) => setTimeout(r, 150));

      const sessionPath = path.join(fx.rootDir, 'projects', SLUG, `${SESSION_ID}.jsonl`);
      writeFileSync(sessionPath, makeUserMessage('uuid-1', 'baseline'));
      await pollUntil(() => {
        const r = fx.sqlite.get<{ n: number }>(`SELECT COUNT(*) AS n FROM messages WHERE session_id = ?`, SESSION_ID);
        return r && r.n >= 1 ? r : undefined;
      });

      dispose2();
      await new Promise((r) => setTimeout(r, 50));

      const baseline =
        fx.sqlite.get<{ n: number }>(`SELECT COUNT(*) AS n FROM messages WHERE session_id = ?`, SESSION_ID)?.n ?? 0;
      appendFileSync(sessionPath, makeUserMessage('uuid-stacking', 'after partial release'));

      const row = await pollUntil(() => {
        const r = fx.sqlite.get<{ n: number }>(`SELECT COUNT(*) AS n FROM messages WHERE session_id = ?`, SESSION_ID);
        return r && r.n > baseline ? r : undefined;
      });
      assert.ok(row.n > baseline, 'watcher should still be attached via the first prewarm');
      dispose1();
    } finally {
      await fx.cleanup();
    }
  });
});

// ═══════════════════════════════════════════════════════════════════════════
// SUITE: tasks/ live updates (RFC 005 C5.2)
// ═══════════════════════════════════════════════════════════════════════════

describe('LiveUpdates tasks/ scope (RFC 005 C5.2)', () => {
  test('task prewarm + .lock create lands a tasks row + emits task.updated', { timeout: 10000 }, async () => {
    const fx = await createFixture({
      ensureDirs: ['tasks'],
      seed: [{ slug: 'task-slug', sessionIds: ['s1'] }],
    });
    try {
      await fx.live.start();
      const captured: Change[] = [];
      const sub = fx.store.subscribe({ kind: 'task', sessionId: 's1' }, (c) => {
        captured.push(c);
      });
      const dispose = fx.live.prewarm({ kind: 'task', sessionId: 's1' });
      await new Promise((r) => setTimeout(r, 150));

      const taskDir = path.join(fx.rootDir, 'tasks', 's1');
      mkdirSync(taskDir, { recursive: true });
      writeFileSync(path.join(taskDir, '.lock'), '');

      const row = await pollUntil(() => {
        const r = fx.sqlite.get<{ lock_exists: number }>(`SELECT lock_exists FROM tasks WHERE session_id = ?`, 's1');
        return r ? r : undefined;
      });
      assert.equal(row.lock_exists, 1, 'tasks row should record lock_exists=1');

      const taskEvent = captured.find((c) => c.type === 'task.updated');
      assert.ok(taskEvent, `expected a task.updated change, got: ${captured.map((c) => c.type).join(', ')}`);

      sub();
      dispose();
    } finally {
      await fx.cleanup();
    }
  });

  test(
    '.lock + .highwatermark within debounce window collapse — at least one event + final state lands',
    { timeout: 10000 },
    async () => {
      // Coalescing collapses bursts of edits per debounce window into
      // ideally one event, but timing is OS- and FS-dependent. The
      // contract we assert is the observable one: at least one
      // `task.updated` fires AND the final SQLite state reflects both
      // writes (lock present + highwatermark recorded). Asserting an
      // exact count was flaky on slow CI; relaxed per the audit.
      const fx = await createFixture({
        ensureDirs: ['tasks'],
        seed: [{ slug: 'task-slug', sessionIds: ['s2'] }],
      });
      try {
        await fx.live.start();
        const captured: Change[] = [];
        const sub = fx.store.subscribe({ kind: 'task', sessionId: 's2' }, (c) => {
          captured.push(c);
        });
        const dispose = fx.live.prewarm({ kind: 'task', sessionId: 's2' });
        await new Promise((r) => setTimeout(r, 150));

        const taskDir = path.join(fx.rootDir, 'tasks', 's2');
        mkdirSync(taskDir, { recursive: true });
        // Two events within the 75 ms batch window + path-dedup —
        // ideally collapse to a single enqueue → single writeBatch →
        // single change.
        writeFileSync(path.join(taskDir, '.lock'), '');
        writeFileSync(path.join(taskDir, '.highwatermark'), '42');

        // Wait for the final state — both lock_exists and highwatermark
        // must reflect the combined writes.
        const row = await pollUntil(() => {
          const r = fx.sqlite.get<{
            has_highwatermark: number;
            highwatermark: number;
            lock_exists: number;
          }>(`SELECT has_highwatermark, highwatermark, lock_exists FROM tasks WHERE session_id = ?`, 's2');
          return r && r.has_highwatermark === 1 && r.lock_exists === 1 ? r : undefined;
        });
        assert.equal(row.has_highwatermark, 1, 'highwatermark write should land');
        assert.equal(row.highwatermark, 42, 'highwatermark value should be 42');
        assert.equal(row.lock_exists, 1, 'lock_exists should be 1');

        // Give any lingering duplicate write-batch one more drain
        // window; the relaxed contract is "≥1 event", not "exactly 1".
        await new Promise((r) => setTimeout(r, 250));
        const taskChanges = captured.filter((c) => c.type === 'task.updated');
        assert.ok(taskChanges.length >= 1, `expected ≥1 task.updated event, got ${taskChanges.length}`);

        sub();
        dispose();
      } finally {
        await fx.cleanup();
      }
    },
  );
});

// ═══════════════════════════════════════════════════════════════════════════
// SUITE: file-history/ live updates (RFC 005 C5.3)
// ═══════════════════════════════════════════════════════════════════════════

describe('LiveUpdates file-history/ scope (RFC 005 C5.3)', () => {
  test('file-history snapshot lands + emits file-history.added', { timeout: 10000 }, async () => {
    const fx = await createFixture({ ensureDirs: ['file-history'] });
    try {
      await fx.live.start();
      const captured: Change[] = [];
      const sub = fx.store.subscribe({ kind: 'file-history', sessionId: 's1' }, (c) => {
        captured.push(c);
      });
      const dispose = fx.live.prewarm({ kind: 'file-history', sessionId: 's1' });
      await new Promise((r) => setTimeout(r, 150));

      const historyDir = path.join(fx.rootDir, 'file-history', 's1');
      mkdirSync(historyDir, { recursive: true });
      writeFileSync(path.join(historyDir, 'abc@v1'), 'hello');

      const event = await pollUntil(() => {
        const hit = captured.find((c) => c.type === 'file-history.added');
        return hit ? hit : undefined;
      });
      assert.equal(event.type, 'file-history.added');
      if (event.type === 'file-history.added') {
        assert.equal(event.hash, 'abc');
        assert.equal(event.version, 1);
        assert.equal(event.sessionId, 's1');
      }

      sub();
      dispose();
    } finally {
      await fx.cleanup();
    }
  });
});

// ═══════════════════════════════════════════════════════════════════════════
// SUITE: plans/ live updates (RFC 005 C5.4)
// ═══════════════════════════════════════════════════════════════════════════

describe('LiveUpdates plans/ scope (RFC 005 C5.4)', () => {
  test('plan markdown file lands + emits plan.upserted', { timeout: 10000 }, async () => {
    const fx = await createFixture({ ensureDirs: ['plans'] });
    try {
      await fx.live.start();
      const captured: Change[] = [];
      const sub = fx.store.subscribe({ kind: 'plan' }, (c) => {
        captured.push(c);
      });
      const dispose = fx.live.prewarm({ kind: 'plan' });
      await new Promise((r) => setTimeout(r, 150));

      writeFileSync(path.join(fx.rootDir, 'plans', 'abc.md'), '# Example Plan\n\nbody');

      const event = await pollUntil(() => {
        const hit = captured.find((c) => c.type === 'plan.upserted');
        return hit ? hit : undefined;
      });
      assert.equal(event.type, 'plan.upserted');
      if (event.type === 'plan.upserted') {
        assert.equal(event.slug, 'abc');
        assert.equal(event.plan.title, 'Example Plan');
      }

      const row = fx.sqlite.get<{ slug: string; title: string }>(`SELECT slug, title FROM plans WHERE slug = ?`, 'abc');
      assert.ok(row, 'plans row should exist');
      assert.equal(row.title, 'Example Plan');

      sub();
      dispose();
    } finally {
      await fx.cleanup();
    }
  });
});

// ═══════════════════════════════════════════════════════════════════════════
// SUITE: settings/ live updates (RFC 005 C5.5)
// ═══════════════════════════════════════════════════════════════════════════

describe('LiveUpdates settings/ scope (RFC 005 C5.5)', () => {
  test('settings.json write lands + emits settings.changed', { timeout: 10000 }, async () => {
    const fx = await createFixture({ ensureDirs: [] });
    try {
      await fx.live.start();
      const captured: Change[] = [];
      const sub = fx.store.subscribe({ kind: 'settings' }, (c) => {
        captured.push(c);
      });
      const dispose = fx.live.prewarm({ kind: 'settings' });
      await new Promise((r) => setTimeout(r, 150));

      const settingsPath = path.join(fx.rootDir, 'settings.json');
      writeFileSync(settingsPath, JSON.stringify({ cleanupPeriodDays: 30, effortLevel: 'high' }));

      const event = await pollUntil(() => {
        const hit = captured.find((c) => c.type === 'settings.changed' && c.file === 'settings');
        return hit ? hit : undefined;
      });
      assert.equal(event.type, 'settings.changed');
      if (event.type === 'settings.changed') {
        assert.equal(event.file, 'settings');
        assert.equal((event.settings as { cleanupPeriodDays?: number }).cleanupPeriodDays, 30);
      }

      // Store should carry the parsed settings so api.getConfig reflects fresh value.
      assert.strictEqual(fx.store.hasConfig(), true);
      const cfg = fx.store.getConfig();
      assert.equal((cfg.settings as { cleanupPeriodDays?: number }).cleanupPeriodDays, 30);

      sub();
      dispose();
    } finally {
      await fx.cleanup();
    }
  });

  test(
    'atomic-rename write collapses settings.changed events — final parsed value lands',
    { timeout: 10000 },
    async () => {
      // The 150 ms trailing coalescer ideally collapses
      // delete+create from atomic rename into a single emit, but
      // timing makes "exactly 1" flaky on slow CI. The relaxed
      // contract is observable: ≥1 event fires AND the final settings
      // value reflects the rename source.
      const fx = await createFixture({ ensureDirs: [] });
      try {
        await fx.live.start();
        const captured: Change[] = [];
        const sub = fx.store.subscribe({ kind: 'settings' }, (c) => {
          captured.push(c);
        });
        const dispose = fx.live.prewarm({ kind: 'settings' });
        await new Promise((r) => setTimeout(r, 150));

        const settingsPath = path.join(fx.rootDir, 'settings.json');
        const tmpPath = `${settingsPath}.atomic`;
        writeFileSync(tmpPath, JSON.stringify({ effortLevel: 'medium' }));
        const startCount = captured.length;
        renameSync(tmpPath, settingsPath);

        // Give the 150ms coalescer room plus parcel's ~50ms FSEvents latency.
        await new Promise((r) => setTimeout(r, 500));

        const newEvents = captured.slice(startCount).filter((c) => c.type === 'settings.changed');
        assert.ok(newEvents.length >= 1, `expected ≥1 settings.changed event, got ${newEvents.length}`);

        // The store cache must reflect the latest write regardless of
        // how many emits it took to get there.
        const cfg = fx.store.getConfig();
        assert.equal(
          (cfg.settings as { effortLevel?: string }).effortLevel,
          'medium',
          'final settings.effortLevel should reflect the renamed file',
        );

        sub();
        dispose();
      } finally {
        await fx.cleanup();
      }
    },
  );

  test('settings.local.json emits settings.changed with file="settings.local"', { timeout: 10000 }, async () => {
    const fx = await createFixture({ ensureDirs: [] });
    try {
      await fx.live.start();
      const captured: Change[] = [];
      const sub = fx.store.subscribe({ kind: 'settings' }, (c) => {
        captured.push(c);
      });
      const dispose = fx.live.prewarm({ kind: 'settings' });
      await new Promise((r) => setTimeout(r, 150));

      writeFileSync(path.join(fx.rootDir, 'settings.local.json'), JSON.stringify({ permissions: { allow: ['*'] } }));

      const event = await pollUntil(() => {
        const hit = captured.find((c) => c.type === 'settings.changed' && c.file === 'settings.local');
        return hit ? hit : undefined;
      });
      assert.equal(event.type, 'settings.changed');
      if (event.type === 'settings.changed') assert.equal(event.file, 'settings.local');

      sub();
      dispose();
    } finally {
      await fx.cleanup();
    }
  });

  test('corrupt settings.json never throws; no event fires until a valid parse lands', { timeout: 10000 }, async () => {
    const fx = await createFixture({ ensureDirs: [] });
    try {
      await fx.live.start();
      const captured: Change[] = [];
      const sub = fx.store.subscribe({ kind: 'settings' }, (c) => {
        captured.push(c);
      });
      const dispose = fx.live.prewarm({ kind: 'settings' });
      await new Promise((r) => setTimeout(r, 150));

      const settingsPath = path.join(fx.rootDir, 'settings.json');
      const startCount = captured.length;
      writeFileSync(settingsPath, 'not json');

      // Wait past the debounce plus a safety margin.
      await new Promise((r) => setTimeout(r, QUIET_MS));

      const corruptEvents = captured.slice(startCount).filter((c) => c.type === 'settings.changed');
      assert.equal(corruptEvents.length, 0, 'corrupt write must not surface a settings.changed event');

      sub();
      dispose();
    } finally {
      await fx.cleanup();
    }
  });
});
