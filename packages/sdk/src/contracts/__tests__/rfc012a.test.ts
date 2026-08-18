import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import {
  classifyRuntimeSupport,
  compareCoverage,
  ContractValidationError,
  parseContractVersionOffer,
  parseContractVersionRequest,
  parseContractVersionSelection,
  parseExternalEntityRef,
  parseNativeIdentityClaim,
  parseQualifiedValue,
  parseSemanticRevisionRef,
  parseSourceCoverageSet,
  selectContractVersions,
  type SourceCoverageSet,
} from '../rfc012a.js';

interface ContractFixture {
  fixture_contract_version: number;
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

const fixture = JSON.parse(
  readFileSync(
    new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012a-v1.json', import.meta.url),
    'utf8',
  ),
) as ContractFixture;

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
  assert.equal(fixture.fixture_contract_version, 1);
  assert.equal(parseExternalEntityRef(fixture.external_entity_ref).external_entity_reference_version, 1);
  assert.equal(parseSemanticRevisionRef(fixture.semantic_revision_ref).semantic_reference_contract_version, 1);
  assert.equal(parseNativeIdentityClaim(fixture.native_identity_claim).identity.value?.native_id, 'session-1');
  assert.equal(parseQualifiedValue<number>(fixture.qualified_known_zero).value, 0);
  assert.equal(parseQualifiedValue(fixture.qualified_unknown).unknown_reason, 'withheld');
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
