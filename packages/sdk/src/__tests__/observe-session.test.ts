/**
 * `observeSession` through a real attachment.
 *
 * Every test drives the public SDK API over a real `.claude`-shaped tree on
 * disk and reads real events back — no mock native handle, no fixture replay.
 * What is under test is the wrapper's contract: `for await` delivers the
 * generated union in order, leaving the loop releases the attachment, and the
 * control events a consumer must handle (bootstrap, reset, closed) actually
 * arrive where the RFC says they do.
 *
 * Skips when the native addon is not loadable, so a host without a rebuilt
 * binary still passes.
 */

import { test, describe, after } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { mkdtempSync, mkdirSync, appendFileSync, writeFileSync, rmSync } from 'node:fs';

import { loadNativeAddon } from '../native.js';
import type { RuntimeSemanticValue, UsageRevisionV2Fact } from '../index.js';
import {
  isSemanticEvent,
  observeSession,
  type ObserveSessionOptions,
  type ObserverEvent,
  type SessionObserver,
} from '../observe-session.js';

const native = loadNativeAddon();
const SESSION = '01234567-89ab-cdef-0123-456789abcdef';
const PROJECT = '-observe-session-fixture';

const roots: string[] = [];
const open: SessionObserver[] = [];

after(async () => {
  for (const observer of open.splice(0)) await observer.close();
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 });
});

/** Synthetic assistant record: fixed ids, no native content, exact usage. */
function assistantRecord(uuid: string, responseId: string, sessionId = SESSION): string {
  return `${JSON.stringify({
    type: 'assistant',
    uuid,
    parentUuid: 'u1',
    timestamp: '2026-08-11T00:00:00Z',
    sessionId,
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
      usage: { input_tokens: 10, output_tokens: 5, cache_creation_input_tokens: 2, cache_read_input_tokens: 3 },
    },
  })}\n`;
}

/** A `.claude`-shaped tree holding one session with one record. */
function fixtureTree(): { root: string; transcript: string; subtree: string } {
  const root = mkdtempSync(path.join(os.tmpdir(), 'spaghetti-observe-'));
  roots.push(root);
  const project = path.join(root, 'projects', PROJECT);
  mkdirSync(project, { recursive: true });
  const transcript = path.join(project, `${SESSION}.jsonl`);
  writeFileSync(transcript, assistantRecord('a-1', 'resp-1'));
  return { root, transcript, subtree: path.join(project, SESSION) };
}

function attach(root: string, transcript: string, options: ObserveSessionOptions = {}): SessionObserver {
  const observer = observeSession(
    {
      adapter_id: 'claude-code',
      agent_root: root,
      transcript_path: transcript,
      poll_interval_ms: 15,
    },
    options,
  );
  open.push(observer);
  return observer;
}

/**
 * Consume the observer until `accept` is satisfied, then stop consuming
 * without closing — the attachment stays live for the next phase of a test.
 */
async function collectUntil(
  observer: SessionObserver,
  accept: (events: ObserverEvent[]) => boolean,
  timeoutMs = 20_000,
): Promise<ObserverEvent[]> {
  const deadline = Date.now() + timeoutMs;
  const collected: ObserverEvent[] = [];
  const iterator = observer[Symbol.asyncIterator]();
  while (Date.now() < deadline) {
    const next = await iterator.next();
    if (next.done) break;
    collected.push(next.value);
    if (accept(collected)) return collected;
  }
  assert.fail(`observer did not reach the expected state; saw ${collected.map((e) => e.type).join(', ')}`);
}

