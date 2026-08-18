import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError } from '../rfc012a.js';
import { parseScopedSourceEnvelope, parseScopedSourceEnvelopeContext } from '../rfc012d-source-envelope.js';

interface SourceEnvelopeFixture {
  fixture_contract_version: number;
  context: any;
  created: any;
  deleted: any;
  reset: any;
  retry_scheduled: any;
  retry_exhausted: any;
  terminal_error: any;
  expected: {
    complete_event_union: boolean;
    supported_variants: string[];
    unsupported_variants: string;
    event_id_authority: string;
    native_evidence: string;
  };
}

const fixture = JSON.parse(
  readFileSync(
    new URL(
      '../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012d-scoped-source-envelope-v1.json',
      import.meta.url,
    ),
    'utf8',
  ),
) as SourceEnvelopeFixture;

function clone<T>(value: T): T {
  return structuredClone(value);
}

function reject(value: unknown, context: unknown = fixture.context): void {
  assert.throws(() => parseScopedSourceEnvelope(value, context), ContractValidationError);
}

test('portable TypeScript independently parses every frozen Rust source control', () => {
  const context = parseScopedSourceEnvelopeContext(fixture.context);
  assert.deepEqual(context, fixture.context);
  for (const envelope of [
    fixture.created,
    fixture.deleted,
    fixture.reset,
    fixture.retry_scheduled,
    fixture.retry_exhausted,
    fixture.terminal_error,
  ]) {
    assert.deepEqual(parseScopedSourceEnvelope(envelope, context), envelope);
  }
  assert.equal(fixture.fixture_contract_version, 1);
  assert.deepEqual(fixture.expected.supported_variants, [
    'source_created',
    'source_deleted',
    'source_reset',
    'source_object_error',
  ]);
  assert.equal(fixture.expected.complete_event_union, false);
  assert.equal(fixture.expected.unsupported_variants, 'observer_lifecycle_and_semantic_fact_events');
  assert.equal(fixture.expected.event_id_authority, 'native_rust_contextual_parser');
  assert.equal(fixture.expected.native_evidence, 'engine_control_only');
});

test('selection, root, and authorized source context are exact', () => {
  const selection = clone(fixture.created);
  selection.contract_selection.event_contract_version = 2;
  reject(selection);

  const root = clone(fixture.created);
  root.root.root_actor_run_key = fixture.created.root.session_key;
  reject(root);

  const source = clone(fixture.created);
  source.source.object_key = fixture.created.root.session_key;
  reject(source);

  const empty = clone(fixture.context);
  empty.authorized_sources = [];
  reject(fixture.created, empty);

  const duplicate = clone(fixture.context);
  duplicate.authorized_sources.push(clone(duplicate.authorized_sources[0]));
  reject(fixture.created, duplicate);

  const oversized = clone(fixture.context);
  oversized.authorized_sources = Array.from({ length: 1_001 }, () => clone(fixture.context.authorized_sources[0]));
  reject(fixture.created, oversized);
});

test('source generations, reset lineage, and evidence fail closed', () => {
  const zero = clone(fixture.created);
  zero.source.generation = 0;
  zero.event.generation = 0;
  reject(zero);

  const skipped = clone(fixture.reset);
  skipped.event.new_generation = 3;
  skipped.source.generation = 3;
  reject(skipped);

  const phase = clone(fixture.reset);
  phase.phase = 'live';
  reject(phase);

  const retryEvidence = clone(fixture.retry_scheduled);
  retryEvidence.evidence.completeness = 'complete';
  reject(retryEvidence);

  const correctionError = clone(fixture.retry_scheduled);
  correctionError.phase = 'correction';
  assert.deepEqual(parseScopedSourceEnvelope(correctionError, fixture.context), correctionError);

  const bootstrapError = clone(fixture.retry_scheduled);
  bootstrapError.phase = 'bootstrap';
  reject(bootstrapError);

  const terminalEvidence = clone(fixture.terminal_error);
  terminalEvidence.evidence.completeness = 'partial';
  reject(terminalEvidence);

  const unsafe = clone(fixture.created);
  unsafe.observer_sequence = Number.MAX_SAFE_INTEGER + 1;
  reject(unsafe);
});

