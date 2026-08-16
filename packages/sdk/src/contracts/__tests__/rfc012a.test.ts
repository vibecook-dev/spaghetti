import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import {
  classifyRuntimeSupport,
  compareCoverage,
  ContractValidationError,
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

test('incompatible majors and malformed complete coverage are rejected', () => {
  assert.throws(
    () =>
      parseExternalEntityRef({
        ...(fixture.external_entity_ref as object),
        external_entity_reference_version: 2,
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
