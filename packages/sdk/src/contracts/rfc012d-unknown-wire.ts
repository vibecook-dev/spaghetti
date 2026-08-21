/** Bounded sidecar negotiation and preservation for additive RFC 012D events.
 *
 * When negotiated, the sidecar is bound to an exact already-negotiated
 * observation selection. It does not authorize native access, interpret an
 * unknown event, or create an observer transport.
 */

import {
  ContractValidationError,
  parseOpaqueContractReference,
  parseSemanticRevisionRef,
  type OpaqueContractReference,
  type SemanticRevisionRef,
} from './rfc012a.js';
import { parseObservationContractSelectionForExpected, type ObservationContractSelection } from './rfc012d.js';

export const OBSERVATION_UNKNOWN_WIRE_NEGOTIATION_CONTRACT_VERSION = 1 as const;
export const OBSERVATION_UNKNOWN_WIRE_EVENT_CONTRACT_VERSION = 1 as const;

const MAX_UNKNOWN_WIRE_PRESERVED_BYTES = 64 * 1024;
const MAX_UNKNOWN_WIRE_DEPTH = 16;
const MAX_UNKNOWN_WIRE_NODES = 1_024;
const MAX_UNKNOWN_WIRE_TYPE_TAG_BYTES = 128;
const MAX_UNKNOWN_WIRE_OBJECT_KEY_BYTES = 256;
const textEncoder = new TextEncoder();

const KNOWN_EVENT_TYPE_TAGS = new Set([
  'actor_affiliation',
  'actor_run',
  'artifact_availability',
  'observer_bootstrap_complete',
  'observer_failed',
  'observer_resync_complete',
  'observer_resync_required',
  'observer_resync_started',
  'source_created',
  'source_deleted',
  'source_object_error',
  'source_reset',
  'usage_v2',
]);

type JsonPrimitive = null | boolean | number | string;
export type ObservationUnknownWireJson =
  | JsonPrimitive
  | ObservationUnknownWireJson[]
  | { [key: string]: ObservationUnknownWireJson };
type UnknownRecord = Record<string, unknown>;

export type ObservationUnknownWireCompatibilityAxis =
  | 'event_contract_version'
  | 'type_tag_preservation'
  | 'encoded_value_preservation'
  | 'envelope_provenance_preservation';

export class IncompatibleObservationUnknownWireContractError extends ContractValidationError {
  readonly axis: ObservationUnknownWireCompatibilityAxis;

  constructor(axis: ObservationUnknownWireCompatibilityAxis) {
    super(`IncompatibleObservationUnknownWireContract: ${axis}`);
    this.name = 'IncompatibleObservationUnknownWireContract';
    this.axis = axis;
  }
}

export interface ObservationUnknownWireCapability {
  unknown_wire_event_contract_version: number;
  preserves_type_tag: boolean;
  preserves_encoded_value: boolean;
  preserves_envelope_provenance: boolean;
  max_preserved_bytes: number;
}

export interface ObservationUnknownWireContractRequest {
  observation_unknown_wire_negotiation_contract_version: typeof OBSERVATION_UNKNOWN_WIRE_NEGOTIATION_CONTRACT_VERSION;
  capability: ObservationUnknownWireCapability;
}

export interface ObservationUnknownWireContractOffer {
  observation_unknown_wire_negotiation_contract_version: typeof OBSERVATION_UNKNOWN_WIRE_NEGOTIATION_CONTRACT_VERSION;
  capability: ObservationUnknownWireCapability;
}

export interface ObservationUnknownWireContractSelection {
  observation_unknown_wire_negotiation_contract_version: typeof OBSERVATION_UNKNOWN_WIRE_NEGOTIATION_CONTRACT_VERSION;
  observation_selection: ObservationContractSelection;
  capability: ObservationUnknownWireCapability;
}

export interface ObservationUnknownWireSource {
  instance_key: OpaqueContractReference;
  stream_key: OpaqueContractReference;
  object_key: OpaqueContractReference;
  generation: number;
}

