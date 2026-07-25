/**
 * Watcher — unit tests for the @parcel/watcher implementation.
 *
 * RFC 005 C2.1. Exercises `createParcelWatcher()` against a real temp
 * directory. The chokidar fallback is intentionally skipped because
 * chokidar's debounce/ready semantics make deterministic fs-event tests
 * brittle; its behaviour is documented in the impl's JSDoc and covered
 * by the existing `file-service` wiring that already uses chokidar in
 * production.
 *
 * Style matches `packages/sdk/src/data/__tests__/agent-data-store.test.ts`:
 * node:test + assert/strict, `mkdtempSync` per-suite fixture, explicit
 * cleanup in `after`.
 */

import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { mkdtempSync, mkdirSync, rmSync, writeFileSync, appendFileSync, unlinkSync } from 'node:fs';

import {
  createParcelWatcher,
  createPollingWatcher,
  createResilientWatcher,
  type Watcher,
  type WatchEvent,
  type Unsubscribe,
} from '../watcher.js';

// ═══════════════════════════════════════════════════════════════════════════
// HELPERS
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Collects watch events into a growing array and exposes a
 * `waitForMatch` that resolves once a predicate sees a matching event
 * (or rejects after `timeoutMs`). Cheaper than polling the array in a
 * loop and avoids arbitrary `setTimeout` waits that get flaky under
 * system load.
 */
function createCollector() {
  const events: WatchEvent[] = [];
  const listeners: Array<(e: WatchEvent[]) => void> = [];

  const onEvents = (batch: WatchEvent[]): void => {
    events.push(...batch);
    for (const l of listeners) l(batch);
  };

  const waitForMatch = async (predicate: (e: WatchEvent) => boolean, timeoutMs = 5000): Promise<WatchEvent> => {
    // First, scan anything already accumulated.
    const hit = events.find(predicate);
    if (hit) return hit;
    return new Promise<WatchEvent>((resolve, reject) => {
      const timer = setTimeout(() => {
        const idx = listeners.indexOf(handler);
        if (idx >= 0) listeners.splice(idx, 1);
        reject(
          new Error(
            `Timed out after ${timeoutMs} ms waiting for watch event. ` + `Collected so far: ${JSON.stringify(events)}`,
          ),
        );
      }, timeoutMs);
      const handler = (batch: WatchEvent[]): void => {
        const match = batch.find(predicate);
        if (match) {
          clearTimeout(timer);
          const idx = listeners.indexOf(handler);
          if (idx >= 0) listeners.splice(idx, 1);
          resolve(match);
        }
      };
      listeners.push(handler);
    });
  };

  return { events, onEvents, waitForMatch };
}

/**
 * Resolves `path.realpath`-equivalent normalisation parcel applies to
 * emitted paths. On macOS, `/tmp` is a symlink to `/private/tmp`, so
 * events come back with the realpath-expanded form; naive equality
 * against the temp-dir we passed in would fail. We canonicalise both
 * sides with `fs.realpathSync`.
 */
function samePath(a: string, b: string): boolean {
  // Tests pass realpathed strings through `fs.realpathSync` before
  // comparing, so string-equal handles POSIX. Windows paths are
  // case-insensitive and backends may differ in casing — compare folded.
  if (a === b) return true;
  return process.platform === 'win32' && a.toLowerCase() === b.toLowerCase();
}

// ═══════════════════════════════════════════════════════════════════════════
// TEST SUITE
// ═══════════════════════════════════════════════════════════════════════════

