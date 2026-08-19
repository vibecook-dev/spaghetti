/** Strict contextual RFC 012D ordered artifact-availability event.
 *
 * Rust mints the caller-held context while the private source-declaration
 * occurrence is still available and remains authoritative for the opaque
 * BLAKE3 event ID. The portable context deliberately carries only that exact
 * issued ID, source/order binding, and expected availability entry; it never
 * exposes a declaration digest, artifact locator, cursor, or native bytes.
 */

import { ContractValidationError, parseOpaqueContractReference, type OpaqueContractReference } from './rfc012a.js';
import {
  parseScopedArtifactAvailabilityEntry,
  type ScopedArtifactAvailabilityEntry,
} from './rfc012d-artifact-availability.js';
import {
  parseScopedSourceEnvelope,
  parseScopedSourceEnvelopeContext,
  type ScopedSourceActor,
  type ScopedSourceAffiliations,
  type ScopedSourceBinding,
  type ScopedSourceCoordinate,
  type ScopedSourceRoot,
} from './rfc012d-source-envelope.js';
import { parseObservationContractSelectionForExpected, type ObservationContractSelection } from './rfc012d.js';

export const SCOPED_ARTIFACT_AVAILABILITY_ENVELOPE_CONTRACT_VERSION = 1 as const;

type UnknownRecord = Record<string, unknown>;

export interface ScopedArtifactAvailabilityEnvelopeContext {
  contract_selection: ObservationContractSelection;
  root: ScopedSourceRoot;
  expected_source: ScopedSourceBinding & { generation: number };
  expected_observer_sequence: number;
  expected_scope_epoch: number;
  expected_event_id: OpaqueContractReference;
  expected_observed_at: number;
  expected_phase: 'bootstrap' | 'live';
  expected_entry: ScopedArtifactAvailabilityEntry;
}

export interface ScopedArtifactAvailabilityEnvelope {
  scoped_artifact_availability_envelope_contract_version: typeof SCOPED_ARTIFACT_AVAILABILITY_ENVELOPE_CONTRACT_VERSION;
  contract_version: number;
  contract_selection: ObservationContractSelection;
  observer_sequence: number;
  scope_epoch: number;
  event_id: OpaqueContractReference;
  semantic_revision_ref: null;
  root: ScopedSourceRoot;
  actor: ScopedSourceActor;
  actor_attribution: { kind: 'scope_fallback'; reason: 'source_lifecycle_control' };
  affiliations: ScopedSourceAffiliations;
  source: ScopedSourceCoordinate;
  native_time: null;
  observed_at: number;
  phase: 'bootstrap' | 'live';
  evidence: {
    authority: 'common_reducer';
    quality: 'derived';
    effective_at: null;
    completeness: 'complete' | 'unknown';
  };
  event: {
    kind: 'artifact_availability';
    entry: ScopedArtifactAvailabilityEntry;
  };
  native_evidence: { kind: 'engine_control' };
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

function positiveInteger(value: unknown, label: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value <= 0) {
    throw new ContractValidationError(`${label} must be a positive portable integer`);
  }
  return value;
}

