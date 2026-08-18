/** Strict RFC 012D wire projection for source lifecycle controls.
 *
 * This contract covers only source created/deleted/reset/object-error events.
 * It is not the complete observation event union. Consumption requires the
 * exact caller-held selection, root, and authorized source set. Rust remains
 * authoritative for recomputing the opaque source-control event ID.
 */

import { ContractValidationError, parseOpaqueContractReference, type OpaqueContractReference } from './rfc012a.js';
import { parseObservationContractSelectionForExpected, type ObservationContractSelection } from './rfc012d.js';
import {
  parseScopedUsageEnvelopeContext,
  type ScopedUsageActor,
  type ScopedUsageAffiliations,
  type ScopedUsageEnvelopeContext,
  type ScopedUsageRoot,
  type ScopedUsageSourceBinding,
} from './rfc012d-usage-envelope.js';

export const SCOPED_SOURCE_ENVELOPE_CONTRACT_VERSION = 1 as const;

const MAX_AUTHORIZED_SOURCES = 1_000;
const MAX_RELATION_ID_BYTES = 128;
const MAX_U32 = 0xffff_ffff;
const MAX_RETRY_ATTEMPTS = 32;
const MAX_RETRY_DELAY_MS = 60 * 60 * 1_000;
const textEncoder = new TextEncoder();

type UnknownRecord = Record<string, unknown>;

export type ScopedSourceEnvelopeContext = ScopedUsageEnvelopeContext;
export type ScopedSourceRoot = ScopedUsageRoot;
export type ScopedSourceActor = ScopedUsageActor;
export type ScopedSourceAffiliations = ScopedUsageAffiliations;
export type ScopedSourceBinding = ScopedUsageSourceBinding;

export interface ScopedSourceCoordinate extends ScopedSourceBinding {
  locator_id: null;
  generation: number;
  source_record_id: null;
  record_index: null;
  cursor_start: null;
  cursor_end: null;
  byte_range: null;
}

export interface ScopedSourceCoveragePosition {
  kind: 'append_cursor';
  opaque: OpaqueContractReference;
  monotonic_order: number;
}

export type ScopedSourceRetry =
  | {
      kind: 'retry_scheduled';
      failed_attempts: number;
      max_attempts: number;
      retry_after_ms: number;
    }
  | { kind: 'retry_exhausted'; failed_attempts: number; max_attempts: number }
  | { kind: 'not_retryable'; failed_attempts: number };

export type ScopedSourceFailureCode =
  | 'source_retry_transient'
  | 'source_unstable'
  | 'source_database'
  | 'source_io'
  | 'source_invalid_configuration'
  | 'source_invalid_cursor'
  | 'source_path_escape'
  | 'source_limit_exceeded'
  | 'decode_retry_transient'
  | 'decode_record_permanent'
  | 'decode_stream_fatal';

export interface ScopedSourceObjectError {
  error_contract_version: 1;
  relation_id: string;
  scope_epoch: number;
  failure_code: ScopedSourceFailureCode;
  provenance: {
    generation: number;
    last_successful_position: ScopedSourceCoveragePosition | null;
  };
  retry: ScopedSourceRetry;
}

export type ScopedSourceControlEvent =
  | { kind: 'source_created'; generation: number }
  | { kind: 'source_deleted'; generation: number }
  | {
      kind: 'source_reset';
      old_generation: number;
      new_generation: number;
      reason: 'truncated' | 'identity_changed' | 'prefix_mismatch' | 'contract_replay';
    }
  | { kind: 'source_object_error'; error: ScopedSourceObjectError };

