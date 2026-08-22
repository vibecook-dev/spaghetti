import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import {
  parseEffectiveStateFixture,
  parseInteractionFixture,
  parseMessageFixture,
  parsePlanFixture,
  parseTaskFixture,
  parseToolFixture,
} from '../rfc012c.js';

interface NativeContractAddon {
  parseRfc012cEffectiveStateV1Json: (json: string) => string;
  parseRfc012cInteractionV1Json: (json: string) => string;
  parseRfc012cMessageV1Json: (json: string) => string;
  parseRfc012cTaskV1Json: (json: string) => string;
  parseRfc012cPlanV1Json: (json: string) => string;
  parseRfc012cToolV1Json: (json: string) => string;
}

const native = createRequire(import.meta.url)('@vibecook/spaghetti-sdk-native') as NativeContractAddon;

const effectiveStateJson = readFileSync(
  new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012c-effective-state-v1.json', import.meta.url),
  'utf8',
);
const effectiveStateDimensionJsons = [
  readFileSync(
    new URL(
      '../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012c-effective-state-effort-v1.json',
      import.meta.url,
    ),
    'utf8',
  ),
  readFileSync(
    new URL(
      '../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012c-effective-state-session-mode-v1.json',
      import.meta.url,
    ),
    'utf8',
  ),
  readFileSync(
    new URL(
      '../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012c-effective-state-permission-mode-v1.json',
      import.meta.url,
    ),
    'utf8',
  ),
];
const interactionJson = readFileSync(
  new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012c-interaction-v1.json', import.meta.url),
  'utf8',
);
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

test('native RFC 012C effective-state helper preserves portable identities', () => {
  assert.equal(typeof native.parseRfc012cEffectiveStateV1Json, 'function');
  const parsedJson = native.parseRfc012cEffectiveStateV1Json(effectiveStateJson);
  const committed = JSON.parse(effectiveStateJson) as unknown;
  const nativeFixture = parseEffectiveStateFixture(JSON.parse(parsedJson), committed);
  const portableFixture = parseEffectiveStateFixture(committed, committed);
  assert.deepEqual(nativeFixture.configured.semantic_revision_ref, portableFixture.configured.semantic_revision_ref);
  assert.notEqual(
    nativeFixture.configured.semantic_revision_ref.fact_revision_id,
    nativeFixture.observed.semantic_revision_ref.fact_revision_id,
  );
  const pathProvenance = structuredClone(committed) as {
    configured: { value: { provenance: { native_field: string } } };
  };
  pathProvenance.configured.value.provenance.native_field = '/Users/alice/model';
  assert.throws(() => native.parseRfc012cEffectiveStateV1Json(JSON.stringify(pathProvenance)));
  assert.throws(() => parseEffectiveStateFixture(pathProvenance, pathProvenance));
});

test('native RFC 012C effective-state helper preserves every independent dimension', () => {
  const dimensions = new Set<string>();
  const factIds = new Set<string>();
  for (const json of [effectiveStateJson, ...effectiveStateDimensionJsons]) {
    const committed = JSON.parse(json) as unknown;
    const nativeFixture = parseEffectiveStateFixture(
      JSON.parse(native.parseRfc012cEffectiveStateV1Json(json)) as unknown,
      committed,
    );
    dimensions.add(nativeFixture.dimension);
    factIds.add(nativeFixture.fact_id);
  }
  assert.deepEqual([...dimensions].sort(), ['effort', 'model', 'permission_mode', 'session_mode']);
  assert.equal(factIds.size, 4);
});

test('native RFC 012C interaction helper preserves RFC 012C §11 lifecycle identities', () => {
  assert.equal(typeof native.parseRfc012cInteractionV1Json, 'function');
  const parsedJson = native.parseRfc012cInteractionV1Json(interactionJson);
  const committed = JSON.parse(interactionJson) as unknown;
  const nativeFixture = parseInteractionFixture(JSON.parse(parsedJson), committed);
  const portableFixture = parseInteractionFixture(committed, committed);
  assert.equal(nativeFixture.pending.state, 'pending');
  assert.equal(nativeFixture.failed.state, 'failed');
  assert.equal(nativeFixture.cancelled.state, 'cancelled');
  assert.deepEqual(nativeFixture.resolved.semantic_revision_ref, portableFixture.resolved.semantic_revision_ref);
  assert.notEqual(
    nativeFixture.pending.semantic_revision_ref.fact_revision_id,
    nativeFixture.resolved.semantic_revision_ref.fact_revision_id,
  );
  assert.throws(() => native.parseRfc012cInteractionV1Json(''), Error);
  const extra = JSON.parse(interactionJson) as Record<string, unknown>;
  extra.native_payload = { prompt: 'raw' };
  assert.throws(() => native.parseRfc012cInteractionV1Json(JSON.stringify(extra)), /invalid semantic fixture/);
});

