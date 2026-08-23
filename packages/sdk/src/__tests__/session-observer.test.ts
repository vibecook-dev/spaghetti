/**
 * session-observer.test.ts — the RFC 012D observer through N-API, end to end.
 *
 * Builds a `.claude`-shaped tree in a temp directory, attaches the native
 * observer to it, and reads real events back. Skips when the native addon is
 * not loadable so hosts without a rebuilt binary still pass.
 *
 * Run: pnpm --filter @vibecook/spaghetti-sdk test
 */

import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { mkdtempSync, mkdirSync, appendFileSync, rmSync } from 'node:fs';

import { loadNativeAddon, openNativeSessionObserver } from '../native.js';
import type { NativeSessionObserver } from '../native.js';

const native = loadNativeAddon();

const SESSION = '01234567-89ab-cdef-0123-456789abcdef';
const PROJECT = '-fixture-project';

/** Synthetic assistant record: fixed ids, empty content, exact usage. */
function assistantRecord(uuid: string, responseId: string, inputTokens: number): string {
  return JSON.stringify({
    type: 'assistant',
    uuid,
    parentUuid: 'u1',
    timestamp: '2026-08-11T00:00:00Z',
    sessionId: SESSION,
    cwd: '/fixture',
    version: '1',
    gitBranch: 'main',
    isSidechain: false,
    userType: 'external',
    requestId: `r-${uuid}`,
    message: {
      model: 'fixture-model',
      id: responseId,
      type: 'message',
      role: 'assistant',
      content: [{ type: 'text', text: 'fixture' }],
      usage: {
        input_tokens: inputTokens,
        output_tokens: 5,
        cache_creation_input_tokens: 2,
        cache_read_input_tokens: 3,
      },
    },
  });
}

type ObserverEvent = { type: string; [key: string]: unknown };

async function drainUntil(
  observer: NativeSessionObserver,
  accept: (events: ObserverEvent[]) => boolean,
  timeoutMs = 15_000,
): Promise<ObserverEvent[]> {
  const deadline = Date.now() + timeoutMs;
  const collected: ObserverEvent[] = [];
  while (!accept(collected) && Date.now() < deadline) {
    const batch = JSON.parse(await observer.waitForEvents(100, 512)) as ObserverEvent[];
    collected.push(...batch);
  }
  return collected;
}

describe('native session observer', { skip: !native }, () => {
  let root: string;
  let transcript: string;

  before(() => {
    root = mkdtempSync(path.join(os.tmpdir(), 'spaghetti-observer-'));
    mkdirSync(path.join(root, 'projects', PROJECT), { recursive: true });
    transcript = path.join(root, 'projects', PROJECT, `${SESSION}.jsonl`);
    appendFileSync(transcript, `${assistantRecord('a-1', 'resp-1', 10)}\n`);
  });

  after(() => {
    rmSync(root, { recursive: true, force: true });
  });

  test('bootstraps a real session tree and delivers typed events', async () => {
    const observer = openNativeSessionObserver({
      adapter_id: 'claude-code',
      agent_root: root,
      transcript_path: transcript,
      native_session_id: SESSION,
      poll_interval_ms: 15,
    });
    try {
      const bootstrap = await drainUntil(observer, (events) =>
        events.some((event) => event.type === 'bootstrap_complete'),
      );

      const usage = bootstrap.find((event) => event.type === 'usage_v2');
      assert.ok(usage, 'the transcript should produce a usage-v2 revision');
      assert.equal(typeof usage.event_id, 'string');
      assert.equal(usage.scope_epoch, 1);
      assert.equal(usage.phase, 'bootstrap');
      assert.equal(usage.operation, 'upsert');
      assert.ok(
        (usage.semantic_revision_ref as { fact_revision_id?: string })?.fact_revision_id,
        'every semantic event carries its durable join identity',
      );
      assert.equal((usage.actor as { native_session_id?: string })?.native_session_id, SESSION);

      const barrier = bootstrap.find((event) => event.type === 'bootstrap_complete');
      assert.ok(barrier, 'bootstrap completes with a barrier');
      assert.equal(barrier.root_present, true);
      assert.equal((barrier.family_manifest as unknown[]).length, 11);

      const status = observer.status();
      assert.equal(status.scopeEpoch, 1);
      assert.equal(status.epochValid, true);
      assert.equal(status.closed, false);

      // A live append arrives without reopening anything.
      appendFileSync(transcript, `${assistantRecord('a-2', 'resp-2', 20)}\n`);
      const live = await drainUntil(observer, (events) => events.some((event) => event.type === 'usage_v2'));
      assert.ok(
        live.some((event) => event.type === 'usage_v2' && event.phase === 'live'),
        'a live append should deliver a live-phase usage revision',
      );
    } finally {
      await observer.close();
    }
  });

  test('refuses a locator outside the adapter’s declared source roots', () => {
    assert.throws(
      () =>
        openNativeSessionObserver({
          adapter_id: 'claude-code',
          agent_root: root,
          transcript_path: path.join(root, 'elsewhere', `${SESSION}.jsonl`),
        }),
      /projects/,
    );
  });

  test('accepts the request as a JSON string too', async () => {
    const observer = openNativeSessionObserver(
      JSON.stringify({
        adapter_id: 'claude-code',
        agent_root: root,
        transcript_path: transcript,
        poll_interval_ms: 15,
      }),
    );
    try {
      assert.equal(observer.status().closed, false);
    } finally {
      await observer.close();
    }
  });
});
