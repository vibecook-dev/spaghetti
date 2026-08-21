import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import {
  classifyRuntimeSupport,
  compareCoverage,
  ContractValidationError,
  parseAccessReportRetrieval,
  parseContractVersionOffer,
  parseContractVersionRequest,
  parseContractVersionSelection,
  parseExternalEntityRef,
  parseNativeIdentityClaim,
  parseNativeProbeGrantRequest,
  parseQualifiedValue,
  parseRfc012aV1Fixture,
  parseRfc012aV1Json,
  parseSemanticRevisionRef,
  parseSourceCoverageSet,
  selectContractVersions,
  type AccessReportRetrievalRequest,
  type NativeProbeGrantRequest,
  type SourceCoverageSet,
} from '../rfc012a.js';
import {
  MAX_SEMANTIC_FIXTURE_DEPTH,
  MAX_SEMANTIC_FIXTURE_JSON_BYTES,
  MAX_SEMANTIC_FIXTURE_NODES,
  preflightSemanticFixtureJson,
} from '../rfc012-semantic-json.js';

interface ContractFixture {
  fixture_contract_version: number;
  canonical_source_instance_key: string;
  external_entity_ref: unknown;
  native_identity_claim: unknown;
  semantic_revision_ref: unknown;
  qualified_known_zero: unknown;
  qualified_unknown: unknown;
  coverage: {
    baseline: unknown;
    dominant: unknown;
    reset: unknown;
    expected: {
      dominant_vs_baseline: string;
      baseline_vs_dominant: string;
      reset_vs_baseline: string;
    };
  };
}

type MutableRecord = Record<string, unknown>;

function mutableJson(value: unknown): MutableRecord {
  const cloned: unknown = structuredClone(value);
  if (cloned === null || typeof cloned !== 'object' || Array.isArray(cloned)) {
    throw new Error('test fixture must be a JSON object');
  }
  return cloned as unknown as MutableRecord;
}

function mutableField(value: unknown): MutableRecord {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('test field must be a JSON object');
  }
  return value as MutableRecord;
}

function mutableList(value: unknown): unknown[] {
  if (!Array.isArray(value)) {
    throw new Error('test field must be a JSON array');
  }
  return value;
}

interface MutableCoverageWire extends MutableRecord {
  coverage_domain: MutableRecord;
  scope: MutableRecord;
  points: MutableRecord[];
  explicit_absence_or_deletion: MutableRecord[];
  explicit_errors: MutableRecord[];
  completeness: string;
}

const fixtureJson = readFileSync(
  new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012a-v1.json', import.meta.url),
  'utf8',
);
const fixture = JSON.parse(fixtureJson) as ContractFixture;

function partialCoverageWire(): MutableCoverageWire {
  const wire = structuredClone(fixture.coverage.baseline) as MutableCoverageWire;
  const point = wire.points[0]!;
  wire.completeness = 'partial';
  wire.explicit_absence_or_deletion = [
    {
      stream_key: point.stream_key,
      object_key: point.object_key,
      generation: (point.generation as number) + 1,
      kind: 'absent',
    },
  ];
  wire.explicit_errors = [
    {
      stream_key: point.stream_key,
      object_key: point.object_key,
      code: 'retryable_read',
    },
  ];
  assert.doesNotThrow(() => parseSourceCoverageSet(wire));
  return wire;
}

interface SupportFixture {
  fixture_contract_version: number;
  releases: unknown[];
  runtime_cases: Array<{ name: string; probe: unknown; expected: unknown }>;
  contract_request: unknown;
  contract_offer: unknown;
  expected_contract_selection: unknown;
}

const supportFixture = JSON.parse(
  readFileSync(
    new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012a-support-v1.json', import.meta.url),
    'utf8',
  ),
) as SupportFixture;

type AccessOperation = 'object_read' | 'parameterized_query' | 'object_listing';
type AccessPhase = 'initial' | 'revalidation';
type AccessOutcome = 'available' | 'unavailable' | 'oversized' | 'failed' | 'abandoned' | 'denied';
type AccessLimit = 'max_fan_out' | 'max_depth' | 'max_objects' | 'max_bytes' | 'max_rows' | 'reservation';

interface AccessTraceWire {
  access_trace_contract_version: number;
  sequence: number;
  relation_id: string;
  operation: AccessOperation;
  phase: AccessPhase;
  parent_token: number[] | null;
  object_token: number[];
  depth: number;
  reserved_bytes: number;
  reserved_rows: number;
  bytes_read: number;
  rows_read: number;
  outcome: AccessOutcome;
  denied_limit: AccessLimit | null;
}

interface AccessRelationWire {
  access_trace_contract_version: number;
  relation_id: string;
  bounds: {
    max_fan_out: number;
    max_depth: number;
    max_objects: number;
    max_bytes: number;
    max_rows: number;
  };
  attempts: number;
  reservations_granted: number;
  completed: number;
  denied: number;
  abandoned: number;
  objects_accessed: number;
  bytes_read: number;
  rows_read: number;
  max_depth_observed: number;
  bytes_reserved: number;
  rows_reserved: number;
  trace_entries_dropped: number;
  trace: AccessTraceWire[];
}

interface ScopeAccessReportWire {
  scope_access_report_contract_version: number;
  adapter_id: string;
  support_release_id: string;
  support_release_digest: number[];
  scope_program_digest: number[];
  declaration_id: string;
  program_id: string;
  selection_contract_version: number;
  observation_contract_version: number;
  relations: AccessRelationWire[];
  digest: number[];
}

interface AccessReportFixture {
  fixture_contract_version: number;
  report: ScopeAccessReportWire;
  expected_digest: string;
}

const accessReportFixture = JSON.parse(
  readFileSync(
    new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012a-access-report-v1.json', import.meta.url),
    'utf8',
  ),
) as AccessReportFixture;

const accessOperationCodes: Record<AccessOperation, number> = {
  object_read: 1,
  parameterized_query: 2,
  object_listing: 3,
};
const accessPhaseCodes: Record<AccessPhase, number> = { initial: 1, revalidation: 2 };
const accessOutcomeCodes: Record<AccessOutcome, number> = {
  available: 1,
  unavailable: 2,
  oversized: 3,
  failed: 4,
  abandoned: 5,
  denied: 6,
};
const accessLimitCodes: Record<AccessLimit, number> = {
  max_fan_out: 1,
  max_depth: 2,
  max_objects: 3,
  max_bytes: 4,
  max_rows: 5,
  reservation: 6,
};

