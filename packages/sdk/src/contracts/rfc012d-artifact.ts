/** Strict RFC 012D bounded-artifact request/result contract.
 *
 * Context is issued by a trusted native attachment and contains no locator or
 * source-access authority. V1 always withholds the native locator. Rust
 * verifies inline SHA-256 before producing/consuming the wire; this portable
 * parser independently enforces request binding, shape, bounds, canonical
 * base64, and content length without claiming to authorize or perform a read.
 */

import { ContractValidationError, parseOpaqueContractReference, type OpaqueContractReference } from './rfc012a.js';
import { parseObservationContractSelectionForExpected, type ObservationContractSelection } from './rfc012d.js';
import { parseScopedUsageRoot, type ScopedUsageRoot } from './rfc012d-usage-envelope.js';

export const SCOPED_ARTIFACT_CONTRACT_VERSION = 1 as const;

const MAX_IDENTIFIER_BYTES = 128;
const MAX_ARTIFACT_REQUEST_BYTES = 2_147_483_648;
const MAX_INLINE_ARTIFACT_BYTES = 8 * 1024 * 1024;
const MAX_INLINE_BASE64_BYTES = 11_184_812;
const UTF8_ENCODER = new TextEncoder();
type UnknownRecord = Record<string, unknown>;

export type ScopedArtifactContentPolicy = 'metadata_only' | 'hash_only' | 'inline';
export type ScopedArtifactUnavailableReason =
  | 'out_of_scope'
  | 'denied'
  | 'missing'
  | 'over_limit'
  | 'changed_generation'
  | 'unsupported'
  | 'malformed'
  | 'unstable';

export interface ScopedArtifactRoot extends ScopedUsageRoot {
  adapter_id: string;
  source_instance_key: OpaqueContractReference;
}

export interface ScopedArtifactReadContext {
  contract_selection: ObservationContractSelection;
  root: ScopedArtifactRoot;
  attachment_ref: OpaqueContractReference;
  request_id: OpaqueContractReference;
  artifact_key: OpaqueContractReference;
  artifact_kind: string;
  expected_generation: number | null;
  max_bytes: number;
  content_policy: ScopedArtifactContentPolicy;
}

export interface ScopedAvailableArtifact {
  kind: 'available';
  generation: number;
  provenance_ref: OpaqueContractReference;
  size_bytes: number;
  completeness: 'complete';
  content_hash: string | null;
  content_base64: string | null;
}

export interface ScopedUnavailableArtifact {
  kind: 'unavailable';
  reason: ScopedArtifactUnavailableReason;
  observed_generation: number | null;
  observed_bytes: number | null;
  provenance_ref: OpaqueContractReference | null;
  completeness: 'unavailable';
}

export interface ScopedObservedArtifact {
  scoped_artifact_contract_version: typeof SCOPED_ARTIFACT_CONTRACT_VERSION;
  request: ScopedArtifactReadContext;
  locator_disclosure: 'withheld';
  outcome: ScopedAvailableArtifact | ScopedUnavailableArtifact;
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

function boundedCanonicalString(value: unknown, label: string, maxBytes: number): string {
  if (typeof value !== 'string' || value.length === 0 || value.length > maxBytes || value.trim() !== value) {
    throw new ContractValidationError(`${label} is not a bounded canonical string`);
  }
  if (UTF8_ENCODER.encode(value).byteLength > maxBytes) {
    throw new ContractValidationError(`${label} exceeds ${maxBytes} UTF-8 bytes`);
  }
  return value;
}

function identifier(value: unknown, label: string): string {
  const parsed = boundedCanonicalString(value, label, MAX_IDENTIFIER_BYTES);
  if (!/^[a-z][a-z0-9._-]*$/.test(parsed)) {
    throw new ContractValidationError(`${label} is not a canonical identifier`);
  }
  return parsed;
}

function adapterId(value: unknown): string {
  return boundedCanonicalString(value, 'artifact root adapter id', MAX_IDENTIFIER_BYTES);
}

function fixedOpaque(value: unknown, label: string): OpaqueContractReference {
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
  let allZero = true;
  for (let index = 0; index < binary.length; index += 1) {
    const byte = binary.charCodeAt(index);
    allZero = allZero && byte === 0;
    roundTrip += String.fromCharCode(byte);
  }
  const canonical = btoa(roundTrip).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/g, '');
  if (binary.length !== 32 || canonical !== encoded || allZero) {
    throw new ContractValidationError(`${label} must contain exactly 32 canonical nonzero bytes`);
  }
  return parsed;
}

function positiveSafeInteger(value: unknown, label: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value <= 0) {
    throw new ContractValidationError(`${label} must be a positive portable integer`);
  }
  return value;
}

function nonNegativeSafeInteger(value: unknown, label: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0) {
    throw new ContractValidationError(`${label} must be a nonnegative portable integer`);
  }
  return value;
}

