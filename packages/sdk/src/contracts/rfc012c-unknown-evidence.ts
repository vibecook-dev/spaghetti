/** Contextual RFC 012C bounded unknown-native-evidence snapshot.
 *
 * This parses exact complete-set totals/digest and policy-bounded samples
 * already reduced by Rust. It does not authorize source access, issue a query,
 * bind a source scope, assign observer ordering, or complete a replacement
 * barrier. An enclosing query/event owns that context.
 */

import { ContractValidationError, parseOpaqueContractReference, type OpaqueContractReference } from './rfc012a.js';

export const UNKNOWN_EVIDENCE_SNAPSHOT_CONTRACT_VERSION = 1 as const;
export const UNKNOWN_EVIDENCE_AGGREGATE_CONTRACT_VERSION = 1 as const;

const MAX_UNKNOWN_EVIDENCE_OCCURRENCES = 65_536;
const MAX_UNKNOWN_EVIDENCE_SAMPLES = 64;
const MAX_UNKNOWN_RAW_PAYLOAD_BYTES = 4 * 1024 * 1024;
const MAX_FAMILY_HINT_BYTES = 128;
const MAX_DIAGNOSTIC_SHAPE_ITEMS = 16;
const UTF8_ENCODER = new TextEncoder();
type UnknownRecord = Record<string, unknown>;

export type SanitizedJsonValueKind = 'null' | 'boolean' | 'number' | 'string' | 'array' | 'object';

export type SanitizedUnknownEvidenceExcerpt =
  | { kind: 'null' | 'boolean' | 'number' | 'string' | 'opaque'; bytes: number; hash: string }
  | {
      kind: 'json_array';
      bytes: number;
      hash: string;
      items: number;
      item_kinds: SanitizedJsonValueKind[];
      truncated: boolean;
    }
  | {
      kind: 'json_object';
      bytes: number;
      hash: string;
      members: number;
      shape: { key_hash: string; value_kind: SanitizedJsonValueKind }[];
      truncated: boolean;
    };

export interface UnknownEvidenceSample {
  source_record_id: OpaqueContractReference;
  family_hint: string | null;
  observed_bytes: number;
  payload_digest: OpaqueContractReference;
  sanitized_excerpt: SanitizedUnknownEvidenceExcerpt;
}

export interface UnknownEvidenceSnapshotContext {
  expected_complete_count: number;
  expected_complete_observed_bytes: number;
  expected_aggregate_digest: OpaqueContractReference;
  expected_samples: UnknownEvidenceSample[];
}

export interface UnknownEvidenceSnapshot {
  unknown_evidence_snapshot_contract_version: typeof UNKNOWN_EVIDENCE_SNAPSHOT_CONTRACT_VERSION;
  unknown_evidence_aggregate_contract_version: typeof UNKNOWN_EVIDENCE_AGGREGATE_CONTRACT_VERSION;
  complete_count: number;
  complete_observed_bytes: number;
  aggregate_digest: OpaqueContractReference;
  samples: UnknownEvidenceSample[];
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
    if (!known.has(key)) throw new ContractValidationError(`${label} contains an unknown field`);
  }
  for (const field of fields) {
    if (!Object.hasOwn(input, field)) throw new ContractValidationError(`${label} is missing a required field`);
  }
  return input;
}

function nonnegativeSafeInteger(value: unknown, label: string, maximum = Number.MAX_SAFE_INTEGER): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0 || value > maximum) {
    throw new ContractValidationError(`${label} must be a bounded nonnegative portable integer`);
  }
  return value;
}

function fixedOpaqueBytes(value: unknown, label: string): { reference: OpaqueContractReference; bytes: Uint8Array } {
  if (typeof value !== 'string' || value.length !== 46) {
    throw new ContractValidationError(`${label} must contain 32 canonical nonzero bytes`);
  }
  const reference = parseOpaqueContractReference(value, label);
  const encoded = reference.slice(3);
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
  let nonzero = false;
  for (const byte of bytes) {
    roundTrip += String.fromCharCode(byte);
    nonzero ||= byte !== 0;
  }
  const canonical = btoa(roundTrip).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/g, '');
  if (bytes.length !== 32 || canonical !== encoded || !nonzero) {
    throw new ContractValidationError(`${label} must contain 32 canonical nonzero bytes`);
  }
  return { reference, bytes };
}