test('typed object errors enforce failure/retry/provenance coherence', () => {
  const scope = clone(fixture.retry_scheduled);
  scope.event.error.scope_epoch = 2;
  reject(scope);

  const generation = clone(fixture.retry_scheduled);
  generation.event.error.provenance.generation = 3;
  reject(generation);

  const failure = clone(fixture.retry_scheduled);
  failure.event.error.failure_code = 'decode_stream_fatal';
  reject(failure);

  const attempts = clone(fixture.retry_scheduled);
  attempts.event.error.retry.failed_attempts = 4;
  reject(attempts);

  const tooManyAttempts = clone(fixture.retry_scheduled);
  tooManyAttempts.event.error.retry.max_attempts = 33;
  reject(tooManyAttempts);

  const delay = clone(fixture.retry_scheduled);
  delay.event.error.retry.retry_after_ms = 60 * 60 * 1_000 + 1;
  reject(delay);

  const position = clone(fixture.retry_scheduled);
  position.event.error.provenance.last_successful_position.monotonic_order = Number.MAX_SAFE_INTEGER + 1;
  reject(position);

  const relation = clone(fixture.retry_scheduled);
  relation.event.error.relation_id = 'Root Object';
  reject(relation);
});

test('record occurrences, native evidence, and unfrozen variants cannot cross this contract', () => {
  for (const [field, value] of [
    ['locator_id', '/native/path'],
    ['source_record_id', fixture.created.root.session_key],
    ['record_index', 0],
    ['cursor_start', 'v1:native'],
    ['cursor_end', 'v1:native'],
    ['byte_range', { start: 0, end: 1 }],
  ] as const) {
    const disclosed = clone(fixture.created);
    disclosed.source[field] = value;
    reject(disclosed);
  }

  const semantic = clone(fixture.created);
  semantic.semantic_revision_ref = {
    semantic_reference_contract_version: 1,
    fact_revision_id: fixture.created.root.session_key,
  };
  reject(semantic);

  const nativeEvidence = clone(fixture.created);
  nativeEvidence.native_evidence.kind = 'inline_source_record';
  reject(nativeEvidence);

  const observer = clone(fixture.created);
  observer.event = { kind: 'observer_bootstrap_complete' };
  reject(observer);

  const eventId = clone(fixture.created);
  eventId.event_id = 'v1:not-canonical';
  reject(eventId);
});

test('portable source parsing does not depend on the Node Buffer global', () => {
  const original = globalThis.Buffer;
  try {
    Object.defineProperty(globalThis, 'Buffer', { configurable: true, value: undefined });
    assert.deepEqual(parseScopedSourceEnvelope(fixture.created, fixture.context), fixture.created);
  } finally {
    Object.defineProperty(globalThis, 'Buffer', { configurable: true, value: original });
  }
});

test('strict nested objects cannot silently discard future meaning', () => {
  const paths = [
    [],
    ['root'],
    ['root', 'native_session_claim', 'identity'],
    ['actor'],
    ['actor_attribution'],
    ['affiliations'],
    ['source'],
    ['evidence'],
    ['event'],
    ['event', 'error'],
    ['event', 'error', 'provenance'],
    ['event', 'error', 'provenance', 'last_successful_position'],
    ['event', 'error', 'retry'],
    ['native_evidence'],
  ];
  for (const path of paths) {
    const unknown = clone(fixture.retry_scheduled);
    let target = unknown;
    for (const segment of path) target = target[segment];
    target.future_meaning = true;
    reject(unknown);
  }

  for (const path of [
    ['semantic_revision_ref'],
    ['root', 'native_session_claim'],
    ['actor', 'parent_run_key'],
    ['source', 'locator_id'],
    ['native_time'],
    ['evidence', 'effective_at'],
    ['event', 'error', 'provenance', 'last_successful_position'],
  ]) {
    const missing = clone(fixture.retry_scheduled);
    const parents = path.slice(0, -1);
    const field = path.at(-1)!;
    let target = missing;
    for (const segment of parents) target = target[segment];
    delete target[field];
    reject(missing);
  }

  const nonPlain = Object.assign(Object.create({ inherited: true }), fixture.created);
  reject(nonPlain);
});
