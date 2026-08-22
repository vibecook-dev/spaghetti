import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError } from '../rfc012a.js';
import {
  parseRfc012cMessageV1Json,
  parseRfc012cPlanV1Json,
  parseRfc012cTaskV1Json,
  parseRfc012cToolV1Json,
} from '../rfc012c.js';

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
const toolJson = readFileSync(
  new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012c-tool-v1.json', import.meta.url),
  'utf8',
);
const messageContext = JSON.parse(messageJson) as unknown;
const taskContext = JSON.parse(taskJson) as unknown;
const planContext = JSON.parse(planJson) as unknown;
const toolContext = JSON.parse(toolJson) as unknown;

function clone<T>(value: T): T {
  return structuredClone(value);
}

test('RFC 012C message fixture validates correction and complete/partial block replacement', () => {
  const fixture = parseRfc012cMessageV1Json(messageJson, messageContext);
  assert.equal(fixture.family, 'runtime.message');
  assert.equal(fixture.role, 'assistant');
  assert.deepEqual(fixture.current.ordered_content_block_keys, ['block-a', 'block-b']);
  assert.deepEqual(fixture.correction.ordered_content_block_keys, ['block-a', 'block-c']);
  assert.deepEqual(fixture.complete_blocks.ordered_content_block_keys, ['block-a']);
  assert.equal(fixture.complete_blocks.completeness, 'complete');
  assert.equal(fixture.partial_blocks.completeness, 'partial');
  assert.equal(fixture.retract.operation, 'retract');
  const contentBlock = fixture.content_block;
  assert.ok(contentBlock);
  assert.equal(contentBlock.family, 'runtime.content-block');
  assert.equal(contentBlock.current.ordinal, 0);
  assert.deepEqual(contentBlock.current.content, { kind: 'text', text: ' draft \n' });
  assert.deepEqual(contentBlock.correction.content, { kind: 'text', text: ' final answer \n' });
  assert.equal(contentBlock.partial_retract.completeness, 'partial');
  assert.equal(contentBlock.partial_retract.operation, 'retract');
  assert.notEqual(contentBlock.fact_id, fixture.fact_id);
  assert.notEqual(
    fixture.current.semantic_revision_ref.fact_revision_id,
    fixture.correction.semantic_revision_ref.fact_revision_id,
  );
  assert.doesNotMatch(JSON.stringify(fixture), /\/Users\/|~\/|\.db\b|claude-code|codex|grok/);
});

test('RFC 012C message-v1 keeps the prior field-absent shape compatible but rejects explicit null', () => {
  const prior = clone(messageContext) as Record<string, unknown>;
  delete prior.content_block;
  const parsed = parseRfc012cMessageV1Json(JSON.stringify(prior), prior);
  assert.equal(Object.hasOwn(parsed, 'content_block'), false);

  const explicitNull = clone(messageContext) as Record<string, unknown>;
  explicitNull.content_block = null;
  assert.throws(() => parseRfc012cMessageV1Json(JSON.stringify(explicitNull), explicitNull), ContractValidationError);
});

test('RFC 012C content blocks reject unbounded, open, and malformed typed payloads', () => {
  const mutations: Array<(value: Record<string, unknown>) => void> = [
    (value) => {
      const block = value.content_block as { current: { content: { text: string } } };
      block.current.content.text = 'x'.repeat(8 * 1024 + 1);
    },
    (value) => {
      const block = value.content_block as { current: { native_tool_call_or_result_id?: unknown } };
      delete block.current.native_tool_call_or_result_id;
    },
    (value) => {
      const block = value.content_block as { current: { content: Record<string, unknown> } };
      block.current.content.future = true;
    },
    (value) => {
      const block = value.content_block as { current: { content: unknown } };
      block.current.content = {
        kind: 'native_extension',
        native_kind: '/Users/alice/raw',
        value_digest: Array.from({ length: 32 }, () => 1),
      };
    },
    (value) => {
      const block = value.content_block as { retract: { completeness: string } };
      block.retract.completeness = 'partial';
    },
  ];
  for (const mutate of mutations) {
    const candidate = clone(messageContext) as Record<string, unknown>;
    mutate(candidate);
    assert.throws(() => parseRfc012cMessageV1Json(JSON.stringify(candidate), messageContext), ContractValidationError);
  }
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

test('RFC 012C tool fixture validates unmatched result and correlation without rekeying', () => {
  const fixture = parseRfc012cToolV1Json(toolJson, toolContext);
  assert.equal(fixture.family, 'runtime.tool');
  assert.equal(fixture.call.kind, 'call');
  assert.equal(fixture.unmatched_result.kind, 'result');
  assert.equal(fixture.unmatched_result.correlated_native_id, null);
  assert.equal(fixture.correlated_call.correlated_native_id, 'fixture-result-1');
  assert.equal(fixture.correlated_result.correlated_native_id, 'fixture-call-1');
  assert.equal(fixture.retract.operation, 'retract');
  assert.equal(fixture.partial.completeness, 'partial');
  assert.notEqual(fixture.fact_id, fixture.result_fact_id);
  assert.doesNotMatch(JSON.stringify(fixture), /\/Users\/|~\/|\.db\b|claude-code|codex|grok/);
});

test('message and task fixtures reject identity and snapshot drift', () => {
  const semanticMessageDrift = clone(messageContext) as { role: string };
  semanticMessageDrift.role = 'user';
  assert.throws(
    () => parseRfc012cMessageV1Json(JSON.stringify(semanticMessageDrift), messageContext),
    ContractValidationError,
  );

  const contentDrift = clone(messageContext) as {
    content_block: { correction: { content: { text: string } } };
  };
  contentDrift.content_block.correction.content.text = 'stale identity';
  assert.throws(() => parseRfc012cMessageV1Json(JSON.stringify(contentDrift), messageContext), ContractValidationError);

  const semanticTaskDrift = clone(taskContext) as { subject: string };
  semanticTaskDrift.subject = 'Different task subject';
  assert.throws(() => parseRfc012cTaskV1Json(JSON.stringify(semanticTaskDrift), taskContext), ContractValidationError);

  const semanticPlanDrift = clone(planContext) as { subject: string };
  semanticPlanDrift.subject = 'Different plan subject';
  assert.throws(() => parseRfc012cPlanV1Json(JSON.stringify(semanticPlanDrift), planContext), ContractValidationError);

  const semanticToolDrift = clone(toolContext) as { tool_name: string };
  semanticToolDrift.tool_name = 'write';
  assert.throws(() => parseRfc012cToolV1Json(JSON.stringify(semanticToolDrift), toolContext), ContractValidationError);

  const drifted = clone(messageContext) as { fact_id: string; session: string };
  drifted.fact_id = drifted.session;
  assert.throws(() => parseRfc012cMessageV1Json(JSON.stringify(drifted), messageContext), ContractValidationError);

  const omitted = clone(taskContext) as { collection_omit: { owned_set: string[] } };
  omitted.collection_omit.owned_set = ['fixture-task-1'];
  assert.throws(() => parseRfc012cTaskV1Json(JSON.stringify(omitted), taskContext), ContractValidationError);

  const planDrifted = clone(planContext) as { fact_id: string; session: string };
  planDrifted.fact_id = planDrifted.session;
  assert.throws(() => parseRfc012cPlanV1Json(JSON.stringify(planDrifted), planContext), ContractValidationError);

  const toolDrifted = clone(toolContext) as { fact_id: string; session: string };
  toolDrifted.fact_id = toolDrifted.session;
  assert.throws(() => parseRfc012cToolV1Json(JSON.stringify(toolDrifted), toolContext), ContractValidationError);
});
