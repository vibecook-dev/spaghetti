/**
 * CheckpointStore — unit tests (RFC 005 C2.2).
 *
 * Covers the documented contract in `live/checkpoints.ts`:
 *   - `load()` tolerates missing and corrupt state files.
 *   - `set`/`get`/`all` roundtrip and preserve insertion order.
 *   - `flush()` + fresh store + `load()` recovers the same state.
 *   - `scheduleFlush()` debounces on a 2 s trailing edge.
 *   - `stop()` forces a final flush regardless of pending debounce.
 *
 * Uses only the built-in node:test runner and real timers, to keep the
 * implementation honest about `setTimeout.unref()` and avoid fake-timer
 * coupling. Debounce-path tests poll for the flush rather than sleeping a
 * fixed interval — see `readAfterFlush`.
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { mkdtempSync, rmSync } from 'node:fs';
import { readFile, writeFile, stat } from 'node:fs/promises';
import { setTimeout as sleep } from 'node:timers/promises';

import { createCheckpointStore } from '../checkpoints.js';
import type { Checkpoint } from '../checkpoints.js';

// ═══════════════════════════════════════════════════════════════════════════
// HELPERS
// ═══════════════════════════════════════════════════════════════════════════

function makeCheckpoint(overrides: Partial<Checkpoint> = {}): Checkpoint {
  return {
    path: '/tmp/fixture.jsonl',
    inode: 12345,
    size: 4096,
    lastOffset: 2048,
    lastMtimeMs: 1_700_000_000_000,
    ...overrides,
  };
}

interface StateFile {
  version?: number;
  checkpoints: Checkpoint[];
}

/**
 * Read the state file once the debounced flush has landed.
 *
 * This used to be `await sleep(2100)` against a 2 s debounce — 100 ms of
 * headroom, which a loaded shared runner eats routinely. It was a recurring
 * Windows CI flake (ENOENT: the timer simply had not fired yet).
 *
 * Polling keeps the fast path fast: on an idle machine this returns at ~2 s
 * just as the sleep did, while the ceiling absorbs a slow scheduler instead of
 * failing. The ceiling only delays the report of a real breakage; it cannot
 * hide one, because a store that never flushes still never produces the file.
 */
async function readAfterFlush(filePath: string, timeoutMs = 20_000): Promise<StateFile> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    try {
      return JSON.parse(await readFile(filePath, 'utf8')) as StateFile;
    } catch (err) {
      if (Date.now() >= deadline) {
        throw new Error(`debounced flush never landed at ${filePath} within ${timeoutMs}ms`, { cause: err });
      }
      await sleep(25);
    }
  }
}

/**
 * Fresh temp dir + state file path. Caller is responsible for cleanup
 * via the returned `cleanup()` fn — we deliberately don't auto-hook
 * `after()` because each test wants an isolated directory.
 */
function makeTempStatePath(prefix: string): { dir: string; filePath: string; cleanup: () => void } {
  const dir = mkdtempSync(path.join(os.tmpdir(), `spaghetti-cp-${prefix}-`));
  const filePath = path.join(dir, '.spaghetti-live-state.json');
  return {
    dir,
    filePath,
    cleanup: () => {
      rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
    },
  };
}

/**
 * Swallow `console.warn` output for the duration of `fn()`. Returns the
 * captured messages so tests can assert on them without leaking warnings
 * into the test runner output.
 */
async function captureWarnings<T>(fn: () => Promise<T>): Promise<{ result: T; warnings: string[] }> {
  const warnings: string[] = [];
  const original = console.warn;
  console.warn = (...args: unknown[]) => {
    warnings.push(args.map((a) => String(a)).join(' '));
  };
  try {
    const result = await fn();
    return { result, warnings };
  } finally {
    console.warn = original;
  }
}

// ═══════════════════════════════════════════════════════════════════════════
// TESTS
// ═══════════════════════════════════════════════════════════════════════════