function pushU8(chunks: Buffer[], value: number): void {
  chunks.push(Buffer.from([value]));
}

function pushU32(chunks: Buffer[], value: number): void {
  const encoded = Buffer.alloc(4);
  encoded.writeUInt32BE(value);
  chunks.push(encoded);
}

function pushU64(chunks: Buffer[], value: number): void {
  assert.ok(Number.isSafeInteger(value) && value >= 0, 'fixture u64 must be a non-negative safe integer');
  const encoded = Buffer.alloc(8);
  encoded.writeBigUInt64BE(BigInt(value));
  chunks.push(encoded);
}

function pushComponent(chunks: Buffer[], value: Uint8Array): void {
  pushU64(chunks, value.byteLength);
  chunks.push(Buffer.from(value));
}

function pushText(chunks: Buffer[], value: string): void {
  pushComponent(chunks, Buffer.from(value, 'utf8'));
}

function pushToken(chunks: Buffer[], value: number[]): void {
  assert.equal(value.length, 32);
  pushComponent(chunks, Uint8Array.from(value));
}

function pushAccessTrace(chunks: Buffer[], entry: AccessTraceWire): void {
  pushU32(chunks, entry.access_trace_contract_version);
  pushU64(chunks, entry.sequence);
  pushText(chunks, entry.relation_id);
  pushU8(chunks, accessOperationCodes[entry.operation]);
  pushU8(chunks, accessPhaseCodes[entry.phase]);
  if (entry.parent_token === null) {
    pushU8(chunks, 0);
  } else {
    pushU8(chunks, 1);
    pushToken(chunks, entry.parent_token);
  }
  pushToken(chunks, entry.object_token);
  pushU32(chunks, entry.depth);
  for (const value of [entry.reserved_bytes, entry.reserved_rows, entry.bytes_read, entry.rows_read]) {
    pushU64(chunks, value);
  }
  pushU8(chunks, accessOutcomeCodes[entry.outcome]);
  if (entry.denied_limit === null) {
    pushU8(chunks, 0);
  } else {
    pushU8(chunks, 1);
    pushU8(chunks, accessLimitCodes[entry.denied_limit]);
  }
}

function pushAccessRelation(chunks: Buffer[], relation: AccessRelationWire): void {
  pushU32(chunks, relation.access_trace_contract_version);
  pushText(chunks, relation.relation_id);
  pushU64(chunks, relation.bounds.max_fan_out);
  pushU32(chunks, relation.bounds.max_depth);
  pushU64(chunks, relation.bounds.max_objects);
  pushU64(chunks, relation.bounds.max_bytes);
  pushU64(chunks, relation.bounds.max_rows);
  for (const value of [
    relation.attempts,
    relation.reservations_granted,
    relation.completed,
    relation.denied,
    relation.abandoned,
    relation.objects_accessed,
    relation.bytes_read,
    relation.rows_read,
  ]) {
    pushU64(chunks, value);
  }
  pushU32(chunks, relation.max_depth_observed);
  for (const value of [relation.bytes_reserved, relation.rows_reserved, relation.trace_entries_dropped]) {
    pushU64(chunks, value);
  }
  pushU64(chunks, relation.trace.length);
  for (const entry of relation.trace) pushAccessTrace(chunks, entry);
}

function scopeAccessReportDigest(report: ScopeAccessReportWire): string {
  const chunks = [Buffer.from('spaghetti/rfc012a/scope-access-report/v1\0', 'utf8')];
  pushU32(chunks, report.scope_access_report_contract_version);
  pushText(chunks, report.adapter_id);
  pushText(chunks, report.support_release_id);
  pushToken(chunks, report.support_release_digest);
  pushToken(chunks, report.scope_program_digest);
  pushText(chunks, report.declaration_id);
  pushText(chunks, report.program_id);
  pushU32(chunks, report.selection_contract_version);
  pushU32(chunks, report.observation_contract_version);
  pushU64(chunks, report.relations.length);
  for (const relation of report.relations) pushAccessRelation(chunks, relation);
  return `sha256:${createHash('sha256').update(Buffer.concat(chunks)).digest('hex')}`;
}

test('Rust RFC 012A v1 fixture validates in the portable SDK', () => {
  const parsed = parseRfc012aV1Fixture(fixture);
  assert.equal(parsed.fixture_contract_version, 1);
  assert.equal(parsed.external_entity_ref.external_entity_reference_version, 1);
  assert.equal(parsed.semantic_revision_ref.semantic_reference_contract_version, 1);
  assert.equal(parsed.native_identity_claim.identity.value?.native_id, 'session-1');
  assert.equal(parsed.qualified_known_zero.value, 0);
  assert.equal(parsed.qualified_unknown.unknown_reason, 'withheld');
  assert.equal(parsed.canonical_source_instance_key, parsed.coverage.baseline.scope.source_instance_key);
  assert.equal(
    compareCoverage(parsed.coverage.dominant, parsed.coverage.baseline),
    parsed.coverage.expected.dominant_vs_baseline,
  );
});

test('Rust and TypeScript coverage comparison outcomes agree', () => {
  const baseline = parseSourceCoverageSet(fixture.coverage.baseline);
  const dominant = parseSourceCoverageSet(fixture.coverage.dominant);
  const reset = parseSourceCoverageSet(fixture.coverage.reset);
  assert.equal(compareCoverage(dominant, baseline), fixture.coverage.expected.dominant_vs_baseline);
  assert.equal(compareCoverage(baseline, dominant), fixture.coverage.expected.baseline_vs_dominant);
  assert.equal(compareCoverage(reset, baseline), fixture.coverage.expected.reset_vs_baseline);

  const partial = structuredClone(baseline);
  partial.completeness = 'partial';
  partial.points[0]!.status = { kind: 'partial' };
  assert.equal(compareCoverage(partial, partial), 'equal');
});

