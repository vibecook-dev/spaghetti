import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { parseRuntimeContractFixture } from '../rfc012c.js';
import { MAX_SEMANTIC_FIXTURE_JSON_BYTES } from '../rfc012-semantic-json.js';

interface NativeContractAddon {
  parseRfc012cRuntimeV1Json: (json: string) => string;
}

const native = createRequire(import.meta.url)('@vibecook/spaghetti-sdk-native') as NativeContractAddon;

const fixtureJson = readFileSync(
  new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012c-runtime-v1.json', import.meta.url),
  'utf8',
);

test('native RFC 012C JSON helper preserves portable identities', () => {
  assert.equal(typeof native.parseRfc012cRuntimeV1Json, 'function');
  const parsedJson = native.parseRfc012cRuntimeV1Json(fixtureJson);
  assert.doesNotMatch(parsedJson, /\/Users\/|~\/|\.db\b|claude-code|codex|grok/);
  const committed = JSON.parse(fixtureJson) as unknown;
  const nativeFixture = parseRuntimeContractFixture(JSON.parse(parsedJson), committed);
  const portableFixture = parseRuntimeContractFixture(committed, committed);
  assert.deepEqual(nativeFixture.actors.root.semantic_revision_ref, portableFixture.actors.root.semantic_revision_ref);
  assert.deepEqual(
    nativeFixture.actors.child.semantic_revision_ref,
    portableFixture.actors.child.semantic_revision_ref,
  );
  assert.equal(nativeFixture.actors.child.revision.parent_actor_run, nativeFixture.actors.root.revision.actor_run);
  assert.deepEqual(
    nativeFixture.affiliations.child_workflow_present.semantic_revision_ref,
    portableFixture.affiliations.child_workflow_present.semantic_revision_ref,
  );
  assert.notEqual(
    nativeFixture.affiliations.child_workflow_present.semantic_revision_ref.fact_revision_id,
    nativeFixture.affiliations.child_workflow_removed.semantic_revision_ref.fact_revision_id,
  );
  assert.deepEqual(
    nativeFixture.usage.response_revisions.a.semantic_revision_ref,
    portableFixture.usage.response_revisions.a.semantic_revision_ref,
  );
  assert.deepEqual(
    nativeFixture.usage.response_revisions.a.semantic_revision_ref,
    nativeFixture.usage.response_revisions.a_repeat.semantic_revision_ref,
  );
  assert.notEqual(
    nativeFixture.usage.response_revisions.a.semantic_revision_ref.fact_revision_id,
    nativeFixture.usage.response_revisions.b.semantic_revision_ref.fact_revision_id,
  );
  assert.equal(nativeFixture.usage.native_message.revision.buckets.input_tokens.value, 0);
});

test('native RFC 012C JSON helper rejects empty and oversized JSON', () => {
  assert.throws(() => native.parseRfc012cRuntimeV1Json(''), Error);
  assert.throws(
    () => native.parseRfc012cRuntimeV1Json('x'.repeat(MAX_SEMANTIC_FIXTURE_JSON_BYTES + 1)),
    /invalid semantic fixture: oversized JSON/,
  );
  assert.throws(
    () => native.parseRfc012cRuntimeV1Json('x'.repeat(MAX_SEMANTIC_FIXTURE_JSON_BYTES + 1_000_000)),
    /invalid semantic fixture: oversized JSON/,
  );
});

test('native RFC 012C JSON helper rejects unknown nested fields', () => {
  const extra = JSON.parse(fixtureJson) as {
    actors: { root: { revision: { extra_future_field?: string } } };
  };
  extra.actors.root.revision.extra_future_field = 'ignored';
  assert.throws(
    () => native.parseRfc012cRuntimeV1Json(JSON.stringify(extra)),
    /invalid semantic fixture: unknown field/,
  );

  const bucket = JSON.parse(fixtureJson) as {
    usage: { native_message: { revision: { buckets: { input_tokens: { future?: boolean } } } } };
  };
  bucket.usage.native_message.revision.buckets.input_tokens.future = true;
  assert.throws(
    () => native.parseRfc012cRuntimeV1Json(JSON.stringify(bucket)),
    /invalid semantic fixture: unknown field/,
  );

  const major = JSON.parse(fixtureJson) as { fixture_contract_version: number };
  major.fixture_contract_version = 2;
  assert.throws(() => native.parseRfc012cRuntimeV1Json(JSON.stringify(major)), /invalid semantic fixture/);
});

test('native RFC 012C JSON helper does not echo path-shaped attacker data', () => {
  const extra = JSON.parse(fixtureJson) as Record<string, unknown>;
  extra['/Users/alice/private/session.jsonl'] = true;
  assert.throws(
    () => native.parseRfc012cRuntimeV1Json(JSON.stringify(extra)),
    (error: unknown) => {
      assert.ok(error instanceof Error);
      assert.match(error.message, /invalid semantic fixture: unknown field/);
      assert.doesNotMatch(error.message, /\/Users\/|alice|private|session\.jsonl/);
      return true;
    },
  );
});
