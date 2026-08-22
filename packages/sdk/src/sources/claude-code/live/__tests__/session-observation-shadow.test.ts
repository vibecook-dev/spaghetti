/** Feature-flagged RFC 012D typed-observer shadow and rollback. */

import assert from 'node:assert/strict';
import { readFileSync, writeFileSync } from 'node:fs';
import { mkdtemp } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { afterEach, describe, test } from 'node:test';

import type {
  ScopedBootstrapCompletionBarrier,
  ScopedResyncCompletionBarrier,
} from '../../../../contracts/rfc012d-completion-envelope.js';
import type { ScopedObservationEventEnvelope } from '../../../../contracts/rfc012d-event-envelope.js';
import type { ScopedObservationWatermark } from '../../../../contracts/rfc012d-watermark.js';
import type { ObservationCapabilities } from '../../../../contracts/rfc012d.js';
import type { SessionObservationRequest, SessionObserver } from '../../../../scoped-observation.js';
import type { SessionObservationShadowEntityEnvelope } from '../session-observation-epoch.js';
import {
  isSessionObservationShadowTail,
  watchSessionObservationShadow,
  watchSessionObservationShadowWithFactory,
} from '../session-observation-shadow.js';
import type { SessionTranscriptTail } from '../session-tail.js';

type JsonObject = Record<string, any>;

const negotiation = fixture('rfc012d-observation-negotiation-v1.json');
const usage = fixture('rfc012d-scoped-usage-envelope-v1.json');
const completion = fixture('rfc012d-scoped-completion-envelope-v1.json');
const continuity = fixture('rfc012d-scoped-continuity-envelope-v1.json');
const capabilities = fixture('rfc012d-scoped-capability-snapshot-v1.json');
const watermark = fixture('rfc012d-scoped-watermark-v2.json');

function fixture(name: string): JsonObject {
  return JSON.parse(
    readFileSync(
      new URL(`../../../../../../../crates/spaghetti-napi/fixtures/contracts/${name}`, import.meta.url),
      'utf8',
    ),
  ) as JsonObject;
}

function clone<T>(value: T): T {
  return structuredClone(value);
}

function outer(family: string, context: JsonObject, event: JsonObject): ScopedObservationEventEnvelope {
  return {
    scoped_observation_event_union_contract_version: 1,
    family,
    context,
    event,
  } as ScopedObservationEventEnvelope;
}

function sequence(): {
  bootstrap: ScopedObservationEventEnvelope[];
  invalidation: ScopedObservationEventEnvelope;
  correction: ScopedObservationEventEnvelope[];
  bootstrapBarrier: ScopedBootstrapCompletionBarrier;
  resyncBarrier: ScopedResyncCompletionBarrier;
} {
  const usageUpsert = clone(usage.upsert);
  usageUpsert.observer_sequence = 1;
  usageUpsert.scope_epoch = 1;
  usageUpsert.phase = 'bootstrap';

  const bootstrap = clone(completion.bootstrap.event);
  bootstrap.observer_sequence = 2;
  bootstrap.scope_epoch = 1;
  bootstrap.event.barrier.scope_epoch = 1;

  const required = clone(continuity.resync_required.watcher_overflow);
  required.observer_sequence = 4;
  required.scope_epoch = 1;
  required.event.invalid_scope_epoch = 1;

  const started = clone(continuity.resync_started);
  started.observer_sequence = 5;
  started.scope_epoch = 2;
  started.event.old_scope_epoch = 1;
  started.event.new_scope_epoch = 2;

  const corrected = clone(usage.upsert);
  corrected.observer_sequence = 6;
  corrected.scope_epoch = 2;
  corrected.phase = 'correction';
  corrected.event_id = corrected.event_id.replace(/.$/, 'A');
  corrected.semantic_revision_ref = {
    ...corrected.semantic_revision_ref,
    fact_revision_id: corrected.semantic_revision_ref.fact_revision_id.replace(/.$/, 'B'),
  };

  const resync = clone(completion.resync.event);
  resync.observer_sequence = 7;
  resync.scope_epoch = 2;
  resync.event.barrier.scope_epoch = 2;

  return {
    bootstrap: [
      outer('usage', usage.context, usageUpsert),
      outer('completion', completion.bootstrap.context, bootstrap),
    ],
    invalidation: outer('continuity', continuity.contexts.resync_required, required),
    correction: [
      outer('continuity', continuity.contexts.resync_started, started),
      outer('usage', usage.context, corrected),
      outer('completion', completion.resync.context, resync),
    ],
    bootstrapBarrier: bootstrap.event.barrier as ScopedBootstrapCompletionBarrier,
    resyncBarrier: resync.event.barrier,
  };
}