describe('createParcelWatcher (RFC 005 C2.1)', () => {
  let tempDir: string;
  let realTempDir: string;

  before(async () => {
    tempDir = mkdtempSync(path.join(os.tmpdir(), 'spaghetti-watcher-test-'));
    // Parcel emits realpath-canonicalised paths — resolve the symlinked
    // `/tmp` → `/private/tmp` on macOS so equality checks below work.
    realTempDir = (await import('node:fs')).realpathSync(tempDir);
  });

  after(() => {
    rmSync(tempDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  });

  // ─── subscribe: create ────────────────────────────────────────────────
  test('subscribe fires `create` when a file is added', async () => {
    const watcher = createParcelWatcher();
    const { onEvents, waitForMatch } = createCollector();

    const unsubscribe: Unsubscribe = await watcher.subscribe(realTempDir, onEvents, {
      ignore: [],
      recursive: true,
    });
    // Warm-up beat: subscribe resolving does not guarantee the backend
    // is delivering yet on every platform.
    await new Promise((r) => setTimeout(r, 100));

    const target = path.join(realTempDir, 'create-me.txt');
    writeFileSync(target, 'hello');

    const evt = await waitForMatch((e) => e.type === 'create' && samePath(e.path, target));
    assert.strictEqual(evt.type, 'create');
    assert.strictEqual(evt.path, target);

    await unsubscribe();
  });

  // ─── subscribe: update ────────────────────────────────────────────────
  test('subscribe fires an event when a watched file is modified', async () => {
    // Note: macOS FSEvents + @parcel/watcher do not reliably distinguish
    // the first post-subscribe event for a file as `create` vs `update`.
    // When a file exists at subscribe time and is then mutated, FSEvents'
    // journal-replay semantics can re-deliver the creation as a `create`
    // event after subscribe, masking the subsequent `update`. We therefore
    // assert the weaker (but correct on every platform) invariant: at
    // least one event lands for the mutated path after subscribe. The
    // `create`-only and `delete`-only tests above pin the per-type
    // contract on pristine paths where FSEvents is deterministic.
    const watcher = createParcelWatcher();
    const target = path.join(realTempDir, 'update-me.txt');
    writeFileSync(target, 'initial');
    await new Promise((r) => setTimeout(r, 200));

    const { onEvents, waitForMatch } = createCollector();
    const unsubscribe = await watcher.subscribe(realTempDir, onEvents, {
      ignore: [],
      recursive: true,
    });
    await new Promise((r) => setTimeout(r, 100));

    appendFileSync(target, '\nappended line');

    const evt = await waitForMatch((e) => (e.type === 'update' || e.type === 'create') && samePath(e.path, target));
    assert.ok(evt.type === 'update' || evt.type === 'create');
    assert.strictEqual(evt.path, target);

    await unsubscribe();
  });

  // ─── subscribe: delete ────────────────────────────────────────────────
  test('subscribe fires `delete` when a watched file is removed', async () => {
    const watcher = createParcelWatcher();
    const target = path.join(realTempDir, 'delete-me.txt');
    writeFileSync(target, 'doomed');
    // Let the pre-subscribe create settle so the backend's snapshot
    // includes the file (mirrors the `update` test above; without it
    // Windows' backend can coalesce create+delete into nothing).
    await new Promise((r) => setTimeout(r, 200));

    const { onEvents, waitForMatch } = createCollector();
    const unsubscribe = await watcher.subscribe(realTempDir, onEvents, {
      ignore: [],
      recursive: true,
    });
    // Warm-up beat: subscribe resolving does not guarantee the backend
    // is delivering yet on every platform.
    await new Promise((r) => setTimeout(r, 100));

    unlinkSync(target);

    const evt = await waitForMatch((e) => e.type === 'delete' && samePath(e.path, target));
    assert.strictEqual(evt.type, 'delete');
    assert.ok(samePath(evt.path, target), evt.path);

    await unsubscribe();
  });

  // ─── unsubscribe stops emission ───────────────────────────────────────
  test('the returned Unsubscribe stops emissions', async () => {
    const watcher = createParcelWatcher();
    const { events, onEvents, waitForMatch } = createCollector();

    const unsubscribe = await watcher.subscribe(realTempDir, onEvents, {
      ignore: [],
      recursive: true,
    });
    // Warm-up beat: subscribe resolving does not guarantee the backend
    // is delivering yet on every platform (see the delete test above).
    await new Promise((r) => setTimeout(r, 100));

    // Prove the watcher is live by catching one event first.
    const preTarget = path.join(realTempDir, 'pre-unsubscribe.txt');
    writeFileSync(preTarget, 'live');
    await waitForMatch((e) => e.type === 'create' && samePath(e.path, preTarget));

    await unsubscribe();

    const countBefore = events.length;
    const postTarget = path.join(realTempDir, 'post-unsubscribe.txt');
    writeFileSync(postTarget, 'silence');

    // Give parcel a generous window in which it would normally have
    // emitted. 500 ms is plenty — parcel's fs-events backend coalesces
    // within ~100 ms on macOS. If any event fires after unsubscribe,
    // the count will have grown.
    await new Promise((r) => setTimeout(r, 500));
    assert.strictEqual(
      events.length,
      countBefore,
      `Expected no new events after unsubscribe, saw: ${JSON.stringify(events.slice(countBefore))}`,
    );
  });

  // ─── ignore glob filters paths ────────────────────────────────────────
  test('`ignore` glob filters out matching paths', async () => {
    const watcher = createParcelWatcher();
    const { events, onEvents, waitForMatch } = createCollector();

    // Ignore any file ending in `.ignore-me`. Parcel uses micromatch-
    // style globs; `**/*.ignore-me` is the conservative cross-platform
    // form.
    const unsubscribe = await watcher.subscribe(realTempDir, onEvents, {
      ignore: ['**/*.ignore-me'],
      recursive: true,
    });
    // Warm-up beat: subscribe resolving does not guarantee the backend
    // is delivering yet on every platform (see the delete test above).
    await new Promise((r) => setTimeout(r, 100));

    const ignored = path.join(realTempDir, 'secret.ignore-me');
    const visible = path.join(realTempDir, 'visible.txt');

    writeFileSync(ignored, 'hidden');
    writeFileSync(visible, 'public');

    // Wait for the visible file so we know the watcher has flushed at
    // least one batch; then assert the ignored path never appeared.
    await waitForMatch((e) => e.type === 'create' && samePath(e.path, visible));

    // Extra breathing room in case parcel batches the ignored event
    // after the visible one (it shouldn't — ignores are enforced at
    // the source — but be defensive).
    await new Promise((r) => setTimeout(r, 250));

    const leaked = events.find((e) => samePath(e.path, ignored));
    assert.strictEqual(leaked, undefined, `Ignored path leaked into event stream: ${JSON.stringify(leaked)}`);

    await unsubscribe();
  });

  // ─── writeSnapshot + getEventsSince round-trip ────────────────────────
  test('writeSnapshot + getEventsSince round-trip captures changes', async () => {
    const watcher = createParcelWatcher();

    // Use a dedicated subdir so the snapshot reflects a known starting
    // state even though earlier tests left artefacts behind in
    // `realTempDir`. Parcel's snapshot APIs operate on the watched
    // directory as a whole.
    const snapRoot = path.join(realTempDir, 'snap-root');
    (await import('node:fs')).mkdirSync(snapRoot, { recursive: true });

    const snapshotFile = path.join(realTempDir, 'snapshot.bin');
    await watcher.writeSnapshot(snapRoot, snapshotFile);

    // Make a change after the snapshot was taken.
    const changed = path.join(snapRoot, 'new-after-snapshot.txt');
    writeFileSync(changed, 'post-snapshot content');

    // Parcel's `getEventsSince` polls the OS for changes since the
    // snapshot — on fs-events backends the event may take a moment to
    // settle. Retry for up to 3 s.
    let events: WatchEvent[] = [];
    const deadline = Date.now() + 3000;
    while (Date.now() < deadline) {
      events = await watcher.getEventsSince(snapRoot, snapshotFile);
      if (events.some((e) => samePath(e.path, changed))) break;
      await new Promise((r) => setTimeout(r, 100));
    }

    const hit = events.find((e) => samePath(e.path, changed));
    assert.ok(hit, `Expected getEventsSince to report the new file, got: ${JSON.stringify(events)}`);
    // The event type for a created-after-snapshot file is `create`.
    assert.strictEqual(hit?.type, 'create');
  });

  // ─── root spelling ────────────────────────────────────────────────────
  // Every other test in this suite subscribes to `realTempDir` to sidestep
  // parcel's canonicalisation. Production can't: sources pass the rootDir
  // they were configured with, and the polling backend echoes that spelling
  // back. Two spellings of one file meant the live tailer missed the byte
  // checkpoint keyed on the cold-ingest path and replayed the file from 0.
  test('events are spelled under the caller-supplied root, not the resolved one', async (t) => {
    if (tempDir === realTempDir) {
      t.skip('root is not symlinked on this platform');
      return;
    }
    const watcher = createParcelWatcher();
    const { onEvents, waitForMatch } = createCollector();

    const unsubscribe: Unsubscribe = await watcher.subscribe(tempDir, onEvents, {
      ignore: [],
      recursive: true,
    });
    await new Promise((r) => setTimeout(r, 100));

    const target = path.join(tempDir, 'unresolved-root.txt');
    writeFileSync(target, 'hello');

    const evt = await waitForMatch((e) => samePath(e.path, target));
    assert.strictEqual(evt.path, target);
    assert.ok(!evt.path.startsWith(realTempDir), `event leaked the resolved root: ${evt.path}`);

    await unsubscribe();
  });
});

describe('resource-bounded polling reconciliation', () => {
  let tempDir: string;

  before(() => {
    tempDir = mkdtempSync(path.join(os.tmpdir(), 'spaghetti-polling-watcher-'));
  });

  after(() => {
    rmSync(tempDir, { recursive: true, force: true });
  });

  test('reconciles a recent startup write and follows subsequent appends', async () => {
    const target = path.join(tempDir, 'rollout-live.jsonl');
    const ignored = path.join(tempDir, 'noise.txt');
    writeFileSync(target, '{"first":true}\n');
    writeFileSync(ignored, 'noise');
    const collector = createCollector();
    const watcher = createPollingWatcher({
      pollIntervalMs: 20,
      discoveryIntervalMs: 40,
      activeWindowMs: 1_000,
      initialLookbackMs: 5_000,
      shouldInclude: (filePath) => filePath.endsWith('.jsonl'),
    });

    const unsubscribe = await watcher.subscribe(tempDir, collector.onEvents, { ignore: [], recursive: true });
    await collector.waitForMatch((event) => event.type === 'update' && samePath(event.path, target));
    assert.equal(
      collector.events.some((event) => samePath(event.path, ignored)),
      false,
    );

    const countBeforeAppend = collector.events.length;
    appendFileSync(target, '{"second":true}\n');
    const deadline = Date.now() + 1_000;
    while (collector.events.length === countBeforeAppend && Date.now() < deadline) {
      await new Promise((resolve) => setTimeout(resolve, 20));
    }
    assert.ok(collector.events.length > countBeforeAppend);
    assert.ok(collector.events.slice(countBeforeAppend).some((event) => samePath(event.path, target)));
    await unsubscribe();
  });

  test('discovers new files without allocating filesystem watch handles', async () => {
    const collector = createCollector();
    const watcher = createPollingWatcher({ pollIntervalMs: 20, discoveryIntervalMs: 40, initialLookbackMs: 0 });
    const unsubscribe = await watcher.subscribe(tempDir, collector.onEvents, { ignore: [], recursive: true });
    const target = path.join(tempDir, 'created-after-start.jsonl');
    writeFileSync(target, '{}\n');

    await collector.waitForMatch((event) => event.type === 'create' && samePath(event.path, target), 1_000);
    await unsubscribe();
  });

  test('honours root-level and nested glob ignores during reconciliation', async () => {
    const root = path.join(tempDir, 'ignored-paths');
    const debugDir = path.join(root, 'debug');
    mkdirSync(debugDir, { recursive: true });
    const rootIgnored = path.join(root, '.DS_Store');
    const nestedIgnored = path.join(debugDir, 'trace.jsonl');
    const included = path.join(root, 'session.jsonl');
    writeFileSync(rootIgnored, 'metadata');
    writeFileSync(nestedIgnored, '{}\n');
    writeFileSync(included, '{}\n');
    const collector = createCollector();
    const watcher = createPollingWatcher({ pollIntervalMs: 20, discoveryIntervalMs: 40 });

    const unsubscribe = await watcher.subscribe(root, collector.onEvents, {
      ignore: ['**/.DS_Store', '**/debug/**'],
      recursive: true,
    });
    await collector.waitForMatch((event) => samePath(event.path, included), 1_000);
    assert.equal(
      collector.events.some((event) => samePath(event.path, rootIgnored) || samePath(event.path, nestedIgnored)),
      false,
    );
    await unsubscribe();
  });

  test('continues through polling when the native backend cannot start', async () => {
    const failure = new Error('simulated FSEvents exhaustion');
    const primary: Watcher = {
      async subscribe() {
        throw failure;
      },
      async writeSnapshot() {},
      async getEventsSince() {
        return [];
      },
    };
    const unavailable: Error[] = [];
    const collector = createCollector();
    const watcher = createResilientWatcher({
      primary,
      pollIntervalMs: 20,
      discoveryIntervalMs: 40,
      initialLookbackMs: 0,
      onPrimaryUnavailable: (error) => unavailable.push(error),
    });
    const unsubscribe = await watcher.subscribe(tempDir, collector.onEvents, { ignore: [], recursive: true });
    // The report names the path that failed to bind — backends supply only a
    // bare reason — and keeps the backend's own error as `cause`.
    assert.equal(unavailable.length, 1);
    assert.equal(unavailable[0]?.cause, failure);
    assert.ok(
      unavailable[0]?.message.includes(tempDir) && unavailable[0]?.message.includes(failure.message),
      `expected the report to name the root and the reason, got: ${unavailable[0]?.message}`,
    );

    const target = path.join(tempDir, 'fallback-live.jsonl');
    writeFileSync(target, '{}\n');
    await collector.waitForMatch((event) => event.type === 'create' && samePath(event.path, target), 1_000);
    await unsubscribe();
  });
});