function optionalPositive(value: unknown, label: string): number | null {
  return value === null ? null : positiveSafeInteger(value, label);
}

function parseContentPolicy(value: unknown): ScopedArtifactContentPolicy {
  if (value !== 'metadata_only' && value !== 'hash_only' && value !== 'inline') {
    throw new ContractValidationError('artifact content policy is unsupported');
  }
  return value;
}

function parseRoot(value: unknown): ScopedArtifactRoot {
  const input = exactRecord(
    value,
    ['adapter_id', 'source_instance_key', 'session_ref', 'session_key', 'root_actor_run_key', 'native_session_claim'],
    'scoped artifact root',
  );
  const common = parseScopedUsageRoot({
    session_ref: input.session_ref,
    session_key: input.session_key,
    root_actor_run_key: input.root_actor_run_key,
    native_session_claim: input.native_session_claim,
  });
  fixedOpaque(common.session_key, 'artifact root session key');
  fixedOpaque(common.session_ref.entity_key, 'artifact root external session key');
  fixedOpaque(common.root_actor_run_key, 'artifact root actor run key');
  if (common.session_key !== common.session_ref.entity_key) {
    throw new ContractValidationError('artifact root session reference and key differ');
  }
  return {
    adapter_id: adapterId(input.adapter_id),
    source_instance_key: fixedOpaque(input.source_instance_key, 'artifact root source instance key'),
    ...common,
  };
}

function parseContext(value: unknown, expectedSelection?: ObservationContractSelection): ScopedArtifactReadContext {
  const input = exactRecord(
    value,
    [
      'contract_selection',
      'root',
      'attachment_ref',
      'request_id',
      'artifact_key',
      'artifact_kind',
      'expected_generation',
      'max_bytes',
      'content_policy',
    ],
    'scoped artifact context',
  );
  const contractSelection = parseObservationContractSelectionForExpected(
    input.contract_selection,
    expectedSelection ?? input.contract_selection,
  );
  const contentPolicy = parseContentPolicy(input.content_policy);
  const maxBytes = positiveSafeInteger(input.max_bytes, 'artifact max bytes');
  if (maxBytes > MAX_ARTIFACT_REQUEST_BYTES) {
    throw new ContractValidationError('artifact max bytes exceeds the portable request safety bound');
  }
  if (contentPolicy === 'inline' && maxBytes > MAX_INLINE_ARTIFACT_BYTES) {
    throw new ContractValidationError('inline artifact max bytes exceeds the portable inline safety bound');
  }
  return {
    contract_selection: contractSelection,
    root: parseRoot(input.root),
    attachment_ref: fixedOpaque(input.attachment_ref, 'artifact attachment reference'),
    request_id: fixedOpaque(input.request_id, 'artifact request id'),
    artifact_key: fixedOpaque(input.artifact_key, 'artifact key'),
    artifact_kind: identifier(input.artifact_kind, 'artifact kind'),
    expected_generation: optionalPositive(input.expected_generation, 'artifact expected generation'),
    max_bytes: maxBytes,
    content_policy: contentPolicy,
  };
}

export function parseScopedArtifactReadContext(value: unknown): ScopedArtifactReadContext {
  return parseContext(value);
}

function sha256(value: unknown, label: string): string {
  if (typeof value !== 'string' || !/^sha256:[0-9a-f]{64}$/.test(value)) {
    throw new ContractValidationError(`${label} must be a canonical SHA-256 digest`);
  }
  return value;
}

function canonicalBase64(value: unknown, expectedBytes: number): string {
  if (typeof value !== 'string' || value.length > MAX_INLINE_BASE64_BYTES) {
    throw new ContractValidationError('inline artifact content exceeds the pre-decode base64 bound');
  }
  let binary: string;
  try {
    binary = atob(value);
  } catch {
    throw new ContractValidationError('inline artifact content is not base64');
  }
  if (binary.length !== expectedBytes || btoa(binary) !== value) {
    throw new ContractValidationError('inline artifact content is noncanonical or has the wrong size');
  }
  return value;
}

