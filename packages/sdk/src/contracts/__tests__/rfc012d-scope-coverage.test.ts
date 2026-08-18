import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError } from '../rfc012a.js';
import { parseScopedScopeCoverage, parseScopedScopeCoverageContext } from '../rfc012d-scope-coverage.js';

interface ScopeCoverageFixture {
  context: Record<string, any>;
  scope_coverage: Record<string, any>;
}

const fixture = JSON.parse(
  readFileSync(
    new URL(
      '../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012d-scoped-scope-coverage-v1.json',
      import.meta.url,
    ),
    'utf8',
  ),
) as ScopeCoverageFixture;

function clone<T>(value: T): T {
  return structuredClone(value);
}

function reject(value: unknown, context: unknown = fixture.context): void {
  assert.throws(() => parseScopedScopeCoverage(value, context), ContractValidationError);
}

test('portable TypeScript independently parses Rust exact-known-object scope coverage', () => {
  const context = parseScopedScopeCoverageContext(fixture.context);
  assert.deepEqual(parseScopedScopeCoverage(fixture.scope_coverage, context), fixture.scope_coverage);
  assert.equal(fixture.scope_coverage.relations.length, 2);
  assert.equal(fixture.scope_coverage.relations[0].state.kind, 'present');
  assert.equal(fixture.scope_coverage.relations[1].state.kind, 'absent');
});

test('program, root, and declaration context cannot drift', () => {
  for (const field of ['program_id', 'scope_program_digest', 'root_relation_id']) {
    const value = clone(fixture.scope_coverage);
    value[field] = field === 'scope_program_digest' ? `sha256:${'0'.repeat(64)}` : 'foreign';
    reject(value);
  }

  const root = clone(fixture.context);
  root.root.session_key = root.root.source_instance_key;
  reject(fixture.scope_coverage, root);

  const omitted = clone(fixture.context);
  omitted.declared_relation_ids.pop();
  assert.throws(() => parseScopedScopeCoverageContext(omitted), ContractValidationError);

  const reordered = clone(fixture.context);
  reordered.declared_relation_ids.reverse();
  assert.throws(() => parseScopedScopeCoverageContext(reordered), ContractValidationError);

  const zeroDigest = clone(fixture.context);
  zeroDigest.scope_program_digest = `sha256:${'0'.repeat(64)}`;
  assert.throws(() => parseScopedScopeCoverageContext(zeroDigest), ContractValidationError);
});

test('relation membership, root designation, and Decode evidence are exact', () => {
  const omitted = clone(fixture.scope_coverage);
  omitted.relations.pop();
  reject(omitted);

  const reordered = clone(fixture.scope_coverage);
  reordered.relations.reverse();
  reject(reordered);

  const swappedRoot = clone(fixture.scope_coverage);
  swappedRoot.relations[0].scope_root = false;
  swappedRoot.relations[1].scope_root = true;
  reject(swappedRoot);

  const retargeted = clone(fixture.scope_coverage);
  retargeted.relations[1].source.object_key = retargeted.relations[0].source.object_key;
  reject(retargeted);

  const falseAbsent = clone(fixture.scope_coverage);
  falseAbsent.relations[0].state = { kind: 'absent', absence_kind: 'absent' };
  reject(falseAbsent);
});

test('portable integer, completeness, revision, and nested shapes are strict', () => {
  for (const generation of [0, Number.MAX_SAFE_INTEGER + 1]) {
    const value = clone(fixture.scope_coverage);
    value.relations[0].generation = generation;
    reject(value);
  }

  const completeness = clone(fixture.scope_coverage);
  completeness.completeness = 'partial';
  reject(completeness);

  const revision = clone(fixture.scope_coverage);
  revision.scope_revision = 'v1:AA';
  reject(revision);

  const alternateRevision = clone(fixture.scope_coverage);
  alternateRevision.scope_revision = fixture.context.decode_coverage.membership_revision;
  reject(alternateRevision);

  const extra = clone(fixture.scope_coverage);
  extra.relations[0].state.future = true;
  reject(extra);

  const sourceExtra = clone(fixture.scope_coverage);
  sourceExtra.relations[0].source.native_path = '/private/session';
  reject(sourceExtra);

  const prototype = Object.assign(Object.create({ inherited: true }), fixture.scope_coverage);
  reject(prototype);
});

test('Decode cursor remains authoritative and outside the scope revision', () => {
  const context = clone(fixture.context);
  context.decode_coverage.points[0].position.opaque = context.decode_coverage.membership_revision;
  context.decode_coverage.points[0].position.monotonic_order += 1;
  assert.deepEqual(parseScopedScopeCoverage(fixture.scope_coverage, context), fixture.scope_coverage);
});

test('scope coverage carries neither locator/path nor completion-barrier claims', () => {
  const serialized = JSON.stringify(fixture.scope_coverage);
  for (const forbidden of [
    'native_path',
    'relative_path',
    'locator_id',
    'barrier_sequence',
    'root_present',
    'family_manifest',
    'ready',
  ]) {
    assert.equal(serialized.includes(forbidden), false);
  }
});

test('portable parsing does not depend on the Node Buffer global', () => {
  const globalWithBuffer = globalThis as unknown as { Buffer?: unknown };
  const original = globalWithBuffer.Buffer;
  try {
    globalWithBuffer.Buffer = undefined;
    assert.deepEqual(parseScopedScopeCoverage(fixture.scope_coverage, fixture.context), fixture.scope_coverage);
  } finally {
    globalWithBuffer.Buffer = original;
  }
});
