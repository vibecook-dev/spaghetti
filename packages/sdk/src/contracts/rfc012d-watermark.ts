/** Strict contextual RFC 012D completed-poll watermarks.
 *
 * Rust alone mints the attachment-bound consumer context. Portable consumers
 * may validate the matching bootstrap/valid watermark, but this contract does
 * not authorize source access or expose an observer transport.
 */

import {
  ContractValidationError,
  parseOpaqueContractReference,
  parseSourceCoverageSet,
  type CoverageError,
  type OpaqueContractReference,
  type SourceCoverageSet,
} from './rfc012a.js';
import {
  parseUnknownEvidenceSnapshot,
  parseUnknownEvidenceSnapshotContext,
  type UnknownEvidenceSnapshot,
  type UnknownEvidenceSnapshotContext,
} from './rfc012c-unknown-evidence.js';
import {
  parseScopedArtifactAvailabilityContext,
  parseScopedArtifactAvailabilitySnapshot,
  type ScopedArtifactAvailabilityContext,
  type ScopedArtifactAvailabilitySnapshot,
} from './rfc012d-artifact-availability.js';
import {
  parseScopedCapabilitySnapshot,
  parseScopedCapabilitySnapshotContext,
  type ScopedCapabilitySnapshot,
  type ScopedCapabilitySnapshotContext,
} from './rfc012d-capability-snapshot.js';
import {
  parseScopedScopeCoverage,
  parseScopedScopeCoverageContext,
  type ScopedScopeCoverage,
  type ScopedScopeCoverageContext,
} from './rfc012d-scope-coverage.js';
import { parseScopedUsageRoot, type ScopedUsageRoot } from './rfc012d-usage-envelope.js';
import { parseObservationContractSelectionForExpected, type ObservationContractSelection } from './rfc012d.js';

export const SCOPED_OBSERVATION_WATERMARK_CONTRACT_VERSION = 2 as const;

const MAX_SOURCE_COVERAGE_SETS = 64;
const MAX_COVERAGE_ERRORS_PER_SET = 4_096;
const MAX_EXPLICIT_OBJECT_ERRORS = MAX_SOURCE_COVERAGE_SETS * MAX_COVERAGE_ERRORS_PER_SET;
type UnknownRecord = Record<string, unknown>;

export interface ScopedObservationWatermarkQueueState {
  scope_epoch: number;
  offered_through_sequence: number;
  delivered_through_sequence: number;
  continuity: 'bootstrap' | 'valid';
  queued_semantic_events: number;
  queued_retained_native_bytes: number;
  queued_source_control_items: number;
}

export interface ScopedObservationWatermarkContext {
  contract_selection: ObservationContractSelection;
  adapter_id: string;
  root: ScopedUsageRoot;
  expected_scope_epoch: number;
  expected_offered_through_sequence: number;
  expected_source_coverage: SourceCoverageSet[];
  expected_explicit_object_errors: CoverageError[];
  expected_queue_state: ScopedObservationWatermarkQueueState;
  capability_context: ScopedCapabilitySnapshotContext;
  scope_coverage_context: ScopedScopeCoverageContext;
  artifact_availability_context: ScopedArtifactAvailabilityContext;
  unknown_evidence_context: UnknownEvidenceSnapshotContext;
}

export interface ScopedObservationWatermark {
  scoped_observation_watermark_contract_version: typeof SCOPED_OBSERVATION_WATERMARK_CONTRACT_VERSION;
  contract_selection: ObservationContractSelection;
  root: ScopedUsageRoot;
  scope_epoch: number;
  offered_through_sequence: number;
  source_coverage: SourceCoverageSet[];
  capability_snapshot: ScopedCapabilitySnapshot;
  scope_coverage: ScopedScopeCoverage;
  explicit_object_errors: CoverageError[];
  artifact_availability: ScopedArtifactAvailabilitySnapshot;
  unknown_evidence: UnknownEvidenceSnapshot;
  queue_state: ScopedObservationWatermarkQueueState;
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

function safeInteger(value: unknown, label: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value)) {
    throw new ContractValidationError(`${label} must be a portable integer`);
  }
  return value;
}

function positiveInteger(value: unknown, label: string): number {
  const parsed = safeInteger(value, label);
  if (parsed <= 0) throw new ContractValidationError(`${label} must be positive`);
  return parsed;
}

function nonnegativeInteger(value: unknown, label: string): number {
  const parsed = safeInteger(value, label);
  if (parsed < 0) throw new ContractValidationError(`${label} must be non-negative`);
  return parsed;
}