function parseAvailable(value: unknown, request: ScopedArtifactReadContext): ScopedAvailableArtifact {
  const input = exactRecord(
    value,
    ['kind', 'generation', 'provenance_ref', 'size_bytes', 'completeness', 'content_hash', 'content_base64'],
    'available scoped artifact',
  );
  if (input.kind !== 'available' || input.completeness !== 'complete') {
    throw new ContractValidationError('available artifact has an invalid kind or completeness');
  }
  const generation = positiveSafeInteger(input.generation, 'artifact generation');
  if (request.expected_generation !== null && generation !== request.expected_generation) {
    throw new ContractValidationError('available artifact generation differs from the expected generation');
  }
  const sizeBytes = nonNegativeSafeInteger(input.size_bytes, 'artifact size');
  if (sizeBytes > request.max_bytes) {
    throw new ContractValidationError('available artifact exceeds the request byte bound');
  }
  let contentHash: string | null;
  let contentBase64: string | null;
  switch (request.content_policy) {
    case 'metadata_only':
      if (input.content_hash !== null || input.content_base64 !== null) {
        throw new ContractValidationError('metadata-only artifact cannot disclose hash or content');
      }
      contentHash = null;
      contentBase64 = null;
      break;
    case 'hash_only':
      if (input.content_base64 !== null) {
        throw new ContractValidationError('hash-only artifact cannot disclose inline content');
      }
      contentHash = sha256(input.content_hash, 'artifact content hash');
      contentBase64 = null;
      break;
    case 'inline':
      if (sizeBytes > MAX_INLINE_ARTIFACT_BYTES) {
        throw new ContractValidationError('inline artifact exceeds the portable inline safety bound');
      }
      contentHash = sha256(input.content_hash, 'artifact content hash');
      contentBase64 = canonicalBase64(input.content_base64, sizeBytes);
      break;
  }
  return {
    kind: 'available',
    generation,
    provenance_ref: fixedOpaque(input.provenance_ref, 'artifact provenance reference'),
    size_bytes: sizeBytes,
    completeness: 'complete',
    content_hash: contentHash,
    content_base64: contentBase64,
  };
}

function parseUnavailable(value: unknown, request: ScopedArtifactReadContext): ScopedUnavailableArtifact {
  const input = exactRecord(
    value,
    ['kind', 'reason', 'observed_generation', 'observed_bytes', 'provenance_ref', 'completeness'],
    'unavailable scoped artifact',
  );
  if (input.kind !== 'unavailable' || input.completeness !== 'unavailable') {
    throw new ContractValidationError('unavailable artifact has an invalid kind or completeness');
  }
  const reasons: readonly ScopedArtifactUnavailableReason[] = [
    'out_of_scope',
    'denied',
    'missing',
    'over_limit',
    'changed_generation',
    'unsupported',
    'malformed',
    'unstable',
  ];
  if (!reasons.includes(input.reason as ScopedArtifactUnavailableReason)) {
    throw new ContractValidationError('artifact unavailable reason is unsupported');
  }
  const reason = input.reason as ScopedArtifactUnavailableReason;
  const observedGeneration = optionalPositive(input.observed_generation, 'observed artifact generation');
  const observedBytes =
    input.observed_bytes === null ? null : nonNegativeSafeInteger(input.observed_bytes, 'observed bytes');
  const provenanceRef =
    input.provenance_ref === null ? null : fixedOpaque(input.provenance_ref, 'artifact provenance reference');
  if ((observedGeneration === null) !== (provenanceRef === null)) {
    throw new ContractValidationError('artifact provenance and observed generation must be present together');
  }
  if (reason === 'changed_generation') {
    if (
      request.expected_generation === null ||
      observedGeneration === null ||
      observedGeneration === request.expected_generation ||
      observedBytes !== null
    ) {
      throw new ContractValidationError('changed-generation evidence is inconsistent');
    }
  } else if (reason === 'over_limit') {
    if (observedBytes === null || observedBytes <= request.max_bytes) {
      throw new ContractValidationError('over-limit evidence is inconsistent');
    }
  } else if (observedBytes !== null) {
    throw new ContractValidationError('only over-limit may report observed artifact bytes');
  }
  return {
    kind: 'unavailable',
    reason,
    observed_generation: observedGeneration,
    observed_bytes: observedBytes,
    provenance_ref: provenanceRef,
    completeness: 'unavailable',
  };
}

function canonicalEqual(left: unknown, right: unknown): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

export function parseScopedObservedArtifact(value: unknown, expectedContextInput: unknown): ScopedObservedArtifact {
  const expected = parseScopedArtifactReadContext(expectedContextInput);
  const input = exactRecord(
    value,
    ['scoped_artifact_contract_version', 'request', 'locator_disclosure', 'outcome'],
    'scoped observed artifact',
  );
  if (input.scoped_artifact_contract_version !== SCOPED_ARTIFACT_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported scoped artifact contract version');
  }
  if (input.locator_disclosure !== 'withheld') {
    throw new ContractValidationError('scoped artifact v1 must withhold the native locator');
  }
  const request = parseContext(input.request, expected.contract_selection);
  if (!canonicalEqual(request, expected)) {
    throw new ContractValidationError('scoped artifact response does not match caller-held request context');
  }
  const outcomeRecord = record(input.outcome, 'scoped artifact outcome');
  const outcome =
    outcomeRecord.kind === 'available'
      ? parseAvailable(input.outcome, request)
      : parseUnavailable(input.outcome, request);
  return {
    scoped_artifact_contract_version: SCOPED_ARTIFACT_CONTRACT_VERSION,
    request,
    locator_disclosure: 'withheld',
    outcome,
  };
}