interface Deferred<T> {
  promise: Promise<T>;
  resolve(value: T): void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

class ScriptedObserver implements SessionObserver {
  readonly script = sequence();
  readonly #ready = deferred<ScopedBootstrapCompletionBarrier>();
  readonly #resyncRequested = deferred<void>();
  readonly #resyncCompleted = deferred<ScopedResyncCompletionBarrier>();
  readonly #allowCompletion = deferred<void>();
  readonly #closed = deferred<void>();
  polls = 0;
  closes = 0;

  capabilities(): ObservationCapabilities {
    return capabilities.exact.snapshot.observation_capabilities as ObservationCapabilities;
  }

  async *events(): AsyncIterableIterator<ScopedObservationEventEnvelope> {
    for (const event of this.script.bootstrap) yield event;
    this.#ready.resolve(this.script.bootstrapBarrier);
    yield this.script.invalidation;
    await this.#resyncRequested.promise;
    yield this.script.correction[0]!;
    yield this.script.correction[1]!;
    await this.#allowCompletion.promise;
    yield this.script.correction[2]!;
    this.#resyncCompleted.resolve(this.script.resyncBarrier);
    await this.#closed.promise;
  }

  async consume(apply: (event: ScopedObservationEventEnvelope) => void | Promise<void>): Promise<void> {
    for await (const event of this.events()) await apply(event);
  }

  poll(): Promise<ScopedObservationWatermark> {
    this.polls += 1;
    return Promise.resolve(watermark.watermark as ScopedObservationWatermark);
  }

  ready(): Promise<ScopedBootstrapCompletionBarrier> {
    return this.#ready.promise;
  }

  resync(): Promise<ScopedResyncCompletionBarrier> {
    this.#resyncRequested.resolve();
    return this.#resyncCompleted.promise;
  }

  close(): Promise<void> {
    this.closes += 1;
    this.#allowCompletion.resolve();
    this.#closed.resolve();
    return Promise.resolve();
  }

  releaseResyncCompletion(): void {
    this.#allowCompletion.resolve();
  }
}

const request: SessionObservationRequest = {
  adapterId: 'claude-code',
  configuredRoots: ['/private/fixture-root'],
  programId: 'observe-root-transcript',
  knownObjectRelativePaths: { 'root-transcript': 'project/session.jsonl' },
  rootIdentity: {
    sessionIdentityKey: new TextEncoder().encode('native-session'),
    relationIdentityInputs: {
      'native-session-id': new TextEncoder().encode('native-session'),
      'transcript-locator': new TextEncoder().encode('project/session.jsonl'),
    },
  },
  contractRequest: negotiation.contract_request,
};

let tails: SessionTranscriptTail[] = [];
afterEach(async () => {
  for (const tail of tails) {
    if (isSessionObservationShadowTail(tail)) await tail.shadow.close();
    tail.stop();
  }
  tails = [];
});

async function transcript(): Promise<string> {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'session-observation-shadow-'));
  const file = path.join(directory, 'session.jsonl');
  writeFileSync(file, `${JSON.stringify({ role: 'user', content: 'legacy stays live' })}\n`);
  return file;
}

async function waitFor(predicate: () => boolean): Promise<void> {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (predicate()) return;
    await new Promise<void>((resolve) => setImmediate(resolve));
  }
  assert.fail('timed out waiting for shadow state');
}

function recordEventId(record: SessionObservationShadowEntityEnvelope | undefined): string | undefined {
  if (record === undefined) return undefined;
  return record.family === 'unknown_wire_event' ? record.event.envelope_provenance.event_id : record.event.event_id;
}