export interface ScopedSourceEnvelope {
  scoped_source_envelope_contract_version: typeof SCOPED_SOURCE_ENVELOPE_CONTRACT_VERSION;
  contract_version: number;
  contract_selection: ObservationContractSelection;
  observer_sequence: number;
  scope_epoch: number;
  event_id: string;
  semantic_revision_ref: null;
  root: ScopedSourceRoot;
  actor: ScopedSourceActor;
  actor_attribution: { kind: 'scope_fallback'; reason: 'source_lifecycle_control' };
  affiliations: ScopedSourceAffiliations;
  source: ScopedSourceCoordinate;
  native_time: null;
  observed_at: number;
  phase: 'bootstrap' | 'live' | 'correction';
  evidence: {
    authority: 'engine_control';
    quality: 'derived';
    effective_at: null;
    completeness: 'complete' | 'partial' | 'unknown';
  };
  event: ScopedSourceControlEvent;
  native_evidence: { kind: 'engine_control' };
}

export const parseScopedSourceEnvelopeContext = parseScopedUsageEnvelopeContext;

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

function assertKnownFields(value: UnknownRecord, fields: readonly string[], label: string): void {
  const unknown = Object.keys(value).find((field) => !fields.includes(field));
  if (unknown !== undefined) {
    throw new ContractValidationError(`${label} contains unknown field ${unknown}`);
  }
}

function assertRequiredFields(value: UnknownRecord, fields: readonly string[], label: string): void {
  const missing = fields.find((field) => !Object.prototype.hasOwnProperty.call(value, field));
  if (missing !== undefined) {
    throw new ContractValidationError(`${label} is missing field ${missing}`);
  }
}

function exactRecord(value: unknown, fields: readonly string[], label: string): UnknownRecord {
  const input = record(value, label);
  assertKnownFields(input, fields, label);
  assertRequiredFields(input, fields, label);
  return input;
}

function safeInteger(value: unknown, label: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value)) {
    throw new ContractValidationError(`${label} must be a portable integer`);
  }
  return value;
}

function positiveSafeInteger(value: unknown, label: string): number {
  const parsed = safeInteger(value, label);
  if (parsed <= 0) throw new ContractValidationError(`${label} must be positive`);
  return parsed;
}

function u32(value: unknown, label: string, positive = false): number {
  const parsed = positive ? positiveSafeInteger(value, label) : safeInteger(value, label);
  if (parsed < 0 || parsed > MAX_U32) {
    throw new ContractValidationError(`${label} exceeds u32`);
  }
  return parsed;
}

function decodeFixedOpaque(value: unknown, label: string): string {
  if (typeof value !== 'string' || !value.startsWith('v1:')) {
    throw new ContractValidationError(`${label} is not a v1 opaque reference`);
  }
  const encoded = value.slice(3);
  if (encoded.length === 0 || encoded.includes('=') || !/^[A-Za-z0-9_-]+$/.test(encoded)) {
    throw new ContractValidationError(`${label} is not canonical base64url`);
  }
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
  if (bytes.byteLength !== 32 || canonical !== encoded) {
    throw new ContractValidationError(`${label} must contain exactly 32 bytes`);
  }
  return value;
}

function sourceKey(value: ScopedSourceBinding): string {
  return `${value.instance_key}\0${value.stream_key}\0${value.object_key}`;
}

function parseSource(value: unknown, context: ScopedSourceEnvelopeContext): ScopedSourceCoordinate {
  const fields = [
    'instance_key',
    'stream_key',
    'object_key',
    'locator_id',
    'generation',
    'source_record_id',
    'record_index',
    'cursor_start',
    'cursor_end',
    'byte_range',
  ];
  const input = exactRecord(value, fields, 'source coordinate');
  const binding: ScopedSourceBinding = {
    instance_key: parseOpaqueContractReference(input.instance_key, 'source instance key'),
    stream_key: parseOpaqueContractReference(input.stream_key, 'source stream key'),
    object_key: parseOpaqueContractReference(input.object_key, 'source object key'),
  };
  if (!context.authorized_sources.some((candidate) => sourceKey(candidate) === sourceKey(binding))) {
    throw new ContractValidationError('source coordinate is outside the caller-held authorized source set');
  }
  for (const field of ['locator_id', 'source_record_id', 'record_index', 'cursor_start', 'cursor_end', 'byte_range']) {
    if (input[field] !== null) {
      throw new ContractValidationError('source lifecycle control cannot disclose record occurrence data');
    }
  }
  return {
    ...binding,
    locator_id: null,
    generation: positiveSafeInteger(input.generation, 'source generation'),
    source_record_id: null,
    record_index: null,
    cursor_start: null,
    cursor_end: null,
    byte_range: null,
  };
}

