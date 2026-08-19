import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError } from '../rfc012a.js';
import { parseScopedObservationWatermark, parseScopedObservationWatermarkContext } from '../rfc012d-watermark.js';

interface WatermarkFixture {
  fixture_contract_version: number;
  context: any;
  watermark: any;
  expected: {
    request_generation_is_flow_control_only: boolean;
    capability_and_support_context_bound: boolean;
    source_and_scope_coverage_exact: boolean;
    artifact_availability_state_bound: boolean;
    queue_continuity: string[];
    source_access_authority: boolean;
    public_observer_transport: boolean;
    native_locator_or_payload: boolean;
  };
}

const fixture = JSON.parse(
  readFileSync(
    new URL(
      '../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012d-scoped-watermark-v1.json',
      import.meta.url,
    ),
    'utf8',
  ),
) as WatermarkFixture;

function clone<T>(value: T): T {
  return structuredClone(value);
}

function reject(value: unknown, context: unknown = fixture.context): void {
  assert.throws(() => parseScopedObservationWatermark(value, context), ContractValidationError);
}

test('portable TypeScript parses the Rust-issued contextual poll watermark', () => {
  const context = parseScopedObservationWatermarkContext(fixture.context);
  assert.deepEqual(context, fixture.context);
  assert.deepEqual(parseScopedObservationWatermark(fixture.watermark, context), fixture.watermark);
  assert.equal(fixture.fixture_contract_version, 1);
  assert.deepEqual(fixture.expected.queue_continuity, ['bootstrap', 'valid']);
  assert.equal(fixture.expected.request_generation_is_flow_control_only, true);
});

test('selection root support and nested semantic state are exact', () => {
  for (const mutate of [
    (value: any) => {
      value.contract_selection.event_contract_version = 2;
    },
    (value: any) => {
      value.root.root_actor_run_key = fixture.context.root.session_key;
    },
    (value: any) => {
      value.capability_snapshot.semantic_digest = fixture.context.root.session_key;
    },
    (value: any) => {
      value.scope_coverage.scope_revision = fixture.context.root.session_key;
    },
    (value: any) => {
      value.artifact_availability.semantic_digest = fixture.context.root.session_key;
    },
  ]) {
    const changed = clone(fixture.watermark);
    mutate(changed);
    reject(changed);
  }

  const foreign = clone(fixture.context);
  foreign.capability_context.support_release_id = 'foreign-support-v1';
  reject(fixture.watermark, foreign);

  const adapter = clone(fixture.context);
  adapter.adapter_id = 'foreign';
  reject(fixture.watermark, adapter);
});

test('selected coverage and canonical errors cannot be omitted duplicated or reordered', () => {
  const missing = clone(fixture.context);
  missing.expected_source_coverage.pop();
  reject(fixture.watermark, missing);

  const duplicate = clone(fixture.context);
  duplicate.expected_source_coverage.push(clone(duplicate.expected_source_coverage[1]));
  reject(fixture.watermark, duplicate);

  const reordered = clone(fixture.watermark);
  reordered.source_coverage.reverse();
  reject(reordered);

  const support = clone(fixture.context);
  support.expected_source_coverage[0].scope.support_release_id = 'foreign-support-v1';
  reject(fixture.watermark, support);

  const errors = clone(fixture.watermark);
  errors.explicit_object_errors = [];
  reject(errors);
});

test('queue boundary continuity and portable integers fail closed', () => {
  for (const [field, replacement] of [
    ['scope_epoch', 0],
    ['offered_through_sequence', 4],
    ['delivered_through_sequence', 5],
    ['continuity', 'resyncing'],
    ['queued_semantic_events', 2],
    ['queued_retained_native_bytes', Number.MAX_SAFE_INTEGER + 1],
  ] as const) {
    const changed = clone(fixture.watermark);
    changed.queue_state[field] = replacement;
    reject(changed);
  }

  const staleContext = clone(fixture.context);
  staleContext.expected_offered_through_sequence -= 1;
  reject(fixture.watermark, staleContext);
});

test('strict shape bounds and privacy claims remain closed', () => {
  const unknown = clone(fixture.watermark);
  unknown.future_meaning = true;
  reject(unknown);

  const nestedUnknown = clone(fixture.watermark);
  nestedUnknown.explicit_object_errors[0].native_message = '/Users/alice/private/session.jsonl';
  reject(nestedUnknown);

  const oversized = clone(fixture.watermark);
  oversized.source_coverage = Array.from({ length: 65 }, () => null);
  reject(oversized);

  const encoded = JSON.stringify(fixture);
  assert.doesNotMatch(encoded, /\/Users\/|native_payload|locator_id":"|watermark-source-instance/);
  assert.equal(fixture.expected.capability_and_support_context_bound, true);
  assert.equal(fixture.expected.source_and_scope_coverage_exact, true);
  assert.equal(fixture.expected.artifact_availability_state_bound, true);
  assert.equal(fixture.expected.source_access_authority, false);
  assert.equal(fixture.expected.public_observer_transport, false);
  assert.equal(fixture.expected.native_locator_or_payload, false);
});

test('portable watermark parsing does not depend on the Node Buffer global', () => {
  const globalWithBuffer = globalThis as unknown as { Buffer?: unknown };
  const original = globalWithBuffer.Buffer;
  try {
    globalWithBuffer.Buffer = undefined;
    assert.deepEqual(parseScopedObservationWatermark(fixture.watermark, fixture.context), fixture.watermark);
  } finally {
    globalWithBuffer.Buffer = original;
  }
});
