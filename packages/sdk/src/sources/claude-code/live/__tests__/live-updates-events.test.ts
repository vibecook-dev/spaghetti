/**
 * LiveUpdates — end-to-end subscriber delivery (RFC 005 C3.3 + C3.4).
 *
 * Seals the "subscribers actually fire" contract through the full
 * public API surface:
 *
 *   createSpaghettiService({ live: true })
 *     .initialize()
 *     .live!.onChange({kind: 'session', ...}, listener)
 *     → write JSONL line → listener fires within 2s
 *
 * Prior commits wired every seam (C3.1 registry, C3.2 lazy attach,
 * C3.3 fan-out); C3.4 exposes them on `api.live`. This test exercises
 * the production path — no reaching into the store directly — with
 * real parcel-watcher + real SQLite + real IncrementalParser.
 */

import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { mkdtempSync, rmSync, mkdirSync, writeFileSync, realpathSync } from 'node:fs';

import { createSpaghettiService } from '../../../../create.js';
import type { SpaghettiAPI } from '../../../../api.js';
import type { Change } from '../../../../live/change-events.js';

const SLUG = 'e2e-slug';
const SESSION_ID = 'e2e-session';
/** Ceilings, not waits — see the note in live-updates.test.ts. */
const DELIVERY_TIMEOUT_MS = 10_000;
const TEST_TIMEOUT_MS = 45_000;

/**
 * Resolve when `api.live.onChange` fires OR `timeoutMs` elapses,
 * whichever first. Returns the first matching Change (or throws on
 * timeout). Subscription is disposed automatically after resolution.
 */
function awaitFirstChangeFromApi(
  api: SpaghettiAPI,
  topic: Parameters<NonNullable<SpaghettiAPI['live']>['onChange']>[0],
  timeoutMs: number,
): Promise<Change> {
  return new Promise<Change>((resolve, reject) => {
    if (!api.live) {
      reject(new Error('api.live is undefined — service must be constructed with { live: true }'));
      return;
    }
    let timer: ReturnType<typeof setTimeout> | null = null;
    const dispose = api.live.onChange(
      // topic form: signature is (topic, listener, options?)
      topic as Parameters<NonNullable<SpaghettiAPI['live']>['onChange']>[0],
      (e) => {
        if (timer !== null) clearTimeout(timer);
        dispose();
        resolve(e);
      },
    );
    timer = setTimeout(() => {
      dispose();
      reject(new Error(`awaitFirstChangeFromApi timed out after ${timeoutMs}ms`));
    }, timeoutMs);
  });
}

function makeUserMessage(uuid: string, text: string): string {
  return (
    JSON.stringify({
      type: 'user',
      uuid,
      timestamp: new Date().toISOString(),
      sessionId: SESSION_ID,
      message: { role: 'user', content: text },
    }) + '\n'
  );
}

describe('api.live.onChange end-to-end delivery (RFC 005 C3.3 + C3.4)', () => {
  let tempRoot: string;
  let rootDir: string;
  let sessionPath: string;
  let dbPath: string;
  let api: SpaghettiAPI;

  before(async () => {
    tempRoot = realpathSync(mkdtempSync(path.join(os.tmpdir(), 'spaghetti-live-events-')));
    rootDir = path.join(tempRoot, '.claude');
    const projectDir = path.join(rootDir, 'projects', SLUG);
    sessionPath = path.join(projectDir, `${SESSION_ID}.jsonl`);
    mkdirSync(projectDir, { recursive: true });
    // todos/ present to avoid spurious onError noise when/if the test
    // grows to prewarm that scope.
    mkdirSync(path.join(rootDir, 'todos'), { recursive: true });

    dbPath = path.join(tempRoot, 'live.db');

    // Force the TS engine path so we don't depend on the native
    // addon being prebuilt in every test environment. The live-
    // updates pipeline itself is engine-agnostic.
    api = createSpaghettiService({
      rootDir,
      dbPath,
      live: true,
      engine: 'ts',
    });

    await api.initialize();
  });

  after(async () => {
    try {
      await api.dispose();
    } catch {
      /* ignore */
    }
    rmSync(tempRoot, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  });

  test(
    'api.live.onChange delivers session.message.added for a live JSONL write',
    { timeout: TEST_TIMEOUT_MS },
    async () => {
      assert.ok(api.live, 'api.live should be defined under { live: true }');

      const topic = { kind: 'session' as const, slug: SLUG, sessionId: SESSION_ID };
      const changePromise = awaitFirstChangeFromApi(api, topic, DELIVERY_TIMEOUT_MS);

      // `onChange(topic, ...)` internally prewarms the projects/
      // scope as part of the composed dispose; it attaches the
      // watcher. Give parcel a tick to actually bind before writing
      // the fixture — the pipeline tolerates write-before-attach
      // (subsequent update events would catch it) but this keeps the
      // timing comfortably within the 2s budget.
      await new Promise((r) => setTimeout(r, 150));

      writeFileSync(sessionPath, makeUserMessage('uuid-api-live-1', 'hello from C3.4'));

      const change = await changePromise;

      assert.equal(change.type, 'session.message.added');
      if (change.type !== 'session.message.added') return; // narrow
      assert.equal(change.slug, SLUG);
      assert.equal(change.sessionId, SESSION_ID);
      assert.ok(change.seq >= 1, `seq should be >= 1, got ${change.seq}`);
      assert.equal(typeof change.ts, 'number');
    },
  );

  test('api.live.prewarm returns a Dispose that idempotently detaches', { timeout: TEST_TIMEOUT_MS }, async () => {
    assert.ok(api.live);
    const dispose = api.live.prewarm({ kind: 'session', slug: SLUG });
    assert.equal(typeof dispose, 'function');
    dispose();
    // Second dispose must be a no-op, not a throw.
    assert.doesNotThrow(() => dispose());
  });

  test('api.live.isSaturated is false on an idle pipeline', () => {
    assert.ok(api.live);
    assert.equal(api.live.isSaturated(), false);
  });
});