function parseActor(value: unknown, root: ScopedSourceRoot): ScopedSourceActor {
  const fields = [
    'root_session_key',
    'run_key',
    'role',
    'parent_run_key',
    'native_session_id',
    'native_actor_id',
    'native_actor_type',
  ];
  const input = exactRecord(value, fields, 'source actor');
  const nativeSession = root.native_session_claim?.identity.value?.native_id ?? null;
  if (
    input.root_session_key !== root.session_key ||
    input.run_key !== root.root_actor_run_key ||
    input.role !== 'root' ||
    input.parent_run_key !== null ||
    input.native_session_id !== nativeSession ||
    input.native_actor_id !== null ||
    input.native_actor_type !== null
  ) {
    throw new ContractValidationError('source lifecycle actor is not the exact root actor');
  }
  return {
    root_session_key: parseOpaqueContractReference(input.root_session_key, 'actor root session key'),
    run_key: parseOpaqueContractReference(input.run_key, 'actor run key'),
    role: 'root',
    parent_run_key: null,
    native_session_id: nativeSession,
    native_actor_id: null,
    native_actor_type: null,
  };
}

function parseAffiliations(value: unknown, actor: ScopedSourceActor): ScopedSourceAffiliations {
  const fields = [
    'actor_run_key',
    'team_key',
    'native_team_id',
    'team_name',
    'member_key',
    'workflow_key',
    'native_workflow_id',
    'completeness',
    'derived_from_revision_refs',
  ];
  const input = exactRecord(value, fields, 'source affiliations');
  if (
    input.actor_run_key !== actor.run_key ||
    input.team_key !== null ||
    input.native_team_id !== null ||
    input.team_name !== null ||
    input.member_key !== null ||
    input.workflow_key !== null ||
    input.native_workflow_id !== null ||
    input.completeness !== 'unknown' ||
    !Array.isArray(input.derived_from_revision_refs) ||
    input.derived_from_revision_refs.length !== 0
  ) {
    throw new ContractValidationError('source lifecycle affiliations must remain explicitly unknown');
  }
  return {
    actor_run_key: parseOpaqueContractReference(input.actor_run_key, 'affiliation actor run key'),
    team_key: null,
    native_team_id: null,
    team_name: null,
    member_key: null,
    workflow_key: null,
    native_workflow_id: null,
    completeness: 'unknown',
    derived_from_revision_refs: [],
  };
}

const failureCodes = new Set<ScopedSourceFailureCode>([
  'source_retry_transient',
  'source_unstable',
  'source_database',
  'source_io',
  'source_invalid_configuration',
  'source_invalid_cursor',
  'source_path_escape',
  'source_limit_exceeded',
  'decode_retry_transient',
  'decode_record_permanent',
  'decode_stream_fatal',
]);

const retryableFailureCodes = new Set<ScopedSourceFailureCode>([
  'source_retry_transient',
  'source_unstable',
  'source_database',
  'source_io',
  'decode_retry_transient',
]);

function parseFailureCode(value: unknown): ScopedSourceFailureCode {
  if (typeof value !== 'string' || !failureCodes.has(value as ScopedSourceFailureCode)) {
    throw new ContractValidationError('source object error has an unsupported failure code');
  }
  return value as ScopedSourceFailureCode;
}