test('Rust, Python, and TypeScript support classification agree', () => {
  assert.equal(supportFixture.fixture_contract_version, 1);
  for (const runtimeCase of supportFixture.runtime_cases) {
    assert.deepEqual(
      classifyRuntimeSupport(runtimeCase.probe, supportFixture.releases),
      runtimeCase.expected,
      runtimeCase.name,
    );
  }
});

test('support capability declarations fail closed before portable classification', () => {
  const releases = structuredClone(supportFixture.releases) as Array<{
    capabilities: Array<{ capability_id: string; topology: string; level: string }>;
  }>;
  releases[1]!.capabilities.push(structuredClone(releases[1]!.capabilities[0]!));
  assert.throws(
    () => classifyRuntimeSupport(supportFixture.runtime_cases[0]!.probe, releases),
    ContractValidationError,
  );

  const oversized = structuredClone(supportFixture.releases) as Array<{
    capabilities: Array<{ capability_id: string }>;
  }>;
  oversized[1]!.capabilities[0]!.capability_id = 'é'.repeat(65);
  assert.throws(
    () => classifyRuntimeSupport(supportFixture.runtime_cases[0]!.probe, oversized),
    ContractValidationError,
  );

  const absent = structuredClone(supportFixture.releases) as Array<{ capabilities: unknown[] }>;
  absent[1]!.capabilities = [];
  assert.throws(() => classifyRuntimeSupport(supportFixture.runtime_cases[0]!.probe, absent), ContractValidationError);

  const absentTopologies = structuredClone(supportFixture.releases) as Array<{
    capabilities: Array<{ capability_id: string }>;
  }>;
  absentTopologies[2]!.capabilities = absentTopologies[2]!.capabilities.filter(
    (capability) => capability.capability_id === 'restricted-history',
  );
  const restrictedProbe = supportFixture.runtime_cases.find(
    (runtimeCase) => runtimeCase.name === 'exact-capability-restricted',
  )!.probe;
  const decision = classifyRuntimeSupport(restrictedProbe, [absentTopologies[2]]);
  assert.equal(decision.permissions.durable, true);
  assert.equal(decision.permissions.catalog, false);
  assert.equal(decision.permissions.scoped_observation, false);
});

test('Rust, Python, and TypeScript contract negotiation agree', () => {
  assert.deepEqual(
    selectContractVersions(supportFixture.contract_request, supportFixture.contract_offer),
    supportFixture.expected_contract_selection,
  );

  const incompatible = structuredClone(supportFixture.contract_request) as { model_major: number };
  incompatible.model_major += 1;
  assert.throws(() => selectContractVersions(incompatible, supportFixture.contract_offer), ContractValidationError);
});

test('portable contract-version fields reject values outside Rust u32 range', () => {
  const oversized = 0x1_0000_0000;

  const request = structuredClone(supportFixture.contract_request) as {
    model_major: number;
    coverage_contract_versions: number[];
  };
  request.model_major = oversized;
  assert.throws(() => parseContractVersionRequest(request), ContractValidationError);
  request.model_major = 1;
  request.coverage_contract_versions[0] = oversized;
  assert.throws(() => parseContractVersionRequest(request), ContractValidationError);

  const offer = structuredClone(supportFixture.contract_offer) as {
    fact_family_versions: Record<string, number[]>;
  };
  offer.fact_family_versions['interaction'] = [oversized];
  assert.throws(() => parseContractVersionOffer(offer), ContractValidationError);

  const selection = structuredClone(supportFixture.expected_contract_selection) as {
    fact_family_versions: Record<string, number>;
  };
  selection.fact_family_versions['usage'] = oversized;
  assert.throws(() => parseContractVersionSelection(selection), ContractValidationError);

  const coverage = structuredClone(fixture.coverage.baseline) as {
    coverage_domain: { version: number };
  };
  coverage.coverage_domain.version = oversized;
  assert.throws(() => parseSourceCoverageSet(coverage), ContractValidationError);

  const unsafeInteger = Number.MAX_SAFE_INTEGER + 1;
  const zeroGeneration = partialCoverageWire();
  zeroGeneration.points[0]!.generation = 0;
  assert.throws(() => parseSourceCoverageSet(zeroGeneration), ContractValidationError);

  const zeroAbsence = partialCoverageWire();
  zeroAbsence.explicit_absence_or_deletion[0]!.generation = 0;
  assert.throws(() => parseSourceCoverageSet(zeroAbsence), ContractValidationError);

  const unsafeCoverage = partialCoverageWire();
  unsafeCoverage.points[0]!.generation = unsafeInteger;
  assert.throws(() => parseSourceCoverageSet(unsafeCoverage), ContractValidationError);

  const unsafeOrder = partialCoverageWire();
  (unsafeOrder.points[0]!.position as MutableRecord).monotonic_order = unsafeInteger;
  assert.throws(() => parseSourceCoverageSet(unsafeOrder), ContractValidationError);

  const unsafeObservedAt = partialCoverageWire();
  (unsafeObservedAt.points[0]!.provenance as MutableRecord).observed_at = -unsafeInteger;
  assert.throws(() => parseSourceCoverageSet(unsafeObservedAt), ContractValidationError);

  const unsafeAbsence = partialCoverageWire();
  unsafeAbsence.explicit_absence_or_deletion[0]!.generation = unsafeInteger;
  assert.throws(() => parseSourceCoverageSet(unsafeAbsence), ContractValidationError);
});

