import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError } from '../rfc012a.js';
import { parseNativeMarkerFixture, parseRfc012cNativeMarkerV1Json } from '../rfc012c.js';

const fixtureJson = readFileSync(
  new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012c-native-marker-v1.json', import.meta.url),
  'utf8',
);
const fixtureContext = JSON.parse(fixtureJson) as unknown;

function record(value: unknown): Record<string, unknown> {
  assert.ok(value !== null && typeof value === 'object' && !Array.isArray(value));
  return value as Record<string, unknown>;
}

function cloneFixture(): Record<string, unknown> {
  return structuredClone(fixtureContext) as Record<string, unknown>;
}

function markerSlot(fixture: Record<string, unknown>, marker: string, slot: string): Record<string, unknown> {
  return record(record(fixture[marker])[slot]);
}

test('RFC 012C native-marker fixture keeps native transitions typed and identity-bound', () => {
  const fixture = parseRfc012cNativeMarkerV1Json(fixtureJson, fixtureContext);
  assert.equal(fixture.family, 'runtime.native-marker');
  assert.equal(fixture.compaction.current.value.kind, 'compaction');
  assert.equal(fixture.progress.current.value.kind, 'progress');
  assert.equal(fixture.queue.current.value.kind, 'queue');
  assert.equal(fixture.progress.current.quality, 'native_claimed');
  assert.equal(fixture.compaction.current.quality, 'exact');
  assert.equal(fixture.compaction.current.correlated_native_id, null);
  assert.equal(fixture.progress.current.value.completed, 0);
  assert.equal(fixture.progress.correction.value.kind, 'progress');
  assert.equal(fixture.progress.correction.value.completed, 2);
  assert.equal(fixture.progress.retract.operation, 'retract');
  assert.equal(fixture.queue.partial.completeness, 'partial');
  assert.equal(new Set([fixture.compaction.fact_id, fixture.progress.fact_id, fixture.queue.fact_id]).size, 3);
  assert.doesNotMatch(JSON.stringify(fixture), /\/Users\/|~\/|\.db\b|claude-code|codex|grok/);
});

test('RFC 012C native-marker portable parser rejects stale identity and host assessments', () => {
  const semanticDrift = cloneFixture();
  record(markerSlot(semanticDrift, 'progress', 'current').value).state = 'completed';
  assert.throws(
    () => parseRfc012cNativeMarkerV1Json(JSON.stringify(semanticDrift), fixtureContext),
    ContractValidationError,
  );

  const hostAssessment = cloneFixture();
  markerSlot(hostAssessment, 'progress', 'current').quality = 'derived';
  assert.throws(() => parseNativeMarkerFixture(hostAssessment, hostAssessment), ContractValidationError);

  const impossible = cloneFixture();
  record(markerSlot(impossible, 'progress', 'current').value).completed = 3;
  assert.throws(() => parseNativeMarkerFixture(impossible, impossible), ContractValidationError);

  const oversized = cloneFixture();
  record(markerSlot(oversized, 'progress', 'current').value).completed = Number.MAX_SAFE_INTEGER + 1;
  assert.throws(() => parseNativeMarkerFixture(oversized, oversized), ContractValidationError);

  const pathProvenance = cloneFixture();
  record(markerSlot(pathProvenance, 'progress', 'current').provenance).native_field = '/Users/alice/progress';
  assert.throws(() => parseNativeMarkerFixture(pathProvenance, pathProvenance), ContractValidationError);

  const unknown = cloneFixture();
  record(markerSlot(unknown, 'queue', 'current').value).future = true;
  assert.throws(() => parseNativeMarkerFixture(unknown, unknown), ContractValidationError);
});

test('RFC 012C native-marker nullable and digest fields are required, dense, and nonzero', () => {
  const missingNullable = cloneFixture();
  delete markerSlot(missingNullable, 'compaction', 'current').correlated_native_id;
  assert.throws(() => parseNativeMarkerFixture(missingNullable, missingNullable), ContractValidationError);

  const missingValueNullable = cloneFixture();
  delete record(markerSlot(missingValueNullable, 'compaction', 'current').value).pre_tokens;
  assert.throws(() => parseNativeMarkerFixture(missingValueNullable, missingValueNullable), ContractValidationError);

  const sparseDigest = cloneFixture();
  const sparse = record(markerSlot(sparseDigest, 'queue', 'current').value).item_digest as number[];
  delete sparse[31];
  assert.throws(() => parseNativeMarkerFixture(sparseDigest, sparseDigest), ContractValidationError);

  const zeroDigest = cloneFixture();
  record(markerSlot(zeroDigest, 'queue', 'current').value).item_digest = new Array<number>(32).fill(0);
  assert.throws(() => parseNativeMarkerFixture(zeroDigest, zeroDigest), ContractValidationError);

  const negativeZero = cloneFixture();
  markerSlot(negativeZero, 'queue', 'current').effective_at = -0;
  assert.throws(() => parseNativeMarkerFixture(negativeZero, negativeZero), ContractValidationError);

  assert.throws(
    () => parseRfc012cNativeMarkerV1Json(fixtureJson.replace('"completed": 0', '"completed": 0.0'), fixtureContext),
    ContractValidationError,
  );
});

test('RFC 012C native-marker caller-held context binds valid-shaped identity fields', () => {
  const keyDrift = cloneFixture();
  markerSlot(keyDrift, 'progress', 'current').semantic_revision_key_hex = 'ab'.repeat(32);
  assert.throws(() => parseNativeMarkerFixture(keyDrift, fixtureContext), ContractValidationError);

  const refDrift = cloneFixture();
  markerSlot(refDrift, 'progress', 'current').semantic_revision_ref = markerSlot(
    refDrift,
    'queue',
    'current',
  ).semantic_revision_ref;
  assert.throws(() => parseNativeMarkerFixture(refDrift, fixtureContext), ContractValidationError);

  const factDrift = cloneFixture();
  record(factDrift.queue).fact_id = record(factDrift.progress).fact_id;
  assert.throws(() => parseNativeMarkerFixture(factDrift, fixtureContext), ContractValidationError);
});