function parseRetry(value: unknown, failureCode: ScopedSourceFailureCode): ScopedSourceRetry {
  const input = record(value, 'source retry state');
  if (input.kind === 'retry_scheduled') {
    const fields = ['kind', 'failed_attempts', 'max_attempts', 'retry_after_ms'];
    assertKnownFields(input, fields, 'scheduled source retry');
    assertRequiredFields(input, fields, 'scheduled source retry');
    const failedAttempts = u32(input.failed_attempts, 'failed attempts', true);
    const maxAttempts = u32(input.max_attempts, 'max attempts', true);
    const retryAfterMs = positiveSafeInteger(input.retry_after_ms, 'retry delay');
    if (
      !retryableFailureCodes.has(failureCode) ||
      failedAttempts >= maxAttempts ||
      maxAttempts > MAX_RETRY_ATTEMPTS ||
      retryAfterMs > MAX_RETRY_DELAY_MS
    ) {
      throw new ContractValidationError('scheduled source retry is inconsistent');
    }
    return {
      kind: 'retry_scheduled',
      failed_attempts: failedAttempts,
      max_attempts: maxAttempts,
      retry_after_ms: retryAfterMs,
    };
  }
  if (input.kind === 'retry_exhausted') {
    const fields = ['kind', 'failed_attempts', 'max_attempts'];
    assertKnownFields(input, fields, 'exhausted source retry');
    assertRequiredFields(input, fields, 'exhausted source retry');
    const failedAttempts = u32(input.failed_attempts, 'failed attempts', true);
    const maxAttempts = u32(input.max_attempts, 'max attempts', true);
    if (!retryableFailureCodes.has(failureCode) || failedAttempts !== maxAttempts || maxAttempts > MAX_RETRY_ATTEMPTS) {
      throw new ContractValidationError('exhausted source retry is inconsistent');
    }
    return { kind: 'retry_exhausted', failed_attempts: failedAttempts, max_attempts: maxAttempts };
  }
  if (input.kind === 'not_retryable') {
    const fields = ['kind', 'failed_attempts'];
    assertKnownFields(input, fields, 'terminal source error');
    assertRequiredFields(input, fields, 'terminal source error');
    if (retryableFailureCodes.has(failureCode)) {
      throw new ContractValidationError('retryable failure cannot use not_retryable state');
    }
    return {
      kind: 'not_retryable',
      failed_attempts: u32(input.failed_attempts, 'failed attempts', true),
    };
  }
  throw new ContractValidationError('source object error has an unsupported retry state');
}

function parseObjectError(value: unknown, source: ScopedSourceCoordinate, scopeEpoch: number): ScopedSourceObjectError {
  const fields = ['error_contract_version', 'relation_id', 'scope_epoch', 'failure_code', 'provenance', 'retry'];
  const input = exactRecord(value, fields, 'source object error');
  if (input.error_contract_version !== 1) {
    throw new ContractValidationError('unsupported source object error contract version');
  }
  if (
    typeof input.relation_id !== 'string' ||
    textEncoder.encode(input.relation_id).byteLength > MAX_RELATION_ID_BYTES ||
    !/^[a-z0-9][a-z0-9._-]{0,127}$/.test(input.relation_id)
  ) {
    throw new ContractValidationError('source object error relation_id is invalid');
  }
  if (positiveSafeInteger(input.scope_epoch, 'error scope_epoch') !== scopeEpoch) {
    throw new ContractValidationError('source object error targets another scope epoch');
  }
  const failureCode = parseFailureCode(input.failure_code);
  const provenanceInput = exactRecord(
    input.provenance,
    ['generation', 'last_successful_position'],
    'source object error provenance',
  );
  const generation = positiveSafeInteger(provenanceInput.generation, 'error provenance generation');
  if (generation !== source.generation) {
    throw new ContractValidationError('source object error targets another generation');
  }
  let lastSuccessfulPosition: ScopedSourceCoveragePosition | null = null;
  if (provenanceInput.last_successful_position !== null) {
    const position = exactRecord(
      provenanceInput.last_successful_position,
      ['kind', 'opaque', 'monotonic_order'],
      'last successful position',
    );
    if (position.kind !== 'append_cursor') {
      throw new ContractValidationError('last successful position is not append-bound');
    }
    lastSuccessfulPosition = {
      kind: 'append_cursor',
      opaque: parseOpaqueContractReference(position.opaque, 'last successful position reference'),
      monotonic_order: safeInteger(position.monotonic_order, 'last successful position order'),
    };
    if (lastSuccessfulPosition.monotonic_order < 0) {
      throw new ContractValidationError('last successful position order must be nonnegative');
    }
  }
  return {
    error_contract_version: 1,
    relation_id: input.relation_id,
    scope_epoch: scopeEpoch,
    failure_code: failureCode,
    provenance: { generation, last_successful_position: lastSuccessfulPosition },
    retry: parseRetry(input.retry, failureCode),
  };
}

