/** Contextual RFC 012D replacement-family manifest.
 *
 * This slice freezes only selected reducer replacement semantics,
 * completeness, counts, and Rust-derived semantic digests. It is not a
 * bootstrap/resync barrier, source-access proof, watermark, or observer
 * transport. Consumption requires caller-held negotiation, RFC 012A family
 * coverage, and the exact expected reducer manifest.
 */

import {
  ContractValidationError,
  parseOpaqueContractReference,
  parseSourceCoverageSet,
  type CoverageSetCompleteness,
  type OpaqueContractReference,
  type SourceCoverageSet,
} from './rfc012a.js';
import {
  ACTOR_AFFILIATION_FAMILY,
  ACTOR_AFFILIATION_FAMILY_VERSION,
  ACTOR_RUN_FAMILY,
  ACTOR_RUN_FAMILY_VERSION,
  USAGE_V2_FAMILY,
  USAGE_V2_FAMILY_VERSION,
} from './rfc012c.js';
import { parseObservationContractSelectionForExpected, type ObservationContractSelection } from './rfc012d.js';

export const SCOPED_REPLACEMENT_MANIFEST_CONTRACT_VERSION = 1 as const;
export const SCOPED_REPLACEMENT_DIGEST_CONTRACT_VERSION = 1 as const;

const MAX_CONTEXT_COVERAGE_SETS = 64;
const MAX_MANIFEST_FAMILIES = 3;
type UnknownRecord = Record<string, unknown>;

export type ScopedReplacementRepresentation = 'revisioned_entity_current' | 'usage_latest_contribution_per_response';

export interface ScopedReplacementFamilyManifest {
  fact_family: string;
  contract_version: number;
  replacement_representation: ScopedReplacementRepresentation;
  completeness: CoverageSetCompleteness;
  entity_or_event_count: number;
  semantic_digest: OpaqueContractReference;
}

export interface ScopedReplacementManifestContext {
  contract_selection: ObservationContractSelection;
  source_coverage: SourceCoverageSet[];
  expected_families: ScopedReplacementFamilyManifest[];
}

export interface ScopedReplacementManifest {
  scoped_replacement_manifest_contract_version: typeof SCOPED_REPLACEMENT_MANIFEST_CONTRACT_VERSION;
  replacement_digest_contract_version: typeof SCOPED_REPLACEMENT_DIGEST_CONTRACT_VERSION;
  contract_selection: ObservationContractSelection;
  families: ScopedReplacementFamilyManifest[];
}

function record(value: unknown, label: string): UnknownRecord {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new ContractValidationError(`${label} must be an object`);
  }
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== Object.prototype && prototype !== null) {
    throw new ContractValidationError(`${label} must be a plain JSON object`);
  }
  return value as UnknownRecord;
}

function exactRecord(value: unknown, fields: readonly string[], label: string): UnknownRecord {
  const input = record(value, label);
  const known = new Set(fields);
  for (const key of Object.keys(input)) {
    if (!known.has(key)) throw new ContractValidationError(`${label} contains unknown field ${key}`);
  }
  for (const field of fields) {
    if (!Object.hasOwn(input, field)) throw new ContractValidationError(`${label} is missing field ${field}`);
  }
  return input;
}

function nonnegativeSafeInteger(value: unknown, label: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0) {
    throw new ContractValidationError(`${label} must be a non-negative portable integer`);
  }
  return value;
}

function completeness(value: unknown): CoverageSetCompleteness {
  if (value !== 'complete' && value !== 'partial' && value !== 'unavailable') {
    throw new ContractValidationError('replacement completeness is unsupported');
  }
  return value;
}

function mergeCompleteness(left: CoverageSetCompleteness, right: CoverageSetCompleteness): CoverageSetCompleteness {
  if (left === 'unavailable' || right === 'unavailable') return 'unavailable';
  if (left === 'partial' || right === 'partial') return 'partial';
  return 'complete';
}

