import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError } from '../rfc012a.js';
import { parseScopedCompletionEnvelope, parseScopedCompletionEnvelopeContext } from '../rfc012d-completion-envelope.js';

interface CompletionFixture {
  fixture_contract_version: number;
  bootstrap: { context: any; event: any };
  resync: { context: any; event: any };
  expected: {
    barrier_contract_version: number;
    coverage_digest_equal_at_equal_state: boolean;
    replacement_digest_equal_at_equal_state: boolean;
    ordered: boolean;
    rust_event_id_authority: string;
    portable_event_id_authority: string;
    native_evidence: string;
    native_payload_disclosure: string;
    source_access_authority: boolean;
    task_artifact_discovery: boolean;
    public_observer_transport: boolean;
    nested_contracts: string[];
  };
}

const fixture = JSON.parse(
  readFileSync(
    new URL(
      '../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012d-scoped-completion-envelope-v1.json',
      import.meta.url,
    ),
    'utf8',
  ),
) as CompletionFixture;

function clone<T>(value: T): T {
  return structuredClone(value);
}

function reject(value: unknown, context: unknown = fixture.bootstrap.context): void {
  assert.throws(() => parseScopedCompletionEnvelope(value, context), ContractValidationError);
}

test('portable TypeScript parses Rust-issued ordered bootstrap and resync completions', () => {
  for (const sample of [fixture.bootstrap, fixture.resync]) {
    const context = parseScopedCompletionEnvelopeContext(sample.context);
    assert.deepEqual(context, sample.context);
    assert.deepEqual(parseScopedCompletionEnvelope(sample.event, context), sample.event);
  }
  assert.equal(fixture.fixture_contract_version, 1);
  assert.equal(fixture.expected.barrier_contract_version, 4);
  assert.equal(fixture.expected.ordered, true);
  assert.equal(fixture.expected.coverage_digest_equal_at_equal_state, true);
  assert.equal(fixture.expected.replacement_digest_equal_at_equal_state, true);
  assert.equal(fixture.expected.rust_event_id_authority, 'completion_snapshot_and_private_root');
  assert.equal(fixture.expected.portable_event_id_authority, 'exact_rust_issued_context');
});

test('ordered receipt root source selection and barrier kind are exact', () => {
  for (const [field, value] of [
    ['observer_sequence', 2],
    ['scope_epoch', 2],
    ['event_id', fixture.bootstrap.context.root.session_key],
    ['observed_at', fixture.bootstrap.event.observed_at + 1],
    ['phase', 'live'],
  ] as const) {
    const changed = clone(fixture.bootstrap.event);
    changed[field] = value;
    reject(changed);
  }

  const source = clone(fixture.bootstrap.event);
  source.source.object_key = fixture.bootstrap.context.root.session_key;
  reject(source);

  const root = clone(fixture.bootstrap.event);
  root.root.root_actor_run_key = fixture.bootstrap.context.root.session_key;
  reject(root);

  const selection = clone(fixture.bootstrap.event);
  selection.contract_selection.event_contract_version = 2;
  reject(selection);

  reject(fixture.bootstrap.event, fixture.resync.context);
  reject(fixture.resync.event, fixture.bootstrap.context);

  const unsupported = clone(fixture.bootstrap.event);
  unsupported.event.kind = 'observer_resync_complete';
  reject(unsupported);
});

test('all nested completion components remain bound to Rust-issued context', () => {
  const barrier = fixture.bootstrap.event.event.barrier;

  const capability = clone(fixture.bootstrap.event);
  capability.event.barrier.capability_snapshot.semantic_digest = fixture.bootstrap.context.root.session_key;
  reject(capability);

  const replacement = clone(fixture.bootstrap.event);
  replacement.event.barrier.replacement_manifest.families[0].semantic_digest =
    fixture.bootstrap.context.root.session_key;
  reject(replacement);

  const coverage = clone(fixture.bootstrap.event);
  coverage.event.barrier.source_coverage[0].membership_revision = fixture.bootstrap.context.root.session_key;
  reject(coverage);

  const scope = clone(fixture.bootstrap.event);
  scope.event.barrier.scope_coverage.scope_revision = fixture.bootstrap.context.root.session_key;
  reject(scope);

  const artifact = clone(fixture.bootstrap.event);
  artifact.event.barrier.artifact_availability.semantic_digest = fixture.bootstrap.context.root.session_key;
  reject(artifact);

  const unknownEvidence = clone(fixture.bootstrap.event);
  unknownEvidence.event.barrier.unknown_evidence.aggregate_digest = fixture.bootstrap.context.root.session_key;
  reject(unknownEvidence);

  const error = clone(fixture.bootstrap.event);
  error.event.barrier.explicit_object_errors = [];
  reject(error);

  const errorUnknown = clone(fixture.bootstrap.event);
  errorUnknown.event.barrier.explicit_object_errors[0].native_message = '/private/session.jsonl';
  reject(errorUnknown);

  const reorderedCoverage = clone(fixture.bootstrap.event);
  reorderedCoverage.event.barrier.source_coverage.reverse();
  reject(reorderedCoverage);

  assert.equal(barrier.snapshot_digest, fixture.bootstrap.context.expected_barrier.snapshot_digest);
  assert.equal(
    barrier.replacement_snapshot_digest,
    fixture.bootstrap.context.expected_barrier.replacement_snapshot_digest,
  );
});

