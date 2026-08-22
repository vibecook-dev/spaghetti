import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError } from '../rfc012a.js';
import { parseUnknownEvidenceSnapshot, parseUnknownEvidenceSnapshotContext } from '../rfc012c-unknown-evidence.js';

interface FixtureSample {
  context: any;
  snapshot: any;
}

interface UnknownEvidenceFixture {
  fixture_contract_version: number;
  empty: FixtureSample;
  populated: FixtureSample;
  expected: {
    complete_count: number;
    sample_count: number;
    raw_native_values_disclosed: boolean;
    source_locator_disclosed: boolean;
    source_access_authority: boolean;
    durable_query: boolean;
    ordered_observer_event: boolean;
    replacement_barrier: boolean;
    aggregate_digest_algorithm: string;
    sampling_rank_basis: string;
  };
}

const fixture = JSON.parse(
  readFileSync(
    new URL(
      '../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012c-unknown-evidence-snapshot-v1.json',
      import.meta.url,
    ),
    'utf8',
  ),
) as UnknownEvidenceFixture;

function clone<T>(value: T): T {
  return structuredClone(value);
}

function reject(value: unknown, context: unknown = fixture.populated.context): void {
  assert.throws(() => parseUnknownEvidenceSnapshot(value, context), ContractValidationError);
}

test('portable TypeScript parses the empty and populated Rust snapshots', () => {
  assert.equal(fixture.fixture_contract_version, 1);
  for (const sample of [fixture.empty, fixture.populated]) {
    const context = parseUnknownEvidenceSnapshotContext(sample.context);
    assert.deepEqual(context, sample.context);
    assert.deepEqual(parseUnknownEvidenceSnapshot(sample.snapshot, context), sample.snapshot);
  }
  assert.equal(fixture.populated.snapshot.complete_count, fixture.expected.complete_count);
  assert.equal(fixture.populated.snapshot.samples.length, fixture.expected.sample_count);
});

test('aggregate totals digest and deterministic samples remain caller-held', () => {
  reject(fixture.populated.snapshot, fixture.empty.context);

  const digest = clone(fixture.populated.snapshot);
  digest.aggregate_digest = fixture.empty.snapshot.aggregate_digest;
  reject(digest);

  const sample = clone(fixture.populated.snapshot);
  sample.samples.reverse();
  reject(sample);

  const expectedSample = clone(fixture.populated.context);
  expectedSample.expected_samples[0].family_hint = 'future.changed';
  reject(fixture.populated.snapshot, expectedSample);
});

test('strict shape versions bounds digests and sanitized excerpts fail closed', () => {
  const unknown = clone(fixture.populated.snapshot);
  unknown.future_meaning = true;
  reject(unknown);

  const nestedUnknown = clone(fixture.populated.snapshot);
  nestedUnknown.samples[0].native_path = '/Users/alice/private';
  reject(nestedUnknown);

  const omittedNullable = clone(fixture.populated.snapshot);
  delete omittedNullable.samples[2].family_hint;
  reject(omittedNullable);

  const version = clone(fixture.populated.snapshot);
  version.unknown_evidence_snapshot_contract_version = 2;
  reject(version);

  const zero = clone(fixture.populated.snapshot);
  zero.aggregate_digest = `v1:${'A'.repeat(43)}`;
  reject(zero);

  const oversizedCount = clone(fixture.populated.snapshot);
  oversizedCount.complete_count = 65_537;
  reject(oversizedCount);

  const rawValue = clone(fixture.populated.snapshot);
  rawValue.samples[0].sanitized_excerpt.private = 'secret';
  reject(rawValue);

  const wrongHash = clone(fixture.populated.snapshot);
  wrongHash.samples[0].sanitized_excerpt.hash = '0'.repeat(64);
  reject(wrongHash);

  const impossibleMembers = clone(fixture.populated.snapshot);
  impossibleMembers.samples[0].sanitized_excerpt.members = 10_000;
  reject(impossibleMembers);

  const hugeDigest = clone(fixture.populated.snapshot);
  hugeDigest.aggregate_digest = `v1:${'A'.repeat(1_000_000)}`;
  reject(hugeDigest);
});

test('complete totals and exact sample policy remain coherent', () => {
  const missing = clone(fixture.populated.snapshot);
  missing.samples.pop();
  reject(missing);

  const duplicate = clone(fixture.populated.snapshot);
  duplicate.samples[1] = clone(duplicate.samples[0]);
  reject(duplicate);

  const bytes = clone(fixture.populated.snapshot);
  bytes.complete_observed_bytes = 1;
  reject(bytes);
});

test('the public value discloses only closed shape evidence and grants no authority', () => {
  assert.equal(fixture.expected.raw_native_values_disclosed, false);
  assert.equal(fixture.expected.source_locator_disclosed, false);
  assert.equal(fixture.expected.source_access_authority, false);
  assert.equal(fixture.expected.durable_query, false);
  assert.equal(fixture.expected.ordered_observer_event, false);
  assert.equal(fixture.expected.replacement_barrier, false);
  assert.equal(fixture.expected.aggregate_digest_algorithm, 'blake3-256');
  assert.equal(fixture.expected.sampling_rank_basis, 'source_record_id');
  const encoded = JSON.stringify(fixture.populated);
  assert.doesNotMatch(encoded, /secret|\/Users\/|alice|private bytes/);
});

test('portable unknown-evidence parsing does not depend on the Node Buffer global', () => {
  const globalWithBuffer = globalThis as unknown as { Buffer?: unknown };
  const original = globalWithBuffer.Buffer;
  try {
    globalWithBuffer.Buffer = undefined;
    assert.deepEqual(
      parseUnknownEvidenceSnapshot(fixture.populated.snapshot, fixture.populated.context),
      fixture.populated.snapshot,
    );
  } finally {
    globalWithBuffer.Buffer = original;
  }
});