function parseEvent(
  value: unknown,
  source: ScopedSourceCoordinate,
  scopeEpoch: number,
  phase: 'bootstrap' | 'live' | 'correction',
): ScopedSourceControlEvent {
  const input = record(value, 'source control event');
  if (input.kind === 'source_created' || input.kind === 'source_deleted') {
    const fields = ['kind', 'generation'];
    assertKnownFields(input, fields, 'source presence event');
    assertRequiredFields(input, fields, 'source presence event');
    const generation = positiveSafeInteger(input.generation, 'source presence generation');
    if (generation !== source.generation) {
      throw new ContractValidationError('source presence generation does not match its coordinate');
    }
    return { kind: input.kind, generation };
  }
  if (input.kind === 'source_reset') {
    const fields = ['kind', 'old_generation', 'new_generation', 'reason'];
    assertKnownFields(input, fields, 'source reset event');
    assertRequiredFields(input, fields, 'source reset event');
    const oldGeneration = positiveSafeInteger(input.old_generation, 'reset old_generation');
    const newGeneration = positiveSafeInteger(input.new_generation, 'reset new_generation');
    if (
      oldGeneration + 1 !== newGeneration ||
      newGeneration !== source.generation ||
      phase !== 'correction' ||
      (input.reason !== 'truncated' &&
        input.reason !== 'identity_changed' &&
        input.reason !== 'prefix_mismatch' &&
        input.reason !== 'contract_replay')
    ) {
      throw new ContractValidationError('source reset has invalid generation lineage or reason');
    }
    return {
      kind: 'source_reset',
      old_generation: oldGeneration,
      new_generation: newGeneration,
      reason: input.reason,
    };
  }
  if (input.kind === 'source_object_error') {
    const fields = ['kind', 'error'];
    assertKnownFields(input, fields, 'source object error event');
    assertRequiredFields(input, fields, 'source object error event');
    if (phase !== 'live' && phase !== 'correction') {
      throw new ContractValidationError('source object errors require live or correction delivery');
    }
    return { kind: 'source_object_error', error: parseObjectError(input.error, source, scopeEpoch) };
  }
  throw new ContractValidationError('unsupported source control event kind');
}