export interface ObservationUnknownWireProvenance {
  observation_selection: ObservationContractSelection;
  observer_sequence: number;
  scope_epoch: number;
  event_id: OpaqueContractReference;
  semantic_revision_ref: SemanticRevisionRef | null;
  source: ObservationUnknownWireSource;
  observed_at: number;
  phase: 'bootstrap' | 'live' | 'correction';
  additional_envelope_provenance: Record<string, ObservationUnknownWireJson>;
}

export interface ObservationUnknownWireEvent {
  unknown_wire_event_contract_version: typeof OBSERVATION_UNKNOWN_WIRE_EVENT_CONTRACT_VERSION;
  family: 'unknown_wire_event';
  type_tag: string;
  encoded_value: ObservationUnknownWireJson;
  envelope_provenance: ObservationUnknownWireProvenance;
}

function plainRecord(value: unknown, label: string): UnknownRecord {
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
  const input = plainRecord(value, label);
  let count = 0;
  for (const field in input) {
    if (!Object.prototype.hasOwnProperty.call(input, field)) continue;
    count += 1;
    if (!fields.includes(field)) throw new ContractValidationError(`${label} contains unknown field ${field}`);
  }
  for (const field of fields) {
    if (!Object.prototype.hasOwnProperty.call(input, field)) {
      throw new ContractValidationError(`${label} is missing field ${field}`);
    }
  }
  if (count !== fields.length) throw new ContractValidationError(`${label} has an invalid field count`);
  return input;
}

function positiveU32(value: unknown, label: string): number {
  if (!Number.isInteger(value) || (value as number) <= 0 || (value as number) > 0xffff_ffff) {
    throw new ContractValidationError(`${label} must be a positive u32`);
  }
  return value as number;
}

function positiveSafeInteger(value: unknown, label: string): number {
  if (!Number.isSafeInteger(value) || (value as number) <= 0) {
    throw new ContractValidationError(`${label} must be a positive safe integer`);
  }
  return value as number;
}

function safeInteger(value: unknown, label: string): number {
  if (!Number.isSafeInteger(value)) throw new ContractValidationError(`${label} must be a safe integer`);
  return value as number;
}

function parseCapability(value: unknown, selected: boolean): ObservationUnknownWireCapability {
  const input = exactRecord(
    value,
    [
      'unknown_wire_event_contract_version',
      'preserves_type_tag',
      'preserves_encoded_value',
      'preserves_envelope_provenance',
      'max_preserved_bytes',
    ],
    'observation unknown-wire capability',
  );
  const maxPreservedBytes = positiveU32(input.max_preserved_bytes, 'unknown-wire preserved-byte bound');
  if (maxPreservedBytes > MAX_UNKNOWN_WIRE_PRESERVED_BYTES) {
    throw new ContractValidationError(
      `unknown-wire preserved-byte bound must be at most ${MAX_UNKNOWN_WIRE_PRESERVED_BYTES}`,
    );
  }
  if (
    typeof input.preserves_type_tag !== 'boolean' ||
    typeof input.preserves_encoded_value !== 'boolean' ||
    typeof input.preserves_envelope_provenance !== 'boolean'
  ) {
    throw new ContractValidationError('unknown-wire preservation flags must be boolean');
  }
  const result: ObservationUnknownWireCapability = {
    unknown_wire_event_contract_version: positiveU32(
      input.unknown_wire_event_contract_version,
      'unknown-wire event contract version',
    ),
    preserves_type_tag: input.preserves_type_tag,
    preserves_encoded_value: input.preserves_encoded_value,
    preserves_envelope_provenance: input.preserves_envelope_provenance,
    max_preserved_bytes: maxPreservedBytes,
  };
  if (
    selected &&
    (result.unknown_wire_event_contract_version !== OBSERVATION_UNKNOWN_WIRE_EVENT_CONTRACT_VERSION ||
      !result.preserves_type_tag ||
      !result.preserves_encoded_value ||
      !result.preserves_envelope_provenance)
  ) {
    throw new ContractValidationError('unknown-wire selection does not preserve the exact v1 carrier');
  }
  return result;
}

