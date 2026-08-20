import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError, parseQualifiedValue, parseRfc012aV1Fixture } from '../rfc012a.js';
import { MAX_SEMANTIC_FIXTURE_JSON_BYTES } from '../rfc012-semantic-json.js';

interface NativeContractAddon {
  parseRfc012aV1Json: (json: string) => string;
}

const native = createRequire(import.meta.url)('@vibecook/spaghetti-sdk-native') as NativeContractAddon;

const fixtureJson = readFileSync(
  new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012a-v1.json', import.meta.url),
  'utf8',
);

test('native RFC 012A JSON helper preserves portable identities', () => {
  assert.equal(typeof native.parseRfc012aV1Json, 'function');
  const parsedJson = native.parseRfc012aV1Json(fixtureJson);
  assert.doesNotMatch(parsedJson, /\/Users\/|~\/|\.db\b/);
  const nativeFixture = parseRfc012aV1Fixture(JSON.parse(parsedJson));
  const portableFixture = parseRfc012aV1Fixture(JSON.parse(fixtureJson));
  assert.deepEqual(nativeFixture.external_entity_ref, portableFixture.external_entity_ref);
  assert.deepEqual(nativeFixture.semantic_revision_ref, portableFixture.semantic_revision_ref);
  assert.equal(nativeFixture.qualified_known_zero.value, 0);
  assert.equal(nativeFixture.qualified_unknown.unknown_reason, 'withheld');
  assert.equal(nativeFixture.canonical_source_instance_key, portableFixture.canonical_source_instance_key);
  assert.equal(
    nativeFixture.coverage.expected.dominant_vs_baseline,
    portableFixture.coverage.expected.dominant_vs_baseline,
  );
  assert.equal(
    nativeFixture.coverage.expected.baseline_vs_dominant,
    portableFixture.coverage.expected.baseline_vs_dominant,
  );
  assert.equal(nativeFixture.coverage.expected.reset_vs_baseline, portableFixture.coverage.expected.reset_vs_baseline);
});

test('native RFC 012A JSON helper rejects empty, oversized, and unbounded graphs', () => {
  assert.throws(() => native.parseRfc012aV1Json(''), Error);
  assert.throws(
    () => native.parseRfc012aV1Json('x'.repeat(MAX_SEMANTIC_FIXTURE_JSON_BYTES + 1)),
    /invalid semantic fixture: oversized JSON/,
  );
  assert.throws(
    () => native.parseRfc012aV1Json('x'.repeat(MAX_SEMANTIC_FIXTURE_JSON_BYTES + 1_000_000)),
    /invalid semantic fixture: oversized JSON/,
  );
  let tooDeep: unknown = 0;
  for (let depth = 0; depth < 16; depth += 1) {
    tooDeep = { child: tooDeep };
  }
  assert.throws(() => native.parseRfc012aV1Json(JSON.stringify(tooDeep)), Error);
});

test('native RFC 012A JSON helper rejects unknown fields and incompatible majors', () => {
  const extra = JSON.parse(fixtureJson) as { future?: boolean };
  extra.future = true;
  assert.throws(() => native.parseRfc012aV1Json(JSON.stringify(extra)), /invalid semantic fixture: unknown field/);

  const qualified = JSON.parse(fixtureJson) as { qualified_known_zero: { future?: boolean } };
  qualified.qualified_known_zero.future = true;
  assert.throws(() => native.parseRfc012aV1Json(JSON.stringify(qualified)), /invalid semantic fixture: unknown field/);

  const major = JSON.parse(fixtureJson) as { semantic_revision_ref: { semantic_reference_contract_version: number } };
  major.semantic_revision_ref.semantic_reference_contract_version = 2;
  assert.throws(() => native.parseRfc012aV1Json(JSON.stringify(major)), /invalid semantic fixture/);
});

test('native RFC 012A JSON helper rejects raw and escaped unpaired UTF-16', () => {
  const raw = fixtureJson.replace('"authority": "native-response"', `"authority": "${'\uD800'}"`);
  assert.throws(() => native.parseRfc012aV1Json(raw), /invalid semantic fixture: unpaired UTF-16/);
  const escaped = fixtureJson.replace('"authority": "native-response"', '"authority": "\\ud800"');
  assert.throws(() => native.parseRfc012aV1Json(escaped), /invalid semantic fixture/);
});

test('native RFC 012A JSON helper does not echo path-shaped attacker data', () => {
  const extra = JSON.parse(fixtureJson) as Record<string, unknown>;
  extra['/Users/alice/private/session.jsonl'] = true;
  assert.throws(
    () => native.parseRfc012aV1Json(JSON.stringify(extra)),
    (error: unknown) => {
      assert.ok(error instanceof Error);
      assert.match(error.message, /invalid semantic fixture: unknown field/);
      assert.doesNotMatch(error.message, /\/Users\/|alice|private|session\.jsonl/);
      return true;
    },
  );
  const valuePath = JSON.parse(fixtureJson) as { qualified_known_zero: { authority: string } };
  valuePath.qualified_known_zero.authority = ' /Users/alice/private/session.jsonl';
  assert.throws(
    () => native.parseRfc012aV1Json(JSON.stringify(valuePath)),
    (error: unknown) => {
      assert.ok(error instanceof Error);
      assert.match(error.message, /invalid semantic fixture/);
      assert.doesNotMatch(error.message, /\/Users\/|alice|private|session\.jsonl/);
      return true;
    },
  );
});

test('portable RFC 012A parser still rejects the same unknown nested fields', () => {
  const qualified = JSON.parse(fixtureJson) as { qualified_known_zero: { future?: boolean } };
  qualified.qualified_known_zero.future = true;
  assert.throws(() => parseQualifiedValue(qualified.qualified_known_zero), ContractValidationError);
});