test('coverage wire rejects unknown nested fields, non-plain objects, and explicit nulls', () => {
  const unknownMutations: Array<[string, (wire: MutableCoverageWire) => void]> = [
    ['set', (wire) => (wire.future = true)],
    ['domain', (wire) => (wire.coverage_domain.future = true)],
    ['decode domain', (wire) => (wire.coverage_domain = { kind: 'decode', future: true })],
    ['point', (wire) => (wire.points[0]!.future = true)],
    [
      'point domain',
      (wire) => {
        (wire.points[0]!.coverage_domain as MutableRecord).future = true;
      },
    ],
    [
      'position',
      (wire) => {
        (wire.points[0]!.position as MutableRecord).future = true;
      },
    ],
    [
      'status',
      (wire) => {
        (wire.points[0]!.status as MutableRecord).future = true;
      },
    ],
    [
      'provenance',
      (wire) => {
        (wire.points[0]!.provenance as MutableRecord).future = true;
      },
    ],
    ['scope', (wire) => (wire.scope.future = true)],
    ['absence', (wire) => (wire.explicit_absence_or_deletion[0]!.future = true)],
    ['error', (wire) => (wire.explicit_errors[0]!.future = true)],
  ];
  for (const [label, mutate] of unknownMutations) {
    const wire = partialCoverageWire();
    mutate(wire);
    assert.throws(() => parseSourceCoverageSet(wire), ContractValidationError, label);
  }

  const nullMutations: Array<[string, (wire: MutableCoverageWire) => void]> = [
    ['position', (wire) => (wire.points[0]!.position = null)],
    ['monotonic order', (wire) => ((wire.points[0]!.position as MutableRecord).monotonic_order = null)],
    ['observed_at', (wire) => ((wire.points[0]!.provenance as MutableRecord).observed_at = null)],
    ['root entity', (wire) => (wire.scope.root_entity_key = null)],
    ['error object', (wire) => (wire.explicit_errors[0]!.object_key = null)],
  ];
  for (const [label, mutate] of nullMutations) {
    const wire = partialCoverageWire();
    mutate(wire);
    assert.throws(() => parseSourceCoverageSet(wire), ContractValidationError, label);
  }

  class NonWireCoverage {}
  const nonPlain = Object.assign(new NonWireCoverage(), partialCoverageWire());
  assert.throws(() => parseSourceCoverageSet(nonPlain), ContractValidationError);
});

test('coverage wire enforces bounded evidence and canonical error coordinates', () => {
  const unavailable = partialCoverageWire();
  unavailable.points[0]!.status = { kind: 'unavailable', reason: 'é'.repeat(513) };
  assert.throws(() => parseSourceCoverageSet(unavailable), ContractValidationError);

  const oversizedIdentifier = partialCoverageWire();
  oversizedIdentifier.explicit_errors[0]!.code = 'a'.repeat(65);
  assert.throws(() => parseSourceCoverageSet(oversizedIdentifier), ContractValidationError);

  for (const invalidCode of ['retryable-read', 'read failed at /Users/alice/private\nretry']) {
    const freeFormError = partialCoverageWire();
    freeFormError.explicit_errors[0]!.code = invalidCode;
    assert.throws(() => parseSourceCoverageSet(freeFormError), ContractValidationError);
  }

  const tooManyErrors = partialCoverageWire();
  tooManyErrors.explicit_errors = new Array(4_097);
  assert.throws(() => parseSourceCoverageSet(tooManyErrors), ContractValidationError);

  const tooManyPoints = partialCoverageWire();
  tooManyPoints.points = new Array(250_001);
  assert.throws(() => parseSourceCoverageSet(tooManyPoints), ContractValidationError);

  const tooManyAbsences = partialCoverageWire();
  tooManyAbsences.explicit_absence_or_deletion = new Array(250_001);
  assert.throws(() => parseSourceCoverageSet(tooManyAbsences), ContractValidationError);

  const duplicateErrors = partialCoverageWire();
  duplicateErrors.explicit_errors.push(structuredClone(duplicateErrors.explicit_errors[0]!));
  assert.throws(() => parseSourceCoverageSet(duplicateErrors), ContractValidationError);

  const orphanObject = partialCoverageWire();
  delete orphanObject.explicit_errors[0]!.stream_key;
  assert.throws(() => parseSourceCoverageSet(orphanObject), ContractValidationError);
});

test('Rust, Python, and TypeScript access-report digests agree', () => {
  assert.equal(accessReportFixture.fixture_contract_version, 1);
  assert.equal(scopeAccessReportDigest(accessReportFixture.report), accessReportFixture.expected_digest);
  assert.equal(
    `sha256:${Buffer.from(accessReportFixture.report.digest).toString('hex')}`,
    accessReportFixture.expected_digest,
  );

  const tampered = structuredClone(accessReportFixture.report);
  tampered.relations[0]!.rows_read += 1;
  assert.notEqual(scopeAccessReportDigest(tampered), accessReportFixture.expected_digest);
});

test('qualified values and native identity claims reject unknown nested fields', () => {
  const qualified = structuredClone(fixture.qualified_known_zero) as MutableRecord;
  qualified.future = true;
  assert.throws(() => parseQualifiedValue(qualified), ContractValidationError);

  const provenance = structuredClone(fixture) as {
    qualified_known_zero: { provenance: Array<Record<string, unknown>> };
  };
  provenance.qualified_known_zero.provenance[0]!.future = true;
  assert.throws(() => parseRfc012aV1Fixture(provenance), ContractValidationError);

  const identity = structuredClone(fixture.native_identity_claim) as MutableRecord;
  identity.future = true;
  assert.throws(() => parseNativeIdentityClaim(identity), ContractValidationError);

  const nativeIdentity = structuredClone(fixture.native_identity_claim) as {
    identity: { value: Record<string, unknown> };
  };
  nativeIdentity.identity.value.future = true;
  assert.throws(() => parseNativeIdentityClaim(nativeIdentity), ContractValidationError);

  const explicitNulls: Array<[string, MutableRecord]> = [
    [
      'unknown_reason',
      Object.assign(structuredClone(fixture.qualified_known_zero) as MutableRecord, { unknown_reason: null }),
    ],
    [
      'effective_at',
      Object.assign(structuredClone(fixture.qualified_known_zero) as MutableRecord, { effective_at: null }),
    ],
    ['authority', Object.assign(structuredClone(fixture.qualified_known_zero) as MutableRecord, { authority: null })],
    ['provenance', Object.assign(structuredClone(fixture.qualified_known_zero) as MutableRecord, { provenance: null })],
  ];
  for (const [label, value] of explicitNulls) {
    assert.throws(() => parseQualifiedValue(value), ContractValidationError, label);
  }
});

