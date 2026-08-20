import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { parseRfc012aV1Json } from '../rfc012a.js';
import { MAX_SEMANTIC_FIXTURE_JSON_BYTES } from '../rfc012-semantic-json.js';
import { parseRfc012cRuntimeV1Json } from '../rfc012c.js';

interface NativeContractAddon {
  parseRfc012aV1Json: (json: string) => string;
  parseRfc012cRuntimeV1Json: (json: string) => string;
}

type Outcome = 'accept' | 'reject';

const native = createRequire(import.meta.url)('@vibecook/spaghetti-sdk-native') as NativeContractAddon;

const rfc012aJson = readFileSync(
  new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012a-v1.json', import.meta.url),
  'utf8',
);
const rfc012cJson = readFileSync(
  new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012c-runtime-v1.json', import.meta.url),
  'utf8',
);
const rfc012cContext = JSON.parse(rfc012cJson) as unknown;

function cloneJson(json: string): unknown {
  return JSON.parse(json) as unknown;
}

function outcome(run: () => void): Outcome {
  try {
    run();
    return 'accept';
  } catch {
    return 'reject';
  }
}

function assertMatrix(
  label: string,
  json: string,
  parseTs: (input: string) => unknown,
  matchingNative: (json: string) => string,
  otherNative: (json: string) => string,
  expected: Outcome,
): void {
  const ts = outcome(() => {
    parseTs(json);
  });
  const matching = outcome(() => {
    matchingNative(json);
  });
  const other = outcome(() => {
    otherNative(json);
  });
  assert.equal(ts, expected, `${label}: TypeScript`);
  assert.equal(matching, expected, `${label}: matching native helper`);
  assert.equal(other, 'reject', `${label}: other native helper`);
}

function mutatedRfc012a(mutate: (value: Record<string, unknown>) => void): string {
  const value = cloneJson(rfc012aJson) as Record<string, unknown>;
  mutate(value);
  return JSON.stringify(value);
}

function mutatedRfc012c(mutate: (value: Record<string, unknown>) => void): string {
  const value = cloneJson(rfc012cJson) as Record<string, unknown>;
  mutate(value);
  return JSON.stringify(value);
}

function paddedJson(json: string, size: number): string {
  const encoded = new TextEncoder().encode(json);
  if (encoded.length > size) {
    throw new Error('fixture already exceeds the requested envelope');
  }
  return `${json}${' '.repeat(size - encoded.length)}`;
}

function replaceOnce(json: string, search: string, replacement: string): string {
  const index = json.indexOf(search);
  assert.notEqual(index, -1, `missing ${search}`);
  return `${json.slice(0, index)}${replacement}${json.slice(index + search.length)}`;
}

