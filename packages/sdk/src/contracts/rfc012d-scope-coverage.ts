/** Strict RFC 012D exact-known-object scope-coverage projection.
 *
 * This is not a watermark, bootstrap barrier, or readiness claim. The parser
 * requires caller-held program/root context and the authoritative RFC 012A
 * Decode set. Source positions and membership revisions remain exclusively on
 * that Decode set; Rust remains authoritative for the BLAKE3 scope revision.
 */

import {
  ContractValidationError,
  parseOpaqueContractReference,
  parseSourceCoverageSet,
  type CoverageSetCompleteness,
  type CoverageStatus,
  type OpaqueContractReference,
  type SourceCoverageSet,
} from './rfc012a.js';

export const SCOPED_SCOPE_COVERAGE_CONTRACT_VERSION = 1 as const;

const MAX_IDENTIFIER_BYTES = 128;
const MAX_UNAVAILABLE_REASON_BYTES = 1_024;
const MAX_SCOPE_COVERAGE_RELATIONS = 500_000;
const UTF8_ENCODER = new TextEncoder();
type UnknownRecord = Record<string, unknown>;
type CoverageAbsenceKind = 'absent' | 'deleted';

export interface ScopedScopeCoverageRoot {
  adapter_id: string;
  source_instance_key: OpaqueContractReference;
  session_key: OpaqueContractReference;
}

export interface ScopedScopeCoverageContext {
  root: ScopedScopeCoverageRoot;
  program_id: string;
  scope_program_digest: string;
  root_relation_id: string;
  declared_relation_ids: string[];
  expected_scope_revision: OpaqueContractReference;
  decode_coverage: SourceCoverageSet;
}

export interface ScopedScopeCoverageSource {
  adapter_id: string;
  source_instance_key: OpaqueContractReference;
  stream_key: OpaqueContractReference;
  object_key: OpaqueContractReference;
}

export type ScopedScopeRelationState =
  | { kind: 'present'; status: CoverageStatus }
  | { kind: 'absent'; absence_kind: CoverageAbsenceKind };

export interface ScopedScopeRelationCoverage {
  relation_id: string;
  scope_root: boolean;
  source: ScopedScopeCoverageSource;
  generation: number;
  state: ScopedScopeRelationState;
  completeness: CoverageSetCompleteness;
}

export interface ScopedScopeCoverage {
  scoped_scope_coverage_contract_version: typeof SCOPED_SCOPE_COVERAGE_CONTRACT_VERSION;
  program_id: string;
  scope_program_digest: string;
  root_relation_id: string;
  scope_revision: OpaqueContractReference;
  relations: ScopedScopeRelationCoverage[];
  completeness: CoverageSetCompleteness;
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

function relationId(value: unknown, label: string): string {
  const parsed = boundedCanonicalString(value, label, MAX_IDENTIFIER_BYTES);
  if (!/^[a-z0-9][a-z0-9._-]*$/.test(parsed)) {
    throw new ContractValidationError(`${label} is not a canonical relation identifier`);
  }
  return parsed;
}

function adapterId(value: unknown): string {
  return boundedCanonicalString(value, 'scope-coverage adapter id', MAX_IDENTIFIER_BYTES);
}

function sha256Digest(value: unknown, label: string): string {
  if (typeof value !== 'string' || !/^sha256:[0-9a-f]{64}$/.test(value) || value === `sha256:${'0'.repeat(64)}`) {
    throw new ContractValidationError(`${label} must be a canonical SHA-256 digest`);
  }
  return value;
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
  for (let index = 0; index < binary.length; index += 1) {
    roundTrip += String.fromCharCode(binary.charCodeAt(index));
  }
  const canonical = btoa(roundTrip).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/g, '');
  if (binary.length !== 32 || canonical !== encoded) {
    throw new ContractValidationError(`${label} must contain exactly 32 canonical bytes`);
  }
  return parsed;
}

function positiveSafeInteger(value: unknown, label: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value <= 0) {
    throw new ContractValidationError(`${label} must be a positive portable integer`);
  }
  return value;
}

function completeness(value: unknown, label: string): CoverageSetCompleteness {
  if (value !== 'complete' && value !== 'partial' && value !== 'unavailable') {
    throw new ContractValidationError(`${label} is unsupported`);
  }
  return value;
}

function mergeCompleteness(left: CoverageSetCompleteness, right: CoverageSetCompleteness): CoverageSetCompleteness {
  if (left === 'unavailable' || right === 'unavailable') return 'unavailable';
  if (left === 'partial' || right === 'partial') return 'partial';
  return 'complete';
}

function parseStatus(value: unknown): CoverageStatus {
  const input = record(value, 'scope relation status');
  switch (input.kind) {
    case 'complete_through':
    case 'exact_snapshot':
    case 'partial':
      exactRecord(value, ['kind'], 'scope relation status');
      return { kind: input.kind };
    case 'unavailable':
      exactRecord(value, ['kind', 'reason'], 'scope relation unavailable status');
      return {
        kind: 'unavailable',
        reason: boundedCanonicalString(input.reason, 'scope relation unavailable reason', MAX_UNAVAILABLE_REASON_BYTES),
      };
    default:
      throw new ContractValidationError('scope relation status is unsupported');
  }
}