function bytesHex(bytes: Uint8Array): string {
  let value = '';
  for (const byte of bytes) value += byte.toString(16).padStart(2, '0');
  return value;
}

function familyHint(value: unknown): string | null {
  if (value === null) return null;
  if (
    typeof value !== 'string' ||
    value.length === 0 ||
    value.length > MAX_FAMILY_HINT_BYTES ||
    !/^[a-z0-9][a-z0-9._-]*$/.test(value) ||
    UTF8_ENCODER.encode(value).byteLength > MAX_FAMILY_HINT_BYTES
  ) {
    throw new ContractValidationError('unknown-evidence family hint is not a bounded machine identifier');
  }
  return value;
}

function valueKind(value: unknown, label: string): SanitizedJsonValueKind {
  if (
    value !== 'null' &&
    value !== 'boolean' &&
    value !== 'number' &&
    value !== 'string' &&
    value !== 'array' &&
    value !== 'object'
  ) {
    throw new ContractValidationError(`${label} is not a closed JSON value kind`);
  }
  return value;
}

function parseSanitizedExcerpt(
  value: unknown,
  expectedBytes: number,
  expectedHash: string,
): SanitizedUnknownEvidenceExcerpt {
  const tagged = record(value, 'unknown-evidence sanitized excerpt');
  if (tagged.kind === 'json_object') {
    const input = exactRecord(
      tagged,
      ['bytes', 'hash', 'kind', 'members', 'shape', 'truncated'],
      'sanitized object excerpt',
    );
    const members = nonnegativeSafeInteger(input.members, 'sanitized object member count', expectedBytes);
    if (!Array.isArray(input.shape) || input.shape.length !== Math.min(members, MAX_DIAGNOSTIC_SHAPE_ITEMS)) {
      throw new ContractValidationError('sanitized object shape does not match its member count');
    }
    const shape = input.shape.map((entry) => {
      const item = exactRecord(entry, ['key_hash', 'value_kind'], 'sanitized object shape item');
      if (typeof item.key_hash !== 'string' || !/^[0-9a-f]{12}$/.test(item.key_hash)) {
        throw new ContractValidationError('sanitized object key hash is invalid');
      }
      return { key_hash: item.key_hash, value_kind: valueKind(item.value_kind, 'sanitized object value kind') };
    });
    if (input.truncated !== members > MAX_DIAGNOSTIC_SHAPE_ITEMS) {
      throw new ContractValidationError('sanitized object truncation flag is invalid');
    }
    validateExcerptCommon(input, expectedBytes, expectedHash);
    return {
      kind: 'json_object',
      bytes: expectedBytes,
      hash: expectedHash,
      members,
      shape,
      truncated: input.truncated,
    };
  }
  if (tagged.kind === 'json_array') {
    const input = exactRecord(
      tagged,
      ['bytes', 'hash', 'item_kinds', 'items', 'kind', 'truncated'],
      'sanitized array excerpt',
    );
    const items = nonnegativeSafeInteger(input.items, 'sanitized array item count', expectedBytes);
    if (!Array.isArray(input.item_kinds) || input.item_kinds.length !== Math.min(items, MAX_DIAGNOSTIC_SHAPE_ITEMS)) {
      throw new ContractValidationError('sanitized array shape does not match its item count');
    }
    const itemKinds = input.item_kinds.map((item) => valueKind(item, 'sanitized array item kind'));
    if (input.truncated !== items > MAX_DIAGNOSTIC_SHAPE_ITEMS) {
      throw new ContractValidationError('sanitized array truncation flag is invalid');
    }
    validateExcerptCommon(input, expectedBytes, expectedHash);
    return {
      kind: 'json_array',
      bytes: expectedBytes,
      hash: expectedHash,
      items,
      item_kinds: itemKinds,
      truncated: input.truncated,
    };
  }
  if (
    tagged.kind === 'null' ||
    tagged.kind === 'boolean' ||
    tagged.kind === 'number' ||
    tagged.kind === 'string' ||
    tagged.kind === 'opaque'
  ) {
    const input = exactRecord(tagged, ['bytes', 'hash', 'kind'], 'sanitized scalar excerpt');
    validateExcerptCommon(input, expectedBytes, expectedHash);
    return { kind: tagged.kind, bytes: expectedBytes, hash: expectedHash };
  }
  throw new ContractValidationError('unknown-evidence sanitized excerpt kind is invalid');
}