function fixedNonzeroOpaque(value: unknown, label: string): OpaqueContractReference {
  const parsed = parseOpaqueContractReference(value, label);
  const encoded = parsed.slice(3);
  const standard = encoded.replace(/-/g, '+').replace(/_/g, '/');
  const padded = standard.padEnd(Math.ceil(standard.length / 4) * 4, '=');
  let binary: string;
  try {
    binary = atob(padded);
  } catch {
    throw new ContractValidationError(`${label} is not canonical base64url`);
  }
  let roundTrip = '';
  let nonzero = false;
  for (let index = 0; index < binary.length; index += 1) {
    const byte = binary.charCodeAt(index);
    roundTrip += String.fromCharCode(byte);
    nonzero ||= byte !== 0;
  }
  const canonical = btoa(roundTrip).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/g, '');
  if (binary.length !== 32 || canonical !== encoded || !nonzero) {
    throw new ContractValidationError(`${label} must contain 32 canonical nonzero bytes`);
  }
  return parsed;
}

function familyContract(family: unknown): {
  family: string;
  version: number;
  representation: ScopedReplacementRepresentation;
} {
  switch (family) {
    case ACTOR_AFFILIATION_FAMILY:
      return {
        family,
        version: ACTOR_AFFILIATION_FAMILY_VERSION,
        representation: 'revisioned_entity_current',
      };
    case ACTOR_RUN_FAMILY:
      return { family, version: ACTOR_RUN_FAMILY_VERSION, representation: 'revisioned_entity_current' };
    case USAGE_V2_FAMILY:
      return {
        family,
        version: USAGE_V2_FAMILY_VERSION,
        representation: 'usage_latest_contribution_per_response',
      };
    default:
      throw new ContractValidationError('replacement manifest contains an unsupported family');
  }
}

function parseFamily(value: unknown): ScopedReplacementFamilyManifest {
  const input = exactRecord(
    value,
    [
      'fact_family',
      'contract_version',
      'replacement_representation',
      'completeness',
      'entity_or_event_count',
      'semantic_digest',
    ],
    'replacement family',
  );
  const contract = familyContract(input.fact_family);
  if (input.contract_version !== contract.version || input.replacement_representation !== contract.representation) {
    throw new ContractValidationError('replacement family does not match its exact v1 contract');
  }
  return {
    fact_family: contract.family,
    contract_version: contract.version,
    replacement_representation: contract.representation,
    completeness: completeness(input.completeness),
    entity_or_event_count: nonnegativeSafeInteger(input.entity_or_event_count, 'replacement entity/event count'),
    semantic_digest: fixedNonzeroOpaque(input.semantic_digest, 'replacement semantic digest'),
  };
}

function parseFamilies(value: unknown): ScopedReplacementFamilyManifest[] {
  if (!Array.isArray(value) || value.length === 0 || value.length > MAX_MANIFEST_FAMILIES) {
    throw new ContractValidationError('replacement families must be a bounded non-empty array');
  }
  const families = value.map(parseFamily);
  for (let index = 1; index < families.length; index += 1) {
    if (families[index - 1]!.fact_family >= families[index]!.fact_family) {
      throw new ContractValidationError('replacement families must be strictly canonical');
    }
  }
  return families;
}

function familyEqual(left: ScopedReplacementFamilyManifest, right: ScopedReplacementFamilyManifest): boolean {
  return (
    left.fact_family === right.fact_family &&
    left.contract_version === right.contract_version &&
    left.replacement_representation === right.replacement_representation &&
    left.completeness === right.completeness &&
    left.entity_or_event_count === right.entity_or_event_count &&
    left.semantic_digest === right.semantic_digest
  );
}

