import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError } from '../rfc012a.js';
import { parseRfc012cMessageV1Json, parseRfc012cPlanV1Json, parseRfc012cTaskV1Json } from '../rfc012c.js';

const messageJson = readFileSync(
  new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012c-message-v1.json', import.meta.url),
  'utf8',
);
const taskJson = readFileSync(
  new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012c-task-v1.json', import.meta.url),
  'utf8',
);
const planJson = readFileSync(
  new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012c-plan-v1.json', import.meta.url),
  'utf8',
);
const messageContext = JSON.parse(messageJson) as unknown;
const taskContext = JSON.parse(taskJson) as unknown;
const planContext = JSON.parse(planJson) as unknown;

function clone<T>(value: T): T {
  return structuredClone(value);
}

test('RFC 012C message fixture validates correction and complete/partial block replacement', () => {
  const fixture = parseRfc012cMessageV1Json(messageJson, messageContext);
  assert.equal(fixture.family, 'runtime.message');
  assert.equal(fixture.role, 'assistant');
  assert.deepEqual(fixture.current.ordered_content_block_keys, ['block-a', 'block-b']);
  assert.deepEqual(fixture.complete_blocks.ordered_content_block_keys, ['block-a']);
  assert.equal(fixture.complete_blocks.completeness, 'complete');
  assert.equal(fixture.partial_blocks.completeness, 'partial');
  assert.equal(fixture.retract.operation, 'retract');
  assert.notEqual(
    fixture.current.semantic_revision_ref.fact_revision_id,
    fixture.correction.semantic_revision_ref.fact_revision_id,
  );
  assert.doesNotMatch(JSON.stringify(fixture), /\/Users\/|~\/|\.db\b|claude-code|codex|grok/);
});

test('RFC 012C task fixture validates lifecycle and complete owned-set omission', () => {
  const fixture = parseRfc012cTaskV1Json(taskJson, taskContext);
  assert.equal(fixture.family, 'runtime.task');
  assert.equal(fixture.created.state, 'created');
  assert.equal(fixture.updated.state, 'updated');
  assert.equal(fixture.completed.state, 'completed');
  assert.equal(fixture.retract.operation, 'retract');
  assert.equal(fixture.partial.completeness, 'partial');
  assert.deepEqual(fixture.collection_omit.owned_set, ['fixture-task-2']);
  assert.notEqual(fixture.fact_id, fixture.peer_fact_id);
});

test('RFC 012C plan fixture validates step replacement and complete owned-set omission', () => {
  const fixture = parseRfc012cPlanV1Json(planJson, planContext);
  assert.equal(fixture.family, 'runtime.plan');
  assert.deepEqual(fixture.current.ordered_step_keys, ['step-a', 'step-b']);
  assert.deepEqual(fixture.complete_steps.ordered_step_keys, ['step-a']);
  assert.equal(fixture.complete_steps.completeness, 'complete');
  assert.equal(fixture.partial_steps.completeness, 'partial');
  assert.equal(fixture.retract.operation, 'retract');
  assert.deepEqual(fixture.collection_omit.owned_set, ['fixture-plan-2']);
  assert.notEqual(fixture.fact_id, fixture.peer_fact_id);
  assert.doesNotMatch(JSON.stringify(fixture), /\/Users\/|~\/|\.db\b|claude-code|codex|grok/);
});

test('message and task fixtures reject identity and snapshot drift', () => {
  const drifted = clone(messageContext) as { fact_id: string; session: string };
  drifted.fact_id = drifted.session;
  assert.throws(() => parseRfc012cMessageV1Json(JSON.stringify(drifted), messageContext), ContractValidationError);

  const omitted = clone(taskContext) as { collection_omit: { owned_set: string[] } };
  omitted.collection_omit.owned_set = ['fixture-task-1'];
  assert.throws(() => parseRfc012cTaskV1Json(JSON.stringify(omitted), taskContext), ContractValidationError);

  const planDrifted = clone(planContext) as { fact_id: string; session: string };
  planDrifted.fact_id = planDrifted.session;
  assert.throws(() => parseRfc012cPlanV1Json(JSON.stringify(planDrifted), planContext), ContractValidationError);
});
