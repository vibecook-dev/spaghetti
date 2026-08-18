import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError } from '../rfc012a.js';
import { parseScopedArtifactReadContext, parseScopedObservedArtifact } from '../rfc012d-artifact.js';

const fixture = JSON.parse(
  readFileSync(
    new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012d-scoped-artifact-v1.json', import.meta.url),
    'utf8',
  ),
) as { context: Record<string, any>; available: Record<string, any> };

function clone<T>(value: T): T {
  return structuredClone(value);
}

function reject(value: unknown, context: unknown = fixture.context): void {
  assert.throws(() => parseScopedObservedArtifact(value, context), ContractValidationError);
}

function unavailable(
  reason: string,
  observedGeneration: number | null,
  observedBytes: number | null,
  provenanceRef: string | null,
): Record<string, unknown> {
  const value = clone(fixture.available);
  value.outcome = {
    kind: 'unavailable',
    reason,
    observed_generation: observedGeneration,
    observed_bytes: observedBytes,
    provenance_ref: provenanceRef,
    completeness: 'unavailable',
  };
  return value;
}

test('portable TypeScript independently parses the Rust scoped artifact fixture', () => {
  const context = parseScopedArtifactReadContext(fixture.context);
  const result = parseScopedObservedArtifact(fixture.available, fixture.context);
  assert.equal(context.content_policy, 'inline');
  assert.equal(result.outcome.kind, 'available');
  if (result.outcome.kind === 'available') {
    assert.equal(atob(result.outcome.content_base64!), 'echo bounded artifact\n');
    assert.equal(result.outcome.size_bytes, 22);
  }
  assert.deepEqual(JSON.parse(JSON.stringify(result)), fixture.available);
});

test('scoped artifact responses bind to the exact caller-held request', () => {
  for (const mutate of [
    (value: Record<string, any>) => (value.request.max_bytes = 4095),
    (value: Record<string, any>) => (value.request.expected_generation = 8),
    (value: Record<string, any>) => (value.request.artifact_kind = 'workflow_journal'),
    (value: Record<string, any>) => (value.request.request_id = value.request.attachment_ref),
    (value: Record<string, any>) => (value.request.unexpected = true),
    (value: Record<string, any>) => (value.outcome.generation = 8),
    (value: Record<string, any>) => (value.locator_disclosure = 'disclosed'),
  ]) {
    const value = clone(fixture.available);
    mutate(value);
    reject(value);
  }
});

test('scoped artifact inline content is canonical, bounded, and internally consistent', () => {
  for (const mutate of [
    (value: Record<string, any>) => (value.outcome.content_base64 = '***'),
    (value: Record<string, any>) => (value.outcome.content_base64 = 'ZWNobyBib3VuZGVkIGFydGlmYWN0Cg'),
    (value: Record<string, any>) => (value.outcome.size_bytes = 21),
    (value: Record<string, any>) => (value.outcome.content_hash = null),
    (value: Record<string, any>) => (value.outcome.completeness = 'partial'),
  ]) {
    const value = clone(fixture.available);
    mutate(value);
    reject(value);
  }
  const huge = clone(fixture.available);
  huge.outcome.content_base64 = 'A'.repeat(11_184_813);
  reject(huge);
});

test('scoped artifact metadata-only and hash-only disclosure stay exact', () => {
  const metadataContext = clone(fixture.context);
  metadataContext.content_policy = 'metadata_only';
  const metadata = clone(fixture.available);
  metadata.request.content_policy = 'metadata_only';
  metadata.outcome.content_hash = null;
  metadata.outcome.content_base64 = null;
  assert.doesNotThrow(() => parseScopedObservedArtifact(metadata, metadataContext));
  metadata.outcome.content_hash = fixture.available.outcome.content_hash;
  reject(metadata, metadataContext);

  const hashContext = clone(fixture.context);
  hashContext.content_policy = 'hash_only';
  const hash = clone(fixture.available);
  hash.request.content_policy = 'hash_only';
  hash.outcome.content_base64 = null;
  assert.doesNotThrow(() => parseScopedObservedArtifact(hash, hashContext));
  delete hash.outcome.content_hash;
  reject(hash, hashContext);
});

test('scoped artifact changed-generation and over-limit evidence is exact', () => {
  const provenance = fixture.available.outcome.provenance_ref as string;
  assert.doesNotThrow(() =>
    parseScopedObservedArtifact(unavailable('changed_generation', 8, null, provenance), fixture.context),
  );
  reject(unavailable('changed_generation', 7, null, provenance));
  assert.doesNotThrow(() =>
    parseScopedObservedArtifact(unavailable('over_limit', 7, 4097, provenance), fixture.context),
  );
  reject(unavailable('over_limit', 7, 4096, provenance));
  reject(unavailable('missing', null, 4097, null));
});

test('scoped artifact ordinary unavailable reasons remain typed and locator-free', () => {
  for (const reason of ['out_of_scope', 'denied', 'missing', 'unsupported', 'malformed', 'unstable']) {
    const result = parseScopedObservedArtifact(unavailable(reason, null, null, null), fixture.context);
    assert.deepEqual(result.outcome, {
      kind: 'unavailable',
      reason,
      observed_generation: null,
      observed_bytes: null,
      provenance_ref: null,
      completeness: 'unavailable',
    });
    assert.equal(JSON.stringify(result).includes('/Users/'), false);
  }
});

test('scoped artifact parsing rejects unsafe numbers, zero identities, and silent fields', () => {
  const unsafe = clone(fixture.context);
  unsafe.max_bytes = Number.MAX_SAFE_INTEGER + 1;
  assert.throws(() => parseScopedArtifactReadContext(unsafe), ContractValidationError);

  const zero = clone(fixture.context);
  zero.artifact_key = 'v1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA';
  assert.throws(() => parseScopedArtifactReadContext(zero), ContractValidationError);

  const unknown = clone(fixture.context);
  unknown.locator = '/Users/alice/private';
  assert.throws(() => parseScopedArtifactReadContext(unknown), ContractValidationError);

  const exotic = Object.create({ inherited: true }) as Record<string, unknown>;
  Object.assign(exotic, fixture.context);
  assert.throws(() => parseScopedArtifactReadContext(exotic), ContractValidationError);
});

test('scoped artifact parsing does not depend on the Node Buffer global', () => {
  const globalWithBuffer = globalThis as unknown as { Buffer?: unknown };
  const original = globalWithBuffer.Buffer;
  try {
    globalWithBuffer.Buffer = undefined;
    assert.deepEqual(parseScopedObservedArtifact(fixture.available, fixture.context), fixture.available);
  } finally {
    globalWithBuffer.Buffer = original;
  }
});