test('native RFC 012C message, task, plan, and tool helpers preserve value-bound identities', () => {
  const cases = [
    [messageJson, native.parseRfc012cMessageV1Json, parseMessageFixture],
    [taskJson, native.parseRfc012cTaskV1Json, parseTaskFixture],
    [planJson, native.parseRfc012cPlanV1Json, parsePlanFixture],
    [toolJson, native.parseRfc012cToolV1Json, parseToolFixture],
  ] as const;
  for (const [json, nativeParse, portableParse] of cases) {
    assert.equal(typeof nativeParse, 'function');
    const committed = JSON.parse(json) as unknown;
    const nativeFixture = portableParse(JSON.parse(nativeParse(json)) as unknown, committed);
    const portableFixture = portableParse(committed, committed);
    assert.deepEqual(nativeFixture, portableFixture);
  }
});

test('native RFC 012C message-v1 preserves the prior field-absent shape', () => {
  const prior = JSON.parse(messageJson) as Record<string, unknown>;
  delete prior.content_block;
  const roundTrip = JSON.parse(native.parseRfc012cMessageV1Json(JSON.stringify(prior))) as Record<string, unknown>;
  assert.equal(Object.hasOwn(roundTrip, 'content_block'), false);

  const explicitNull = JSON.parse(messageJson) as Record<string, unknown>;
  explicitNull.content_block = null;
  assert.throws(() => native.parseRfc012cMessageV1Json(JSON.stringify(explicitNull)), /invalid semantic fixture/);
});

test('native and portable RFC 012C C1 helpers reject semantic mutation with stale identity', () => {
  const cases: Array<{
    json: string;
    nativeParse: (json: string) => string;
    portableParse: (value: unknown, expected: unknown) => unknown;
    mutate: (value: Record<string, unknown>) => void;
  }> = [
    {
      json: effectiveStateJson,
      nativeParse: native.parseRfc012cEffectiveStateV1Json,
      portableParse: parseEffectiveStateFixture,
      mutate: (value) => {
        const configured = value.configured as Record<string, unknown>;
        (configured.value as Record<string, unknown>).value = 'claude-opus';
      },
    },
    {
      json: interactionJson,
      nativeParse: native.parseRfc012cInteractionV1Json,
      portableParse: parseInteractionFixture,
      mutate: (value) => {
        const questions = value.questions as Array<Record<string, unknown>>;
        questions[0]!.prompt = 'Which different option should we take?';
      },
    },
    {
      json: messageJson,
      nativeParse: native.parseRfc012cMessageV1Json,
      portableParse: parseMessageFixture,
      mutate: (value) => {
        const contentBlock = value.content_block as Record<string, unknown>;
        const correction = contentBlock.correction as Record<string, unknown>;
        (correction.content as Record<string, unknown>).text = 'stale identity';
      },
    },
    {
      json: taskJson,
      nativeParse: native.parseRfc012cTaskV1Json,
      portableParse: parseTaskFixture,
      mutate: (value) => {
        value.subject = 'Different task subject';
      },
    },
    {
      json: planJson,
      nativeParse: native.parseRfc012cPlanV1Json,
      portableParse: parsePlanFixture,
      mutate: (value) => {
        value.subject = 'Different plan subject';
      },
    },
    {
      json: toolJson,
      nativeParse: native.parseRfc012cToolV1Json,
      portableParse: parseToolFixture,
      mutate: (value) => {
        value.tool_name = 'write';
      },
    },
  ];

  for (const { json, nativeParse, portableParse, mutate } of cases) {
    const expected = JSON.parse(json) as Record<string, unknown>;
    const mutated = structuredClone(expected);
    mutate(mutated);
    const mutatedJson = JSON.stringify(mutated);
    assert.throws(() => nativeParse(mutatedJson), /invalid semantic fixture/);
    assert.throws(() => portableParse(mutated, expected));
  }
});