describe('CheckpointStore', () => {
  test('load() on missing file starts empty and does not throw', async () => {
    const { filePath, cleanup } = makeTempStatePath('missing');
    try {
      const store = createCheckpointStore({ filePath });
      // `captureWarnings` also proves we don't warn on the ENOENT case.
      const { warnings } = await captureWarnings(async () => store.load());
      assert.equal(store.all().size, 0);
      assert.equal(warnings.length, 0, `expected no warnings, got: ${warnings.join('\n')}`);
    } finally {
      cleanup();
    }
  });

  test('load() on a corrupt file warns and starts empty', async () => {
    const { filePath, cleanup } = makeTempStatePath('corrupt');
    try {
      await writeFile(filePath, 'not json', 'utf8');
      const store = createCheckpointStore({ filePath });
      const { warnings } = await captureWarnings(async () => store.load());
      assert.equal(store.all().size, 0);
      assert.ok(
        warnings.some((w) => w.includes('Corrupt state file') || w.includes('JSON')),
        `expected a parse-error warning, got: ${warnings.join('\n')}`,
      );
    } finally {
      cleanup();
    }
  });

  test('load() on file with wrong shape warns and starts empty', async () => {
    const { filePath, cleanup } = makeTempStatePath('wrongshape');
    try {
      await writeFile(filePath, JSON.stringify({ hello: 'world' }), 'utf8');
      const store = createCheckpointStore({ filePath });
      const { warnings } = await captureWarnings(async () => store.load());
      assert.equal(store.all().size, 0);
      assert.ok(
        warnings.some((w) => w.includes('unexpected shape')),
        `expected a shape-mismatch warning, got: ${warnings.join('\n')}`,
      );
    } finally {
      cleanup();
    }
  });

  test('set + get + all roundtrip (insertion order preserved)', async () => {
    const { filePath, cleanup } = makeTempStatePath('roundtrip');
    try {
      const store = createCheckpointStore({ filePath });
      const a = makeCheckpoint({ path: '/a.jsonl', inode: 1 });
      const b = makeCheckpoint({ path: '/b.jsonl', inode: 2 });
      const c = makeCheckpoint({ path: '/c.jsonl', inode: 3 });

      store.set(a.path, a);
      store.set(b.path, b);
      store.set(c.path, c);

      assert.deepEqual(store.get('/a.jsonl'), a);
      assert.deepEqual(store.get('/b.jsonl'), b);
      assert.equal(store.get('/missing'), undefined);

      const keys = Array.from(store.all().keys());
      assert.deepEqual(keys, ['/a.jsonl', '/b.jsonl', '/c.jsonl']);

      store.delete('/b.jsonl');
      assert.equal(store.get('/b.jsonl'), undefined);
      assert.deepEqual(Array.from(store.all().keys()), ['/a.jsonl', '/c.jsonl']);
    } finally {
      cleanup();
    }
  });

  test('flush + fresh-store + load recovers the same checkpoints', async () => {
    const { filePath, cleanup } = makeTempStatePath('persist');
    try {
      const writer = createCheckpointStore({ filePath });
      const a = makeCheckpoint({ path: '/a.jsonl', inode: 1, lastOffset: 100 });
      const b = makeCheckpoint({ path: '/b.jsonl', inode: 2, lastOffset: 200 });
      writer.set(a.path, a);
      writer.set(b.path, b);
      await writer.flush();

      // File exists, is human-readable JSON, and carries version=1.
      const raw = await readFile(filePath, 'utf8');
      const parsed = JSON.parse(raw) as { version: number; checkpoints: Checkpoint[] };
      assert.equal(parsed.version, 1);
      assert.equal(parsed.checkpoints.length, 2);
      assert.ok(raw.includes('\n'), 'expected pretty-printed JSON');

      // Temp file must not be left behind on a clean flush.
      await assert.rejects(stat(`${filePath}.tmp`), /ENOENT/);

      // Fresh store over the same path recovers everything.
      const reader = createCheckpointStore({ filePath });
      await reader.load();
      assert.deepEqual(reader.get('/a.jsonl'), a);
      assert.deepEqual(reader.get('/b.jsonl'), b);
      assert.deepEqual(Array.from(reader.all().keys()), ['/a.jsonl', '/b.jsonl']);
    } finally {
      cleanup();
    }
  });

  test('scheduleFlush writes to disk after the 2 s debounce window', async () => {
    const { filePath, cleanup } = makeTempStatePath('debounce');
    try {
      const store = createCheckpointStore({ filePath });
      const cp = makeCheckpoint({ path: '/debounced.jsonl', inode: 42, lastOffset: 7 });
      store.set(cp.path, cp);
      store.scheduleFlush();

      const parsed = await readAfterFlush(filePath);
      assert.equal(parsed.version, 1);
      assert.equal(parsed.checkpoints.length, 1);
      assert.deepEqual(parsed.checkpoints[0], cp);

      // Explicit stop() to make sure we don't leak timers between tests.
      await store.stop();
    } finally {
      cleanup();
    }
  });

  test('scheduleFlush is idempotent — repeated calls reset the timer', async () => {
    const { filePath, cleanup } = makeTempStatePath('debounce-reset');
    try {
      const store = createCheckpointStore({ filePath });
      store.set('/x', makeCheckpoint({ path: '/x', inode: 9 }));
      store.scheduleFlush();
      store.scheduleFlush();
      store.scheduleFlush();
      // Should still coalesce to a single record after the window.
      const parsed = await readAfterFlush(filePath);
      assert.equal(parsed.checkpoints.length, 1);
      await store.stop();
    } finally {
      cleanup();
    }
  });

  test('stop() forces a final flush even before the 2 s debounce fires', async () => {
    const { filePath, cleanup } = makeTempStatePath('stop');
    try {
      const store = createCheckpointStore({ filePath });
      const cp = makeCheckpoint({ path: '/forced.jsonl', inode: 77, lastOffset: 13 });
      store.set(cp.path, cp);
      store.scheduleFlush();
      // Don't wait — stop() must still produce a written file.
      await store.stop();

      const raw = await readFile(filePath, 'utf8');
      const parsed = JSON.parse(raw) as { checkpoints: Checkpoint[] };
      assert.equal(parsed.checkpoints.length, 1);
      assert.deepEqual(parsed.checkpoints[0], cp);
    } finally {
      cleanup();
    }
  });

  test('concurrent flush() calls serialize (no interleaved writes)', async () => {
    const { filePath, cleanup } = makeTempStatePath('concurrent');
    try {
      const store = createCheckpointStore({ filePath });
      store.set('/a', makeCheckpoint({ path: '/a', inode: 1 }));

      // Kick off multiple overlapping flushes. Each must complete without
      // throwing. The store's `flushing` chain prevents interleaving.
      const inflight = [store.flush(), store.flush(), store.flush()];
      store.set('/b', makeCheckpoint({ path: '/b', inode: 2 }));
      inflight.push(store.flush());

      await Promise.all(inflight);

      const raw = await readFile(filePath, 'utf8');
      const parsed = JSON.parse(raw) as { checkpoints: Checkpoint[] };
      // At minimum the final flush observed `/a`; if it ran after the
      // second set, it also observed `/b`. Both are valid outcomes —
      // the invariant we care about is "no partial writes".
      assert.ok(parsed.checkpoints.length >= 1 && parsed.checkpoints.length <= 2);
      assert.equal(parsed.checkpoints[0]?.path, '/a');

      await store.stop();
    } finally {
      cleanup();
    }
  });
});