describe('observeSession', { skip: !native }, () => {
  test('for await delivers the generated union, bootstrap first', async () => {
    const { root, transcript } = fixtureTree();
    const observer = attach(root, transcript);

    const events = await collectUntil(observer, (seen) => seen.some((e) => e.type === 'bootstrap_complete'));

    const usage = events.find((event) => event.type === 'usage_v2');
    assert.ok(usage, 'a transcript with usage should produce a usage-v2 revision');
    // Narrowing is on the generated discriminator; nothing here parses.
    assert.ok(isSemanticEvent(usage));
    assert.equal(usage.family, 'usage_v2');
    assert.equal(usage.phase, 'bootstrap');
    assert.equal(usage.operation, 'upsert');
    assert.equal(usage.scope_epoch, 1);
    assert.equal(usage.actor.native_session_id, SESSION);
    assert.ok(usage.semantic_revision_ref.fact_revision_id, 'every semantic event carries its durable join identity');
    assert.equal(usage.source.root_name, 'projects');

    const barrier = events.at(-1);
    assert.equal(barrier?.type, 'bootstrap_complete', 'the barrier closes the bootstrap, it does not lead it');
    assert.ok(barrier && 'root_present' in barrier && barrier.root_present);
    assert.equal(barrier && 'family_manifest' in barrier ? barrier.family_manifest.length : 0, 11);

    // Control events are not semantic events, and the guard agrees.
    assert.equal(isSemanticEvent(barrier), false);

    // Sequence is monotonic across the whole stream.
    const sequences = events.map((event) => event.sequence);
    assert.deepEqual(
      sequences,
      [...sequences].sort((a, b) => a - b),
    );
  });

  test('a live append arrives without reattaching', async () => {
    const { root, transcript } = fixtureTree();
    const observer = attach(root, transcript);
    await collectUntil(observer, (seen) => seen.some((e) => e.type === 'bootstrap_complete'));

    appendFileSync(transcript, assistantRecord('a-2', 'resp-2'));

    const live = await collectUntil(observer, (seen) =>
      seen.some((event) => isSemanticEvent(event) && event.phase === 'live'),
    );
    const event = live.find((candidate) => isSemanticEvent(candidate) && candidate.phase === 'live');
    assert.ok(event && isSemanticEvent(event));
    assert.equal(event.scope_epoch, 1, 'an ordinary append does not start a new epoch');
  });

  test('a subagent transcript created after attach joins the scope', async () => {
    const { root, transcript, subtree } = fixtureTree();
    const observer = attach(root, transcript);
    await collectUntil(observer, (seen) => seen.some((e) => e.type === 'bootstrap_complete'));

    const subagents = path.join(subtree, 'subagents');
    mkdirSync(subagents, { recursive: true });
    writeFileSync(path.join(subagents, 'agent-fixture.jsonl'), assistantRecord('s-1', 'resp-sub'));

    const events = await collectUntil(observer, (seen) =>
      seen.some((event) => isSemanticEvent(event) && event.source.object_path.includes('subagents')),
    );
    const fromSubagent = events.find(
      (event) => isSemanticEvent(event) && event.source.object_path.includes('subagents'),
    );
    assert.ok(fromSubagent && isSemanticEvent(fromSubagent));
    assert.match(fromSubagent.source.object_path, /agent-fixture\.jsonl$/);
    assert.equal(
      fromSubagent.actor.native_session_id,
      SESSION,
      'a descendant is attributed to the root session it belongs to',
    );
  });

  test('rewriting the transcript reports a reset rather than replaying silently', async () => {
    const { root, transcript } = fixtureTree();
    const observer = attach(root, transcript);
    await collectUntil(observer, (seen) => seen.some((e) => e.type === 'bootstrap_complete'));

    // Truncate and write different content: the generation changed under us.
    writeFileSync(transcript, assistantRecord('b-1', 'resp-rewritten'));

    const events = await collectUntil(observer, (seen) => seen.some((e) => e.type === 'reset'));
    const reset = events.find((event) => event.type === 'reset');
    assert.ok(reset && 'old_generation' in reset);
    assert.ok(reset.new_generation > reset.old_generation, 'a reset advances the object generation');
    assert.ok(reset.reason.length > 0);
  });

  test('leaving the loop closes the attachment', async () => {
    const { root, transcript } = fixtureTree();
    const observer = attach(root, transcript);

    for await (const event of observer) {
      if (event.type === 'bootstrap_complete') break;
    }

    assert.equal(observer.status().closed, true, 'break must release the attachment, not leak a watch');
    // Idempotent, and safe after the iterator already closed it.
    await observer.close();
    await observer.close();
  });

  test('iteration is single-consumer and ends after the closed event', async () => {
    const { root, transcript } = fixtureTree();
    const observer = attach(root, transcript);

    assert.equal(
      observer[Symbol.asyncIterator](),
      observer[Symbol.asyncIterator](),
      'a second iterator would take events from the first, so there is only one',
    );

    await collectUntil(observer, (seen) => seen.some((e) => e.type === 'bootstrap_complete'));
    await observer.close();

    const rest: ObserverEvent[] = [];
    for await (const event of observer) rest.push(event);

    assert.ok(
      rest.every((event, index) => index === rest.length - 1 || event.type !== 'closed'),
      'closed is emitted once and is always last',
    );
    // Draining after close terminates instead of blocking on a dead attachment.
    assert.equal(observer.status().closed, true);
  });

  test('aborting the signal ends iteration cleanly rather than throwing', async () => {
    const { root, transcript } = fixtureTree();
    const controller = new AbortController();
    const observer = attach(root, transcript, { signal: controller.signal });

    const seen: ObserverEvent[] = [];
    // No try/catch: an abort that rejected here would lose every event the
    // consumer had already applied, which is the whole reason it does not.
    for await (const event of observer) {
      seen.push(event);
      if (event.type === 'bootstrap_complete') controller.abort();
    }

    assert.equal(seen.at(-1)?.type, 'closed', 'the final closed event still arrives after an abort');
    assert.ok(
      seen.some((event) => event.type === 'bootstrap_complete'),
      'events delivered before the abort are kept',
    );
    assert.equal(observer.status().closed, true);

    // Propagating the abort is the consumer's call, made after the loop.
    assert.throws(() => controller.signal.throwIfAborted());
  });

  test('an already-aborted signal closes immediately, but after the request is validated', async () => {
    const { root, transcript } = fixtureTree();
    const controller = new AbortController();
    controller.abort();

    // Validation still runs first, so a bad locator is not masked by the abort.
    assert.throws(
      () =>
        observeSession(
          { adapter_id: 'claude-code', agent_root: root, transcript_path: path.join(root, 'elsewhere', 'x.jsonl') },
          { signal: controller.signal },
        ),
      /projects/,
    );

    const observer = attach(root, transcript, { signal: controller.signal });
    const drained: ObserverEvent[] = [];
    // Drains whatever was already queued, then returns instead of parking.
    for await (const event of observer) drained.push(event);

    assert.equal(observer.status().closed, true);
    assert.equal(drained.at(-1)?.type, 'closed', 'even an immediate abort reports the close');
  });

  test('a consumer can name the value type a handler takes', async () => {
    // The point of exporting the per-family types: this handler signature is
    // written, not derived. Before they were on the barrel a consumer had to
    // reach for `Extract<NonNullable<SemanticEvent['value']>, …>` because the
    // names were generated but unreachable from the package entry point.
    // `value` is nullable because a qualified value may be unknown — the
    // generated type says so, which is the reason to import it rather than
    // hand-write an optimistic one.
    const readUsage = (fact: UsageRevisionV2Fact): number | null => fact.buckets.input_tokens.value;

    const { root, transcript } = fixtureTree();
    const observer = attach(root, transcript);
    const events = await collectUntil(observer, (seen) => seen.some((e) => e.type === 'bootstrap_complete'));

    const usage = events.find((event) => event.type === 'usage_v2');
    assert.ok(usage && isSemanticEvent(usage));
    const value: RuntimeSemanticValue | null = usage.value;
    assert.ok(value && 'UsageRevisionV2' in value, 'the usage family carries its own variant');
    assert.equal(readUsage(value.UsageRevisionV2), 10, 'the fixture response reports its input tokens');
  });

  test('an invalid locator fails at attach, not as an event', () => {
    const { root } = fixtureTree();
    assert.throws(
      () =>
        observeSession({
          adapter_id: 'claude-code',
          agent_root: root,
          transcript_path: path.join(root, 'elsewhere', `${SESSION}.jsonl`),
        }),
      /projects/,
    );
  });
});