export function parseObservationUnknownWireContractRequest(value: unknown): ObservationUnknownWireContractRequest {
  const input = exactRecord(
    value,
    ['observation_unknown_wire_negotiation_contract_version', 'capability'],
    'observation unknown-wire request',
  );
  if (
    input.observation_unknown_wire_negotiation_contract_version !==
    OBSERVATION_UNKNOWN_WIRE_NEGOTIATION_CONTRACT_VERSION
  ) {
    throw new ContractValidationError('unsupported unknown-wire request version');
  }
  return {
    observation_unknown_wire_negotiation_contract_version: OBSERVATION_UNKNOWN_WIRE_NEGOTIATION_CONTRACT_VERSION,
    capability: parseCapability(input.capability, false),
  };
}

export function parseObservationUnknownWireContractOffer(value: unknown): ObservationUnknownWireContractOffer {
  const input = exactRecord(
    value,
    ['observation_unknown_wire_negotiation_contract_version', 'capability'],
    'observation unknown-wire offer',
  );
  if (
    input.observation_unknown_wire_negotiation_contract_version !==
    OBSERVATION_UNKNOWN_WIRE_NEGOTIATION_CONTRACT_VERSION
  ) {
    throw new ContractValidationError('unsupported unknown-wire offer version');
  }
  return {
    observation_unknown_wire_negotiation_contract_version: OBSERVATION_UNKNOWN_WIRE_NEGOTIATION_CONTRACT_VERSION,
    capability: parseCapability(input.capability, false),
  };
}

function parseSelectionShape(
  value: unknown,
  expectedObservationSelection: ObservationContractSelection,
): ObservationUnknownWireContractSelection {
  const input = exactRecord(
    value,
    ['observation_unknown_wire_negotiation_contract_version', 'observation_selection', 'capability'],
    'observation unknown-wire selection',
  );
  if (
    input.observation_unknown_wire_negotiation_contract_version !==
    OBSERVATION_UNKNOWN_WIRE_NEGOTIATION_CONTRACT_VERSION
  ) {
    throw new ContractValidationError('unsupported unknown-wire selection version');
  }
  return {
    observation_unknown_wire_negotiation_contract_version: OBSERVATION_UNKNOWN_WIRE_NEGOTIATION_CONTRACT_VERSION,
    observation_selection: parseObservationContractSelectionForExpected(
      input.observation_selection,
      expectedObservationSelection,
    ),
    capability: parseCapability(input.capability, true),
  };
}

function capabilitiesEqual(left: ObservationUnknownWireCapability, right: ObservationUnknownWireCapability): boolean {
  return (
    left.unknown_wire_event_contract_version === right.unknown_wire_event_contract_version &&
    left.preserves_type_tag === right.preserves_type_tag &&
    left.preserves_encoded_value === right.preserves_encoded_value &&
    left.preserves_envelope_provenance === right.preserves_envelope_provenance &&
    left.max_preserved_bytes === right.max_preserved_bytes
  );
}

export function negotiateObservationUnknownWire(
  requestInput: unknown,
  offerInput: unknown,
  observationSelectionInput: ObservationContractSelection,
): ObservationUnknownWireContractSelection {
  const request = parseObservationUnknownWireContractRequest(requestInput);
  const offer = parseObservationUnknownWireContractOffer(offerInput);
  const observationSelection = parseObservationContractSelectionForExpected(
    observationSelectionInput,
    observationSelectionInput,
  );
  if (
    request.capability.unknown_wire_event_contract_version !== OBSERVATION_UNKNOWN_WIRE_EVENT_CONTRACT_VERSION ||
    offer.capability.unknown_wire_event_contract_version !== OBSERVATION_UNKNOWN_WIRE_EVENT_CONTRACT_VERSION
  ) {
    throw new IncompatibleObservationUnknownWireContractError('event_contract_version');
  }
  for (const [requested, offered, axis] of [
    [request.capability.preserves_type_tag, offer.capability.preserves_type_tag, 'type_tag_preservation'],
    [
      request.capability.preserves_encoded_value,
      offer.capability.preserves_encoded_value,
      'encoded_value_preservation',
    ],
    [
      request.capability.preserves_envelope_provenance,
      offer.capability.preserves_envelope_provenance,
      'envelope_provenance_preservation',
    ],
  ] as const) {
    if (!requested || !offered) throw new IncompatibleObservationUnknownWireContractError(axis);
  }
  return {
    observation_unknown_wire_negotiation_contract_version: OBSERVATION_UNKNOWN_WIRE_NEGOTIATION_CONTRACT_VERSION,
    observation_selection: observationSelection,
    capability: {
      unknown_wire_event_contract_version: OBSERVATION_UNKNOWN_WIRE_EVENT_CONTRACT_VERSION,
      preserves_type_tag: true,
      preserves_encoded_value: true,
      preserves_envelope_provenance: true,
      max_preserved_bytes: Math.min(request.capability.max_preserved_bytes, offer.capability.max_preserved_bytes),
    },
  };
}