function safeInteger(value: unknown, label: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value)) {
    throw new ContractValidationError(`${label} must be a portable integer`);
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
  const bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0));
  let roundTrip = '';
  for (const byte of bytes) roundTrip += String.fromCharCode(byte);
  const canonical = btoa(roundTrip).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/g, '');
  if (bytes.length !== 32 || bytes.every((byte) => byte === 0) || canonical !== encoded) {
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

export function parseScopedArtifactAvailabilityEnvelopeContext(
  value: unknown,
): ScopedArtifactAvailabilityEnvelopeContext {
  const input = exactRecord(
    value,
    [
      'contract_selection',
      'root',
      'expected_source',
      'expected_observer_sequence',
      'expected_scope_epoch',
      'expected_event_id',
      'expected_observed_at',
      'expected_phase',
      'expected_entry',
    ],
    'scoped artifact availability envelope context',
  );
  const selection = parseObservationContractSelectionForExpected(input.contract_selection, input.contract_selection);
  const sourceInput = exactRecord(
    input.expected_source,
    ['instance_key', 'stream_key', 'object_key', 'generation'],
    'expected artifact availability source',
  );
  const commonContext = parseScopedSourceEnvelopeContext({
    contract_selection: selection,
    root: input.root,
    authorized_sources: [
      {
        instance_key: sourceInput.instance_key,
        stream_key: sourceInput.stream_key,
        object_key: sourceInput.object_key,
      },
    ],
  });
  const expectedSourceGeneration = positiveInteger(
    sourceInput.generation,
    'expected artifact availability source generation',
  );
  const expectedEntry = parseScopedArtifactAvailabilityEntry(input.expected_entry);
  const entryGeneration =
    expectedEntry.state.kind === 'available' || expectedEntry.state.kind === 'over_limit'
      ? expectedEntry.state.generation
      : expectedEntry.state.kind === 'missing'
        ? (expectedEntry.state.observed_generation ?? 1)
        : expectedSourceGeneration;
  if (entryGeneration !== expectedSourceGeneration) {
    throw new ContractValidationError('expected artifact availability entry/source generation mismatch');
  }
  if (input.expected_phase !== 'bootstrap' && input.expected_phase !== 'live') {
    throw new ContractValidationError('expected artifact availability phase is unsupported');
  }
  return {
    contract_selection: commonContext.contract_selection,
    root: commonContext.root,
    expected_source: {
      ...commonContext.authorized_sources[0]!,
      generation: expectedSourceGeneration,
    },
    expected_observer_sequence: positiveInteger(
      input.expected_observer_sequence,
      'expected artifact availability observer_sequence',
    ),
    expected_scope_epoch: positiveInteger(input.expected_scope_epoch, 'expected artifact availability scope_epoch'),
    expected_event_id: fixedOpaque(input.expected_event_id, 'expected artifact availability event_id'),
    expected_observed_at: safeInteger(input.expected_observed_at, 'expected artifact availability observed_at'),
    expected_phase: input.expected_phase,
    expected_entry: expectedEntry,
  };
}

export function parseScopedArtifactAvailabilityEnvelope(
  value: unknown,
  expectedContextInput: unknown,
): ScopedArtifactAvailabilityEnvelope {
  const context = parseScopedArtifactAvailabilityEnvelopeContext(expectedContextInput);
  const fields = [
    'scoped_artifact_availability_envelope_contract_version',
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
  const input = exactRecord(value, fields, 'scoped artifact availability envelope');
  if (
    input.scoped_artifact_availability_envelope_contract_version !==
    SCOPED_ARTIFACT_AVAILABILITY_ENVELOPE_CONTRACT_VERSION
  ) {
    throw new ContractValidationError('unsupported scoped artifact availability envelope contract version');
  }
  const selection = parseObservationContractSelectionForExpected(input.contract_selection, context.contract_selection);
  if (input.contract_version !== selection.envelope_contract_version || selection.event_contract_version !== 1) {
    throw new ContractValidationError('artifact availability envelope does not match the selected event contract');
  }
  const observerSequence = positiveInteger(input.observer_sequence, 'artifact availability observer_sequence');
  const scopeEpoch = positiveInteger(input.scope_epoch, 'artifact availability scope_epoch');
  const eventId = fixedOpaque(input.event_id, 'artifact availability event_id');
  const observedAt = safeInteger(input.observed_at, 'artifact availability observed_at');
  if (
    observerSequence !== context.expected_observer_sequence ||
    scopeEpoch !== context.expected_scope_epoch ||
    eventId !== context.expected_event_id ||
    observedAt !== context.expected_observed_at ||
    input.phase !== context.expected_phase
  ) {
    throw new ContractValidationError('artifact availability event does not match caller-held order identity');
  }
  if (input.phase !== 'bootstrap' && input.phase !== 'live') {
    throw new ContractValidationError('artifact availability event requires bootstrap or live delivery');
  }
  const eventInput = exactRecord(input.event, ['kind', 'entry'], 'artifact availability event');
  if (eventInput.kind !== 'artifact_availability') {
    throw new ContractValidationError('artifact availability event kind is invalid');
  }
  const entry = parseScopedArtifactAvailabilityEntry(eventInput.entry);
  if (canonicalJson(entry) !== canonicalJson(context.expected_entry)) {
    throw new ContractValidationError('artifact availability entry does not match caller-held reducer authority');
  }
  const evidenceInput = exactRecord(
    input.evidence,
    ['authority', 'quality', 'effective_at', 'completeness'],
    'artifact availability evidence',
  );
  const completeness = entry.state.kind === 'unstable' ? 'unknown' : 'complete';
  if (
    evidenceInput.authority !== 'common_reducer' ||
    evidenceInput.quality !== 'derived' ||
    evidenceInput.effective_at !== null ||
    evidenceInput.completeness !== completeness
  ) {
    throw new ContractValidationError('artifact availability evidence is inconsistent');
  }

  const sourceContext = {
    contract_selection: context.contract_selection,
    root: context.root,
    authorized_sources: [
      {
        instance_key: context.expected_source.instance_key,
        stream_key: context.expected_source.stream_key,
        object_key: context.expected_source.object_key,
      },
    ],
  };
  const common = parseScopedSourceEnvelope(
    {
      scoped_source_envelope_contract_version: 1,
      contract_version: input.contract_version,
      contract_selection: input.contract_selection,
      observer_sequence: input.observer_sequence,
      scope_epoch: input.scope_epoch,
      event_id: input.event_id,
      semantic_revision_ref: input.semantic_revision_ref,
      root: input.root,
      actor: input.actor,
      actor_attribution: input.actor_attribution,
      affiliations: input.affiliations,
      source: input.source,
      native_time: input.native_time,
      observed_at: input.observed_at,
      phase: input.phase,
      evidence: {
        authority: 'engine_control',
        quality: 'derived',
        effective_at: null,
        completeness: 'complete',
      },
      event: { kind: 'source_created', generation: context.expected_source.generation },
      native_evidence: input.native_evidence,
    },
    sourceContext,
  );
  const nativeEvidence = exactRecord(input.native_evidence, ['kind'], 'artifact availability native evidence');
  if (nativeEvidence.kind !== 'engine_control') {
    throw new ContractValidationError('artifact availability native evidence must be engine control only');
  }

  return {
    scoped_artifact_availability_envelope_contract_version: SCOPED_ARTIFACT_AVAILABILITY_ENVELOPE_CONTRACT_VERSION,
    contract_version: selection.envelope_contract_version,
    contract_selection: selection,
    observer_sequence: observerSequence,
    scope_epoch: scopeEpoch,
    event_id: eventId,
    semantic_revision_ref: null,
    root: common.root,
    actor: common.actor,
    actor_attribution: { kind: 'scope_fallback', reason: 'source_lifecycle_control' },
    affiliations: common.affiliations,
    source: common.source,
    native_time: null,
    observed_at: observedAt,
    phase: input.phase,
    evidence: {
      authority: 'common_reducer',
      quality: 'derived',
      effective_at: null,
      completeness,
    },
    event: { kind: 'artifact_availability', entry },
    native_evidence: { kind: 'engine_control' },
  };
}
