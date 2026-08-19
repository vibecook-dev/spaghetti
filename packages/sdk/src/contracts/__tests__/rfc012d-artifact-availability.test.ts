import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError } from '../rfc012a.js';
import {
  parseScopedArtifactAvailabilityContext,
  parseScopedArtifactAvailabilitySnapshot,
} from '../rfc012d-artifact-availability.js';

interface ArtifactAvailabilityFixture {
  fixture_contract_version: number;
  empty: { context: any; snapshot: any };
  populated: { context: any; snapshot: any };
  expected: {
    states: string[];
    semantic_digest_algorithm: string;
    ordered_observer_event: boolean;
    bootstrap_or_resync_barrier: boolean;
    source_access_authority: boolean;
    native_locator_disclosure: string;
    native_content_disclosure: string;
    portable_observer_transport: boolean;
  };
}

const fixture = JSON.parse(
  readFileSync(
    new URL(
      '../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012d-scoped-artifact-availability-v1.json',
      import.meta.url,
    ),
    'utf8',
  ),
) as ArtifactAvailabilityFixture;

function clone<T>(value: T): T {
  return structuredClone(value);
}

function reject(value: unknown, context: unknown = fixture.populated.context): void {
  assert.throws(() => parseScopedArtifactAvailabilitySnapshot(value, context), ContractValidationError);
}

function stateIndex(kind: string): number {
  return fixture.populated.snapshot.entries.findIndex((entry: any) => entry.state.kind === kind);
}

test('portable TypeScript parses empty and populated Rust availability snapshots', () => {
  assert.equal(fixture.fixture_contract_version, 1);
  for (const sample of [fixture.empty, fixture.populated]) {
    const context = parseScopedArtifactAvailabilityContext(sample.context);
    assert.deepEqual(context, sample.context);
    assert.deepEqual(parseScopedArtifactAvailabilitySnapshot(sample.snapshot, context), sample.snapshot);
  }
  assert.deepEqual(
    fixture.populated.snapshot.entries.map((entry: any) => entry.state.kind).sort(),
    [...fixture.expected.states].sort(),
  );
});

test('selection root entries and digest remain exact caller-held context', () => {
  reject(fixture.populated.snapshot, fixture.empty.context);

  const selection = clone(fixture.populated.snapshot);
  selection.contract_selection.event_contract_version = 2;
  reject(selection);

  const root = clone(fixture.populated.snapshot);
  root.root_session_key = fixture.empty.snapshot.semantic_digest;
  reject(root);

  const digest = clone(fixture.populated.snapshot);
  digest.semantic_digest = fixture.empty.snapshot.semantic_digest;
  reject(digest);

  const revision = clone(fixture.populated.snapshot);
  revision.entries[0].revision = fixture.empty.snapshot.semantic_digest;
  reject(revision);

  const rewrittenContext = clone(fixture.populated.context);
  rewrittenContext.expected_entries[stateIndex('available')].state = { kind: 'unstable' };
  reject(fixture.populated.snapshot, rewrittenContext);
});

test('strict shapes canonical order bounds and state laws fail closed', () => {
  const topUnknown = clone(fixture.populated.snapshot);
  topUnknown.future_meaning = true;
  reject(topUnknown);

  const entryUnknown = clone(fixture.populated.snapshot);
  entryUnknown.entries[0].native_path = '/Users/alice/private';
  reject(entryUnknown);

  const stateUnknown = clone(fixture.populated.snapshot);
  stateUnknown.entries[0].state.native_id = 'secret';
  reject(stateUnknown);

  const version = clone(fixture.populated.snapshot);
  version.scoped_artifact_availability_contract_version = 2;
  reject(version);

  const count = clone(fixture.populated.snapshot);
  count.entry_count -= 1;
  reject(count);

  const reordered = clone(fixture.populated.snapshot);
  reordered.entries.reverse();
  reject(reordered);

  const duplicate = clone(fixture.populated.snapshot);
  duplicate.entries[1] = clone(duplicate.entries[0]);
  reject(duplicate);

  const zeroGeneration = clone(fixture.populated.snapshot);
  zeroGeneration.entries[stateIndex('available')].state.generation = 0;
  reject(zeroGeneration);

  const missingPair = clone(fixture.populated.snapshot);
  missingPair.entries[stateIndex('missing')].state.provenance_ref = null;
  reject(missingPair);

  const notOverLimit = clone(fixture.populated.snapshot);
  const overLimit = notOverLimit.entries[stateIndex('over_limit')].state;
  overLimit.observed_bytes = overLimit.request_max_bytes;
  reject(notOverLimit);

  const unsafeInteger = clone(fixture.populated.snapshot);
  unsafeInteger.entries[stateIndex('available')].state.generation = Number.MAX_SAFE_INTEGER + 1;
  reject(unsafeInteger);

  const invalidKind = clone(fixture.populated.snapshot);
  invalidKind.entries[0].artifact_kind = 'Workflow Definition';
  reject(invalidKind);

  const zeroRevision = clone(fixture.populated.snapshot);
  zeroRevision.entries[0].revision = `v1:${'A'.repeat(43)}`;
  reject(zeroRevision);

  const oversized = clone(fixture.populated.snapshot);
  oversized.entries = Array.from({ length: 4_097 }, () => clone(oversized.entries[0]));
  oversized.entry_count = oversized.entries.length;
  reject(oversized);
});

test('availability snapshot is state-only, path-free, and non-authorizing', () => {
  assert.equal(fixture.expected.semantic_digest_algorithm, 'blake3-256');
  assert.equal(fixture.expected.ordered_observer_event, false);
  assert.equal(fixture.expected.bootstrap_or_resync_barrier, false);
  assert.equal(fixture.expected.source_access_authority, false);
  assert.equal(fixture.expected.native_locator_disclosure, 'none');
  assert.equal(fixture.expected.native_content_disclosure, 'none');
  assert.equal(fixture.expected.portable_observer_transport, false);

  for (const field of [
    'observer_sequence',
    'scope_epoch',
    'phase',
    'source',
    'relation_id',
    'object_token',
    'locator',
    'content',
    'barrier_sequence',
    'source_access',
  ]) {
    const changed = clone(fixture.populated.snapshot);
    changed[field] = null;
    reject(changed);
  }
  const encoded = JSON.stringify(fixture.populated.snapshot);
  for (const secret of ['/Users/alice/private', 'native-backup', 'file-artifact']) {
    assert.equal(encoded.includes(secret), false);
  }
});

test('portable availability parsing does not depend on the Node Buffer global', () => {
  const globalWithBuffer = globalThis as unknown as { Buffer?: unknown };
  const original = globalWithBuffer.Buffer;
  try {
    globalWithBuffer.Buffer = undefined;
    assert.deepEqual(
      parseScopedArtifactAvailabilitySnapshot(fixture.populated.snapshot, fixture.populated.context),
      fixture.populated.snapshot,
    );
  } finally {
    globalWithBuffer.Buffer = original;
  }
});
