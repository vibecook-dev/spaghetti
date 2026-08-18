import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError } from '../rfc012a.js';
import { parseScopedContinuityEnvelope, parseScopedContinuityEnvelopeContext } from '../rfc012d-continuity-envelope.js';

interface ContinuityFixture {
  fixture_contract_version: number;
  contexts: Record<string, any>;
  resync_required: Record<string, any>;
  resync_started: any;
  observer_failed: Record<string, any>;
  expected: {
    complete_event_union: boolean;
    supported_variants: string[];
    unsupported_variants: string;
    event_id_authority: string;
    diagnostic_discard_counts_in_event_identity: boolean;
    replacement: string;
  };
}

const fixture = JSON.parse(
  readFileSync(
    new URL(
      '../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012d-scoped-continuity-envelope-v1.json',
      import.meta.url,
    ),
    'utf8',
  ),
) as ContinuityFixture;

function clone<T>(value: T): T {
  return structuredClone(value);
}

function reject(value: unknown, context: unknown): void {
  assert.throws(() => parseScopedContinuityEnvelope(value, context), ContractValidationError);
}

test('portable TypeScript independently parses every frozen Rust continuity control', () => {
  const requiredContext = parseScopedContinuityEnvelopeContext(fixture.contexts.required);
  for (const envelope of Object.values(fixture.resync_required)) {
    assert.deepEqual(parseScopedContinuityEnvelope(envelope, requiredContext), envelope);
  }
  assert.deepEqual(
    parseScopedContinuityEnvelope(fixture.resync_started, fixture.contexts.started),
    fixture.resync_started,
  );
  for (const [phase, envelope] of Object.entries(fixture.observer_failed)) {
    assert.deepEqual(parseScopedContinuityEnvelope(envelope, fixture.contexts[`failed_${phase}`]), envelope);
  }
  assert.equal(fixture.fixture_contract_version, 1);
  assert.deepEqual(fixture.expected.supported_variants, [
    'observer_resync_required',
    'observer_resync_started',
    'observer_failed',
  ]);
  assert.equal(fixture.expected.complete_event_union, false);
  assert.equal(fixture.expected.event_id_authority, 'native_rust_contextual_parser');
  assert.equal(fixture.expected.diagnostic_discard_counts_in_event_identity, false);
  assert.equal(fixture.expected.replacement, 'full_snapshot_only');
});

test('selection, root, control source, and caller continuity state are exact', () => {
  const envelope = fixture.resync_required.watcher_overflow;

  const selection = clone(envelope);
  selection.contract_selection.lifecycle_contract_version = 2;
  reject(selection, fixture.contexts.required);

  const root = clone(envelope);
  root.root.root_actor_run_key = root.root.session_key;
  reject(root, fixture.contexts.required);

  const source = clone(envelope);
  source.source.object_key = source.root.session_key;
  reject(source, fixture.contexts.required);

  const sourceContext = clone(fixture.contexts.required);
  sourceContext.control_source.object_key = sourceContext.root.session_key;
  reject(envelope, sourceContext);

  const epoch = clone(fixture.contexts.required);
  epoch.state.current_scope_epoch = 2;
  reject(envelope, epoch);

  const watermark = clone(fixture.contexts.required);
  watermark.state.last_contiguous_sequence = 2;
  reject(envelope, watermark);

  const baseline = clone(fixture.contexts.required);
  baseline.state.baseline_snapshot_digest = fixture.resync_started.event_id;
  reject(envelope, baseline);

  const duplicate = clone(envelope);
  duplicate.observer_sequence = 5;
  duplicate.event.control_sequence = 5;
  duplicate.event.last_contiguous_sequence = 4;
  reject(duplicate, fixture.contexts.started);
});

test('replacement start requires the exact delivered invalidation lineage', () => {
  const envelope = fixture.resync_started;

  const missing = clone(fixture.contexts.started);
  missing.state.prior_resync_required = null;
  reject(envelope, missing);

  const undelivered = clone(fixture.contexts.started);
  undelivered.state.last_contiguous_sequence = 1;
  reject(envelope, undelivered);

  const wrongReason = clone(fixture.contexts.started);
  wrongReason.state.prior_resync_required.reason = 'transport_continuity_loss';
  reject(envelope, wrongReason);

  const wrongRequired = clone(envelope);
  wrongRequired.event.required_control_sequence = 3;
  reject(wrongRequired, fixture.contexts.started);

  const skippedEpoch = clone(envelope);
  skippedEpoch.scope_epoch = 3;
  skippedEpoch.source.generation = 3;
  skippedEpoch.event.new_scope_epoch = 3;
  reject(skippedEpoch, fixture.contexts.started);

  const incremental = clone(envelope);
  incremental.event.replacement = 'incremental';
  reject(incremental, fixture.contexts.started);
});

