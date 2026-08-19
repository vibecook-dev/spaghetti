/** Contextual RFC 012D artifact-availability snapshot.
 *
 * This parses state already frozen by Rust and bound into completion identity.
 * It does not authorize artifact access, assign observer ordering, or create a
 * bootstrap/resync barrier. Portable consumers compare every entry and the
 * Rust-derived BLAKE3 digest to caller-held context.
 */

import { ContractValidationError, parseOpaqueContractReference, type OpaqueContractReference } from './rfc012a.js';
import { parseObservationContractSelectionForExpected, type ObservationContractSelection } from './rfc012d.js';

export const SCOPED_ARTIFACT_AVAILABILITY_CONTRACT_VERSION = 1 as const;

const MAX_ARTIFACT_AVAILABILITY_ENTRIES = 4_096;
const MAX_ARTIFACT_KIND_BYTES = 128;
const UTF8_ENCODER = new TextEncoder();
type UnknownRecord = Record<string, unknown>;

export type ScopedArtifactAvailabilityState =
  | {
      kind: 'available';
      generation: number;
      provenance_ref: OpaqueContractReference;
      size_bytes: number;
    }
  | {
      kind: 'missing';
      observed_generation: number | null;
      provenance_ref: OpaqueContractReference | null;
    }
  | {
      kind: 'over_limit';
      generation: number;
      provenance_ref: OpaqueContractReference;
      observed_bytes: number;
      request_max_bytes: number;
    }
  | { kind: 'unstable' };

export interface ScopedArtifactAvailabilityEntry {
  artifact_key: OpaqueContractReference;
  artifact_kind: string;
  revision: OpaqueContractReference;
  state: ScopedArtifactAvailabilityState;
}

export interface ScopedArtifactAvailabilityContext {
  contract_selection: ObservationContractSelection;
  root_session_key: OpaqueContractReference;
  expected_entry_count: number;
  expected_semantic_digest: OpaqueContractReference;
  expected_entries: ScopedArtifactAvailabilityEntry[];
}

export interface ScopedArtifactAvailabilitySnapshot {
  scoped_artifact_availability_contract_version: typeof SCOPED_ARTIFACT_AVAILABILITY_CONTRACT_VERSION;
  contract_selection: ObservationContractSelection;
  root_session_key: OpaqueContractReference;
  entry_count: number;
  semantic_digest: OpaqueContractReference;
  entries: ScopedArtifactAvailabilityEntry[];
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

function boundedIdentifier(value: unknown, label: string): string {
  if (
    typeof value !== 'string' ||
    value.length === 0 ||
    value.length > MAX_ARTIFACT_KIND_BYTES ||
    value.trim() !== value ||
    !/^[a-z][a-z0-9._-]*$/.test(value) ||
    UTF8_ENCODER.encode(value).byteLength > MAX_ARTIFACT_KIND_BYTES
  ) {
    throw new ContractValidationError(`${label} is not a bounded canonical identifier`);
  }
  return value;
}

function opaqueBytes(value: OpaqueContractReference, label: string): Uint8Array {
  const encoded = value.slice(3);
  const standard = encoded.replace(/-/g, '+').replace(/_/g, '/');
  const padded = standard.padEnd(Math.ceil(standard.length / 4) * 4, '=');
  let binary: string;
  try {
    binary = atob(padded);
  } catch {
    throw new ContractValidationError(`${label} is not canonical base64url`);
  }
  const bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0));
  let roundTrip = '';
  for (const byte of bytes) roundTrip += String.fromCharCode(byte);
  const canonical = btoa(roundTrip).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/g, '');
  if (bytes.length !== 32 || canonical !== encoded || bytes.every((byte) => byte === 0)) {
    throw new ContractValidationError(`${label} must contain 32 canonical nonzero bytes`);
  }
  return bytes;
}

function fixedOpaque(value: unknown, label: string): OpaqueContractReference {
  const parsed = parseOpaqueContractReference(value, label);
  opaqueBytes(parsed, label);
  return parsed;
}

