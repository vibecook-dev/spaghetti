import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError } from '../rfc012a.js';
import {
  parseActorAffiliationRevision,
  parseActorRunRevision,
  parseRuntimeContractFixture,
  parseUsageRevisionV2,
  RUNTIME_SEMANTIC_CONTRACT_VERSION,
} from '../rfc012c.js';

const rawFixture = JSON.parse(
  readFileSync(
    new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012c-runtime-v1.json', import.meta.url),
    'utf8',
  ),
) as unknown;

function clone<T>(value: T): T {
  return structuredClone(value);
}

function reject(value: unknown, parse: (input: unknown) => unknown): void {
  assert.throws(() => parse(value), ContractValidationError);
}

test('Rust RFC 012C v1 fixture validates in the portable SDK', () => {
  const fixture = parseRuntimeContractFixture(rawFixture);
  assert.equal(fixture.fixture_contract_version, RUNTIME_SEMANTIC_CONTRACT_VERSION);
  assert.equal(fixture.runtime_semantic_contract_version, RUNTIME_SEMANTIC_CONTRACT_VERSION);
  assert.deepEqual(
    fixture.families.map((family) => [family.family, family.version]),
    [
      ['runtime.actor-run', 1],
      ['runtime.actor-affiliation', 1],
      ['runtime.usage-v2', 1],
    ],
  );

  assert.equal(fixture.actors.root.revision.role, 'root');
  assert.equal(fixture.actors.root.revision.parent_actor_run, null);
  assert.equal(fixture.actors.child.revision.role, 'child');
  assert.equal(fixture.actors.child.revision.parent_actor_run, fixture.actors.root.revision.actor_run);
  assert.equal(fixture.actors.child.revision.session, fixture.source.session.entity_key);
  assert.equal(fixture.source.session.external_ref.entity_key, fixture.source.session.entity_key);
  assert.match(fixture.source.session.entity_key, /^v1:[A-Za-z0-9_-]{43}$/);
  assert.doesNotMatch(JSON.stringify(fixture), /\/Users\/|~\/|\.db\b|claude-code|codex|grok/);

  assert.equal(fixture.affiliations.child_team_present.revision.dimension, 'team');
  assert.equal(fixture.affiliations.child_workflow_present.revision.dimension, 'workflow');
  assert.equal(fixture.affiliations.child_workflow_present.revision.state, 'present');
  assert.equal(fixture.affiliations.child_workflow_removed.revision.state, 'removed');
  assert.equal(fixture.affiliations.child_team_present.revision.actor_run, fixture.actors.child.revision.actor_run);
  assert.equal(fixture.affiliations.child_workflow_present.revision.actor_run, fixture.actors.child.revision.actor_run);
  assert.equal(
    fixture.affiliations.child_workflow_present.fact_id,
    fixture.affiliations.child_workflow_removed.fact_id,
  );
  assert.notEqual(
    fixture.affiliations.child_workflow_present.semantic_revision_ref.fact_revision_id,
    fixture.affiliations.child_workflow_removed.semantic_revision_ref.fact_revision_id,
  );

  const native = fixture.usage.native_message.revision;
  assert.equal(native.response_identity, 'native_message_id');
  assert.equal(native.native_message_id, 'msg_fixture_native_1');
  assert.equal(native.buckets.input_tokens.value, 0);
  assert.equal(native.buckets.output_tokens.value, 42);
  assert.equal(native.buckets.cache_read_input_tokens.value, null);
  assert.equal(native.buckets.cache_read_input_tokens.unknown_reason, 'missing');
  assert.equal(native.model?.value, 'fixture-model-1');
  assert.equal(native.model?.authority, 'native_response');
  assert.equal(native.effort?.value, 'high');
  assert.equal(native.effort?.authority, 'adapter_derived');
  assert.equal(native.effort?.completeness, 'partial');

  const fallback = fixture.usage.source_record_fallback.revision;
  assert.equal(fallback.response_identity, 'source_record_fallback');
  assert.equal(fallback.native_message_id, null);

  const aba = fixture.usage.response_revisions;
  assert.equal(aba.a.semantic_revision_ref.fact_revision_id, aba.a_repeat.semantic_revision_ref.fact_revision_id);
  assert.equal(aba.a.semantic_revision_key_hex, aba.a_repeat.semantic_revision_key_hex);
  assert.notEqual(aba.a.semantic_revision_ref.fact_revision_id, aba.b.semantic_revision_ref.fact_revision_id);
  assert.equal(aba.a.fact_id, aba.b.fact_id);
  assert.equal(aba.a.revision.buckets.input_tokens.value, 10);
  assert.equal(aba.b.revision.buckets.input_tokens.value, 20);
});

