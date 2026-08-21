import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { parseEffectiveStateFixture, parseInteractionFixture } from '../rfc012c.js';

interface NativeContractAddon {
  parseRfc012cEffectiveStateV1Json: (json: string) => string;
  parseRfc012cInteractionV1Json: (json: string) => string;
}

const native = createRequire(import.meta.url)('@vibecook/spaghetti-sdk-native') as NativeContractAddon;

const effectiveStateJson = readFileSync(
  new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012c-effective-state-v1.json', import.meta.url),
  'utf8',
);
const interactionJson = readFileSync(
  new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012c-interaction-v1.json', import.meta.url),
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