function positiveInteger(value: unknown, label: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value <= 0) {
    throw new ContractValidationError(`${label} must be a positive portable integer`);
  }
  return value;
}

function nonnegativeInteger(value: unknown, label: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0) {
    throw new ContractValidationError(`${label} must be a nonnegative portable integer`);
  }
  return value;
}

function nullablePositive(value: unknown, label: string): number | null {
  return value === null ? null : positiveInteger(value, label);
}

function nullableOpaque(value: unknown, label: string): OpaqueContractReference | null {
  return value === null ? null : fixedOpaque(value, label);
}

function parseState(value: unknown): ScopedArtifactAvailabilityState {
  const tagged = record(value, 'artifact availability state');
  switch (tagged.kind) {
    case 'available': {
      const input = exactRecord(
        tagged,
        ['kind', 'generation', 'provenance_ref', 'size_bytes'],
        'available artifact state',
      );
      return {
        kind: 'available',
        generation: positiveInteger(input.generation, 'artifact availability generation'),
        provenance_ref: fixedOpaque(input.provenance_ref, 'artifact availability provenance'),
        size_bytes: nonnegativeInteger(input.size_bytes, 'artifact availability size'),
      };
    }
    case 'missing': {
      const input = exactRecord(tagged, ['kind', 'observed_generation', 'provenance_ref'], 'missing artifact state');
      const observedGeneration = nullablePositive(input.observed_generation, 'missing artifact observed generation');
      const provenanceRef = nullableOpaque(input.provenance_ref, 'missing artifact provenance');
      if ((observedGeneration === null) !== (provenanceRef === null)) {
        throw new ContractValidationError('missing artifact generation and provenance must be present together');
      }
      return {
        kind: 'missing',
        observed_generation: observedGeneration,
        provenance_ref: provenanceRef,
      };
    }
    case 'over_limit': {
      const input = exactRecord(
        tagged,
        ['kind', 'generation', 'provenance_ref', 'observed_bytes', 'request_max_bytes'],
        'over-limit artifact state',
      );
      const observedBytes = nonnegativeInteger(input.observed_bytes, 'over-limit artifact observed bytes');
      const requestMaxBytes = positiveInteger(input.request_max_bytes, 'over-limit artifact request maximum');
      if (observedBytes <= requestMaxBytes) {
        throw new ContractValidationError('over-limit artifact must exceed its request maximum');
      }
      return {
        kind: 'over_limit',
        generation: positiveInteger(input.generation, 'over-limit artifact generation'),
        provenance_ref: fixedOpaque(input.provenance_ref, 'over-limit artifact provenance'),
        observed_bytes: observedBytes,
        request_max_bytes: requestMaxBytes,
      };
    }
    case 'unstable': {
      exactRecord(tagged, ['kind'], 'unstable artifact state');
      return { kind: 'unstable' };
    }
    default:
      throw new ContractValidationError('artifact availability state kind is unsupported');
  }
}

function compareOpaque(left: OpaqueContractReference, right: OpaqueContractReference): number {
  const leftBytes = opaqueBytes(left, 'left artifact key');
  const rightBytes = opaqueBytes(right, 'right artifact key');
  for (let index = 0; index < leftBytes.length; index += 1) {
    if (leftBytes[index] !== rightBytes[index]) return leftBytes[index]! - rightBytes[index]!;
  }
  return 0;
}