function statusEqual(left: CoverageStatus, right: CoverageStatus): boolean {
  return (
    left.kind === right.kind &&
    (left.kind !== 'unavailable' || (right.kind === 'unavailable' && left.reason === right.reason))
  );
}

function parseRoot(value: unknown): ScopedScopeCoverageRoot {
  const input = exactRecord(value, ['adapter_id', 'source_instance_key', 'session_key'], 'scope-coverage root');
  return {
    adapter_id: adapterId(input.adapter_id),
    source_instance_key: fixedOpaque(input.source_instance_key, 'scope-coverage source instance key'),
    session_key: fixedOpaque(input.session_key, 'scope-coverage session key'),
  };
}

function strictlySortedRelationIds(value: unknown): string[] {
  if (!Array.isArray(value) || value.length === 0 || value.length > MAX_SCOPE_COVERAGE_RELATIONS) {
    throw new ContractValidationError('declared scope relations must be a bounded non-empty array');
  }
  const result = value.map((entry) => relationId(entry, 'declared relation id'));
  for (let index = 1; index < result.length; index += 1) {
    if (result[index - 1]! >= result[index]!) {
      throw new ContractValidationError('declared relation ids must be strictly canonical');
    }
  }
  return result;
}

export function parseScopedScopeCoverageContext(value: unknown): ScopedScopeCoverageContext {
  const input = exactRecord(
    value,
    [
      'root',
      'program_id',
      'scope_program_digest',
      'root_relation_id',
      'declared_relation_ids',
      'expected_scope_revision',
      'decode_coverage',
    ],
    'scope-coverage context',
  );
  const root = parseRoot(input.root);
  const programId = relationId(input.program_id, 'scope program id');
  const rootRelationId = relationId(input.root_relation_id, 'scope root relation id');
  const declaredRelationIds = strictlySortedRelationIds(input.declared_relation_ids);
  if (!declaredRelationIds.includes(rootRelationId)) {
    throw new ContractValidationError('scope root relation is not declared');
  }
  const decodeCoverage = parseSourceCoverageSet(input.decode_coverage);
  if (
    decodeCoverage.coverage_domain.kind !== 'decode' ||
    decodeCoverage.scope.adapter_id !== root.adapter_id ||
    decodeCoverage.scope.source_instance_key !== root.source_instance_key ||
    decodeCoverage.scope.root_entity_key !== root.session_key ||
    decodeCoverage.points.length + decodeCoverage.explicit_absence_or_deletion.length !== declaredRelationIds.length
  ) {
    throw new ContractValidationError('caller-held Decode coverage does not belong to the exact scoped root');
  }
  return {
    root,
    program_id: programId,
    scope_program_digest: sha256Digest(input.scope_program_digest, 'scope program digest'),
    root_relation_id: rootRelationId,
    declared_relation_ids: declaredRelationIds,
    expected_scope_revision: fixedOpaque(input.expected_scope_revision, 'expected scope coverage revision'),
    decode_coverage: decodeCoverage,
  };
}

function parseSource(value: unknown): ScopedScopeCoverageSource {
  const input = exactRecord(
    value,
    ['adapter_id', 'source_instance_key', 'stream_key', 'object_key'],
    'scope relation source',
  );
  return {
    adapter_id: adapterId(input.adapter_id),
    source_instance_key: fixedOpaque(input.source_instance_key, 'relation source instance key'),
    stream_key: fixedOpaque(input.stream_key, 'relation stream key'),
    object_key: fixedOpaque(input.object_key, 'relation object key'),
  };
}

function coordinate(source: ScopedScopeCoverageSource): string {
  return `${source.stream_key}\0${source.object_key}`;
}