function validateExcerptCommon(input: UnknownRecord, expectedBytes: number, expectedHash: string): void {
  if (input.bytes !== expectedBytes || input.hash !== expectedHash) {
    throw new ContractValidationError('unknown-evidence sanitized excerpt does not match its evidence');
  }
}

function parseSample(value: unknown): UnknownEvidenceSample {
  const input = exactRecord(
    value,
    ['source_record_id', 'family_hint', 'observed_bytes', 'payload_digest', 'sanitized_excerpt'],
    'unknown-evidence sample',
  );
  const sourceRecordId = fixedOpaqueBytes(input.source_record_id, 'unknown-evidence source record id').reference;
  const observedBytes = nonnegativeSafeInteger(
    input.observed_bytes,
    'unknown-evidence observed bytes',
    MAX_UNKNOWN_RAW_PAYLOAD_BYTES,
  );
  const payload = fixedOpaqueBytes(input.payload_digest, 'unknown-evidence payload digest');
  return {
    source_record_id: sourceRecordId,
    family_hint: familyHint(input.family_hint),
    observed_bytes: observedBytes,
    payload_digest: payload.reference,
    sanitized_excerpt: parseSanitizedExcerpt(input.sanitized_excerpt, observedBytes, bytesHex(payload.bytes)),
  };
}

function parseSamples(value: unknown): UnknownEvidenceSample[] {
  if (!Array.isArray(value) || value.length > MAX_UNKNOWN_EVIDENCE_SAMPLES) {
    throw new ContractValidationError('unknown-evidence samples exceed the portable bound');
  }
  return value.map(parseSample);
}

function validateSnapshotValues(
  completeCount: number,
  completeObservedBytes: number,
  aggregateDigest: OpaqueContractReference,
  samples: UnknownEvidenceSample[],
): void {
  if (samples.length !== Math.min(completeCount, MAX_UNKNOWN_EVIDENCE_SAMPLES)) {
    throw new ContractValidationError('unknown-evidence sample count does not match the bounded policy');
  }
  const identities = new Set<string>();
  let sampledBytes = 0;
  for (const sample of samples) {
    if (identities.has(sample.source_record_id)) {
      throw new ContractValidationError('unknown-evidence samples contain duplicate source identity');
    }
    identities.add(sample.source_record_id);
    sampledBytes += sample.observed_bytes;
    if (!Number.isSafeInteger(sampledBytes)) {
      throw new ContractValidationError('unknown-evidence sample byte total exceeds the portable range');
    }
  }
  if (
    sampledBytes > completeObservedBytes ||
    (completeCount <= MAX_UNKNOWN_EVIDENCE_SAMPLES && sampledBytes !== completeObservedBytes)
  ) {
    throw new ContractValidationError('unknown-evidence sample bytes do not match complete totals');
  }
  fixedOpaqueBytes(aggregateDigest, 'unknown-evidence aggregate digest');
}