export function parseObservationUnknownWireContractSelection(
  value: unknown,
  requestInput: unknown,
  offerInput: unknown,
  observationSelectionInput: ObservationContractSelection,
): ObservationUnknownWireContractSelection {
  const parsed = parseSelectionShape(value, observationSelectionInput);
  const expected = negotiateObservationUnknownWire(requestInput, offerInput, observationSelectionInput);
  if (!capabilitiesEqual(parsed.capability, expected.capability)) {
    throw new ContractValidationError('unknown-wire selection does not match the exact negotiated result');
  }
  return parsed;
}

export function parseObservationUnknownWireContractSelectionForExpected(
  value: unknown,
  expected: ObservationUnknownWireContractSelection,
): ObservationUnknownWireContractSelection {
  const parsed = parseSelectionShape(value, expected.observation_selection);
  if (!capabilitiesEqual(parsed.capability, expected.capability)) {
    throw new ContractValidationError('unknown-wire selection does not match caller-held state');
  }
  return parsed;
}

interface UnknownWireBudget {
  bytes: number;
  nodes: number;
  maxBytes: number;
}

function addBudgetBytes(budget: UnknownWireBudget, bytes: number): void {
  budget.bytes += bytes;
  if (!Number.isSafeInteger(budget.bytes) || budget.bytes > budget.maxBytes) {
    throw new ContractValidationError(
      `unknown-wire preserved value exceeds the negotiated ${budget.maxBytes} byte bound`,
    );
  }
}

function addBudgetNode(budget: UnknownWireBudget): void {
  budget.nodes += 1;
  if (budget.nodes > MAX_UNKNOWN_WIRE_NODES) {
    throw new ContractValidationError(`unknown-wire preserved value exceeds ${MAX_UNKNOWN_WIRE_NODES} nodes`);
  }
  addBudgetBytes(budget, 1);
}

function hasUnpairedSurrogate(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (!Number.isInteger(next) || next < 0xdc00 || next > 0xdfff) return true;
      index += 1;
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      return true;
    }
  }
  return false;
}

function utf8BytesBounded(value: string, maxBytes: number, label: string): number {
  if (value.length > maxBytes || hasUnpairedSurrogate(value)) {
    throw new ContractValidationError(`${label} exceeds its byte bound or is not well-formed Unicode`);
  }
  const bytes = textEncoder.encode(value).byteLength;
  if (bytes > maxBytes) throw new ContractValidationError(`${label} exceeds its byte bound`);
  return bytes;
}

function hasAsciiControl(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const codeUnit = value.charCodeAt(index);
    if (codeUnit <= 0x1f || codeUnit === 0x7f) return true;
  }
  return false;
}

function validateObjectKey(value: string): string {
  if (
    value.length === 0 ||
    value.length > MAX_UNKNOWN_WIRE_OBJECT_KEY_BYTES ||
    value.trim() !== value ||
    hasAsciiControl(value) ||
    value === '__proto__' ||
    value === 'prototype' ||
    value === 'constructor' ||
    utf8BytesBounded(value, MAX_UNKNOWN_WIRE_OBJECT_KEY_BYTES, 'unknown-wire object key') >
      MAX_UNKNOWN_WIRE_OBJECT_KEY_BYTES
  ) {
    throw new ContractValidationError('unknown-wire object key is not canonical');
  }
  return value;
}