test('incompatible majors and malformed complete coverage are rejected', () => {
  assert.throws(
    () =>
      parseExternalEntityRef({
        ...(fixture.external_entity_ref as object),
        external_entity_reference_version: 2,
      }),
    ContractValidationError,
  );
  assert.throws(
    () =>
      parseExternalEntityRef({
        ...(fixture.external_entity_ref as object),
        future_identity_meaning: true,
      }),
    ContractValidationError,
  );
  assert.throws(
    () =>
      parseSemanticRevisionRef({
        ...(fixture.semantic_revision_ref as object),
        future_revision_meaning: true,
      }),
    ContractValidationError,
  );
  const malformed = structuredClone(fixture.coverage.baseline) as SourceCoverageSet;
  malformed.points[0]!.status = { kind: 'partial' };
  assert.throws(() => parseSourceCoverageSet(malformed), ContractValidationError);

  const malformedIdentity = structuredClone(fixture.native_identity_claim) as {
    identity: { value: { native_id: string } };
  };
  malformedIdentity.identity.value.native_id = '';
  assert.throws(() => parseNativeIdentityClaim(malformedIdentity), ContractValidationError);
});

test('typed RFC 012A fixture consumer rejects invalid nested types and drifted source keys', () => {
  class FixtureWire {}
  assert.throws(
    () => parseRfc012aV1Fixture(Object.assign(new FixtureWire(), structuredClone(fixture))),
    ContractValidationError,
  );

  const authority = structuredClone(fixture) as { qualified_known_zero: { authority: unknown } };
  authority.qualified_known_zero.authority = 1;
  assert.throws(() => parseRfc012aV1Fixture(authority), ContractValidationError);

  const provenanceShape = structuredClone(fixture) as { qualified_known_zero: { provenance: unknown } };
  provenanceShape.qualified_known_zero.provenance = { kind: 'not-a-semantic-ref' };
  assert.throws(() => parseRfc012aV1Fixture(provenanceShape), ContractValidationError);

  const knownValue = structuredClone(fixture) as { qualified_known_zero: { value: unknown } };
  knownValue.qualified_known_zero.value = '0';
  assert.throws(() => parseRfc012aV1Fixture(knownValue), ContractValidationError);

  const identityAuthority = structuredClone(fixture) as {
    native_identity_claim: { identity: { authority: unknown } };
  };
  identityAuthority.native_identity_claim.identity.authority = 1;
  assert.throws(() => parseRfc012aV1Fixture(identityAuthority), ContractValidationError);

  const drifted = structuredClone(fixture) as { canonical_source_instance_key: string };
  drifted.canonical_source_instance_key = 'v1:Up3RqSE5g49YtzL63uFtRuykhY0GLdTm-Z_JdpHkezs';
  assert.throws(() => parseRfc012aV1Fixture(drifted), ContractValidationError);

  const emptyAuthority = structuredClone(fixture) as { qualified_known_zero: { authority: unknown } };
  emptyAuthority.qualified_known_zero.authority = '';
  assert.throws(() => parseRfc012aV1Fixture(emptyAuthority), ContractValidationError);

  const exactIdentity = structuredClone(fixture) as {
    native_identity_claim: { identity: { value: { native_id: string } } };
  };
  exactIdentity.native_identity_claim.identity.value.native_id = 'a'.repeat(256);
  assert.doesNotThrow(() => parseRfc012aV1Fixture(exactIdentity));
  exactIdentity.native_identity_claim.identity.value.native_id = 'a'.repeat(257);
  assert.throws(() => parseRfc012aV1Fixture(exactIdentity), ContractValidationError);
  exactIdentity.native_identity_claim.identity.value.native_id = 'a'.repeat(10_000);
  assert.throws(() => parseRfc012aV1Fixture(exactIdentity), ContractValidationError);

  const exactAuthority = structuredClone(fixture) as { qualified_known_zero: { authority: string } };
  exactAuthority.qualified_known_zero.authority = 'a'.repeat(256);
  assert.doesNotThrow(() => parseRfc012aV1Fixture(exactAuthority));
  exactAuthority.qualified_known_zero.authority = 'a'.repeat(257);
  assert.throws(() => parseRfc012aV1Fixture(exactAuthority), ContractValidationError);
  exactAuthority.qualified_known_zero.authority = 'a'.repeat(200_000);
  assert.throws(() => parseRfc012aV1Fixture(exactAuthority), ContractValidationError);

  const identityAuthorityBound = structuredClone(fixture) as {
    native_identity_claim: { identity: { authority: string } };
  };
  identityAuthorityBound.native_identity_claim.identity.authority = 'a'.repeat(256);
  assert.doesNotThrow(() => parseRfc012aV1Fixture(identityAuthorityBound));
  identityAuthorityBound.native_identity_claim.identity.authority = 'a'.repeat(257);
  assert.throws(() => parseRfc012aV1Fixture(identityAuthorityBound), ContractValidationError);

  const driftedRef = structuredClone(fixture) as {
    semantic_revision_ref: { fact_revision_id: string };
    canonical_source_instance_key: string;
  };
  driftedRef.semantic_revision_ref.fact_revision_id = driftedRef.canonical_source_instance_key;
  assert.throws(() => parseRfc012aV1Fixture(driftedRef), ContractValidationError);

  const driftedKnownZero = structuredClone(fixture) as {
    qualified_known_zero: { provenance: Array<{ fact_revision_id: string }> };
    canonical_source_instance_key: string;
  };
  driftedKnownZero.qualified_known_zero.provenance[0]!.fact_revision_id =
    driftedKnownZero.canonical_source_instance_key;
  assert.throws(() => parseRfc012aV1Fixture(driftedKnownZero), ContractValidationError);

  const driftedIdentity = structuredClone(fixture) as {
    native_identity_claim: { identity: { provenance: Array<{ fact_revision_id: string }> } };
    canonical_source_instance_key: string;
  };
  driftedIdentity.native_identity_claim.identity.provenance[0]!.fact_revision_id =
    driftedIdentity.canonical_source_instance_key;
  assert.throws(() => parseRfc012aV1Fixture(driftedIdentity), ContractValidationError);

  const unknownProvenance = structuredClone(fixture) as {
    qualified_unknown: { provenance: unknown[] };
    semantic_revision_ref: unknown;
  };
  unknownProvenance.qualified_unknown.provenance = [unknownProvenance.semantic_revision_ref];
  assert.throws(() => parseRfc012aV1Fixture(unknownProvenance), ContractValidationError);

  const surrogateAuthority = structuredClone(fixture) as { qualified_known_zero: { authority: string } };
  surrogateAuthority.qualified_known_zero.authority = '\ud800';
  assert.throws(() => parseRfc012aV1Fixture(surrogateAuthority), ContractValidationError);
});