function fixedNonzeroOpaque(value: unknown, label: string): OpaqueContractReference {
  const parsed = parseOpaqueContractReference(value, label);
  const encoded = parsed.slice(3);
  if (encoded.length !== 43 || encoded.includes('=') || !/^[A-Za-z0-9_-]+$/.test(encoded)) {
    throw new ContractValidationError(`${label} must contain 32 canonical nonzero bytes`);
  }
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

function sameValue(left: unknown, right: unknown): boolean {
  return canonicalJson(left) === canonicalJson(right);
}

function parseQueueState(value: unknown): ScopedObservationWatermarkQueueState {
  const input = exactRecord(
    value,
    [
      'scope_epoch',
      'offered_through_sequence',
      'delivered_through_sequence',
      'continuity',
      'queued_semantic_events',
      'queued_retained_native_bytes',
      'queued_source_control_items',
    ],
    'watermark queue state',
  );
  if (input.continuity !== 'bootstrap' && input.continuity !== 'valid') {
    throw new ContractValidationError('watermark queue continuity must be bootstrap or valid');
  }
  const result: ScopedObservationWatermarkQueueState = {
    scope_epoch: positiveInteger(input.scope_epoch, 'watermark queue scope_epoch'),
    offered_through_sequence: nonnegativeInteger(input.offered_through_sequence, 'watermark offered sequence'),
    delivered_through_sequence: nonnegativeInteger(input.delivered_through_sequence, 'watermark delivered sequence'),
    continuity: input.continuity,
    queued_semantic_events: nonnegativeInteger(input.queued_semantic_events, 'queued semantic events'),
    queued_retained_native_bytes: nonnegativeInteger(
      input.queued_retained_native_bytes,
      'queued retained-native bytes',
    ),
    queued_source_control_items: nonnegativeInteger(input.queued_source_control_items, 'queued source-control items'),
  };
  const queuedItems = result.queued_semantic_events + result.queued_source_control_items;
  if (
    !Number.isSafeInteger(queuedItems) ||
    result.delivered_through_sequence > result.offered_through_sequence ||
    result.offered_through_sequence - result.delivered_through_sequence !== queuedItems
  ) {
    throw new ContractValidationError('watermark queue counts do not match its offered boundary');
  }
  return result;
}

function parseExplicitError(value: unknown): CoverageError {
  const input = record(value, 'watermark explicit object error');
  for (const key of Object.keys(input)) {
    if (key !== 'stream_key' && key !== 'object_key' && key !== 'code') {
      throw new ContractValidationError(`watermark explicit object error contains unknown field ${key}`);
    }
  }
  if (
    !Object.hasOwn(input, 'code') ||
    typeof input.code !== 'string' ||
    input.code.length === 0 ||
    input.code.length > 64 ||
    !/^[a-z][a-z0-9_]*$/.test(input.code)
  ) {
    throw new ContractValidationError('watermark explicit object error code is not a bounded machine code');
  }
  const result: CoverageError = { code: input.code };
  if (Object.hasOwn(input, 'stream_key')) {
    result.stream_key = fixedNonzeroOpaque(input.stream_key, 'watermark error stream key');
  }
  if (Object.hasOwn(input, 'object_key')) {
    if (result.stream_key === undefined) {
      throw new ContractValidationError('watermark error object key requires a stream key');
    }
    result.object_key = fixedNonzeroOpaque(input.object_key, 'watermark error object key');
  }
  return result;
}

function parseSourceCoverage(value: unknown, label: string): SourceCoverageSet[] {
  if (!Array.isArray(value) || value.length === 0 || value.length > MAX_SOURCE_COVERAGE_SETS) {
    throw new ContractValidationError(`${label} must be a bounded non-empty array`);
  }
  const parsed = value.map(parseSourceCoverageSet);
  if (parsed.some((coverage) => coverage.explicit_errors.length > MAX_COVERAGE_ERRORS_PER_SET)) {
    throw new ContractValidationError(`${label} contains too many explicit errors`);
  }
  return parsed;
}

function canonicalExplicitErrors(sourceCoverage: SourceCoverageSet[]): CoverageError[] {
  const count = sourceCoverage.reduce((total, coverage) => total + coverage.explicit_errors.length, 0);
  if (!Number.isSafeInteger(count) || count > MAX_EXPLICIT_OBJECT_ERRORS) {
    throw new ContractValidationError('watermark explicit errors exceed the portable bound');
  }
  const keyed = new Map<string, CoverageError>();
  for (const coverage of sourceCoverage) {
    for (const error of coverage.explicit_errors) {
      const key = JSON.stringify([error.stream_key ?? null, error.object_key ?? null, error.code]);
      keyed.set(key, error);
    }
  }
  return [...keyed.values()].sort((left, right) => {
    const leftKey = [left.stream_key ?? '', left.object_key ?? '', left.code];
    const rightKey = [right.stream_key ?? '', right.object_key ?? '', right.code];
    for (let index = 0; index < leftKey.length; index += 1) {
      if (leftKey[index]! < rightKey[index]!) return -1;
      if (leftKey[index]! > rightKey[index]!) return 1;
    }
    return 0;
  });
}

function parseExplicitErrors(value: unknown, sourceCoverage: SourceCoverageSet[]): CoverageError[] {
  const expected = canonicalExplicitErrors(sourceCoverage);
  if (!Array.isArray(value) || value.length !== expected.length || value.length > MAX_EXPLICIT_OBJECT_ERRORS) {
    throw new ContractValidationError('watermark explicit errors are not the canonical coverage error set');
  }
  const parsed = value.map(parseExplicitError);
  if (!sameValue(parsed, expected)) {
    throw new ContractValidationError('watermark explicit errors are not the canonical coverage error set');
  }
  return parsed;
}

function validateCoverageAuthority(
  coverage: SourceCoverageSet[],
  context: {
    adapter_id: string;
    root: ScopedUsageRoot;
    capability_context: ScopedCapabilitySnapshotContext;
    scope_coverage_context: ScopedScopeCoverageContext;
    contract_selection: ObservationContractSelection;
  },
): void {
  const selectedFamilies = Object.entries(context.contract_selection.contract_versions.fact_family_versions);
  let decodeCount = 0;
  const observed = new Set<string>();
  for (const set of coverage) {
    if (
      set.scope.adapter_id !== context.adapter_id ||
      set.scope.source_instance_key !== context.scope_coverage_context.root.source_instance_key ||
      set.scope.root_entity_key !== context.root.session_key ||
      set.scope.support_release_id !== context.capability_context.support_release_id
    ) {
      throw new ContractValidationError('watermark source coverage is not bound to its scoped authority');
    }
    if (set.coverage_domain.kind === 'decode') {
      decodeCount += 1;
    } else if (set.coverage_domain.kind === 'fact_family') {
      const expectedVersion =
        context.contract_selection.contract_versions.fact_family_versions[set.coverage_domain.family];
      if (expectedVersion !== set.coverage_domain.version) {
        throw new ContractValidationError('watermark contains unselected fact-family coverage');
      }
      const key = `${set.coverage_domain.family}\0${set.coverage_domain.version}`;
      if (observed.has(key)) throw new ContractValidationError('watermark duplicates fact-family coverage');
      observed.add(key);
    } else {
      throw new ContractValidationError('watermark cannot contain projection-pack coverage');
    }
  }
  if (
    decodeCount !== 1 ||
    coverage.length !== selectedFamilies.length + 1 ||
    selectedFamilies.some(([family, version]) => !observed.has(`${family}\0${version}`))
  ) {
    throw new ContractValidationError('watermark does not contain exactly the selected coverage domains');
  }
  const decode = coverage.find((set) => set.coverage_domain.kind === 'decode');
  if (!sameValue(decode, context.scope_coverage_context.decode_coverage)) {
    throw new ContractValidationError('watermark Decode coverage does not match its scope context');
  }
}

export function parseScopedObservationWatermarkContext(value: unknown): ScopedObservationWatermarkContext {
  const input = exactRecord(
    value,
    [
      'contract_selection',
      'adapter_id',
      'root',
      'expected_scope_epoch',
      'expected_offered_through_sequence',
      'expected_source_coverage',
      'expected_explicit_object_errors',
      'expected_queue_state',
      'capability_context',
      'scope_coverage_context',
      'artifact_availability_context',
      'unknown_evidence_context',
    ],
    'scoped observation watermark context',
  );
  const contractSelection = parseObservationContractSelectionForExpected(
    input.contract_selection,
    input.contract_selection,
  );
  const root = parseScopedUsageRoot(input.root);
  const capabilityContext = parseScopedCapabilitySnapshotContext(input.capability_context);
  const scopeCoverageContext = parseScopedScopeCoverageContext(input.scope_coverage_context);
  const artifactContext = parseScopedArtifactAvailabilityContext(input.artifact_availability_context);
  const unknownEvidenceContext = parseUnknownEvidenceSnapshotContext(input.unknown_evidence_context);
  const sourceCoverage = parseSourceCoverage(input.expected_source_coverage, 'expected watermark source coverage');
  const explicitErrors = parseExplicitErrors(input.expected_explicit_object_errors, sourceCoverage);
  const queueState = parseQueueState(input.expected_queue_state);
  const scopeEpoch = positiveInteger(input.expected_scope_epoch, 'expected watermark scope_epoch');
  const offeredSequence = nonnegativeInteger(
    input.expected_offered_through_sequence,
    'expected watermark offered sequence',
  );
  if (
    typeof input.adapter_id !== 'string' ||
    input.adapter_id !== scopeCoverageContext.root.adapter_id ||
    root.session_key !== scopeCoverageContext.root.session_key ||
    root.session_key !== artifactContext.root_session_key ||
    queueState.scope_epoch !== scopeEpoch ||
    queueState.offered_through_sequence !== offeredSequence ||
    !sameValue(capabilityContext.contract_selection, contractSelection) ||
    !sameValue(artifactContext.contract_selection, contractSelection)
  ) {
    throw new ContractValidationError('watermark context does not describe one scoped authority');
  }
  const result: ScopedObservationWatermarkContext = {
    contract_selection: contractSelection,
    adapter_id: input.adapter_id,
    root,
    expected_scope_epoch: scopeEpoch,
    expected_offered_through_sequence: offeredSequence,
    expected_source_coverage: sourceCoverage,
    expected_explicit_object_errors: explicitErrors,
    expected_queue_state: queueState,
    capability_context: capabilityContext,
    scope_coverage_context: scopeCoverageContext,
    artifact_availability_context: artifactContext,
    unknown_evidence_context: unknownEvidenceContext,
  };
  validateCoverageAuthority(sourceCoverage, result);
  return result;
}

export function parseScopedObservationWatermark(
  value: unknown,
  expectedContextInput: unknown,
): ScopedObservationWatermark {
  const context = parseScopedObservationWatermarkContext(expectedContextInput);
  const input = exactRecord(
    value,
    [
      'scoped_observation_watermark_contract_version',
      'contract_selection',
      'root',
      'scope_epoch',
      'offered_through_sequence',
      'source_coverage',
      'capability_snapshot',
      'scope_coverage',
      'explicit_object_errors',
      'artifact_availability',
      'unknown_evidence',
      'queue_state',
    ],
    'scoped observation watermark',
  );
  if (input.scoped_observation_watermark_contract_version !== SCOPED_OBSERVATION_WATERMARK_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported scoped observation watermark contract version');
  }
  const contractSelection = parseObservationContractSelectionForExpected(
    input.contract_selection,
    context.contract_selection,
  );
  const root = parseScopedUsageRoot(input.root);
  const scopeEpoch = positiveInteger(input.scope_epoch, 'watermark scope_epoch');
  const offeredSequence = nonnegativeInteger(input.offered_through_sequence, 'watermark offered sequence');
  const sourceCoverage = parseSourceCoverage(input.source_coverage, 'watermark source coverage');
  const explicitErrors = parseExplicitErrors(input.explicit_object_errors, sourceCoverage);
  const queueState = parseQueueState(input.queue_state);
  if (
    !sameValue(root, context.root) ||
    scopeEpoch !== context.expected_scope_epoch ||
    offeredSequence !== context.expected_offered_through_sequence ||
    !sameValue(sourceCoverage, context.expected_source_coverage) ||
    !sameValue(explicitErrors, context.expected_explicit_object_errors) ||
    !sameValue(queueState, context.expected_queue_state)
  ) {
    throw new ContractValidationError('watermark does not match caller-held context');
  }
  return {
    scoped_observation_watermark_contract_version: SCOPED_OBSERVATION_WATERMARK_CONTRACT_VERSION,
    contract_selection: contractSelection,
    root,
    scope_epoch: scopeEpoch,
    offered_through_sequence: offeredSequence,
    source_coverage: sourceCoverage,
    capability_snapshot: parseScopedCapabilitySnapshot(input.capability_snapshot, context.capability_context),
    scope_coverage: parseScopedScopeCoverage(input.scope_coverage, context.scope_coverage_context),
    explicit_object_errors: explicitErrors,
    artifact_availability: parseScopedArtifactAvailabilitySnapshot(
      input.artifact_availability,
      context.artifact_availability_context,
    ),
    unknown_evidence: parseUnknownEvidenceSnapshot(input.unknown_evidence, context.unknown_evidence_context),
    queue_state: queueState,
  };
}