test('terminal failure cannot retarget epoch, watermark, or delivery phase', () => {
  const envelope = fixture.observer_failed.correction;
  const context = fixture.contexts.failed_correction;

  const phase = clone(envelope);
  phase.event.phase = 'live';
  reject(phase, context);

  const callerPhase = clone(context);
  callerPhase.state.phase = 'live';
  reject(envelope, callerPhase);

  const epoch = clone(envelope);
  epoch.event.failed_scope_epoch = 1;
  reject(epoch, context);

  const watermark = clone(envelope);
  watermark.event.last_contiguous_sequence = 4;
  reject(watermark, context);

  const unsupportedReason = clone(envelope);
  unsupportedReason.event.reason = 'future_failure';
  reject(unsupportedReason, context);
});

test('portable counters, engine evidence, and source disclosure fail closed', () => {
  const envelope = fixture.resync_required.watcher_overflow;
  const context = fixture.contexts.required;

  for (const path of [
    ['observer_sequence'],
    ['event', 'discarded_semantic_events'],
    ['event', 'discarded_source_controls'],
    ['event', 'discarded_retained_native_bytes'],
  ]) {
    const unsafe = clone(envelope);
    let target = unsafe;
    for (const segment of path.slice(0, -1)) target = target[segment];
    target[path.at(-1)!] = Number.MAX_SAFE_INTEGER + 1;
    reject(unsafe, context);
  }

  const zeroEpoch = clone(envelope);
  zeroEpoch.scope_epoch = 0;
  zeroEpoch.source.generation = 0;
  zeroEpoch.event.invalid_scope_epoch = 0;
  reject(zeroEpoch, context);

  const disclosure = clone(envelope);
  disclosure.source.locator_id = '/native/path';
  reject(disclosure, context);

  const semantic = clone(envelope);
  semantic.semantic_revision_ref = {
    semantic_reference_contract_version: 1,
    fact_revision_id: envelope.root.session_key,
  };
  reject(semantic, context);

  const evidence = clone(envelope);
  evidence.evidence.completeness = 'partial';
  reject(evidence, context);

  const native = clone(envelope);
  native.native_evidence.kind = 'inline_source_record';
  reject(native, context);

  const eventId = clone(envelope);
  eventId.event_id = 'v1:not-canonical';
  reject(eventId, context);
});

test('strict context and envelope objects cannot silently discard future meaning', () => {
  const envelope = fixture.resync_started;
  const context = fixture.contexts.started;
  for (const path of [
    [],
    ['root'],
    ['root', 'native_session_claim', 'identity'],
    ['actor'],
    ['actor_attribution'],
    ['affiliations'],
    ['source'],
    ['evidence'],
    ['event'],
    ['native_evidence'],
  ]) {
    const unknown = clone(envelope);
    let target = unknown;
    for (const segment of path) target = target[segment];
    target.future_meaning = true;
    reject(unknown, context);
  }

  for (const path of [
    ['semantic_revision_ref'],
    ['root', 'native_session_claim'],
    ['actor', 'parent_run_key'],
    ['source', 'locator_id'],
    ['native_time'],
    ['evidence', 'effective_at'],
  ]) {
    const missing = clone(envelope);
    let target = missing;
    for (const segment of path.slice(0, -1)) target = target[segment];
    delete target[path.at(-1)!];
    reject(missing, context);
  }

  const futureContext = clone(context);
  futureContext.state.future_meaning = true;
  reject(envelope, futureContext);

  const missingPrior = clone(context);
  delete missingPrior.state.prior_resync_required;
  reject(envelope, missingPrior);

  const nonPlain = Object.assign(Object.create({ inherited: true }), envelope);
  reject(nonPlain, context);
});

test('portable continuity parsing does not depend on the Node Buffer global', () => {
  const original = globalThis.Buffer;
  try {
    Object.defineProperty(globalThis, 'Buffer', { configurable: true, value: undefined });
    assert.deepEqual(
      parseScopedContinuityEnvelope(fixture.resync_started, fixture.contexts.started),
      fixture.resync_started,
    );
  } finally {
    Object.defineProperty(globalThis, 'Buffer', { configurable: true, value: original });
  }
});