test('semantic fixture envelope accepts exact bounds and rejects one over', () => {
  const compactJson = JSON.stringify(fixture);
  const exactBytes = `${compactJson}${' '.repeat(MAX_SEMANTIC_FIXTURE_JSON_BYTES - new TextEncoder().encode(compactJson).length)}`;
  assert.equal(new TextEncoder().encode(exactBytes).length, MAX_SEMANTIC_FIXTURE_JSON_BYTES);
  assert.doesNotThrow(() => parseRfc012aV1Json(exactBytes));
  assert.throws(() => parseRfc012aV1Json(`${exactBytes} `), ContractValidationError);
  assert.throws(() => parseRfc012aV1Json(`${exactBytes}${' '.repeat(1_000_000)}`), ContractValidationError);

  assert.throws(
    () => parseRfc012aV1Json(fixtureJson.replace('"effective_at": 1776211200000', '"effective_at": 1776211200000.0')),
    ContractValidationError,
  );
  assert.throws(
    () => parseRfc012aV1Json(fixtureJson.replace('"authority": "native-response"', '"authority": "\\ud800"')),
    ContractValidationError,
  );

  let exactDepth: unknown = 0;
  for (let depth = 0; depth < MAX_SEMANTIC_FIXTURE_DEPTH - 1; depth += 1) {
    exactDepth = { child: exactDepth };
  }
  assert.doesNotThrow(() => preflightSemanticFixtureJson(JSON.stringify(exactDepth)));
  assert.throws(() => preflightSemanticFixtureJson(JSON.stringify({ child: exactDepth })), ContractValidationError);

  assert.doesNotThrow(() =>
    preflightSemanticFixtureJson(JSON.stringify(new Array(MAX_SEMANTIC_FIXTURE_NODES - 1).fill(0))),
  );
  assert.throws(
    () => preflightSemanticFixtureJson(JSON.stringify(new Array(MAX_SEMANTIC_FIXTURE_NODES).fill(0))),
    ContractValidationError,
  );
});

interface AccessRequestFixture {
  fixture_contract_version: number;
  probe_grant_request: NativeProbeGrantRequest;
  catalog_probe_grant_request: NativeProbeGrantRequest;
  durable_probe_grant_request: NativeProbeGrantRequest;
  retrieval_request: AccessReportRetrievalRequest;
  expected_probe_grant_digest: string;
  expected_durable_probe_grant_digest: string;
  expected_retrieval_digest: string;
}

const accessRequestFixture = JSON.parse(
  readFileSync(
    new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012a-access-request-v1.json', import.meta.url),
    'utf8',
  ),
) as AccessRequestFixture;

function recomputeProbeGrantDigest(request: NativeProbeGrantRequest): void {
  const hex = nodeNativeProbeGrantRequestDigest(request).slice('sha256:'.length);
  request.digest = [...Buffer.from(hex, 'hex')];
}

function nodeNativeProbeGrantRequestDigest(request: NativeProbeGrantRequest): string {
  const requestTopologyCodes = { catalog: 1, durable: 2, scoped: 3 } as const;
  const requestOperationCodes = {
    catalog_discovery: 1,
    durable_history_runtime: 2,
    scoped_typed_observation: 3,
  } as const;
  const chunks = [Buffer.from('spaghetti/rfc012a/native-probe-grant-request/v1\0', 'utf8')];
  pushU32(chunks, request.access_request_contract_version);
  pushText(chunks, request.adapter_id);
  pushText(chunks, request.support_release_id);
  pushToken(chunks, request.support_release_digest);
  pushToken(chunks, request.source_declaration_digest);
  pushToken(chunks, request.scope_program_digest);
  pushText(chunks, request.declaration_id);
  pushText(chunks, request.program_id);
  pushU8(chunks, requestTopologyCodes[request.capability_topology]);
  pushU8(chunks, requestOperationCodes[request.operation]);
  pushU32(chunks, request.selection.selection_contract_version);
  pushU32(chunks, request.selection.model_major);
  pushU32(chunks, request.selection.external_entity_reference_version);
  pushU32(chunks, request.selection.semantic_revision_reference_version);
  pushU32(chunks, request.selection.coverage_contract_version);
  const families = Object.keys(request.selection.fact_family_versions).sort();
  pushU64(chunks, families.length);
  for (const family of families) {
    pushText(chunks, family);
    pushU32(chunks, request.selection.fact_family_versions[family]!);
  }
  for (const version of [request.selection.query_pack_version, request.selection.observation_contract_version]) {
    if (version === null) {
      pushU8(chunks, 0);
    } else {
      pushU8(chunks, 1);
      pushU32(chunks, version);
    }
  }
  pushToken(chunks, request.access_policy_digest);
  pushText(chunks, request.probe.family);
  pushText(chunks, request.probe.platform);
  if (request.probe.version === null) {
    pushU8(chunks, 0);
  } else {
    pushU8(chunks, 1);
    pushText(chunks, request.probe.version);
  }
  const markers = [...new Set(request.probe.markers)].sort();
  pushU64(chunks, markers.length);
  for (const marker of markers) pushText(chunks, marker);
  pushU8(chunks, request.probe.contradictory_markers ? 1 : 0);
  pushU64(chunks, request.grants.length);
  for (const grant of request.grants) {
    pushText(chunks, grant.relation_id);
    pushU8(chunks, grant.scope_root ? 1 : 0);
    pushText(chunks, grant.access_root);
    pushU64(chunks, grant.identity_input_names.length);
    for (const name of grant.identity_input_names) pushText(chunks, name);
  }
  return `sha256:${createHash('sha256').update(Buffer.concat(chunks)).digest('hex')}`;
}

