import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import {
  compareCoverage,
  ContractValidationError,
  parseExternalEntityRef,
  parseNativeIdentityClaim,
  parseQualifiedValue,
  parseSemanticRevisionRef,
  parseSourceCoverageSet,
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