test('RFC 012A/012C differential mutation matrix agrees across TypeScript and both native helpers', () => {
  assert.equal(typeof native.parseRfc012aV1Json, 'function');
  assert.equal(typeof native.parseRfc012cRuntimeV1Json, 'function');

  const parse012c = (json: string) => parseRfc012cRuntimeV1Json(json, rfc012cContext);

  const rfc012aCases: Array<[string, string, Outcome]> = [
    ['valid RFC 012A fixture', rfc012aJson, 'accept'],
    [
      'invalid qualified authority type',
      mutatedRfc012a((value) => {
        (value.qualified_known_zero as { authority: unknown }).authority = 1;
      }),
      'reject',
    ],
    [
      'empty qualified authority',
      mutatedRfc012a((value) => {
        (value.qualified_known_zero as { authority: unknown }).authority = '';
      }),
      'reject',
    ],
    [
      'invalid qualified provenance shape',
      mutatedRfc012a((value) => {
        (value.qualified_known_zero as { provenance: unknown }).provenance = { kind: 'not-a-semantic-ref' };
      }),
      'reject',
    ],
    [
      'invalid known-value type',
      mutatedRfc012a((value) => {
        (value.qualified_known_zero as { value: unknown }).value = '0';
      }),
      'reject',
    ],
    [
      'invalid identity authority type',
      mutatedRfc012a((value) => {
        (
          value.native_identity_claim as {
            identity: { authority: unknown };
          }
        ).identity.authority = 1;
      }),
      'reject',
    ],
    [
      'RFC012A native identity at 256 bytes',
      mutatedRfc012a((value) => {
        (
          value.native_identity_claim as {
            identity: { value: { native_id: string } };
          }
        ).identity.value.native_id = 'a'.repeat(256);
      }),
      'accept',
    ],
    [
      'RFC012A native identity at 257 bytes',
      mutatedRfc012a((value) => {
        (
          value.native_identity_claim as {
            identity: { value: { native_id: string } };
          }
        ).identity.value.native_id = 'a'.repeat(257);
      }),
      'reject',
    ],
    [
      'drifted canonical_source_instance_key',
      mutatedRfc012a((value) => {
        value.canonical_source_instance_key = 'v1:Up3RqSE5g49YtzL63uFtRuykhY0GLdTm-Z_JdpHkezs';
      }),
      'reject',
    ],
    ['valid fixture JSON at 1 MiB', paddedJson(rfc012aJson, MAX_SEMANTIC_FIXTURE_JSON_BYTES), 'accept'],
    ['valid fixture JSON over 1 MiB', paddedJson(rfc012aJson, MAX_SEMANTIC_FIXTURE_JSON_BYTES + 1), 'reject'],
    [
      'valid fixture JSON far over 1 MiB',
      paddedJson(rfc012aJson, MAX_SEMANTIC_FIXTURE_JSON_BYTES + 1_000_000),
      'reject',
    ],
    [
      'RFC012A effective_at float lexeme',
      replaceOnce(rfc012aJson, '"effective_at": 1776211200000', '"effective_at": 1776211200000.0'),
      'reject',
    ],
    [
      'RFC012A unpaired surrogate authority',
      replaceOnce(rfc012aJson, '"authority": "native-response"', '"authority": "\\ud800"'),
      'reject',
    ],
    [
      'RFC012A U+0085 prefix on authority',
      replaceOnce(rfc012aJson, '"authority": "native-response"', '"authority": "\\u0085native-response"'),
      'reject',
    ],
    [
      'RFC012A U+FEFF prefix on authority',
      replaceOnce(rfc012aJson, '"authority": "native-response"', '"authority": "\\ufeffnative-response"'),
      'accept',
    ],
    [
      'RFC012A raw unpaired UTF-16 authority',
      replaceOnce(rfc012aJson, '"authority": "native-response"', `"authority": "${'\uD800'}"`),
      'reject',
    ],
    [
      'RFC012A authority at 256 bytes',
      mutatedRfc012a((value) => {
        (value.qualified_known_zero as { authority: string }).authority = 'a'.repeat(256);
      }),
      'accept',
    ],
    [
      'RFC012A authority at 257 bytes',
      mutatedRfc012a((value) => {
        (value.qualified_known_zero as { authority: string }).authority = 'a'.repeat(257);
      }),
      'reject',
    ],
    [
      'RFC012A semantic_revision_ref drift',
      mutatedRfc012a((value) => {
        (value.semantic_revision_ref as { fact_revision_id: string }).fact_revision_id =
          value.canonical_source_instance_key as string;
      }),
      'reject',
    ],
    [
      'RFC012A known-zero provenance drift',
      mutatedRfc012a((value) => {
        (
          value.qualified_known_zero as { provenance: Array<{ fact_revision_id: string }> }
        ).provenance[0]!.fact_revision_id = value.canonical_source_instance_key as string;
      }),
      'reject',
    ],
    [
      'RFC012A unknown provenance is nonempty',
      mutatedRfc012a((value) => {
        (value.qualified_unknown as { provenance: unknown[] }).provenance = [value.semantic_revision_ref];
      }),
      'reject',
    ],
  ];

  for (const [label, json, expected] of rfc012aCases) {
    assertMatrix(
      label,
      json,
      parseRfc012aV1Json,
      native.parseRfc012aV1Json,
      native.parseRfc012cRuntimeV1Json,
      expected,
    );
  }

  const rfc012cCases: Array<[string, string, Outcome]> = [
    ['valid RFC 012C fixture', rfc012cJson, 'accept'],
    [
      'valid RFC 012C fixture JSON far over 1 MiB',
      paddedJson(rfc012cJson, MAX_SEMANTIC_FIXTURE_JSON_BYTES + 1_000_000),
      'reject',
    ],
    [
      'reordered families',
      mutatedRfc012c((value) => {
        const families = value.families as unknown[];
        const first = families[0];
        families[0] = families[2];
        families[2] = first;
      }),
      'reject',
    ],
    [
      'extra family',
      mutatedRfc012c((value) => {
        (value.families as unknown[]).push({ family: 'runtime.session', version: 1 });
      }),
      'reject',
    ],
    [
      'workflow removal changes fact_id',
      mutatedRfc012c((value) => {
        const affiliations = value.affiliations as {
          child_team_present: { fact_id: string };
          child_workflow_removed: { fact_id: string };
        };
        affiliations.child_workflow_removed.fact_id = affiliations.child_team_present.fact_id;
      }),
      'reject',
    ],
    [
      'workflow removal reuses semantic revision',
      mutatedRfc012c((value) => {
        const affiliations = value.affiliations as {
          child_workflow_present: { semantic_revision_ref: unknown };
          child_workflow_removed: { semantic_revision_ref: unknown };
        };
        affiliations.child_workflow_removed.semantic_revision_ref =
          affiliations.child_workflow_present.semantic_revision_ref;
      }),
      'reject',
    ],
    [
      'usage references a non-fixture actor',
      mutatedRfc012c((value) => {
        const affiliations = value.affiliations as { child_team_present: { revision: { target: string } } };
        const usage = value.usage as { native_message: { revision: { actor_run: string } } };
        usage.native_message.revision.actor_run = affiliations.child_team_present.revision.target;
      }),
      'reject',
    ],
    [
      'change usage request_id without updating revision identity',
      mutatedRfc012c((value) => {
        (value.usage as { native_message: { revision: { request_id: string } } }).native_message.revision.request_id =
          'req_mutated_without_identity';
      }),
      'reject',
    ],
    [
      'change actor semantic field',
      mutatedRfc012c((value) => {
        (value.actors as { root: { revision: { native_actor_id: string } } }).root.revision.native_actor_id =
          'mutated-root-actor';
      }),
      'reject',
    ],
    [
      'replace semantic_revision_key_hex with valid-shaped digest',
      mutatedRfc012c((value) => {
        (value.actors as { root: { semantic_revision_key_hex: string } }).root.semantic_revision_key_hex = 'ab'.repeat(
          32,
        );
      }),
      'reject',
    ],
    [
      'replace semantic_revision_ref with valid-shaped reference',
      mutatedRfc012c((value) => {
        const actors = value.actors as {
          root: { semantic_revision_ref: unknown };
          child: { semantic_revision_ref: unknown };
        };
        actors.root.semantic_revision_ref = actors.child.semantic_revision_ref;
      }),
      'reject',
    ],
    [
      'empty source adapter_id',
      mutatedRfc012c((value) => {
        (value.source as { adapter_id: string }).adapter_id = '';
      }),
      'reject',
    ],
    [
      'U+0085 prefix on adapter_id',
      replaceOnce(rfc012cJson, '"adapter_id": "fixture-adapter"', '"adapter_id": "\\u0085fixture-adapter"'),
      'reject',
    ],
    [
      'U+FEFF prefix on adapter_id',
      replaceOnce(rfc012cJson, '"adapter_id": "fixture-adapter"', '"adapter_id": "\\ufefffixture-adapter"'),
      'accept',
    ],
    [
      'adapter_id at 128 bytes',
      mutatedRfc012c((value) => {
        (value.source as { adapter_id: string }).adapter_id = 'a'.repeat(128);
      }),
      'accept',
    ],
    [
      'adapter_id at 129 bytes',
      mutatedRfc012c((value) => {
        (value.source as { adapter_id: string }).adapter_id = 'a'.repeat(129);
      }),
      'reject',
    ],
    [
      'native_session_id at 8 KiB',
      mutatedRfc012c((value) => {
        (value.source as { session: { native_session_id: string } }).session.native_session_id = 'a'.repeat(8 * 1024);
      }),
      'accept',
    ],
    [
      'native_session_id at 8 KiB plus one',
      mutatedRfc012c((value) => {
        (value.source as { session: { native_session_id: string } }).session.native_session_id = 'a'.repeat(
          8 * 1024 + 1,
        );
      }),
      'reject',
    ],
    [
      'source_record_id valid-shaped drift',
      mutatedRfc012c((value) => {
        (value.actors as { root: { source_record_id: string } }).root.source_record_id = (
          value.source as { session: { entity_key: string } }
        ).session.entity_key;
      }),
      'reject',
    ],
    [
      'empty source native_session_id',
      mutatedRfc012c((value) => {
        (value.source as { session: { native_session_id: string } }).session.native_session_id = '';
      }),
      'reject',
    ],
    [
      'normalization_contract_version = 2^32',
      mutatedRfc012c((value) => {
        (
          value.usage as {
            native_message: {
              revision: { buckets: { input_tokens: { provenance: { normalization_contract_version: number } } } };
            };
          }
        ).native_message.revision.buckets.input_tokens.provenance.normalization_contract_version = 2 ** 32;
      }),
      'reject',
    ],
    ['valid RFC 012C fixture JSON at 1 MiB', paddedJson(rfc012cJson, MAX_SEMANTIC_FIXTURE_JSON_BYTES), 'accept'],
    ['valid RFC 012C fixture JSON over 1 MiB', paddedJson(rfc012cJson, MAX_SEMANTIC_FIXTURE_JSON_BYTES + 1), 'reject'],
    ['RFC012C token value 0.0', replaceOnce(rfc012cJson, '"value": 0', '"value": 0.0'), 'reject'],
    ['RFC012C token value -0', replaceOnce(rfc012cJson, '"value": 0', '"value": -0'), 'reject'],
    ['RFC012C token value 0e0', replaceOnce(rfc012cJson, '"value": 0', '"value": 0e0'), 'reject'],
  ];

  for (const [label, json, expected] of rfc012cCases) {
    assertMatrix(label, json, parse012c, native.parseRfc012cRuntimeV1Json, native.parseRfc012aV1Json, expected);
  }
});