function validateContext(
  selection: ObservationContractSelection,
  sourceCoverage: SourceCoverageSet[],
  expectedFamilies: ScopedReplacementFamilyManifest[],
): void {
  const selected = selection.contract_versions.fact_family_versions;
  const selectedFamilies = Object.keys(selected).sort();
  if (selectedFamilies.length !== expectedFamilies.length || selectedFamilies.length > MAX_MANIFEST_FAMILIES) {
    throw new ContractValidationError('replacement manifest family count does not match selection');
  }

  const coverageCompleteness = new Map<string, CoverageSetCompleteness>();
  for (const coverage of sourceCoverage) {
    const domain = coverage.coverage_domain;
    if (domain.kind === 'decode') continue;
    if (domain.kind === 'projection_pack') {
      throw new ContractValidationError('projection-pack coverage cannot authorize a replacement family');
    }
    const contract = familyContract(domain.family);
    if (domain.version !== contract.version || selected[domain.family] !== domain.version) {
      throw new ContractValidationError('source coverage does not match the selected replacement families');
    }
    const current = coverageCompleteness.get(domain.family);
    coverageCompleteness.set(
      domain.family,
      current === undefined ? coverage.completeness : mergeCompleteness(current, coverage.completeness),
    );
  }

  for (let index = 0; index < selectedFamilies.length; index += 1) {
    const selectedFamily = selectedFamilies[index]!;
    const contract = familyContract(selectedFamily);
    const expected = expectedFamilies[index]!;
    if (
      selected[selectedFamily] !== contract.version ||
      expected.fact_family !== selectedFamily ||
      expected.contract_version !== contract.version ||
      expected.replacement_representation !== contract.representation ||
      expected.completeness !== coverageCompleteness.get(selectedFamily)
    ) {
      throw new ContractValidationError('replacement families are not canonical or coverage-bound');
    }
    coverageCompleteness.delete(selectedFamily);
  }
  if (coverageCompleteness.size !== 0) {
    throw new ContractValidationError('replacement coverage contains an unconsumed family');
  }
}

export function parseScopedReplacementManifestContext(value: unknown): ScopedReplacementManifestContext {
  const input = exactRecord(
    value,
    ['contract_selection', 'source_coverage', 'expected_families'],
    'scoped replacement manifest context',
  );
  const contractSelection = parseObservationContractSelectionForExpected(
    input.contract_selection,
    input.contract_selection,
  );
  if (
    !Array.isArray(input.source_coverage) ||
    input.source_coverage.length === 0 ||
    input.source_coverage.length > MAX_CONTEXT_COVERAGE_SETS
  ) {
    throw new ContractValidationError('replacement manifest context has an invalid source-coverage set count');
  }
  const sourceCoverage = input.source_coverage.map(parseSourceCoverageSet);
  const expectedFamilies = parseFamilies(input.expected_families);
  validateContext(contractSelection, sourceCoverage, expectedFamilies);
  return {
    contract_selection: contractSelection,
    source_coverage: sourceCoverage,
    expected_families: expectedFamilies,
  };
}

export function parseScopedReplacementManifest(value: unknown, contextInput: unknown): ScopedReplacementManifest {
  const context = parseScopedReplacementManifestContext(contextInput);
  const input = exactRecord(
    value,
    [
      'scoped_replacement_manifest_contract_version',
      'replacement_digest_contract_version',
      'contract_selection',
      'families',
    ],
    'scoped replacement manifest',
  );
  if (
    input.scoped_replacement_manifest_contract_version !== SCOPED_REPLACEMENT_MANIFEST_CONTRACT_VERSION ||
    input.replacement_digest_contract_version !== SCOPED_REPLACEMENT_DIGEST_CONTRACT_VERSION
  ) {
    throw new ContractValidationError('unsupported scoped replacement manifest contract version');
  }
  const contractSelection = parseObservationContractSelectionForExpected(
    input.contract_selection,
    context.contract_selection,
  );
  const families = parseFamilies(input.families);
  validateContext(contractSelection, context.source_coverage, families);
  if (
    families.length !== context.expected_families.length ||
    families.some((family, index) => !familyEqual(family, context.expected_families[index]!))
  ) {
    throw new ContractValidationError('replacement manifest does not match caller-held reducer state');
  }
  return {
    scoped_replacement_manifest_contract_version: SCOPED_REPLACEMENT_MANIFEST_CONTRACT_VERSION,
    replacement_digest_contract_version: SCOPED_REPLACEMENT_DIGEST_CONTRACT_VERSION,
    contract_selection: contractSelection,
    families,
  };
}
