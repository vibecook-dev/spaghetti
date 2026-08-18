import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError } from '../rfc012a.js';
import { parseScopedCapabilitySnapshot, parseScopedCapabilitySnapshotContext } from '../rfc012d-capability-snapshot.js';

interface CapabilitySnapshotFixture {
  fixture_contract_version: number;
  exact: { context: any; snapshot: any };
  range: { context: any; snapshot: any };
  expected: {
    selected_family: string;
    unselected_family: string;
    phase_independent: boolean;
    coverage_or_readiness_claim: boolean;
    bootstrap_or_resync_barrier: boolean;
    artifact_availability_state: boolean;
    source_access_authority: boolean;
    portable_observer_transport: boolean;
    native_payload_disclosure: string;
    semantic_digest_algorithm: string;
  };
}

const fixture = JSON.parse(
  readFileSync(
    new URL(
      '../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012d-scoped-capability-snapshot-v1.json',
      import.meta.url,
    ),
    'utf8',
  ),
) as CapabilitySnapshotFixture;

function clone<T>(value: T): T {
  return structuredClone(value);
}

function reject(value: unknown, context: unknown = fixture.exact.context): void {
  assert.throws(() => parseScopedCapabilitySnapshot(value, context), ContractValidationError);
}

test('portable TypeScript parses exact and range Rust capability snapshots', () => {
  assert.equal(fixture.fixture_contract_version, 1);
  for (const sample of [fixture.exact, fixture.range]) {
    const context = parseScopedCapabilitySnapshotContext(sample.context);
    assert.deepEqual(context, sample.context);
    assert.deepEqual(parseScopedCapabilitySnapshot(sample.snapshot, context), sample.snapshot);
  }
  assert.notEqual(fixture.exact.snapshot.semantic_digest, fixture.range.snapshot.semantic_digest);
  assert.equal(fixture.exact.snapshot.observation_capabilities.fact_families[0].status, 'unsupported');
  assert.equal(fixture.exact.snapshot.observation_capabilities.fact_families[1].status, 'supported');
  assert.equal(fixture.range.snapshot.observation_capabilities.fact_families[1].status, 'degraded');
});

test('selection offer compatibility release and semantic digest remain caller-held', () => {
  reject(fixture.exact.snapshot, fixture.range.context);

  const selection = clone(fixture.exact.snapshot);
  selection.observation_capabilities.selection.event_contract_version = 2;
  reject(selection);

  const offer = clone(fixture.exact.context);
  offer.contract_offer.event_contract_versions = [2];
  reject(fixture.exact.snapshot, offer);

  const compatibility = clone(fixture.exact.context);
  compatibility.compatibility_class = 'RangeSupported';
  reject(fixture.exact.snapshot, compatibility);

  const release = clone(fixture.exact.context);
  release.support_release_id = 'different-release';
  reject(fixture.exact.snapshot, release);

  const digest = clone(fixture.exact.snapshot);
  digest.semantic_digest = fixture.range.snapshot.semantic_digest;
  reject(digest);
});

test('strict shapes versions capability semantics and digest encoding fail closed', () => {
  const topUnknown = clone(fixture.exact.snapshot);
  topUnknown.future_meaning = true;
  reject(topUnknown);

  const nestedUnknown = clone(fixture.exact.snapshot);
  nestedUnknown.observation_capabilities.future_meaning = true;
  reject(nestedUnknown);

  const familyUnknown = clone(fixture.exact.snapshot);
  familyUnknown.observation_capabilities.fact_families[0].future_meaning = true;
  reject(familyUnknown);

  const version = clone(fixture.exact.snapshot);
  version.capability_digest_contract_version = 2;
  reject(version);

  const status = clone(fixture.exact.snapshot);
  status.observation_capabilities.fact_families[1].status = 'degraded';
  reject(status);

  const reordered = clone(fixture.exact.snapshot);
  reordered.observation_capabilities.fact_families.reverse();
  reject(reordered);

  const shortDigest = clone(fixture.exact.snapshot);
  shortDigest.semantic_digest = 'v1:AQ';
  reject(shortDigest);

  const zeroDigest = clone(fixture.exact.snapshot);
  zeroDigest.semantic_digest = `v1:${'A'.repeat(43)}`;
  reject(zeroDigest);
});

test('capability snapshot cannot claim readiness barrier artifact or access state', () => {
  assert.equal(fixture.expected.phase_independent, true);
  assert.equal(fixture.expected.coverage_or_readiness_claim, false);
  assert.equal(fixture.expected.bootstrap_or_resync_barrier, false);
  assert.equal(fixture.expected.artifact_availability_state, false);
  assert.equal(fixture.expected.source_access_authority, false);
  assert.equal(fixture.expected.portable_observer_transport, false);
  assert.equal(fixture.expected.native_payload_disclosure, 'none');
  assert.equal(fixture.expected.semantic_digest_algorithm, 'blake3-256');

  for (const field of [
    'phase',
    'root',
    'source_coverage',
    'current_readiness',
    'artifact_availability',
    'barrier_sequence',
    'source_access',
    'native_payload',
  ]) {
    const changed = clone(fixture.exact.snapshot);
    changed[field] = null;
    reject(changed);
  }
});

test('expected capability state cannot be rewritten alongside a received snapshot', () => {
  const context = clone(fixture.exact.context);
  context.expected_capabilities.fact_families[1].quality = 'qualified';
  reject(fixture.exact.snapshot, context);

  const digest = clone(fixture.exact.context);
  digest.expected_semantic_digest = fixture.range.context.expected_semantic_digest;
  reject(fixture.exact.snapshot, digest);
});

test('portable contextual comparison is insensitive to JSON map insertion order', () => {
  const context = clone(fixture.exact.context);
  const offered = context.contract_offer.contract_versions.fact_family_versions;
  context.contract_offer.contract_versions.fact_family_versions = {
    'runtime.usage-v2': offered['runtime.usage-v2'],
    'runtime.actor-run': offered['runtime.actor-run'],
  };
  assert.deepEqual(parseScopedCapabilitySnapshot(fixture.exact.snapshot, context), fixture.exact.snapshot);
});

test('portable capability-snapshot parsing does not depend on the Node Buffer global', () => {
  const globalWithBuffer = globalThis as unknown as { Buffer?: unknown };
  const original = globalWithBuffer.Buffer;
  try {
    globalWithBuffer.Buffer = undefined;
    assert.deepEqual(
      parseScopedCapabilitySnapshot(fixture.exact.snapshot, fixture.exact.context),
      fixture.exact.snapshot,
    );
  } finally {
    globalWithBuffer.Buffer = original;
  }
});
