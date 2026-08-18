/** Contextual RFC 012D observation-capability snapshot.
 *
 * This freezes one already-validated, phase-independent capability report and
 * its Rust-derived semantic digest. It is not source coverage, current
 * readiness, an artifact manifest, a bootstrap/resync barrier, source-access
 * authority, or portable observer transport. Portable consumers compare the
 * digest to caller-held context and do not claim to recompute BLAKE3.
 */

import {
  ContractValidationError,
  parseOpaqueContractReference,
  type CompatibilityClass,
  type OpaqueContractReference,
} from './rfc012a.js';
import {
  parseObservationCapabilities,
  parseObservationContractOffer,
  parseObservationContractSelectionForExpected,
  type ObservationCapabilities,
  type ObservationContractOffer,
  type ObservationContractSelection,
} from './rfc012d.js';

export const SCOPED_CAPABILITY_SNAPSHOT_CONTRACT_VERSION = 1 as const;
export const SCOPED_CAPABILITY_DIGEST_CONTRACT_VERSION = 1 as const;

type AuthorizedObservationCompatibility = Extract<CompatibilityClass, 'ExactSupported' | 'RangeSupported'>;
type UnknownRecord = Record<string, unknown>;

export interface ScopedCapabilitySnapshotContext {
  contract_selection: ObservationContractSelection;
  contract_offer: ObservationContractOffer;
  compatibility_class: AuthorizedObservationCompatibility;
  support_release_id: string;
  expected_capabilities: ObservationCapabilities;
  expected_semantic_digest: OpaqueContractReference;
}

export interface ScopedCapabilitySnapshot {
  scoped_capability_snapshot_contract_version: typeof SCOPED_CAPABILITY_SNAPSHOT_CONTRACT_VERSION;
  capability_digest_contract_version: typeof SCOPED_CAPABILITY_DIGEST_CONTRACT_VERSION;
  observation_capabilities: ObservationCapabilities;
  semantic_digest: OpaqueContractReference;
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

function authorizedCompatibility(value: unknown): AuthorizedObservationCompatibility {
  if (value !== 'ExactSupported' && value !== 'RangeSupported') {
    throw new ContractValidationError('capability snapshot requires authorized support context');
  }
  return value;
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

function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  if (value !== null && typeof value === 'object') {
    const input = value as UnknownRecord;
    return `{${Object.keys(input)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalJson(input[key])}`)
      .join(',')}}`;
  }
  return JSON.stringify(value);
}

function capabilityReportsEqual(left: ObservationCapabilities, right: ObservationCapabilities): boolean {
  return canonicalJson(left) === canonicalJson(right);
}

export function parseScopedCapabilitySnapshotContext(value: unknown): ScopedCapabilitySnapshotContext {
  const input = exactRecord(
    value,
    [
      'contract_selection',
      'contract_offer',
      'compatibility_class',
      'support_release_id',
      'expected_capabilities',
      'expected_semantic_digest',
    ],
    'scoped capability snapshot context',
  );
  const contractSelection = parseObservationContractSelectionForExpected(
    input.contract_selection,
    input.contract_selection,
  );
  const contractOffer = parseObservationContractOffer(input.contract_offer);
  const compatibilityClass = authorizedCompatibility(input.compatibility_class);
  const expectedCapabilities = parseObservationCapabilities(
    input.expected_capabilities,
    contractSelection,
    contractOffer,
    compatibilityClass,
    input.support_release_id,
  );
  if (typeof input.support_release_id !== 'string') {
    throw new ContractValidationError('capability snapshot support release id must be a string');
  }
  return {
    contract_selection: contractSelection,
    contract_offer: contractOffer,
    compatibility_class: compatibilityClass,
    support_release_id: input.support_release_id,
    expected_capabilities: expectedCapabilities,
    expected_semantic_digest: fixedNonzeroOpaque(input.expected_semantic_digest, 'expected capability semantic digest'),
  };
}

export function parseScopedCapabilitySnapshot(value: unknown, contextInput: unknown): ScopedCapabilitySnapshot {
  const context = parseScopedCapabilitySnapshotContext(contextInput);
  const input = exactRecord(
    value,
    [
      'scoped_capability_snapshot_contract_version',
      'capability_digest_contract_version',
      'observation_capabilities',
      'semantic_digest',
    ],
    'scoped capability snapshot',
  );
  if (
    input.scoped_capability_snapshot_contract_version !== SCOPED_CAPABILITY_SNAPSHOT_CONTRACT_VERSION ||
    input.capability_digest_contract_version !== SCOPED_CAPABILITY_DIGEST_CONTRACT_VERSION
  ) {
    throw new ContractValidationError('unsupported scoped capability snapshot contract version');
  }
  const observationCapabilities = parseObservationCapabilities(
    input.observation_capabilities,
    context.contract_selection,
    context.contract_offer,
    context.compatibility_class,
    context.support_release_id,
  );
  const semanticDigest = fixedNonzeroOpaque(input.semantic_digest, 'capability semantic digest');
  if (
    !capabilityReportsEqual(observationCapabilities, context.expected_capabilities) ||
    semanticDigest !== context.expected_semantic_digest
  ) {
    throw new ContractValidationError('capability snapshot does not match caller-held context');
  }
  return {
    scoped_capability_snapshot_contract_version: SCOPED_CAPABILITY_SNAPSHOT_CONTRACT_VERSION,
    capability_digest_contract_version: SCOPED_CAPABILITY_DIGEST_CONTRACT_VERSION,
    observation_capabilities: observationCapabilities,
    semantic_digest: semanticDigest,
  };
}
