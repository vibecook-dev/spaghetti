import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError } from '../rfc012a.js';
import { parseScopedActorEnvelope, parseScopedActorEnvelopeContext } from '../rfc012d-actor-envelope.js';

interface ActorEnvelopeFixture {
  fixture_contract_version: number;
  context: any;
  actor_upsert: any;
  affiliation_reset_retraction: any;
  expected: {
    fact_families: string[];
    fact_family_contract_version: number;
    complete_event_union: boolean;
    unsupported_variants: string;
    native_payload_disclosure: string;
    portable_transport: boolean;
  };
}

const fixture = JSON.parse(
  readFileSync(
    new URL(
      '../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012d-scoped-actor-envelope-v1.json',
      import.meta.url,
    ),
    'utf8',
  ),
) as ActorEnvelopeFixture;

function clone<T>(value: T): T {
  return structuredClone(value);
}

function reject(value: unknown, context: unknown = fixture.context): void {
  assert.throws(() => parseScopedActorEnvelope(value, context), ContractValidationError);
}

test('portable TypeScript parses both frozen Rust actor event families', () => {
  assert.equal(fixture.fixture_contract_version, 1);
  const context = parseScopedActorEnvelopeContext(fixture.context);
  assert.deepEqual(context, fixture.context);
  assert.deepEqual(parseScopedActorEnvelope(fixture.actor_upsert, context), fixture.actor_upsert);
  assert.deepEqual(
    parseScopedActorEnvelope(fixture.affiliation_reset_retraction, context),
    fixture.affiliation_reset_retraction,
  );
  assert.deepEqual(fixture.expected.fact_families, ['runtime.actor-affiliation', 'runtime.actor-run']);
  assert.equal(fixture.expected.fact_family_contract_version, 1);
  assert.equal(fixture.expected.complete_event_union, false);
  assert.equal(fixture.expected.unsupported_variants, 'usage_source_and_observer_lifecycle_controls');
  assert.equal(fixture.expected.native_payload_disclosure, 'withheld_at_projection_boundary');
  assert.equal(fixture.expected.portable_transport, false);
});

test('selection, root, and source authority remain exact caller-held context', () => {
  const selection = clone(fixture.actor_upsert);
  selection.contract_selection.event_contract_version = 2;
  reject(selection);

  const root = clone(fixture.actor_upsert);
  root.root.root_actor_run_key = root.actor.run_key;
  reject(root);

  const claim = clone(fixture.actor_upsert);
  claim.root.native_session_claim.identity.value.native_id = 'another-session';
  reject(claim);

  const source = clone(fixture.actor_upsert);
  source.source.object_key = fixture.context.root.session_key;
  reject(source);

  const emptySources = clone(fixture.context);
  emptySources.authorized_sources = [];
  reject(fixture.actor_upsert, emptySources);

  const duplicateSources = clone(fixture.context);
  duplicateSources.authorized_sources.push(clone(duplicateSources.authorized_sources[0]));
  reject(fixture.actor_upsert, duplicateSources);
});

test('normalized actor and affiliation values must match their envelope contexts', () => {
  const actor = clone(fixture.actor_upsert);
  actor.event.revision.native_actor_id = 'different-actor';
  reject(actor);

  const parent = clone(fixture.actor_upsert);
  parent.actor.parent_run_key = parent.actor.run_key;
  reject(parent);

  const session = clone(fixture.affiliation_reset_retraction);
  session.event.revision.session = session.actor.run_key;
  reject(session);

  const missingOwnRevision = clone(fixture.affiliation_reset_retraction);
  missingOwnRevision.affiliations.derived_from_revision_refs = [];
  reject(missingOwnRevision);

  const duplicateRevision = clone(fixture.affiliation_reset_retraction);
  duplicateRevision.affiliations.derived_from_revision_refs.push(
    clone(duplicateRevision.affiliations.derived_from_revision_refs[0]),
  );
  reject(duplicateRevision);

  const timestamp = clone(fixture.affiliation_reset_retraction);
  timestamp.event.revision.effective_at.value = '2026-08-18T00:00:01Z';
  reject(timestamp);

  const actorTimestamp = clone(fixture.actor_upsert);
  actorTimestamp.native_time = { value: '2026-08-18T00:00:00Z', quality: 'NativeExact' };
  actorTimestamp.evidence.effective_at = clone(actorTimestamp.native_time);
  reject(actorTimestamp);
});