test('unknown extras are retained only as uninterpreted members', () => {
  const raw = clone(rawFixture) as {
    usage: { native_message: { revision: { extra_future_field?: string } } };
  };
  raw.usage.native_message.revision.extra_future_field = 'ignored';
  const parsed = parseRuntimeContractFixture(raw);
  assert.equal(Object.prototype.hasOwnProperty.call(parsed.usage.native_message.revision, 'extra_future_field'), false);
});

test('unknown contract majors and malformed opaque references are rejected', () => {
  const fixture = clone(rawFixture) as { fixture_contract_version: number };
  fixture.fixture_contract_version = 2;
  reject(fixture, parseRuntimeContractFixture);

  const semantic = clone(rawFixture) as { runtime_semantic_contract_version: number };
  semantic.runtime_semantic_contract_version = 2;
  reject(semantic, parseRuntimeContractFixture);

  const family = clone(rawFixture) as { families: Array<{ version: number }> };
  family.families[2]!.version = 2;
  reject(family, parseRuntimeContractFixture);

  const actor = clone((rawFixture as { actors: { root: { revision: unknown } } }).actors.root.revision) as {
    actor_run: string;
  };
  actor.actor_run = 'not-a-ref';
  reject(actor, parseActorRunRevision);

  const versioned = clone((rawFixture as { actors: { root: { revision: unknown } } }).actors.root.revision) as {
    actor_run: string;
  };
  versioned.actor_run = 'v2:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA';
  reject(versioned, parseActorRunRevision);

  const ref = clone(
    (rawFixture as { actors: { root: { semantic_revision_ref: { semantic_reference_contract_version: number } } } })
      .actors.root.semantic_revision_ref,
  );
  ref.semantic_reference_contract_version = 2;
  const wrapped = clone(rawFixture) as {
    actors: { root: { semantic_revision_ref: typeof ref } };
  };
  wrapped.actors.root.semantic_revision_ref = ref;
  reject(wrapped, parseRuntimeContractFixture);
});

test('actor lineage and affiliation enumerations are rejected when invalid', () => {
  const root = clone((rawFixture as { actors: { root: { revision: unknown } } }).actors.root.revision) as {
    parent_actor_run: string | null;
    role: string;
    native_session_id: string | null;
  };
  root.parent_actor_run = (
    rawFixture as { actors: { root: { revision: { actor_run: string } } } }
  ).actors.root.revision.actor_run;
  reject(root, parseActorRunRevision);

  const child = clone((rawFixture as { actors: { child: { revision: unknown } } }).actors.child.revision) as {
    parent_actor_run: string | null;
    actor_run: string;
    role: string;
    native_actor_id: string | null;
  };
  child.parent_actor_run = null;
  reject(child, parseActorRunRevision);

  child.parent_actor_run = child.actor_run;
  reject(child, parseActorRunRevision);

  root.role = 'subagent';
  reject(root, parseActorRunRevision);
  root.role = 'root';
  root.native_session_id = '';
  reject(root, parseActorRunRevision);

  const affiliation = clone(
    (rawFixture as { affiliations: { child_team_present: { revision: unknown } } }).affiliations.child_team_present
      .revision,
  ) as { dimension: string; state: string };
  affiliation.dimension = 'project';
  reject(affiliation, parseActorAffiliationRevision);
  affiliation.dimension = 'team';
  affiliation.state = 'expired';
  reject(affiliation, parseActorAffiliationRevision);
});

