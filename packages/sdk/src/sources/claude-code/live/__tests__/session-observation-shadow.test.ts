/**
 * session-observation-shadow.test.ts — feature-flagged RFC 012D shadow.
 *
 * Drives the real watchSessionTranscript path. The enabled flag records
 * SemanticRevisionRef/event_id from the Rust-produced usage envelope fixture
 * without calling the N-API observer. Turning the flag off returns the legacy
 * tail only.
 */

import { test, describe, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';

import { isSessionObservationShadowTail, watchSessionObservationShadow } from '../session-observation-shadow.js';
import { watchSessionTranscript, type SessionTranscriptTail } from '../session-tail.js';
import { rfc012dUsageEnvelopeShadowRecords } from '../rfc012d-usage-envelope-shadow.js';

const SESSION_ID = '3fef0014-58b0-4938-905e-ad50b553cb76';

const fixture = JSON.parse(
  readFileSync(
    new URL(
      '../../../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012d-scoped-usage-envelope-v1.json',
      import.meta.url,
    ),
    'utf8',
  ),
) as {
  upsert: { event_id: string; semantic_revision_ref: { fact_revision_id: string } };
  reset_retraction: { event_id: string; semantic_revision_ref: { fact_revision_id: string } };
};

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

  test('rfc012dObserver true runs the legacy tail and records fixture event_id/semantic refs', async () => {
    const dir = makeDir();
    const file = path.join(dir, 'later', `${SESSION_ID}.jsonl`);
    mkdirSync(path.join(dir, 'later'), { recursive: true });
    writeFileSync(file, line({ role: 'assistant', content: 'yo' }));

    const events: unknown[] = [];
    const tail = watchSessionObservationShadow(file, {
      pollIntervalMs: 60_000,
      rfc012dObserver: true,
    });
    tails.push(tail);
    tail.onMessage((event) => events.push(event));
    await tail.poll();

    assert.equal(isSessionObservationShadowTail(tail), true);
    if (!isSessionObservationShadowTail(tail)) return;
    assert.equal(tail.rfc012dObserver, true);
    assert.equal(events.length, 1);

    const records = tail.shadow.records();
    const helper = rfc012dUsageEnvelopeShadowRecords();
    assert.deepEqual(records, helper);
    assert.equal(records.length, 2);
    assert.equal(records[0]?.eventId, fixture.upsert.event_id);
    assert.equal(
      records[0]?.semanticRevisionRef.fact_revision_id,
      fixture.upsert.semantic_revision_ref.fact_revision_id,
    );
    assert.equal(records[1]?.eventId, fixture.reset_retraction.event_id);
    assert.equal(
      records[1]?.semanticRevisionRef.fact_revision_id,
      fixture.reset_retraction.semantic_revision_ref.fact_revision_id,
    );
    assert.notEqual(records[0]?.eventId, records[1]?.eventId);
  });

  test('turning rfc012dObserver off returns the legacy tail only', async () => {
    const dir = makeDir();
    const file = path.join(dir, `${SESSION_ID}.jsonl`);
    writeFileSync(file, line({ role: 'user', content: 'rollback' }));

    const enabled = watchSessionObservationShadow(file, {
      pollIntervalMs: 60_000,
      rfc012dObserver: true,
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