function parseEntries(value: unknown, expectedCount: number, label: string): ScopedArtifactAvailabilityEntry[] {
  if (!Array.isArray(value) || value.length > MAX_ARTIFACT_AVAILABILITY_ENTRIES || value.length !== expectedCount) {
    throw new ContractValidationError(`${label} has an invalid bounded entry count`);
  }
  const entries = value.map((entry, index) => {
    const input = exactRecord(entry, ['artifact_key', 'artifact_kind', 'revision', 'state'], `${label}[${index}]`);
    return {
      artifact_key: fixedOpaque(input.artifact_key, `${label}[${index}] artifact key`),
      artifact_kind: boundedIdentifier(input.artifact_kind, `${label}[${index}] artifact kind`),
      revision: fixedOpaque(input.revision, `${label}[${index}] revision`),
      state: parseState(input.state),
    };
  });
  for (let index = 1; index < entries.length; index += 1) {
    const prior = entries[index - 1]!;
    const current = entries[index]!;
    const keyOrder = compareOpaque(prior.artifact_key, current.artifact_key);
    if (keyOrder > 0 || (keyOrder === 0 && prior.artifact_kind >= current.artifact_kind)) {
      throw new ContractValidationError(`${label} is not canonical and unique`);
    }
  }
  return entries;
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

export function parseScopedArtifactAvailabilityContext(value: unknown): ScopedArtifactAvailabilityContext {
  const input = exactRecord(
    value,
    ['contract_selection', 'root_session_key', 'expected_entry_count', 'expected_semantic_digest', 'expected_entries'],
    'scoped artifact availability context',
  );
  const expectedEntryCount = nonnegativeInteger(
    input.expected_entry_count,
    'expected artifact availability entry count',
  );
  if (expectedEntryCount > MAX_ARTIFACT_AVAILABILITY_ENTRIES) {
    throw new ContractValidationError('expected artifact availability entry count exceeds the portable bound');
  }
  return {
    contract_selection: parseObservationContractSelectionForExpected(
      input.contract_selection,
      input.contract_selection,
    ),
    root_session_key: fixedOpaque(input.root_session_key, 'artifact availability root session key'),
    expected_entry_count: expectedEntryCount,
    expected_semantic_digest: fixedOpaque(
      input.expected_semantic_digest,
      'expected artifact availability semantic digest',
    ),
    expected_entries: parseEntries(
      input.expected_entries,
      expectedEntryCount,
      'expected artifact availability entries',
    ),
  };
}

export function parseScopedArtifactAvailabilitySnapshot(
  value: unknown,
  contextValue: unknown,
): ScopedArtifactAvailabilitySnapshot {
  const context = parseScopedArtifactAvailabilityContext(contextValue);
  const input = exactRecord(
    value,
    [
      'scoped_artifact_availability_contract_version',
      'contract_selection',
      'root_session_key',
      'entry_count',
      'semantic_digest',
      'entries',
    ],
    'scoped artifact availability snapshot',
  );
  if (input.scoped_artifact_availability_contract_version !== SCOPED_ARTIFACT_AVAILABILITY_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported scoped artifact availability contract version');
  }
  const entryCount = nonnegativeInteger(input.entry_count, 'artifact availability entry count');
  if (entryCount > MAX_ARTIFACT_AVAILABILITY_ENTRIES) {
    throw new ContractValidationError('artifact availability entry count exceeds the portable bound');
  }
  const snapshot: ScopedArtifactAvailabilitySnapshot = {
    scoped_artifact_availability_contract_version: SCOPED_ARTIFACT_AVAILABILITY_CONTRACT_VERSION,
    contract_selection: parseObservationContractSelectionForExpected(
      input.contract_selection,
      context.contract_selection,
    ),
    root_session_key: fixedOpaque(input.root_session_key, 'artifact availability root session key'),
    entry_count: entryCount,
    semantic_digest: fixedOpaque(input.semantic_digest, 'artifact availability semantic digest'),
    entries: parseEntries(input.entries, entryCount, 'artifact availability entries'),
  };
  if (
    snapshot.root_session_key !== context.root_session_key ||
    snapshot.entry_count !== context.expected_entry_count ||
    snapshot.semantic_digest !== context.expected_semantic_digest ||
    canonicalJson(snapshot.entries) !== canonicalJson(context.expected_entries)
  ) {
    throw new ContractValidationError('artifact availability snapshot does not match caller-held context');
  }
  return snapshot;
}
