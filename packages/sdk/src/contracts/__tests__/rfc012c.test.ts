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

function parseFixture(value: unknown) {
  return parseRuntimeContractFixture(value, rawFixture);
}

test('Rust RFC 012C v1 fixture validates in the portable SDK', () => {
  const fixture = parseFixture(rawFixture);
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
  assert.match(fixture.actors.root.semantic_revision_key_hex, /^[0-9a-f]{64}$/);
  assert.match(fixture.actors.child.semantic_revision_key_hex, /^[0-9a-f]{64}$/);
  assert.equal(fixture.source.session.external_ref.entity_key, fixture.source.session.entity_key);
  assert.match(fixture.source.session.entity_key, /^v1:[A-Za-z0-9_-]{43}$/);
  assert.doesNotMatch(JSON.stringify(fixture), /\/Users\/|~\/|\.db\b|claude-code|codex|grok/);

  assert.equal(fixture.affiliations.child_team_present.revision.dimension, 'team');
  assert.equal(fixture.affiliations.child_workflow_present.revision.dimension, 'workflow');
  assert.equal(fixture.affiliations.child_workflow_present.revision.state, 'present');
  assert.equal(fixture.affiliations.child_workflow_present.revision.effective_at, null);
  assert.equal(fixture.affiliations.child_workflow_removed.revision.state, 'removed');
  assert.match(fixture.affiliations.child_team_present.semantic_revision_key_hex, /^[0-9a-f]{64}$/);
  assert.notEqual(
    fixture.affiliations.child_workflow_present.semantic_revision_key_hex,
    fixture.affiliations.child_workflow_removed.semantic_revision_key_hex,
  );
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
  assert.equal(native.source_time, null);

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

test('unknown extras are rejected at every nested runtime value', () => {
  const extras: Array<[string, (value: Record<string, unknown>) => void]> = [
    ['fixture', (value) => (value.future = true)],
    [
      'actor revision',
      (value) => {
        const revision = (value.actors as { root: { revision: Record<string, unknown> } }).root.revision;
        revision.extra_future_field = 'ignored';
      },
    ],
    [
      'affiliation revision',
      (value) => {
        const revision = (value.affiliations as { child_team_present: { revision: Record<string, unknown> } })
          .child_team_present.revision;
        revision.extra_future_field = 'ignored';
      },
    ],
    [
      'usage revision',
      (value) => {
        const revision = (value.usage as { native_message: { revision: Record<string, unknown> } }).native_message
          .revision;
        revision.extra_future_field = 'ignored';
      },
    ],
    [
      'usage bucket',
      (value) => {
        const bucket = (
          value.usage as {
            native_message: { revision: { buckets: { input_tokens: Record<string, unknown> } } };
          }
        ).native_message.revision.buckets.input_tokens;
        bucket.future = true;
      },
    ],
    [
      'usage provenance',
      (value) => {
        const provenance = (
          value.usage as {
            native_message: { revision: { buckets: { input_tokens: { provenance: Record<string, unknown> } } } };
          }
        ).native_message.revision.buckets.input_tokens.provenance;
        provenance.future = true;
      },
    ],
  ];
  for (const [label, mutate] of extras) {
    const raw = clone(rawFixture) as Record<string, unknown>;
    mutate(raw);
    assert.throws(() => parseFixture(raw), ContractValidationError, label);
  }
});

test('portable parsing does not depend on the Node Buffer global', () => {
  const globals = globalThis as unknown as { Buffer?: unknown };
  const originalBuffer = globals.Buffer;
  globals.Buffer = undefined;
  try {
    const fixture = parseFixture(rawFixture);
    assert.equal(fixture.usage.native_message.revision.native_message_id, 'msg_fixture_native_1');
  } finally {
    globals.Buffer = originalBuffer;
  }
});

test('known native timestamp fields are preserved and validated', () => {
  const affiliation = clone(
    (rawFixture as { affiliations: { child_team_present: { revision: unknown } } }).affiliations.child_team_present
      .revision,
  ) as { effective_at: unknown };
  affiliation.effective_at = { value: '2026-08-16T00:00:00Z', quality: 'NativeExact' };
  assert.deepEqual(parseActorAffiliationRevision(affiliation).effective_at, affiliation.effective_at);

  affiliation.effective_at = { value: '', quality: 'NativeExact' };
  reject(affiliation, parseActorAffiliationRevision);
  affiliation.effective_at = { value: '2026-08-16T00:00:00Z', quality: 'instant' };
  reject(affiliation, parseActorAffiliationRevision);

  const usage = clone(
    (rawFixture as { usage: { native_message: { revision: unknown } } }).usage.native_message.revision,
  ) as { source_time: unknown };
  usage.source_time = { value: '2026-08-16T00:00:00Z', quality: 'NativeApproximate' };
  assert.deepEqual(parseUsageRevisionV2(usage).source_time, usage.source_time);

  usage.source_time = { value: '2026-08-16T00:00:00Z', quality: 'wall_clock' };
  reject(usage, parseUsageRevisionV2);
});

test('unknown contract majors and malformed opaque references are rejected', () => {
  const fixture = clone(rawFixture) as { fixture_contract_version: number };
  fixture.fixture_contract_version = 2;
  reject(fixture, parseFixture);

  const semantic = clone(rawFixture) as { runtime_semantic_contract_version: number };
  semantic.runtime_semantic_contract_version = 2;
  reject(semantic, parseFixture);

  const family = clone(rawFixture) as { families: Array<{ version: number }> };
  family.families[2]!.version = 2;
  reject(family, parseFixture);

  const duplicateFamily = clone(rawFixture) as { families: Array<{ family: string; version: number }> };
  duplicateFamily.families.push({ ...duplicateFamily.families[0]! });
  reject(duplicateFamily, parseFixture);

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
  reject(wrapped, parseFixture);

  const actorRevisionKey = clone(rawFixture) as {
    actors: { root: { semantic_revision_key_hex: string } };
  };
  actorRevisionKey.actors.root.semantic_revision_key_hex = 'ABC';
  reject(actorRevisionKey, parseFixture);

  const affiliationRevisionKey = clone(rawFixture) as {
    affiliations: { child_team_present: { semantic_revision_key_hex: string } };
  };
  affiliationRevisionKey.affiliations.child_team_present.semantic_revision_key_hex = '0'.repeat(63);
  reject(affiliationRevisionKey, parseFixture);
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

test('runtime fixture record() rejects non-plain objects', () => {
  class RuntimeWire {}
  reject(Object.assign(new RuntimeWire(), clone(rawFixture)), parseFixture);

  class FamilyWire {}
  const fixture = clone(rawFixture) as { families: unknown[] };
  fixture.families[0] = Object.assign(new FamilyWire(), fixture.families[0]);
  reject(fixture, parseFixture);
});

test('usage values accept exact portable bounds and reject one over', () => {
  const native = clone(
    (rawFixture as { usage: { native_message: { revision: unknown } } }).usage.native_message.revision,
  ) as {
    request_id: string | null;
    native_message_id: string | null;
    response_key: string;
    buckets: { input_tokens: { value: unknown; effective_at?: number; provenance: { native_field: string } } };
    model: { provenance: { normalization_contract_version: number } };
  };

  native.buckets.input_tokens.value = Number.MAX_SAFE_INTEGER;
  assert.doesNotThrow(() => parseUsageRevisionV2(native));
  native.buckets.input_tokens.value = Number.MAX_SAFE_INTEGER + 1;
  reject(native, parseUsageRevisionV2);
  native.buckets.input_tokens.value = 0;

  native.buckets.input_tokens.effective_at = Number.MAX_SAFE_INTEGER;
  assert.doesNotThrow(() => parseUsageRevisionV2(native));
  native.buckets.input_tokens.effective_at = Number.MAX_SAFE_INTEGER + 1;
  reject(native, parseUsageRevisionV2);
  delete native.buckets.input_tokens.effective_at;

  native.request_id = 'r'.repeat(8 * 1024);
  assert.doesNotThrow(() => parseUsageRevisionV2(native));
  native.request_id = 'r'.repeat(8 * 1024 + 1);
  reject(native, parseUsageRevisionV2);
  native.request_id = 'req_fixture_1';

  native.buckets.input_tokens.provenance.native_field = 'p'.repeat(256);
  assert.doesNotThrow(() => parseUsageRevisionV2(native));
  native.buckets.input_tokens.provenance.native_field = 'p'.repeat(257);
  reject(native, parseUsageRevisionV2);
  native.buckets.input_tokens.provenance.native_field = 'message.usage.input_tokens';

  native.model.provenance.normalization_contract_version = 0xffff_ffff;
  assert.doesNotThrow(() => parseUsageRevisionV2(native));
  native.model.provenance.normalization_contract_version = 2 ** 32;
  reject(native, parseUsageRevisionV2);
  native.model.provenance.normalization_contract_version = 1;

  native.request_id = 'r'.repeat(100_000);
  reject(native, parseUsageRevisionV2);
  native.request_id = 'req_fixture_1';
  native.buckets.input_tokens.provenance.native_field = 'p'.repeat(10_000);
  reject(native, parseUsageRevisionV2);
  native.buckets.input_tokens.provenance.native_field = 'message.usage.input_tokens';
  const exactPayload = 'a'.repeat(8 * 1024);
  native.native_message_id = exactPayload;
  native.response_key = Buffer.from(exactPayload).toString('base64');
  assert.doesNotThrow(() => parseUsageRevisionV2(native));
  native.response_key = `${native.response_key}A`;
  reject(native, parseUsageRevisionV2);
  native.native_message_id = 'msg_fixture_native_1';
  native.response_key = 'A'.repeat(100_000);
  reject(native, parseUsageRevisionV2);

  const hugeDigest = clone(rawFixture) as { actors: { root: { semantic_revision_key_hex: string } } };
  hugeDigest.actors.root.semantic_revision_key_hex = 'a'.repeat(10_000);
  reject(hugeDigest, parseFixture);

  const driftedSource = clone(rawFixture) as {
    actors: { root: { source_record_id: string } };
    source: { session: { entity_key: string } };
  };
  driftedSource.actors.root.source_record_id = driftedSource.source.session.entity_key;
  reject(driftedSource, parseFixture);

  const exactAdapter = clone(rawFixture) as { source: { adapter_id: string } };
  exactAdapter.source.adapter_id = 'a'.repeat(128);
  assert.doesNotThrow(() => parseFixture(exactAdapter));
  exactAdapter.source.adapter_id = 'a'.repeat(129);
  reject(exactAdapter, parseFixture);
  exactAdapter.source.adapter_id = 'a'.repeat(200_000);
  reject(exactAdapter, parseFixture);

  const exactSession = clone(rawFixture) as { source: { session: { native_session_id: string } } };
  exactSession.source.session.native_session_id = 'a'.repeat(8 * 1024);
  assert.doesNotThrow(() => parseFixture(exactSession));
  exactSession.source.session.native_session_id = 'a'.repeat(8 * 1024 + 1);
  reject(exactSession, parseFixture);
  exactSession.source.session.native_session_id = 'a'.repeat(200_000);
  reject(exactSession, parseFixture);
});
