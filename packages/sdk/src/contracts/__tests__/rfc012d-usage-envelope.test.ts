import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError } from '../rfc012a.js';
import { parseScopedUsageEnvelope, parseScopedUsageEnvelopeContext } from '../rfc012d-usage-envelope.js';

interface UsageEnvelopeFixture {
  fixture_contract_version: number;
  context: any;
  upsert: any;
  reset_retraction: any;
  expected: {
    fact_family: string;
    fact_family_contract_version: number;
    complete_event_union: boolean;
    unsupported_variants: string;
    native_payload_disclosure: string;
  };
}

const fixture = JSON.parse(
  readFileSync(
    new URL(
      '../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012d-scoped-usage-envelope-v1.json',
      import.meta.url,
    ),
    'utf8',
  ),
) as UsageEnvelopeFixture;

function clone<T>(value: T): T {
  return structuredClone(value);
}

function reject(value: unknown, context: unknown = fixture.context): void {
  assert.throws(() => parseScopedUsageEnvelope(value, context), ContractValidationError);
}

test('portable TypeScript independently parses both Rust usage envelope operations', () => {
  assert.equal(fixture.fixture_contract_version, 1);
  const context = parseScopedUsageEnvelopeContext(fixture.context);
  assert.deepEqual(context, fixture.context);
  assert.deepEqual(parseScopedUsageEnvelope(fixture.upsert, context), fixture.upsert);
  assert.deepEqual(parseScopedUsageEnvelope(fixture.reset_retraction, context), fixture.reset_retraction);
  assert.equal(fixture.upsert.event.fact_family, fixture.expected.fact_family);
  assert.equal(fixture.upsert.event.fact_family_contract_version, fixture.expected.fact_family_contract_version);
  assert.equal(fixture.expected.complete_event_union, false);
  assert.equal(fixture.expected.unsupported_variants, 'source_and_observer_lifecycle_controls');
  assert.equal(fixture.expected.native_payload_disclosure, 'withheld_at_projection_boundary');
});

test('selection, root, and source authority are exact caller-held context', () => {
  const selection = clone(fixture.upsert);
  selection.contract_selection.event_contract_version = 2;
  reject(selection);

  const root = clone(fixture.upsert);
  root.root.root_actor_run_key = fixture.upsert.actor.run_key;
  reject(root);

  const source = clone(fixture.upsert);
  source.source.object_key = fixture.context.root.session_key;
  reject(source);

  const emptySources = clone(fixture.context);
  emptySources.authorized_sources = [];
  reject(fixture.upsert, emptySources);

  const duplicateSources = clone(fixture.context);
  duplicateSources.authorized_sources.push(clone(duplicateSources.authorized_sources[0]));
  reject(fixture.upsert, duplicateSources);

  const oversizedSources = clone(fixture.context);
  oversizedSources.authorized_sources = Array.from({ length: 1_001 }, () =>
    clone(fixture.context.authorized_sources[0]),
  );
  reject(fixture.upsert, oversizedSources);
});

test('portable occurrence bounds and operation evidence fail closed', () => {
  const unsafe = clone(fixture.upsert);
  unsafe.observer_sequence = Number.MAX_SAFE_INTEGER + 1;
  reject(unsafe);

  const zeroEpoch = clone(fixture.upsert);
  zeroEpoch.scope_epoch = 0;
  reject(zeroEpoch);

  const cursor = clone(fixture.upsert);
  cursor.source.byte_range.end += 1;
  reject(cursor);

  const recordIndex = clone(fixture.upsert);
  recordIndex.source.record_index = 0x1_0000_0000;
  reject(recordIndex);

  const falseRetraction = clone(fixture.upsert);
  falseRetraction.event.operation = 'retract';
  reject(falseRetraction);

  const falseEvidence = clone(fixture.upsert);
  falseEvidence.evidence.authority = 'common_reducer';
  reject(falseEvidence);

  const reset = clone(fixture.reset_retraction);
  reset.event.retraction.new_generation = 3;
  reject(reset);

  const disclosure = clone(fixture.upsert);
  disclosure.source.locator_id = '/native/path';
  reject(disclosure);
});

test('strict nested RFC 012A/012C values cannot silently discard future meaning', () => {
  const paths = [
    ['root', 'native_session_claim'],
    ['root', 'native_session_claim', 'identity'],
    ['root', 'native_session_claim', 'identity', 'value'],
    ['native_time'],
    ['event', 'revision'],
    ['event', 'revision', 'buckets'],
    ['event', 'revision', 'buckets', 'input_tokens'],
    ['event', 'revision', 'buckets', 'input_tokens', 'provenance'],
  ];
  for (const path of paths) {
    const value = clone(fixture.upsert);
    let target = value;
    for (const segment of path) target = target[segment];
    target.future_meaning = true;
    reject(value);
  }

  const top = clone(fixture.upsert);
  top.future_meaning = true;
  reject(top);

  const retraction = clone(fixture.reset_retraction);
  retraction.event.retraction.future_meaning = true;
  reject(retraction);

  const nonPlain = Object.assign(Object.create({ inherited: true }), fixture.upsert);
  reject(nonPlain);
});

test('the specialized parser rejects unfrozen variants and malformed opaque evidence', () => {
  const family = clone(fixture.upsert);
  family.event.kind = 'observer_bootstrap_complete';
  reject(family);

  const unknownFamily = clone(fixture.upsert);
  unknownFamily.event.fact_family = 'runtime.future';
  reject(unknownFamily);

  const eventId = clone(fixture.upsert);
  eventId.event_id = 'v1:not-canonical';
  reject(eventId);

  const payloadHash = clone(fixture.upsert);
  payloadHash.native_evidence.payload_hash = `v1:${'A'.repeat(42)}`;
  reject(payloadHash);

  const inline = clone(fixture.upsert);
  inline.native_evidence.kind = 'inline_source_record';
  reject(inline);

  const normalization = clone(fixture.upsert);
  normalization.event.revision.buckets.input_tokens.provenance.normalization_contract_version = 2;
  reject(normalization);

  const nativeIdentity = clone(fixture.upsert);
  nativeIdentity.root.native_session_claim.identity.value.native_id = 'x'.repeat(257);
  reject(nativeIdentity);

  const requestId = clone(fixture.upsert);
  requestId.event.revision.request_id = 'x'.repeat(8 * 1024 + 1);
  reject(requestId);

  const model = clone(fixture.upsert);
  model.event.revision.model.value = 'x'.repeat(8 * 1024 + 1);
  reject(model);

  const provenance = clone(fixture.upsert);
  provenance.root.native_session_claim.identity.provenance = Array.from({ length: 65 }, () => ({
    semantic_reference_contract_version: 1,
    fact_revision_id: fixture.upsert.semantic_revision_ref.fact_revision_id,
  }));
  reject(provenance);
});
