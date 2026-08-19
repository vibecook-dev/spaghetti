import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError } from '../rfc012a.js';
import {
  parseScopedArtifactAvailabilityEnvelope,
  parseScopedArtifactAvailabilityEnvelopeContext,
} from '../rfc012d-artifact-availability-envelope.js';

interface ArtifactAvailabilityEnvelopeFixture {
  fixture_contract_version: number;
  context: any;
  event: any;
  expected: {
    ordered: boolean;
    rust_event_id_authority: string;
    portable_event_id_authority: string;
    native_evidence: string;
    source_declaration_digest_disclosed: boolean;
    snapshot_revision_contract: string;
  };
}

const fixture = JSON.parse(
  readFileSync(
    new URL(
      '../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012d-scoped-artifact-availability-envelope-v1.json',
      import.meta.url,
    ),
    'utf8',
  ),
) as ArtifactAvailabilityEnvelopeFixture;

function clone<T>(value: T): T {
  return structuredClone(value);
}

function reject(value: unknown, context: unknown = fixture.context): void {
  assert.throws(() => parseScopedArtifactAvailabilityEnvelope(value, context), ContractValidationError);
}

test('portable TypeScript parses the Rust-produced ordered availability event', () => {
  const context = parseScopedArtifactAvailabilityEnvelopeContext(fixture.context);
  assert.deepEqual(context, fixture.context);
  assert.deepEqual(parseScopedArtifactAvailabilityEnvelope(fixture.event, context), fixture.event);
  assert.equal(fixture.fixture_contract_version, 1);
  assert.equal(fixture.expected.ordered, true);
  assert.equal(fixture.expected.rust_event_id_authority, 'private_source_declaration_occurrence');
  assert.equal(fixture.expected.portable_event_id_authority, 'exact_rust_issued_context');
  assert.equal(fixture.expected.native_evidence, 'engine_control_only');
  assert.equal(fixture.expected.source_declaration_digest_disclosed, false);
  assert.equal(fixture.expected.snapshot_revision_contract, 'unchanged_v1');
  assert.doesNotMatch(JSON.stringify(fixture.context), /source_declaration_digest/);
  assert.doesNotMatch(JSON.stringify(fixture.event), /source_declaration_digest/);
});

test('ordered receipt, source, root, and selected contract are exact', () => {
  for (const [field, value] of [
    ['observer_sequence', 2],
    ['scope_epoch', 2],
    ['event_id', fixture.context.root.session_key],
  ] as const) {
    const changed = clone(fixture.event);
    changed[field] = value;
    reject(changed);
  }

  const source = clone(fixture.event);
  source.source.object_key = fixture.context.root.session_key;
  reject(source);

  const root = clone(fixture.event);
  root.root.root_actor_run_key = fixture.context.root.session_key;
  reject(root);

  const selection = clone(fixture.event);
  selection.contract_selection.event_contract_version = 2;
  reject(selection);

  const contextOrder = clone(fixture.context);
  contextOrder.expected_observer_sequence = 2;
  reject(fixture.event, contextOrder);

  const contextGeneration = clone(fixture.context);
  contextGeneration.expected_source.generation = 2;
  reject(fixture.event, contextGeneration);

  const observedAt = clone(fixture.event);
  observedAt.observed_at += 1;
  reject(observedAt);

  const phase = clone(fixture.event);
  phase.phase = 'live';
  reject(phase);
});

test('entry state and reducer evidence cannot drift from Rust authority', () => {
  const state = clone(fixture.event);
  state.event.entry.state.kind = 'unstable';
  delete state.event.entry.state.observed_generation;
  delete state.event.entry.state.provenance_ref;
  state.evidence.completeness = 'unknown';
  reject(state);

  const revision = clone(fixture.event);
  revision.event.entry.revision = fixture.context.root.session_key;
  reject(revision);

  const authority = clone(fixture.event);
  authority.evidence.authority = 'engine_control';
  reject(authority);

  const completeness = clone(fixture.event);
  completeness.evidence.completeness = 'unknown';
  reject(completeness);

  const correction = clone(fixture.event);
  correction.phase = 'correction';
  reject(correction);
});

test('native coordinates, bytes, semantic identity, and unknown fields fail closed', () => {
  for (const [field, value] of [
    ['locator_id', 'private/path'],
    ['source_record_id', fixture.context.root.session_key],
    ['record_index', 0],
    ['cursor_start', fixture.context.root.session_key],
    ['byte_range', { start: 0, end: 1 }],
  ] as const) {
    const changed = clone(fixture.event);
    changed.source[field] = value;
    reject(changed);
  }

  const semantic = clone(fixture.event);
  semantic.semantic_revision_ref = {
    semantic_reference_contract_version: 1,
    fact_revision_id: fixture.context.root.session_key,
  };
  reject(semantic);

  const native = clone(fixture.event);
  native.native_evidence = { kind: 'withheld' };
  reject(native);

  const unknown = clone(fixture.event);
  unknown.extra = true;
  reject(unknown);

  const oversized = clone(fixture.event);
  oversized.event.entry.artifact_kind = `a${'b'.repeat(128)}`;
  reject(oversized);
});
