import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError } from '../rfc012a.js';
import { parseRfc012cEffectiveStateV1Json, parseRfc012cInteractionV1Json } from '../rfc012c.js';

const effectiveStateJson = readFileSync(
  new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012c-effective-state-v1.json', import.meta.url),
  'utf8',
);
const interactionJson = readFileSync(
  new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012c-interaction-v1.json', import.meta.url),
  'utf8',
);
const effectiveStateContext = JSON.parse(effectiveStateJson) as unknown;
const interactionContext = JSON.parse(interactionJson) as unknown;

function clone<T>(value: T): T {
  return structuredClone(value);
}

test('RFC 012C effective-state fixture validates configured, observed, and retract identities', () => {
  const fixture = parseRfc012cEffectiveStateV1Json(effectiveStateJson, effectiveStateContext);
  assert.equal(fixture.family, 'runtime.effective-state');
  assert.equal(fixture.dimension, 'model');
  assert.equal(fixture.configured.evidence_kind, 'configured_intent');
  assert.equal(fixture.observed.evidence_kind, 'response_observed');
  assert.equal(fixture.retract.operation, 'retract');
  assert.notEqual(
    fixture.configured.semantic_revision_ref.fact_revision_id,
    fixture.observed.semantic_revision_ref.fact_revision_id,
  );
  assert.doesNotMatch(JSON.stringify(fixture), /\/Users\/|~\/|\.db\b|claude-code|codex|grok/);
});

test('RFC 012C interaction fixture validates RFC 012C §11 lifecycle slots', () => {
  const fixture = parseRfc012cInteractionV1Json(interactionJson, interactionContext);
  assert.equal(fixture.family, 'runtime.user-input-request');
  assert.equal(fixture.kind, 'choice');
  assert.equal(fixture.pending.state, 'pending');
  assert.equal(fixture.resolved.state, 'resolved');
  assert.equal(fixture.failed.state, 'failed');
  assert.equal(fixture.cancelled.state, 'cancelled');
  assert.equal(fixture.retract.operation, 'retract');
  assert.equal(fixture.partial.completeness, 'partial');
  assert.equal(fixture.pending.result_reference, null);
  assert.equal(fixture.resolved.result_reference, 'continue');
  assert.equal(fixture.questions[0]?.prompt, 'Which option should we take?');
  assert.notEqual(
    fixture.pending.semantic_revision_ref.fact_revision_id,
    fixture.resolved.semantic_revision_ref.fact_revision_id,
  );
});

test('effective-state and interaction fixtures reject identity and lifecycle drift', () => {
  const driftedState = clone(effectiveStateContext) as {
    fact_id: string;
    session: string;
    observed: { semantic_revision_ref: unknown; semantic_revision_key_hex: string };
    configured: { semantic_revision_ref: unknown; semantic_revision_key_hex: string };
  };
  driftedState.fact_id = driftedState.session;
  assert.throws(
    () => parseRfc012cEffectiveStateV1Json(JSON.stringify(driftedState), effectiveStateContext),
    ContractValidationError,
  );

  const reused = clone(effectiveStateContext) as {
    observed: { semantic_revision_ref: unknown; semantic_revision_key_hex: string };
    configured: { semantic_revision_ref: unknown; semantic_revision_key_hex: string };
  };
  reused.observed.semantic_revision_ref = reused.configured.semantic_revision_ref;
  reused.observed.semantic_revision_key_hex = reused.configured.semantic_revision_key_hex;
  assert.throws(
    () => parseRfc012cEffectiveStateV1Json(JSON.stringify(reused), effectiveStateContext),
    ContractValidationError,
  );

  const pendingResult = clone(interactionContext) as { pending: { result_reference: string | null } };
  pendingResult.pending.result_reference = 'continue';
  assert.throws(
    () => parseRfc012cInteractionV1Json(JSON.stringify(pendingResult), interactionContext),
    ContractValidationError,
  );

  const extra = clone(interactionContext) as Record<string, unknown>;
  extra.native_payload = { prompt: 'raw' };
  assert.throws(
    () => parseRfc012cInteractionV1Json(JSON.stringify(extra), interactionContext),
    ContractValidationError,
  );
});