function cloneBoundedJson(value: unknown, depth: number, budget: UnknownWireBudget): ObservationUnknownWireJson {
  if (depth > MAX_UNKNOWN_WIRE_DEPTH) {
    throw new ContractValidationError(`unknown-wire preserved value exceeds depth ${MAX_UNKNOWN_WIRE_DEPTH}`);
  }
  addBudgetNode(budget);
  if (value === null) return null;
  if (typeof value === 'boolean') {
    addBudgetBytes(budget, 1);
    return value;
  }
  if (typeof value === 'number') {
    if (!Number.isSafeInteger(value)) {
      throw new ContractValidationError('unknown-wire numbers must be JavaScript-safe integers');
    }
    addBudgetBytes(budget, 8);
    return value;
  }
  if (typeof value === 'string') {
    addBudgetBytes(budget, utf8BytesBounded(value, budget.maxBytes, 'unknown-wire string'));
    return value;
  }
  if (Array.isArray(value)) {
    if (value.length > MAX_UNKNOWN_WIRE_NODES - budget.nodes) {
      throw new ContractValidationError(`unknown-wire preserved value exceeds ${MAX_UNKNOWN_WIRE_NODES} nodes`);
    }
    const result: ObservationUnknownWireJson[] = [];
    for (const entry of value) result.push(cloneBoundedJson(entry, depth + 1, budget));
    return result;
  }
  if (typeof value === 'object') {
    const prototype = Object.getPrototypeOf(value);
    if (prototype !== Object.prototype && prototype !== null) {
      throw new ContractValidationError('unknown-wire preserved value contains a non-JSON object');
    }
    const result: Record<string, ObservationUnknownWireJson> = {};
    for (const rawKey in value as UnknownRecord) {
      if (!Object.prototype.hasOwnProperty.call(value, rawKey)) continue;
      const entry = (value as UnknownRecord)[rawKey];
      const key = validateObjectKey(rawKey);
      addBudgetBytes(budget, utf8BytesBounded(key, budget.maxBytes, 'unknown-wire object key'));
      result[key] = cloneBoundedJson(entry, depth + 1, budget);
    }
    return result;
  }
  throw new ContractValidationError('unknown-wire preserved value contains a non-JSON value');
}

function validateExactEncodedBound(
  encodedValue: ObservationUnknownWireJson,
  additionalEnvelopeProvenance: Record<string, ObservationUnknownWireJson>,
  maxBytes: number,
): void {
  let total = 0;
  for (const value of [encodedValue, additionalEnvelopeProvenance]) {
    const encoded = JSON.stringify(value);
    if (encoded.length > maxBytes - total) {
      throw new ContractValidationError(`unknown-wire encoded value exceeds the negotiated ${maxBytes} byte bound`);
    }
    total += utf8BytesBounded(encoded, maxBytes - total, 'unknown-wire encoded value');
  }
}

function typeTag(value: unknown): string {
  if (
    typeof value !== 'string' ||
    value.length === 0 ||
    value.length > MAX_UNKNOWN_WIRE_TYPE_TAG_BYTES ||
    !/^[a-z][a-z0-9._-]*$/.test(value) ||
    KNOWN_EVENT_TYPE_TAGS.has(value) ||
    utf8BytesBounded(value, MAX_UNKNOWN_WIRE_TYPE_TAG_BYTES, 'unknown-wire type tag') > MAX_UNKNOWN_WIRE_TYPE_TAG_BYTES
  ) {
    throw new ContractValidationError('unknown-wire type tag is noncanonical or shadows a known event');
  }
  return value;
}

function parseUnknownSource(value: unknown): ObservationUnknownWireSource {
  const input = exactRecord(value, ['instance_key', 'stream_key', 'object_key', 'generation'], 'unknown-wire source');
  return {
    instance_key: parseOpaqueContractReference(input.instance_key, 'unknown-wire source instance key'),
    stream_key: parseOpaqueContractReference(input.stream_key, 'unknown-wire stream key'),
    object_key: parseOpaqueContractReference(input.object_key, 'unknown-wire object key'),
    generation: positiveSafeInteger(input.generation, 'unknown-wire source generation'),
  };
}