test('usage identity, base64, and token-value rejection classes are covered', () => {
  const native = clone(
    (rawFixture as { usage: { native_message: { revision: unknown } } }).usage.native_message.revision,
  ) as {
    response_key: unknown;
    native_message_id: string | null;
    request_id: string | null;
    response_identity: string;
    buckets: { input_tokens: { value: unknown; quality: string; unknown_reason?: string; completeness: string } };
    model: { authority: string; provenance: { native_field: string; normalization_contract_version: unknown } };
  };

  native.response_key = 'bXNnX2ZpeHR1cmVfYWJh';
  reject(native, parseUsageRevisionV2);

  const fallback = clone(
    (rawFixture as { usage: { source_record_fallback: { revision: unknown } } }).usage.source_record_fallback.revision,
  ) as { native_message_id: string | null };
  fallback.native_message_id = 'msg_fixture_native_1';
  reject(fallback, parseUsageRevisionV2);

  native.response_key = '';
  reject(native, parseUsageRevisionV2);
  native.response_key = '%%%';
  reject(native, parseUsageRevisionV2);
  native.response_key = Buffer.from('msg_fixture_native_1').toString('base64url');
  reject(native, parseUsageRevisionV2);
  native.response_key = Buffer.from('msg_fixture_native_1').toString('base64').replace(/=+$/, '');
  reject(native, parseUsageRevisionV2);
  native.response_key = Array.from(Buffer.from('msg_fixture_native_1'));
  reject(native, parseUsageRevisionV2);

  native.response_key = 'bXNnX2ZpeHR1cmVfbmF0aXZlXzE=';
  native.request_id = '';
  reject(native, parseUsageRevisionV2);
  native.request_id = 'req_fixture_1';

  native.buckets.input_tokens.value = -1;
  reject(native, parseUsageRevisionV2);
  native.buckets.input_tokens.value = 1.5;
  reject(native, parseUsageRevisionV2);
  native.buckets.input_tokens.value = '10';
  reject(native, parseUsageRevisionV2);
  native.buckets.input_tokens.value = 2 ** 53;
  reject(native, parseUsageRevisionV2);
  native.buckets.input_tokens.value = 0;

  native.model.authority = 'guessed';
  reject(native, parseUsageRevisionV2);
  native.model.authority = 'native_response';
  native.model.provenance.native_field = '';
  reject(native, parseUsageRevisionV2);
  native.model.provenance.native_field = 'message.model';
  native.model.provenance.normalization_contract_version = 0;
  reject(native, parseUsageRevisionV2);
  native.model.provenance.normalization_contract_version = 1.2;
  reject(native, parseUsageRevisionV2);
  native.model.provenance.normalization_contract_version = 1;

  native.buckets.input_tokens.quality = 'unknown';
  native.buckets.input_tokens.value = 0;
  native.buckets.input_tokens.unknown_reason = 'missing';
  reject(native, parseUsageRevisionV2);

  native.buckets.input_tokens.value = null;
  delete native.buckets.input_tokens.unknown_reason;
  reject(native, parseUsageRevisionV2);

  native.buckets.input_tokens.unknown_reason = 'missing';
  native.buckets.input_tokens.completeness = 'complete';
  reject(native, parseUsageRevisionV2);

  native.buckets.input_tokens.quality = 'exact';
  native.buckets.input_tokens.value = null;
  native.buckets.input_tokens.completeness = 'complete';
  delete native.buckets.input_tokens.unknown_reason;
  reject(native, parseUsageRevisionV2);

  native.buckets.input_tokens.value = 0;
  native.buckets.input_tokens.unknown_reason = 'missing';
  reject(native, parseUsageRevisionV2);
});
