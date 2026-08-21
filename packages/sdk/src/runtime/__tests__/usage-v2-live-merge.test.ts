import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { parseSourceCoverageSet, type SourceCoverageSet } from '../../contracts/rfc012a.js';
import { parseRfc012cRuntimeV1Json } from '../../contracts/rfc012c.js';
import { mergeDurableAndScopedUsage } from '../usage-v2-live-merge.js';

const rfc012a = JSON.parse(
  readFileSync(
    new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012a-v1.json', import.meta.url),
    'utf8',
  ),
) as { coverage: { baseline: unknown; dominant: unknown } };

const rfc012cJson = readFileSync(
  new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012c-runtime-v1.json', import.meta.url),
  'utf8',
);

function coverage(value: unknown): SourceCoverageSet {
  return parseSourceCoverageSet(value);
}

test('SDK merge consumer joins typed durable and scoped usage without native payloads', () => {
  const runtime = parseRfc012cRuntimeV1Json(rfc012cJson, JSON.parse(rfc012cJson));
  const aba = runtime.usage.response_revisions;
  const baseline = coverage(rfc012a.coverage.baseline);
  const partial: SourceCoverageSet = { ...baseline, completeness: 'partial' };

  const merged = mergeDurableAndScopedUsage(
    [],
    baseline,
    [
      {
        eventId: 'evt-a-1',
        factId: aba.a.fact_id,
        semanticRevisionRef: aba.a.semantic_revision_ref,
        operation: 'upsert',
      },
      {
        eventId: 'evt-b',
        factId: aba.b.fact_id,
        semanticRevisionRef: aba.b.semantic_revision_ref,
        operation: 'upsert',
      },
      {
        eventId: 'evt-a-1',
        factId: aba.a.fact_id,
        semanticRevisionRef: aba.a.semantic_revision_ref,
        operation: 'upsert',
      },
      {
        eventId: 'evt-a-2',
        factId: aba.a_repeat.fact_id,
        semanticRevisionRef: aba.a_repeat.semantic_revision_ref,
        operation: 'upsert',
      },
    ],
    partial,
  );

  assert.deepEqual(merged.overlay, { retained: { stale: true } });
  assert.equal(merged.deliveredObserverOccurrences.length, 3);
  assert.equal(merged.deliveredObserverOccurrences[0]?.eventId, 'evt-a-1');
  assert.equal(merged.deliveredObserverOccurrences[2]?.eventId, 'evt-a-2');
  assert.equal(merged.contributions.length, 1);
  assert.equal(merged.contributions[0]?.origin, 'overlay');
});

test('retract drops durable facts that are not re-upserted', () => {
  const runtime = parseRfc012cRuntimeV1Json(rfc012cJson, JSON.parse(rfc012cJson));
  const example = runtime.usage.response_revisions.a;
  const baseline = coverage(rfc012a.coverage.baseline);
  const partial: SourceCoverageSet = { ...baseline, completeness: 'partial' };
  const merged = mergeDurableAndScopedUsage(
    [
      {
        factId: example.fact_id,
        semanticRevisionRef: example.semantic_revision_ref,
      },
    ],
    baseline,
    [
      {
        eventId: 'evt-retract',
        factId: example.fact_id,
        semanticRevisionRef: example.semantic_revision_ref,
        operation: 'retract',
      },
    ],
    partial,
  );
  assert.deepEqual(merged.overlay, { retained: { stale: true } });
  assert.equal(merged.contributions.length, 0);
});

test('complete comparable observer coverage retires the overlay', () => {
  const runtime = parseRfc012cRuntimeV1Json(rfc012cJson, JSON.parse(rfc012cJson));
  const example = runtime.usage.response_revisions.a;
  const baseline = coverage(rfc012a.coverage.baseline);
  const merged = mergeDurableAndScopedUsage(
    [
      {
        factId: example.fact_id,
        semanticRevisionRef: example.semantic_revision_ref,
      },
    ],
    baseline,
    [
      {
        eventId: 'evt-ignored-after-retire',
        factId: example.fact_id,
        semanticRevisionRef: example.semantic_revision_ref,
        operation: 'upsert',
      },
    ],
    baseline,
  );
  assert.equal(merged.overlay, 'retired');
  assert.equal(merged.contributions[0]?.origin, 'durable');
});

test('newer complete observer coverage remains live until durable coverage subsumes it', () => {
  const runtime = parseRfc012cRuntimeV1Json(rfc012cJson, JSON.parse(rfc012cJson));
  const durableExample = runtime.usage.native_message;
  const liveExample = runtime.usage.source_record_fallback;
  const baseline = coverage(rfc012a.coverage.baseline);
  const dominant = coverage(rfc012a.coverage.dominant);

  const merged = mergeDurableAndScopedUsage(
    [
      {
        factId: durableExample.fact_id,
        semanticRevisionRef: durableExample.semantic_revision_ref,
      },
    ],
    baseline,
    [
      {
        eventId: 'evt-current-overlay',
        factId: liveExample.fact_id,
        semanticRevisionRef: liveExample.semantic_revision_ref,
        operation: 'upsert',
      },
    ],
    dominant,
  );

  assert.deepEqual(merged.overlay, { retained: { stale: false } });
  assert.equal(merged.contributions.length, 2);
  assert.ok(merged.contributions.some((item) => item.factId === liveExample.fact_id && item.origin === 'overlay'));
});

test('incomplete durable coverage cannot retire an equal complete observer overlay', () => {
  const runtime = parseRfc012cRuntimeV1Json(rfc012cJson, JSON.parse(rfc012cJson));
  const durableExample = runtime.usage.native_message;
  const liveExample = runtime.usage.source_record_fallback;
  const baseline = coverage(rfc012a.coverage.baseline);
  const incompleteDurable: SourceCoverageSet = { ...baseline, completeness: 'partial' };

  const merged = mergeDurableAndScopedUsage(
    [
      {
        factId: durableExample.fact_id,
        semanticRevisionRef: durableExample.semantic_revision_ref,
      },
    ],
    incompleteDurable,
    [
      {
        eventId: 'evt-incomplete-durable',
        factId: liveExample.fact_id,
        semanticRevisionRef: liveExample.semantic_revision_ref,
        operation: 'upsert',
      },
    ],
    baseline,
  );

  assert.deepEqual(merged.overlay, { retained: { stale: false } });
  assert.equal(merged.contributions.length, 2);
});