export function parseScopedScopeCoverage(value: unknown, expectedContextInput: unknown): ScopedScopeCoverage {
  const expected = parseScopedScopeCoverageContext(expectedContextInput);
  const input = exactRecord(
    value,
    [
      'scoped_scope_coverage_contract_version',
      'program_id',
      'scope_program_digest',
      'root_relation_id',
      'scope_revision',
      'relations',
      'completeness',
    ],
    'scoped scope coverage',
  );
  if (input.scoped_scope_coverage_contract_version !== SCOPED_SCOPE_COVERAGE_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported scoped scope-coverage contract version');
  }
  const programId = relationId(input.program_id, 'scope program id');
  const programDigest = sha256Digest(input.scope_program_digest, 'scope program digest');
  const rootRelationId = relationId(input.root_relation_id, 'scope root relation id');
  const scopeRevision = fixedOpaque(input.scope_revision, 'scope coverage revision');
  if (
    programId !== expected.program_id ||
    programDigest !== expected.scope_program_digest ||
    rootRelationId !== expected.root_relation_id ||
    scopeRevision !== expected.expected_scope_revision
  ) {
    throw new ContractValidationError('scope coverage does not match caller-held program context');
  }
  if (!Array.isArray(input.relations) || input.relations.length !== expected.declared_relation_ids.length) {
    throw new ContractValidationError('scope coverage does not contain the exact declared relation set');
  }
  const decodePoints = new Map(
    expected.decode_coverage.points.map((point) => [
      `${point.stream_key}\0${point.object_key}\0${point.generation}`,
      point,
    ]),
  );
  const decodeAbsences = new Map(
    expected.decode_coverage.explicit_absence_or_deletion.map((absence) => [
      `${absence.stream_key}\0${absence.object_key}\0${absence.generation}`,
      absence,
    ]),
  );
  const usedCoordinates = new Set<string>();
  let combinedCompleteness: CoverageSetCompleteness = 'complete';
  const relations = input.relations.map((entry, index): ScopedScopeRelationCoverage => {
    const relationInput = exactRecord(
      entry,
      ['relation_id', 'scope_root', 'source', 'generation', 'state', 'completeness'],
      'scope relation coverage',
    );
    const relation_id = relationId(relationInput.relation_id, 'scope relation id');
    if (relation_id !== expected.declared_relation_ids[index]) {
      throw new ContractValidationError('scope coverage relation order/set does not match caller-held declarations');
    }
    if (
      typeof relationInput.scope_root !== 'boolean' ||
      relationInput.scope_root !== (relation_id === rootRelationId)
    ) {
      throw new ContractValidationError('scope coverage root designation is not declaration-derived');
    }
    const source = parseSource(relationInput.source);
    if (
      source.adapter_id !== expected.root.adapter_id ||
      source.source_instance_key !== expected.root.source_instance_key
    ) {
      throw new ContractValidationError('scope relation source does not belong to the caller-held root');
    }
    const sourceCoordinate = coordinate(source);
    if (usedCoordinates.has(sourceCoordinate)) {
      throw new ContractValidationError('scope coverage contains a duplicate source coordinate');
    }
    usedCoordinates.add(sourceCoordinate);
    const generation = positiveSafeInteger(relationInput.generation, 'scope relation generation');
    const generationCoordinate = `${sourceCoordinate}\0${generation}`;
    const stateInput = record(relationInput.state, 'scope relation state');
    let state: ScopedScopeRelationState;
    if (stateInput.kind === 'present') {
      exactRecord(relationInput.state, ['kind', 'status'], 'present scope relation state');
      const status = parseStatus(stateInput.status);
      const point = decodePoints.get(generationCoordinate);
      if (point === undefined || !statusEqual(status, point.status)) {
        throw new ContractValidationError('present scope relation does not match Decode evidence');
      }
      decodePoints.delete(generationCoordinate);
      state = { kind: 'present', status };
    } else if (stateInput.kind === 'absent') {
      exactRecord(relationInput.state, ['kind', 'absence_kind'], 'absent scope relation state');
      if (stateInput.absence_kind !== 'absent' && stateInput.absence_kind !== 'deleted') {
        throw new ContractValidationError('scope relation absence kind is unsupported');
      }
      const absence = decodeAbsences.get(generationCoordinate);
      if (absence === undefined || absence.kind !== stateInput.absence_kind) {
        throw new ContractValidationError('absent scope relation does not match Decode evidence');
      }
      decodeAbsences.delete(generationCoordinate);
      state = { kind: 'absent', absence_kind: stateInput.absence_kind };
    } else {
      throw new ContractValidationError('scope relation state is unsupported');
    }
    const relationCompleteness = completeness(relationInput.completeness, 'scope relation completeness');
    if (
      relationCompleteness === 'complete' &&
      state.kind === 'present' &&
      (state.status.kind === 'partial' || state.status.kind === 'unavailable')
    ) {
      throw new ContractValidationError('complete scope relation cannot carry incomplete status');
    }
    combinedCompleteness = mergeCompleteness(combinedCompleteness, relationCompleteness);
    return {
      relation_id,
      scope_root: relationInput.scope_root,
      source,
      generation,
      state,
      completeness: relationCompleteness,
    };
  });
  const parsedCompleteness = completeness(input.completeness, 'scope coverage completeness');
  if (
    decodePoints.size !== 0 ||
    decodeAbsences.size !== 0 ||
    parsedCompleteness !== combinedCompleteness ||
    parsedCompleteness !== expected.decode_coverage.completeness
  ) {
    throw new ContractValidationError('scope coverage completeness does not match relation/Decode evidence');
  }
  return {
    scoped_scope_coverage_contract_version: SCOPED_SCOPE_COVERAGE_CONTRACT_VERSION,
    program_id: programId,
    scope_program_digest: programDigest,
    root_relation_id: rootRelationId,
    scope_revision: scopeRevision,
    relations,
    completeness: parsedCompleteness,
  };
}