export function parseObservationUnknownWireEvent(
  value: unknown,
  selectionInput: ObservationUnknownWireContractSelection,
): ObservationUnknownWireEvent {
  const selection = parseObservationUnknownWireContractSelectionForExpected(selectionInput, selectionInput);
  const input = exactRecord(
    value,
    ['unknown_wire_event_contract_version', 'family', 'type_tag', 'encoded_value', 'envelope_provenance'],
    'observation unknown-wire event',
  );
  if (
    input.unknown_wire_event_contract_version !== OBSERVATION_UNKNOWN_WIRE_EVENT_CONTRACT_VERSION ||
    input.family !== 'unknown_wire_event'
  ) {
    throw new ContractValidationError('unknown-wire event does not match the exact v1 family');
  }
  const parsedTypeTag = typeTag(input.type_tag);
  const provenance = exactRecord(
    input.envelope_provenance,
    [
      'observation_selection',
      'observer_sequence',
      'scope_epoch',
      'event_id',
      'semantic_revision_ref',
      'source',
      'observed_at',
      'phase',
      'additional_envelope_provenance',
    ],
    'unknown-wire envelope provenance',
  );
  const phase = provenance.phase;
  if (phase !== 'bootstrap' && phase !== 'live' && phase !== 'correction') {
    throw new ContractValidationError('unknown-wire phase is not canonical');
  }
  const additional = plainRecord(
    provenance.additional_envelope_provenance,
    'unknown-wire additional envelope provenance',
  );
  const budget: UnknownWireBudget = { bytes: 0, nodes: 0, maxBytes: selection.capability.max_preserved_bytes };
  const encodedValue = cloneBoundedJson(input.encoded_value, 1, budget);
  addBudgetNode(budget);
  const additionalEnvelopeProvenance: Record<string, ObservationUnknownWireJson> = {};
  for (const rawKey in additional) {
    if (!Object.prototype.hasOwnProperty.call(additional, rawKey)) continue;
    const entry = additional[rawKey];
    const key = validateObjectKey(rawKey);
    addBudgetBytes(budget, utf8BytesBounded(key, budget.maxBytes, 'unknown-wire object key'));
    additionalEnvelopeProvenance[key] = cloneBoundedJson(entry, 1, budget);
  }
  validateExactEncodedBound(encodedValue, additionalEnvelopeProvenance, selection.capability.max_preserved_bytes);
  const semanticRevision =
    provenance.semantic_revision_ref === null ? null : parseSemanticRevisionRef(provenance.semantic_revision_ref);
  return {
    unknown_wire_event_contract_version: OBSERVATION_UNKNOWN_WIRE_EVENT_CONTRACT_VERSION,
    family: 'unknown_wire_event',
    type_tag: parsedTypeTag,
    encoded_value: encodedValue,
    envelope_provenance: {
      observation_selection: parseObservationContractSelectionForExpected(
        provenance.observation_selection,
        selection.observation_selection,
      ),
      observer_sequence: positiveSafeInteger(provenance.observer_sequence, 'unknown-wire observer sequence'),
      scope_epoch: positiveSafeInteger(provenance.scope_epoch, 'unknown-wire scope epoch'),
      event_id: parseOpaqueContractReference(provenance.event_id, 'unknown-wire event ID'),
      semantic_revision_ref: semanticRevision,
      source: parseUnknownSource(provenance.source),
      observed_at: safeInteger(provenance.observed_at, 'unknown-wire observed_at'),
      phase,
      additional_envelope_provenance: additionalEnvelopeProvenance,
    },
  };
}

export function serializeObservationUnknownWireEvent(
  value: ObservationUnknownWireEvent,
  selection: ObservationUnknownWireContractSelection,
): ObservationUnknownWireEvent {
  return parseObservationUnknownWireEvent(value, selection);
}