test('queue and resync lineage cannot be skipped replayed or relabeled', () => {
  const queue = clone(fixture.resync.event);
  queue.event.barrier.queue_state.delivered_through_sequence = queue.event.barrier.queue_state.offered_through_sequence;
  reject(queue, fixture.resync.context);

  const queuedControl = clone(fixture.resync.event);
  queuedControl.event.barrier.queue_state.queued_source_control_items = 0;
  reject(queuedControl, fixture.resync.context);

  const started = clone(fixture.resync.event);
  started.event.barrier.started_control_sequence = started.event.barrier.barrier_sequence;
  reject(started, fixture.resync.context);

  const replacement = clone(fixture.resync.event);
  replacement.event.barrier.replacement = 'delta';
  reject(replacement, fixture.resync.context);

  const replayContext = clone(fixture.resync.context);
  replayContext.expected_observer_sequence = fixture.bootstrap.context.expected_observer_sequence;
  reject(fixture.resync.event, replayContext);

  const queueContext = clone(fixture.resync.context);
  queueContext.expected_queue_state.offered_through_sequence -= 1;
  reject(fixture.resync.event, queueContext);
});

test('capability authority privacy and portable bounds fail closed', () => {
  const capability = clone(fixture.bootstrap.context);
  capability.capability_context.compatibility_class = 'RangeSupported';
  reject(fixture.bootstrap.event, capability);

  const offer = clone(fixture.bootstrap.context);
  offer.capability_context.contract_offer.event_contract_versions = [2];
  reject(fixture.bootstrap.event, offer);

  const sourceContext = clone(fixture.bootstrap.context);
  sourceContext.expected_source.instance_key = fixture.bootstrap.context.root.session_key;
  reject(fixture.bootstrap.event, sourceContext);

  const releaseContext = clone(fixture.bootstrap.context);
  releaseContext.replacement_manifest_context.source_coverage[0].scope.support_release_id = 'different-support-v1';
  reject(fixture.bootstrap.event, releaseContext);

  const unsafe = clone(fixture.bootstrap.event);
  unsafe.source.locator_id = '/Users/alice/private/session.jsonl';
  reject(unsafe);

  const native = clone(fixture.bootstrap.event);
  native.native_evidence = { kind: 'withheld', payload: 'native bytes' };
  reject(native);

  const unknown = clone(fixture.bootstrap.event);
  unknown.event.barrier.future_meaning = true;
  reject(unknown);

  const unsafeInteger = clone(fixture.bootstrap.context);
  unsafeInteger.expected_observer_sequence = Number.MAX_SAFE_INTEGER + 1;
  reject(fixture.bootstrap.event, unsafeInteger);

  const encoded = JSON.stringify(fixture);
  assert.doesNotMatch(encoded, /\/Users\/|\\\\|locator_id":"[^n]|native bytes|future_private_field|secret/);
  assert.match(encoded, /source_record_id":"v1:/);
  assert.equal(fixture.expected.native_evidence, 'engine_control_only');
  assert.equal(fixture.expected.native_payload_disclosure, 'none');
  assert.equal(fixture.expected.source_access_authority, false);
  assert.equal(fixture.expected.task_artifact_discovery, false);
  assert.equal(fixture.expected.public_observer_transport, false);
  assert.deepEqual(fixture.expected.nested_contracts, [
    'capability_snapshot',
    'replacement_manifest',
    'scope_coverage',
    'artifact_availability',
    'unknown_evidence',
    'rfc012a_source_coverage',
  ]);
});