describe('watchSessionObservationShadow', () => {
  test('flag off is an exact legacy rollback and never opens an observer', async () => {
    const file = await transcript();
    let opens = 0;
    const tail = watchSessionObservationShadowWithFactory(file, { pollIntervalMs: 60_000 }, async () => {
      opens += 1;
      throw new Error('must not open');
    });
    tails.push(tail);
    const messages: unknown[] = [];
    tail.onMessage((event) => messages.push(event));
    await tail.poll();
    assert.equal(opens, 0);
    assert.equal(isSessionObservationShadowTail(tail), false);
    assert.equal(messages.length, 1);
  });

  test('enabled shadow requires an authority-bound typed request', async () => {
    const file = await transcript();
    assert.throws(
      () => watchSessionObservationShadow(file, { rfc012dObserver: true, pollIntervalMs: 60_000 }),
      /observerRequest/,
    );
  });

  test('a pending native attach never blocks the compatibility tail', async () => {
    const file = await transcript();
    const opened = deferred<SessionObserver>();
    const observer = new ScriptedObserver();
    const tail = watchSessionObservationShadowWithFactory(
      file,
      { rfc012dObserver: true, observerRequest: request, pollIntervalMs: 60_000 },
      () => opened.promise,
    );
    tails.push(tail);
    const legacy: unknown[] = [];
    tail.onMessage((event) => legacy.push(event));
    await tail.poll();
    assert.equal(legacy.length, 1);
    assert.equal(observer.polls, 0);
    opened.resolve(observer);
    assert.ok(isSessionObservationShadowTail(tail));
    await tail.shadow.ready();
  });

  test('close does not wait for a pending native attach and closes a late owner', async () => {
    const file = await transcript();
    const opened = deferred<SessionObserver>();
    const observer = new ScriptedObserver();
    const tail = watchSessionObservationShadowWithFactory(
      file,
      { rfc012dObserver: true, observerRequest: request, pollIntervalMs: 60_000 },
      () => opened.promise,
    );
    tails.push(tail);
    assert.ok(isSessionObservationShadowTail(tail));

    await tail.shadow.close();
    assert.equal(tail.shadow.snapshot().phase, 'closed');

    opened.resolve(observer);
    await waitFor(() => observer.closes === 1);
  });

  test('owned typed stream stages replacement and swaps only at completion', async () => {
    const file = await transcript();
    const observer = new ScriptedObserver();
    const tail = watchSessionObservationShadowWithFactory(
      file,
      { rfc012dObserver: true, observerRequest: request, pollIntervalMs: 60_000 },
      async () => observer,
    );
    tails.push(tail);
    assert.ok(isSessionObservationShadowTail(tail));
    const ready = await tail.shadow.ready();
    assert.equal(ready.phase, 'live');
    assert.equal(ready.scopeEpoch, 1);
    assert.equal(ready.records.length, 1);

    await waitFor(() => tail.shadow.snapshot().phase === 'resync');
    const staging = tail.shadow.snapshot();
    assert.equal(staging.scopeEpoch, 1);
    assert.equal(recordEventId(staging.records[0]), usage.upsert.event_id);

    observer.releaseResyncCompletion();
    await waitFor(() => tail.shadow.snapshot().scopeEpoch === 2);
    const replaced = tail.shadow.snapshot();
    assert.equal(replaced.phase, 'live');
    assert.equal(replaced.records.length, 1);
    assert.notEqual(recordEventId(replaced.records[0]), usage.upsert.event_id);

    const legacy: unknown[] = [];
    tail.onMessage((event) => legacy.push(event));
    await tail.poll();
    assert.equal(legacy.length, 1);
    await waitFor(() => observer.polls === 1);
  });

  test('observer failure is path-free and does not stop the legacy tail', async () => {
    const file = await transcript();
    const tail = watchSessionObservationShadowWithFactory(
      file,
      { rfc012dObserver: true, observerRequest: request, pollIntervalMs: 60_000 },
      async () => {
        throw new Error('/Users/alice/private/session.jsonl');
      },
    );
    tails.push(tail);
    assert.ok(isSessionObservationShadowTail(tail));
    const errors: Error[] = [];
    tail.onError((error) => errors.push(error));
    await assert.rejects(tail.shadow.ready(), /RFC 012D observation shadow failed/);
    await waitFor(() => errors.length === 1);
    assert.equal(tail.shadow.snapshot().phase, 'failed');
    assert.equal(tail.shadow.snapshot().failure, 'transport_failed');
    assert.doesNotMatch(errors[0]!.message, /Users|alice|private|session\.jsonl/);

    const legacy: unknown[] = [];
    tail.onMessage((event) => legacy.push(event));
    await tail.poll();
    assert.equal(legacy.length, 1);
  });
});