test('operation, retraction lineage, evidence, and occurrence bounds fail closed', () => {
  const falseRetraction = clone(fixture.actor_upsert);
  falseRetraction.event.operation = 'retract';
  reject(falseRetraction);

  const evidence = clone(fixture.actor_upsert);
  evidence.evidence.authority = 'common_reducer';
  reject(evidence);

  const reset = clone(fixture.affiliation_reset_retraction);
  reset.event.retraction.new_generation = 3;
  reject(reset);

  const cursor = clone(fixture.actor_upsert);
  cursor.source.byte_range.end += 1;
  reject(cursor);

  const generation = clone(fixture.actor_upsert);
  generation.source.generation = 0;
  reject(generation);

  const recordIndex = clone(fixture.actor_upsert);
  recordIndex.source.record_index = 0x1_0000_0000;
  reject(recordIndex);

  const unsafe = clone(fixture.actor_upsert);
  unsafe.observer_sequence = Number.MAX_SAFE_INTEGER + 1;
  reject(unsafe);

  const locator = clone(fixture.actor_upsert);
  locator.source.locator_id = '/native/private/path';
  reject(locator);
});

test('strict nested values cannot silently discard future meaning', () => {
  const paths = [
    ['root', 'native_session_claim'],
    ['root', 'native_session_claim', 'identity'],
    ['root', 'native_session_claim', 'identity', 'value'],
    ['actor'],
    ['affiliations'],
    ['source'],
    ['source', 'byte_range'],
    ['event'],
    ['event', 'revision'],
    ['native_evidence'],
  ];
  for (const path of paths) {
    const value = clone(fixture.affiliation_reset_retraction);
    let target = value;
    for (const segment of path) target = target[segment];
    target.future_meaning = true;
    reject(value);
  }

  const effectiveAt = clone(fixture.affiliation_reset_retraction);
  effectiveAt.event.revision.effective_at.future_meaning = true;
  reject(effectiveAt);

  const retraction = clone(fixture.affiliation_reset_retraction);
  retraction.event.retraction.future_meaning = true;
  reject(retraction);

  const top = clone(fixture.actor_upsert);
  top.future_meaning = true;
  reject(top);

  const nonPlain = Object.assign(Object.create({ inherited: true }), fixture.actor_upsert);
  reject(nonPlain);
});

test('specialized family and opaque identity shapes remain closed', () => {
  const unsupported = clone(fixture.actor_upsert);
  unsupported.event.kind = 'usage_v2';
  reject(unsupported);

  const crossed = clone(fixture.actor_upsert);
  crossed.event.fact_family = 'runtime.actor-affiliation';
  reject(crossed);

  const unselected = clone(fixture.context);
  delete unselected.contract_selection.contract_versions.fact_family_versions['runtime.actor-run'];
  reject(fixture.actor_upsert, unselected);

  const malformedEvent = clone(fixture.actor_upsert);
  malformedEvent.event_id = 'v1:not-canonical';
  reject(malformedEvent);

  const malformedPayload = clone(fixture.actor_upsert);
  malformedPayload.native_evidence.payload_hash = `v1:${'A'.repeat(42)}`;
  reject(malformedPayload);

  const zeroFact = clone(fixture.actor_upsert);
  zeroFact.event.fact_id = `v1:${'A'.repeat(43)}`;
  reject(zeroFact);

  const wrongReferenceVersion = clone(fixture.actor_upsert);
  wrongReferenceVersion.semantic_revision_ref.semantic_reference_contract_version = 2;
  reject(wrongReferenceVersion);

  const inline = clone(fixture.actor_upsert);
  inline.native_evidence.kind = 'inline_source_record';
  reject(inline);
});

test('bounded actor text and withheld projection cannot carry hidden native material', () => {
  const actor = clone(fixture.actor_upsert);
  actor.actor.native_actor_type = 'subagent\n/private/path';
  reject(actor);

  const affiliation = clone(fixture.affiliation_reset_retraction);
  affiliation.event.revision.native_target_id = 'workflow\u0000private';
  reject(affiliation);

  const identity = clone(fixture.actor_upsert);
  identity.root.native_session_claim.identity.authority = 'fixture\n/private/path';
  reject(identity);

  const oversized = clone(fixture.actor_upsert);
  oversized.actor.native_actor_id = 'x'.repeat(8 * 1024 + 1);
  reject(oversized);

  const provenance = clone(fixture.affiliation_reset_retraction);
  provenance.affiliations.derived_from_revision_refs = Array.from({ length: 65 }, () => ({
    semantic_reference_contract_version: 1,
    fact_revision_id: fixture.affiliation_reset_retraction.semantic_revision_ref.fact_revision_id,
  }));
  reject(provenance);

  const encoded = JSON.stringify(parseScopedActorEnvelope(fixture.actor_upsert, fixture.context));
  assert.equal(encoded.includes('/native/private/path'), false);
  assert.equal(Object.hasOwn(JSON.parse(encoded).native_evidence, 'payload'), false);
});