test('Rust, Python, and TypeScript access-request digests agree', () => {
  assert.equal(accessRequestFixture.fixture_contract_version, 1);
  const probeGrant = parseNativeProbeGrantRequest(accessRequestFixture.probe_grant_request);
  assert.equal(nodeNativeProbeGrantRequestDigest(probeGrant), accessRequestFixture.expected_probe_grant_digest);
  assert.equal(
    `sha256:${Buffer.from(probeGrant.digest).toString('hex')}`,
    accessRequestFixture.expected_probe_grant_digest,
  );
  const catalog = parseNativeProbeGrantRequest(accessRequestFixture.catalog_probe_grant_request);
  assert.equal(catalog.grants.length, 0);
  assert.equal(catalog.program_id, '');
  assert.equal(nodeNativeProbeGrantRequestDigest(catalog), `sha256:${Buffer.from(catalog.digest).toString('hex')}`);
  const durable = parseNativeProbeGrantRequest(accessRequestFixture.durable_probe_grant_request);
  assert.equal(durable.operation, 'durable_history_runtime');
  assert.equal(durable.grants.length, 0);
  assert.equal(nodeNativeProbeGrantRequestDigest(durable), accessRequestFixture.expected_durable_probe_grant_digest);
  const retrieval = parseAccessReportRetrieval(accessRequestFixture.retrieval_request);
  assert.equal(
    `sha256:${Buffer.from(retrieval.digest).toString('hex')}`,
    accessRequestFixture.expected_retrieval_digest,
  );
  assert.deepEqual(retrieval.expected_report_digest, accessReportFixture.report.digest);
});

function exactBoundMarkers(): string[] {
  return ['native.marker', ...Array.from({ length: 63 }, (_, index) => `marker-${String(index + 1).padStart(2, '0')}`)];
}

function oneOverMarkers(): string[] {
  return [...exactBoundMarkers(), 'marker-64'];
}

function exactBoundFamilies(): Record<string, number> {
  return Object.fromEntries(Array.from({ length: 64 }, (_, index) => [`family-${String(index).padStart(2, '0')}`, 1]));
}

function oneOverFamilies(): Record<string, number> {
  return Object.fromEntries(Array.from({ length: 65 }, (_, index) => [`family-${String(index).padStart(2, '0')}`, 1]));
}

function paddedMachineId(prefix: string, length: number): string {
  return prefix + 'a'.repeat(length - prefix.length);
}

function exactBoundGrants(): Array<{
  relation_id: string;
  scope_root: boolean;
  access_root: string;
  identity_input_names: string[];
}> {
  return Array.from({ length: 256 }, (_, index) => ({
    relation_id: paddedMachineId(`g${String(index).padStart(3, '0')}`, 128),
    scope_root: index === 0,
    access_root: paddedMachineId('r', 127),
    identity_input_names: ['x'],
  }));
}

function oneOverGrants(): Array<{
  relation_id: string;
  scope_root: boolean;
  access_root: string;
  identity_input_names: string[];
}> {
  const grants = exactBoundGrants();
  grants[255]!.identity_input_names = ['xx'];
  return grants;
}

function assertPrivateError(run: () => unknown, leaked: RegExp): void {
  assert.throws(run, (error: unknown) => {
    assert.ok(error instanceof ContractValidationError);
    assert.doesNotMatch(error.message, leaked);
    return true;
  });
}

