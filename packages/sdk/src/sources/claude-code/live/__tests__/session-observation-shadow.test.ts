/**
 * session-observation-shadow.test.ts — feature-flagged RFC 012D shadow.
 *
 * Drives the real watchSessionTranscript path. The enabled flag consumes a
 * live typed observer source with scope-epoch swap/rollback. Fixture event_ids
 * are not compared against themselves.
 */

import { test, describe, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';

import {
  isSessionObservationShadowTail,
  watchSessionObservationShadow,
  type ObserverRecordSource,
  type ScopedUsageShadowRecord,
} from '../session-observation-shadow.js';
import { watchSessionTranscript, type SessionTranscriptTail } from '../session-tail.js';

const SESSION_ID = '3fef0014-58b0-4938-905e-ad50b553cb76';

const revision = (id: string) =>
  ({
    semantic_reference_contract_version: 1,
    fact_revision_id: id,
  }) as ScopedUsageShadowRecord['semanticRevisionRef'];

function liveSource(records: ScopedUsageShadowRecord[], epoch = 1): ObserverRecordSource {
  return {
    scopeEpoch: () => epoch,
    poll: () => records,
  };
}

let tails: SessionTranscriptTail[] = [];
afterEach(() => {
  for (const tail of tails) tail.stop();
  tails = [];
});

function makeDir(): string {
  return mkdtempSync(path.join(os.tmpdir(), 'session-observation-shadow-'));
}

const line = (record: object) => `${JSON.stringify(record)}\n`;

describe('watchSessionObservationShadow', () => {
  test('rfc012dObserver default false delegates to the legacy transcript tail', async () => {
    const dir = makeDir();
    const file = path.join(dir, `${SESSION_ID}.jsonl`);
    writeFileSync(file, line({ role: 'user', content: 'hi' }));

    const events: unknown[] = [];
    const tail = watchSessionObservationShadow(file, { pollIntervalMs: 60_000 });
    tails.push(tail);
    tail.onMessage((event) => events.push(event));
    await tail.poll();

    assert.equal(isSessionObservationShadowTail(tail), false);
    assert.equal(Object.hasOwn(tail, 'shadow'), false);
    assert.equal(Object.hasOwn(tail, 'rfc012dObserver'), false);
    assert.equal(events.length, 1);

    const legacy = watchSessionTranscript(file, { pollIntervalMs: 60_000 });
    tails.push(legacy);
    assert.deepEqual(Object.keys(tail).sort(), Object.keys(legacy).sort());
  });

  test('rfc012dObserver true runs the legacy tail and live observer records', async () => {
    const dir = makeDir();
    const file = path.join(dir, 'later', `${SESSION_ID}.jsonl`);
    mkdirSync(path.join(dir, 'later'), { recursive: true });
    writeFileSync(file, line({ role: 'assistant', content: 'yo' }));

    const live: ScopedUsageShadowRecord[] = [
      {
        eventId: 'live-upsert-1',
        factId: 'fact-a',
        semanticRevisionRef: revision('rev-a'),
        operation: 'upsert',
        scopeEpoch: 1,
      },
    ];
    const events: unknown[] = [];
    const tail = watchSessionObservationShadow(file, {
      pollIntervalMs: 60_000,
      rfc012dObserver: true,
      observerSource: liveSource(live, 1),
    });
    tails.push(tail);
    tail.onMessage((event) => events.push(event));
    await tail.poll();

    assert.equal(isSessionObservationShadowTail(tail), true);
    if (!isSessionObservationShadowTail(tail)) return;
    assert.equal(tail.rfc012dObserver, true);
    assert.equal(events.length, 1);
    const records = tail.shadow.records();
    assert.equal(records.length, 1);
    assert.equal(records[0]?.eventId, 'live-upsert-1');
    assert.equal(records[0]?.scopeEpoch, 1);
    assert.notEqual(records[0]?.eventId, 'rev-a');
  });

  test('epoch swap drops old-epoch records and admits the new epoch', () => {
    const dir = makeDir();
    const file = path.join(dir, `${SESSION_ID}.jsonl`);
    writeFileSync(file, line({ role: 'user', content: 'swap' }));

    const queue: ScopedUsageShadowRecord[] = [
      {
        eventId: 'epoch-1',
        factId: 'fact-a',
        semanticRevisionRef: revision('rev-a'),
        operation: 'upsert',
        scopeEpoch: 1,
      },
    ];
    const tail = watchSessionObservationShadow(file, {
      pollIntervalMs: 60_000,
      rfc012dObserver: true,
      observerSource: {
        scopeEpoch: () => queue[0]?.scopeEpoch ?? 1,
        poll: () => queue,
      },
    });
    tails.push(tail);
    assert.equal(isSessionObservationShadowTail(tail), true);
    if (!isSessionObservationShadowTail(tail)) return;
    assert.equal(tail.shadow.records()[0]?.eventId, 'epoch-1');
    tail.shadow.swapEpoch(2);
    assert.equal(tail.shadow.scopeEpoch(), 2);
    assert.deepEqual(tail.shadow.records(), []);
    queue.splice(0, queue.length, {
      eventId: 'epoch-2',
      factId: 'fact-a',
      semanticRevisionRef: revision('rev-b'),
      operation: 'upsert',
      scopeEpoch: 2,
    });
    assert.equal(tail.shadow.records()[0]?.eventId, 'epoch-2');
    assert.throws(() => tail.shadow.swapEpoch(2), /strictly greater/);
  });

  test('turning rfc012dObserver off returns the legacy tail only', async () => {
    const dir = makeDir();
    const file = path.join(dir, `${SESSION_ID}.jsonl`);
    writeFileSync(file, line({ role: 'user', content: 'rollback' }));

    const enabled = watchSessionObservationShadow(file, {
      pollIntervalMs: 60_000,
      rfc012dObserver: true,
      observerSource: liveSource([], 1),
    });
    const disabled = watchSessionObservationShadow(file, {
      pollIntervalMs: 60_000,
      rfc012dObserver: false,
    });
    tails.push(enabled, disabled);

    assert.equal(isSessionObservationShadowTail(enabled), true);
    assert.equal(isSessionObservationShadowTail(disabled), false);
    assert.equal(Object.hasOwn(disabled, 'shadow'), false);
    assert.equal(Object.hasOwn(disabled, 'rfc012dObserver'), false);

    const events: unknown[] = [];
    disabled.onMessage((event) => events.push(event));
    await disabled.poll();
    assert.equal(events.length, 1);
  });
});