function samplesEqual(left: UnknownEvidenceSample[], right: UnknownEvidenceSample[]): boolean {
  return (
    left.length === right.length &&
    left.every(
      (sample, index) =>
        sample.source_record_id === right[index]!.source_record_id &&
        sample.family_hint === right[index]!.family_hint &&
        sample.observed_bytes === right[index]!.observed_bytes &&
        sample.payload_digest === right[index]!.payload_digest &&
        JSON.stringify(sample.sanitized_excerpt) === JSON.stringify(right[index]!.sanitized_excerpt),
    )
  );
}

export function parseUnknownEvidenceSnapshotContext(value: unknown): UnknownEvidenceSnapshotContext {
  const input = exactRecord(
    value,
    ['expected_complete_count', 'expected_complete_observed_bytes', 'expected_aggregate_digest', 'expected_samples'],
    'unknown-evidence snapshot context',
  );
  const expectedCompleteCount = nonnegativeSafeInteger(
    input.expected_complete_count,
    'expected unknown-evidence complete count',
    MAX_UNKNOWN_EVIDENCE_OCCURRENCES,
  );
  const expectedCompleteObservedBytes = nonnegativeSafeInteger(
    input.expected_complete_observed_bytes,
    'expected unknown-evidence observed bytes',
  );
  const expectedAggregateDigest = fixedOpaqueBytes(
    input.expected_aggregate_digest,
    'expected unknown-evidence aggregate digest',
  ).reference;
  const expectedSamples = parseSamples(input.expected_samples);
  validateSnapshotValues(
    expectedCompleteCount,
    expectedCompleteObservedBytes,
    expectedAggregateDigest,
    expectedSamples,
  );
  return {
    expected_complete_count: expectedCompleteCount,
    expected_complete_observed_bytes: expectedCompleteObservedBytes,
    expected_aggregate_digest: expectedAggregateDigest,
    expected_samples: expectedSamples,
  };
}

export function parseUnknownEvidenceSnapshot(value: unknown, contextInput: unknown): UnknownEvidenceSnapshot {
  const context = parseUnknownEvidenceSnapshotContext(contextInput);
  const input = exactRecord(
    value,
    [
      'unknown_evidence_snapshot_contract_version',
      'unknown_evidence_aggregate_contract_version',
      'complete_count',
      'complete_observed_bytes',
      'aggregate_digest',
      'samples',
    ],
    'unknown-evidence snapshot',
  );
  if (
    input.unknown_evidence_snapshot_contract_version !== UNKNOWN_EVIDENCE_SNAPSHOT_CONTRACT_VERSION ||
    input.unknown_evidence_aggregate_contract_version !== UNKNOWN_EVIDENCE_AGGREGATE_CONTRACT_VERSION
  ) {
    throw new ContractValidationError('unsupported unknown-evidence snapshot contract version');
  }
  const completeCount = nonnegativeSafeInteger(
    input.complete_count,
    'unknown-evidence complete count',
    MAX_UNKNOWN_EVIDENCE_OCCURRENCES,
  );
  const completeObservedBytes = nonnegativeSafeInteger(
    input.complete_observed_bytes,
    'unknown-evidence observed bytes',
  );
  const aggregateDigest = fixedOpaqueBytes(input.aggregate_digest, 'unknown-evidence aggregate digest').reference;
  const samples = parseSamples(input.samples);
  validateSnapshotValues(completeCount, completeObservedBytes, aggregateDigest, samples);
  if (
    completeCount !== context.expected_complete_count ||
    completeObservedBytes !== context.expected_complete_observed_bytes ||
    aggregateDigest !== context.expected_aggregate_digest ||
    !samplesEqual(samples, context.expected_samples)
  ) {
    throw new ContractValidationError('unknown-evidence snapshot does not match caller-held reducer state');
  }
  return {
    unknown_evidence_snapshot_contract_version: UNKNOWN_EVIDENCE_SNAPSHOT_CONTRACT_VERSION,
    unknown_evidence_aggregate_contract_version: UNKNOWN_EVIDENCE_AGGREGATE_CONTRACT_VERSION,
    complete_count: completeCount,
    complete_observed_bytes: completeObservedBytes,
    aggregate_digest: aggregateDigest,
    samples,
  };
}
