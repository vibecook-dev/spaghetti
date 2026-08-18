import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError } from '../rfc012a.js';
import {
  parseScopedReplacementManifest,
  parseScopedReplacementManifestContext,
} from '../rfc012d-replacement-manifest.js';

interface ReplacementManifestFixture {
  fixture_contract_version: number;
  context: any;
  manifest: any;
  expected: {
    selected_fact_families: string[];
    completeness_by_family: Record<string, string>;
    phase_independent: boolean;
    bootstrap_or_resync_barrier: boolean;
    source_access_authority: boolean;
    portable_observer_transport: boolean;
    native_payload_disclosure: string;
  };
}

const fixture = JSON.parse(
  readFileSync(
    new URL(
      '../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012d-scoped-replacement-manifest-v1.json',
      import.meta.url,
    ),
    'utf8',
  ),
) as ReplacementManifestFixture;

function clone<T>(value: T): T {
  return structuredClone(value);
}

function reject(value: unknown, context: unknown = fixture.context): void {
  assert.throws(() => parseScopedReplacementManifest(value, context), ContractValidationError);
}

test('portable TypeScript parses the frozen contextual replacement manifest', () => {
  assert.equal(fixture.fixture_contract_version, 1);
  const context = parseScopedReplacementManifestContext(fixture.context);
  assert.deepEqual(context, fixture.context);
  assert.deepEqual(parseScopedReplacementManifest(fixture.manifest, context), fixture.manifest);
  assert.deepEqual(fixture.expected.selected_fact_families, [
    'runtime.actor-affiliation',
    'runtime.actor-run',
    'runtime.usage-v2',
  ]);
  assert.deepEqual(fixture.expected.completeness_by_family, {
    'runtime.actor-affiliation': 'complete',
    'runtime.actor-run': 'partial',
    'runtime.usage-v2': 'unavailable',
  });
  assert.equal(fixture.expected.phase_independent, true);
  assert.equal(fixture.expected.bootstrap_or_resync_barrier, false);
  assert.equal(fixture.expected.source_access_authority, false);
  assert.equal(fixture.expected.portable_observer_transport, false);
  assert.equal(fixture.expected.native_payload_disclosure, 'none');
});

test('selection, exact reducer state, and merged family coverage are caller-held', () => {
  const selection = clone(fixture.manifest);
  selection.contract_selection.contract_versions.fact_family_versions['runtime.actor-run'] = 2;
  reject(selection);

  const count = clone(fixture.manifest);
  count.families[1].entity_or_event_count += 1;
  reject(count);

  const digest = clone(fixture.manifest);
  digest.families[1].semantic_digest = fixture.manifest.families[0].semantic_digest;
  reject(digest);

  const completeness = clone(fixture.manifest);
  completeness.families[1].completeness = 'complete';
  reject(completeness);

  const coverage = clone(fixture.context);
  coverage.source_coverage[3].completeness = 'unavailable';
  reject(fixture.manifest, coverage);

  const missing = clone(fixture.context);
  missing.source_coverage = missing.source_coverage.filter(
    (entry: any) => entry.coverage_domain.family !== 'runtime.usage-v2',
  );
  reject(fixture.manifest, missing);
});

test('foreign, projection-pack, and unselected coverage cannot widen the manifest', () => {
  const foreign = clone(fixture.context);
  const foreignSet = clone(foreign.source_coverage[1]);
  foreignSet.coverage_domain.family = 'runtime.foreign';
  foreign.source_coverage.push(foreignSet);
  reject(fixture.manifest, foreign);

  const projection = clone(fixture.context);
  const projectionSet = clone(projection.source_coverage[1]);
  projectionSet.coverage_domain = { kind: 'projection_pack', pack: 'library.catalog', version: 1 };
  projection.source_coverage.push(projectionSet);
  reject(fixture.manifest, projection);

  const unselected = clone(fixture.context);
  delete unselected.contract_selection.contract_versions.fact_family_versions['runtime.usage-v2'];
  reject(fixture.manifest, unselected);
});

test('family order, shape, versions, representations, counts, and digests fail closed', () => {
  const topUnknown = clone(fixture.manifest);
  topUnknown.future_meaning = true;
  reject(topUnknown);

  const nestedUnknown = clone(fixture.manifest);
  nestedUnknown.families[0].future_meaning = true;
  reject(nestedUnknown);

  const reordered = clone(fixture.manifest);
  [reordered.families[0], reordered.families[1]] = [reordered.families[1], reordered.families[0]];
  reject(reordered);

  const duplicate = clone(fixture.manifest);
  duplicate.families[1] = clone(duplicate.families[0]);
  reject(duplicate);

  const version = clone(fixture.manifest);
  version.families[0].contract_version = 2;
  reject(version);

  const representation = clone(fixture.manifest);
  representation.families[2].replacement_representation = 'revisioned_entity_current';
  reject(representation);

  const unsafeCount = clone(fixture.manifest);
  unsafeCount.families[0].entity_or_event_count = Number.MAX_SAFE_INTEGER + 1;
  reject(unsafeCount);

  const shortDigest = clone(fixture.manifest);
  shortDigest.families[0].semantic_digest = 'v1:AQ';
  reject(shortDigest);

  const zeroDigest = clone(fixture.manifest);
  zeroDigest.families[0].semantic_digest = `v1:${'A'.repeat(43)}`;
  reject(zeroDigest);
});

test('context is bounded and the manifest cannot claim barrier or access fields', () => {
  const empty = clone(fixture.context);
  empty.source_coverage = [];
  reject(fixture.manifest, empty);

  const oversized = clone(fixture.context);
  oversized.source_coverage = Array.from({ length: 65 }, () => clone(fixture.context.source_coverage[0]));
  reject(fixture.manifest, oversized);

  for (const field of ['phase', 'root', 'source_access', 'native_payload', 'barrier_sequence']) {
    const changed = clone(fixture.manifest);
    changed[field] = null;
    reject(changed);
  }
});

test('portable replacement-manifest parsing does not depend on the Node Buffer global', () => {
  const globalWithBuffer = globalThis as unknown as { Buffer?: unknown };
  const original = globalWithBuffer.Buffer;
  try {
    globalWithBuffer.Buffer = undefined;
    assert.deepEqual(parseScopedReplacementManifest(fixture.manifest, fixture.context), fixture.manifest);
  } finally {
    globalWithBuffer.Buffer = original;
  }
});