test('access-request portable values reject authority-shaped drift', () => {
  const probeGrant = mutableJson(accessRequestFixture.probe_grant_request);
  probeGrant['/Users/alice/private/session.jsonl'] = true;
  assertPrivateError(() => parseNativeProbeGrantRequest(probeGrant), /\/Users\/alice|session\.jsonl/);

  const wrongTypedPath = structuredClone(accessRequestFixture.probe_grant_request);
  wrongTypedPath.probe.version = ['/tmp/secret'] as unknown as string;
  assertPrivateError(() => parseNativeProbeGrantRequest(wrongTypedPath), /\/tmp\/secret|secret/);

  const zeroPolicy = structuredClone(accessRequestFixture.probe_grant_request) as {
    access_policy_digest: number[];
  };
  zeroPolicy.access_policy_digest = new Array(32).fill(0);
  assert.throws(() => parseNativeProbeGrantRequest(zeroPolicy), ContractValidationError);

  const pathMarker = structuredClone(accessRequestFixture.probe_grant_request);
  pathMarker.probe.markers = [...pathMarker.probe.markers, '/tmp/secret'];
  recomputeProbeGrantDigest(pathMarker);
  assert.throws(
    () => parseNativeProbeGrantRequest(pathMarker),
    (error: unknown) => {
      assert.ok(error instanceof ContractValidationError);
      assert.match(error.message, /machine identifier/);
      assert.doesNotMatch(error.message, /\/tmp\/secret/);
      return true;
    },
  );

  const nulMarker = structuredClone(accessRequestFixture.probe_grant_request);
  nulMarker.probe.markers = [...nulMarker.probe.markers, '\u0000secret'];
  recomputeProbeGrantDigest(nulMarker);
  assert.throws(
    () => parseNativeProbeGrantRequest(nulMarker),
    (error: unknown) => {
      assert.ok(error instanceof ContractValidationError);
      assert.match(error.message, /machine identifier/);
      assert.equal(error.message.includes('\u0000'), false);
      assert.doesNotMatch(error.message, /secret/);
      return true;
    },
  );

  const unicodeVersion = structuredClone(accessRequestFixture.probe_grant_request);
  unicodeVersion.probe.version = 'é'.repeat(80);
  recomputeProbeGrantDigest(unicodeVersion);
  assert.throws(
    () => parseNativeProbeGrantRequest(unicodeVersion),
    (error: unknown) => {
      assert.ok(error instanceof ContractValidationError);
      assert.match(error.message, /machine identifier/);
      assert.doesNotMatch(error.message, /é/);
      return true;
    },
  );

  const oversizedFamilies = structuredClone(accessRequestFixture.probe_grant_request);
  oversizedFamilies.selection.fact_family_versions = Object.fromEntries(
    Array.from({ length: 5_000 }, (_, index) => [`family-${index}`, 1]),
  );
  assert.throws(() => parseNativeProbeGrantRequest(oversizedFamilies), ContractValidationError);

  const catalogWithoutPack = structuredClone(accessRequestFixture.catalog_probe_grant_request);
  catalogWithoutPack.selection.query_pack_version = null;
  recomputeProbeGrantDigest(catalogWithoutPack);
  assert.throws(() => parseNativeProbeGrantRequest(catalogWithoutPack), ContractValidationError);

  const missingVersion = mutableJson(accessRequestFixture.probe_grant_request);
  delete mutableField(missingVersion.probe).version;
  assert.throws(() => parseNativeProbeGrantRequest(missingVersion), ContractValidationError);

  const missingQueryPack = mutableJson(accessRequestFixture.durable_probe_grant_request);
  delete mutableField(missingQueryPack.selection).query_pack_version;
  assert.throws(() => parseNativeProbeGrantRequest(missingQueryPack), ContractValidationError);

  const missingObservation = mutableJson(accessRequestFixture.probe_grant_request);
  delete mutableField(missingObservation.selection).observation_contract_version;
  assert.throws(() => parseNativeProbeGrantRequest(missingObservation), ContractValidationError);

  const exactMarkers = structuredClone(accessRequestFixture.probe_grant_request);
  exactMarkers.probe.markers = exactBoundMarkers();
  recomputeProbeGrantDigest(exactMarkers);
  parseNativeProbeGrantRequest(exactMarkers);

  const overMarkers = structuredClone(accessRequestFixture.probe_grant_request);
  overMarkers.probe.markers = oneOverMarkers();
  assert.throws(() => parseNativeProbeGrantRequest(overMarkers), ContractValidationError);

  const exactFamilies = structuredClone(accessRequestFixture.probe_grant_request);
  exactFamilies.selection.fact_family_versions = exactBoundFamilies();
  recomputeProbeGrantDigest(exactFamilies);
  parseNativeProbeGrantRequest(exactFamilies);

  const overFamilies = structuredClone(accessRequestFixture.probe_grant_request);
  overFamilies.selection.fact_family_versions = oneOverFamilies();
  assert.throws(() => parseNativeProbeGrantRequest(overFamilies), ContractValidationError);

  const overIdentifier = structuredClone(accessRequestFixture.probe_grant_request);
  overIdentifier.probe.version = 'a'.repeat(129);
  assert.throws(() => parseNativeProbeGrantRequest(overIdentifier), ContractValidationError);

  const oversizedProgram = structuredClone(accessRequestFixture.probe_grant_request);
  oversizedProgram.program_id = 'a'.repeat(4224);
  assert.throws(() => parseNativeProbeGrantRequest(oversizedProgram), ContractValidationError);

  const exactGrants = structuredClone(accessRequestFixture.probe_grant_request);
  exactGrants.grants = exactBoundGrants();
  recomputeProbeGrantDigest(exactGrants);
  parseNativeProbeGrantRequest(exactGrants);

  const overGrants = structuredClone(accessRequestFixture.probe_grant_request);
  overGrants.grants = oneOverGrants();
  assert.throws(() => parseNativeProbeGrantRequest(overGrants), ContractValidationError);

  const malformedDigest = mutableJson(accessRequestFixture.probe_grant_request);
  malformedDigest.digest = '/tmp/secret';
  assertPrivateError(() => parseNativeProbeGrantRequest(malformedDigest), /\/tmp\/secret|secret/);

  const malformedPolicy = mutableJson(accessRequestFixture.probe_grant_request);
  malformedPolicy.access_policy_digest = '/tmp/secret';
  assertPrivateError(() => parseNativeProbeGrantRequest(malformedPolicy), /\/tmp\/secret|secret/);

  const sparsePolicy = structuredClone(accessRequestFixture.probe_grant_request);
  delete (sparsePolicy.access_policy_digest as number[])[29];
  assert.throws(() => parseNativeProbeGrantRequest(sparsePolicy), ContractValidationError);

  const markerOrderA = structuredClone(accessRequestFixture.probe_grant_request);
  markerOrderA.probe.markers = ['native.marker', 'extra.marker'];
  const markerOrderB = structuredClone(accessRequestFixture.probe_grant_request);
  markerOrderB.probe.markers = ['extra.marker', 'native.marker'];
  assert.equal(nodeNativeProbeGrantRequestDigest(markerOrderA), nodeNativeProbeGrantRequestDigest(markerOrderB));
  recomputeProbeGrantDigest(markerOrderA);
  assert.deepEqual(parseNativeProbeGrantRequest(markerOrderA).probe.markers, ['extra.marker', 'native.marker']);

  const extraGrant = mutableJson(accessRequestFixture.probe_grant_request);
  mutableList(extraGrant.grants).push({
    relation_id: 'sibling-object',
    scope_root: true,
    access_root: 'root',
    identity_input_names: ['native-session-id'],
  });
  assert.throws(() => parseNativeProbeGrantRequest(extraGrant), ContractValidationError);

  const digestDrift = structuredClone(accessRequestFixture.probe_grant_request);
  digestDrift.access_policy_digest = digestDrift.access_policy_digest.map((byte, index) =>
    index === 0 ? byte ^ 1 : byte,
  );
  assert.throws(() => parseNativeProbeGrantRequest(digestDrift), ContractValidationError);

  const catalogGrants = mutableJson(accessRequestFixture.catalog_probe_grant_request);
  mutableList(catalogGrants.grants).push(structuredClone(accessRequestFixture.probe_grant_request.grants[0]));
  assert.throws(() => parseNativeProbeGrantRequest(catalogGrants), ContractValidationError);

  const selectionDrift = structuredClone(accessRequestFixture.probe_grant_request);
  selectionDrift.selection.model_major = 2;
  assert.throws(() => parseNativeProbeGrantRequest(selectionDrift), ContractValidationError);

  const retrieval = mutableJson(accessRequestFixture.retrieval_request);
  retrieval.operation = 'catalog_discovery';
  retrieval.capability_topology = 'catalog';
  assert.throws(() => parseAccessReportRetrieval(retrieval), ContractValidationError);

  const zeroReport = structuredClone(accessRequestFixture.retrieval_request) as {
    expected_report_digest: number[];
  };
  zeroReport.expected_report_digest = new Array(32).fill(0);
  assert.throws(() => parseAccessReportRetrieval(zeroReport), ContractValidationError);
});