export function parseScopedSourceEnvelope(value: unknown, expectedContextInput: unknown): ScopedSourceEnvelope {
  const context = parseScopedSourceEnvelopeContext(expectedContextInput);
  if (
    context.authorized_sources.length === 0 ||
    context.authorized_sources.length > MAX_AUTHORIZED_SOURCES ||
    new Set(context.authorized_sources.map(sourceKey)).size !== context.authorized_sources.length
  ) {
    throw new ContractValidationError('invalid caller-held authorized source set');
  }
  const fields = [
    'scoped_source_envelope_contract_version',
    'contract_version',
    'contract_selection',
    'observer_sequence',
    'scope_epoch',
    'event_id',
    'semantic_revision_ref',
    'root',
    'actor',
    'actor_attribution',
    'affiliations',
    'source',
    'native_time',
    'observed_at',
    'phase',
    'evidence',
    'event',
    'native_evidence',
  ];
  const input = exactRecord(value, fields, 'scoped source envelope');
  if (input.scoped_source_envelope_contract_version !== SCOPED_SOURCE_ENVELOPE_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported scoped source envelope contract version');
  }
  const selection = parseObservationContractSelectionForExpected(input.contract_selection, context.contract_selection);
  if (input.contract_version !== selection.envelope_contract_version || selection.event_contract_version !== 1) {
    throw new ContractValidationError('source envelope does not match the selected source-control contract');
  }
  const envelopeContext = parseScopedSourceEnvelopeContext({
    contract_selection: selection,
    root: input.root,
    authorized_sources: context.authorized_sources,
  });
  if (JSON.stringify(envelopeContext.root) !== JSON.stringify(context.root)) {
    throw new ContractValidationError('source envelope does not match the caller-held root');
  }
  if (input.semantic_revision_ref !== null || input.native_time !== null) {
    throw new ContractValidationError('source lifecycle control cannot carry semantic or native time');
  }
  const actor = parseActor(input.actor, context.root);
  const attribution = exactRecord(input.actor_attribution, ['kind', 'reason'], 'source actor attribution');
  if (attribution.kind !== 'scope_fallback' || attribution.reason !== 'source_lifecycle_control') {
    throw new ContractValidationError('source lifecycle actor attribution is invalid');
  }
  const affiliations = parseAffiliations(input.affiliations, actor);
  const source = parseSource(input.source, context);
  const observerSequence = positiveSafeInteger(input.observer_sequence, 'observer_sequence');
  const scopeEpoch = positiveSafeInteger(input.scope_epoch, 'scope_epoch');
  const observedAt = safeInteger(input.observed_at, 'observed_at');
  if (input.phase !== 'bootstrap' && input.phase !== 'live' && input.phase !== 'correction') {
    throw new ContractValidationError('unsupported source delivery phase');
  }
  const event = parseEvent(input.event, source, scopeEpoch, input.phase);
  const evidence = exactRecord(
    input.evidence,
    ['authority', 'quality', 'effective_at', 'completeness'],
    'source evidence',
  );
  const terminal = event.kind === 'source_object_error' && event.error.retry.kind !== 'retry_scheduled';
  const expectedCompleteness = event.kind !== 'source_object_error' ? 'complete' : terminal ? 'unknown' : 'partial';
  if (
    evidence.authority !== 'engine_control' ||
    evidence.quality !== 'derived' ||
    evidence.effective_at !== null ||
    evidence.completeness !== expectedCompleteness
  ) {
    throw new ContractValidationError('source lifecycle evidence is inconsistent');
  }
  const nativeEvidence = exactRecord(input.native_evidence, ['kind'], 'source native evidence');
  if (nativeEvidence.kind !== 'engine_control') {
    throw new ContractValidationError('source lifecycle native evidence must be engine control only');
  }
  return {
    scoped_source_envelope_contract_version: SCOPED_SOURCE_ENVELOPE_CONTRACT_VERSION,
    contract_version: selection.envelope_contract_version,
    contract_selection: selection,
    observer_sequence: observerSequence,
    scope_epoch: scopeEpoch,
    event_id: decodeFixedOpaque(input.event_id, 'source event_id'),
    semantic_revision_ref: null,
    root: context.root,
    actor,
    actor_attribution: { kind: 'scope_fallback', reason: 'source_lifecycle_control' },
    affiliations,
    source,
    native_time: null,
    observed_at: observedAt,
    phase: input.phase,
    evidence: {
      authority: 'engine_control',
      quality: 'derived',
      effective_at: null,
      completeness: expectedCompleteness,
    },
    event,
    native_evidence: { kind: 'engine_control' },
  };
}
