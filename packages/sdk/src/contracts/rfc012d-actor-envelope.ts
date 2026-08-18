/** Strict RFC 012D wire projection for the selected RFC 012C actor-run and
 * actor-affiliation event families.
 *
 * Consumption is contextual: the repeated selection, root, and source
 * coordinate must match authority already retained by the caller. Rust is the
 * sole authority that derives and recomputes opaque semantic revision and
 * event identities; this portable parser validates their canonical wire shape
 * together with every inspectable relation, occurrence, and lifecycle rule.
 * Native payloads remain withheld and this module does not expose a source
 * access path or the complete observation event union.
 */

import {
  ContractValidationError,
  parseOpaqueContractReference,
  parseSemanticRevisionRef,
  type OpaqueContractReference,
  type SemanticRevisionRef,
} from './rfc012a.js';
import {
  ACTOR_AFFILIATION_FAMILY,
  ACTOR_AFFILIATION_FAMILY_VERSION,
  ACTOR_RUN_FAMILY,
  ACTOR_RUN_FAMILY_VERSION,
  parseActorAffiliationRevision,
  parseActorRunRevision,
  type ActorAffiliationRevision,
  type ActorRunRevision,
  type QualifiedTimestamp,
} from './rfc012c.js';
import { parseObservationContractSelectionForExpected, type ObservationContractSelection } from './rfc012d.js';
import {
  parseScopedUsageEnvelopeContext,
  parseScopedUsageRoot,
  type ScopedUsageEnvelopeContext,
  type ScopedUsageRoot,
  type ScopedUsageSourceBinding,
} from './rfc012d-usage-envelope.js';

export const SCOPED_ACTOR_ENVELOPE_CONTRACT_VERSION = 1 as const;

const MAX_CURSOR_BYTES = 128;
const MAX_MEDIA_TYPE_BYTES = 256;
const MAX_RUNTIME_TEXT_BYTES = 8 * 1024;
const MAX_IDENTIFIER_BYTES = 256;
const MAX_AFFILIATION_REVISIONS = 64;
const MAX_U32 = 0xffff_ffff;
const textEncoder = new TextEncoder();

type UnknownRecord = Record<string, unknown>;

export type ScopedActorRoot = ScopedUsageRoot;
export type ScopedActorSourceBinding = ScopedUsageSourceBinding;
export type ScopedActorEnvelopeContext = ScopedUsageEnvelopeContext;

export interface ScopedActorContext {
  root_session_key: OpaqueContractReference;
  run_key: OpaqueContractReference;
  role: 'root' | 'child';
  parent_run_key: OpaqueContractReference | null;
  native_session_id: string | null;
  native_actor_id: string | null;
  native_actor_type: string | null;
}

export interface ScopedActorAffiliations {
  actor_run_key: OpaqueContractReference;
  team_key: OpaqueContractReference | null;
  native_team_id: string | null;
  team_name: string | null;
  member_key: OpaqueContractReference | null;
  workflow_key: OpaqueContractReference | null;
  native_workflow_id: string | null;
  completeness: 'partial' | 'unknown';
  derived_from_revision_refs: SemanticRevisionRef[];
}

export interface ScopedActorSource extends ScopedActorSourceBinding {
  locator_id: null;
  generation: number;
  source_record_id: OpaqueContractReference;
  record_index: number;
  cursor_start: string;
  cursor_end: string;
  byte_range: { start: number; end: number };
}

export type ScopedActorRetraction =
  | {
      kind: 'reset';
      old_generation: number;
      new_generation: number;
      reason: 'truncated' | 'identity_changed' | 'prefix_mismatch' | 'contract_replay';
    }
  | { kind: 'source_deleted'; generation: number };

export interface ScopedActorRunEvent {
  kind: 'actor_run';
  fact_family: typeof ACTOR_RUN_FAMILY;
  fact_family_contract_version: typeof ACTOR_RUN_FAMILY_VERSION;
  fact_id: OpaqueContractReference;
  operation: 'upsert' | 'retract';
  retraction: ScopedActorRetraction | null;
  revision: ActorRunRevision;
}

export interface ScopedActorAffiliationEvent {
  kind: 'actor_affiliation';
  fact_family: typeof ACTOR_AFFILIATION_FAMILY;
  fact_family_contract_version: typeof ACTOR_AFFILIATION_FAMILY_VERSION;
  fact_id: OpaqueContractReference;
  operation: 'upsert' | 'retract';
  retraction: ScopedActorRetraction | null;
  revision: ActorAffiliationRevision;
}

export type ScopedActorEvent = ScopedActorRunEvent | ScopedActorAffiliationEvent;

export interface ScopedActorEnvelope {
  scoped_actor_envelope_contract_version: typeof SCOPED_ACTOR_ENVELOPE_CONTRACT_VERSION;
  contract_version: number;
  contract_selection: ObservationContractSelection;
  observer_sequence: number;
  scope_epoch: number;
  event_id: string;
  semantic_revision_ref: SemanticRevisionRef;
  root: ScopedActorRoot;
  actor: ScopedActorContext;
  actor_attribution: 'derived_exact';
  affiliations: ScopedActorAffiliations;
  source: ScopedActorSource;
  native_time: QualifiedTimestamp | null;
  observed_at: number;
  phase: 'bootstrap' | 'live' | 'correction';
  evidence: {
    authority: 'native_record' | 'common_reducer';
    quality: 'exact' | 'derived';
    effective_at: QualifiedTimestamp | null;
    completeness: 'complete';
  };
  event: ScopedActorEvent;
  native_evidence: {
    kind: 'withheld';
    media_type: string;
    state: 'present';
    payload_hash: string;
    reason: 'projection_boundary';
  };
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

function assertKnownFields(input: UnknownRecord, fields: readonly string[], label: string): void {
  const known = new Set(fields);
  for (const key of Object.keys(input)) {
    if (!known.has(key)) throw new ContractValidationError(`${label} contains unknown field ${key}`);
  }
}

function assertRequiredFields(input: UnknownRecord, fields: readonly string[], label: string): void {
  for (const field of fields) {
    if (!Object.hasOwn(input, field)) throw new ContractValidationError(`${label} is missing field ${field}`);
  }
}

function boundedText(value: unknown, label: string, maxBytes: number): string {
  if (
    typeof value !== 'string' ||
    value.length === 0 ||
    value.length > maxBytes ||
    value.trim() !== value ||
    [...value].some((character) => /\p{Cc}/u.test(character)) ||
    textEncoder.encode(value).byteLength > maxBytes
  ) {
    throw new ContractValidationError(`${label} is not bounded canonical text`);
  }
  return value;
}

function nullableText(value: unknown, label: string): string | null {
  return value === null ? null : boundedText(value, label, MAX_RUNTIME_TEXT_BYTES);
}

function safeInteger(value: unknown, label: string): number {
  if (!Number.isSafeInteger(value)) throw new ContractValidationError(`${label} must be a safe integer`);
  return value as number;
}

function positiveSafeInteger(value: unknown, label: string): number {
  const parsed = safeInteger(value, label);
  if (parsed <= 0) throw new ContractValidationError(`${label} must be greater than zero`);
  return parsed;
}

function nonnegativeSafeInteger(value: unknown, label: string): number {
  const parsed = safeInteger(value, label);
  if (parsed < 0) throw new ContractValidationError(`${label} must be non-negative`);
  return parsed;
}

function decodeOpaqueBytes(value: unknown, label: string, maxBytes: number): Uint8Array {
  const maxEncodedBytes = 3 + Math.ceil((maxBytes * 4) / 3);
  if (
    typeof value !== 'string' ||
    value.length <= 3 ||
    value.length > maxEncodedBytes ||
    !/^v1:[A-Za-z0-9_-]+$/.test(value)
  ) {
    throw new ContractValidationError(`${label} is not canonical v1 base64url`);
  }
  const encoded = value.slice(3);
  const standard = encoded.replace(/-/g, '+').replace(/_/g, '/');
  const padded = standard.padEnd(Math.ceil(standard.length / 4) * 4, '=');
  let binary: string;
  try {
    binary = atob(padded);
  } catch {
    throw new ContractValidationError(`${label} is not canonical v1 base64url`);
  }
  const bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0));
  let roundTrip = '';
  for (const byte of bytes) roundTrip += String.fromCharCode(byte);
  const canonical = btoa(roundTrip).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/g, '');
  if (bytes.byteLength === 0 || bytes.byteLength > maxBytes || canonical !== encoded) {
    throw new ContractValidationError(`${label} has invalid bounds or encoding`);
  }
  return bytes;
}

function parseNonzeroReference(value: unknown, label: string): OpaqueContractReference {
  const reference = parseOpaqueContractReference(value, label);
  const bytes = decodeOpaqueBytes(reference, label, 32);
  if (bytes.byteLength !== 32 || bytes.every((byte) => byte === 0)) {
    throw new ContractValidationError(`${label} must be a nonzero 32-byte reference`);
  }
  return reference;
}

function parseFixedOpaque(value: unknown, label: string): string {
  const bytes = decodeOpaqueBytes(value, label, 32);
  if (bytes.byteLength !== 32 || bytes.every((byte) => byte === 0)) {
    throw new ContractValidationError(`${label} must contain nonzero 32 bytes`);
  }
  return value as string;
}

function compareReferences(left: OpaqueContractReference, right: OpaqueContractReference): number {
  const leftBytes = decodeOpaqueBytes(left, 'left semantic revision', 32);
  const rightBytes = decodeOpaqueBytes(right, 'right semantic revision', 32);
  for (let index = 0; index < leftBytes.length; index += 1) {
    const delta = leftBytes[index]! - rightBytes[index]!;
    if (delta !== 0) return delta;
  }
  return 0;
}

function parseTimestamp(value: unknown, label: string): QualifiedTimestamp | null {
  if (value === null) return null;
  const input = record(value, label);
  assertKnownFields(input, ['value', 'quality'], label);
  assertRequiredFields(input, ['value', 'quality'], label);
  if (
    input.quality !== 'NativeExact' &&
    input.quality !== 'NativeApproximate' &&
    input.quality !== 'FileMetadataFallback' &&
    input.quality !== 'Derived'
  ) {
    throw new ContractValidationError(`${label} has an unsupported timestamp quality`);
  }
  return {
    value: boundedText(input.value, `${label} value`, MAX_RUNTIME_TEXT_BYTES),
    quality: input.quality,
  };
}

function parseAppendCursor(value: unknown, label: string): { wire: string; offset: number } {
  const bytes = decodeOpaqueBytes(value, label, MAX_CURSOR_BYTES);
  if (bytes.byteLength !== 10 || bytes[0] !== 1 || bytes[1] !== 1) {
    throw new ContractValidationError(`${label} is not an RFC 012 append cursor`);
  }
  let offset = 0n;
  for (const byte of bytes.subarray(2)) offset = (offset << 8n) | BigInt(byte);
  if (offset > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new ContractValidationError(`${label} exceeds the portable integer range`);
  }
  return { wire: value as string, offset: Number(offset) };
}

function validateRoot(value: unknown): ScopedActorRoot {
  const root = parseScopedUsageRoot(value);
  parseNonzeroReference(root.session_ref.entity_key, 'actor root external entity key');
  parseNonzeroReference(root.session_key, 'actor root session key');
  parseNonzeroReference(root.root_actor_run_key, 'actor root actor-run key');
  if (root.native_session_claim !== null) {
    boundedText(root.native_session_claim.identity.authority, 'native session claim authority', MAX_IDENTIFIER_BYTES);
    if (root.native_session_claim.identity.value !== null) {
      boundedText(
        root.native_session_claim.identity.value.native_namespace,
        'native session namespace',
        MAX_IDENTIFIER_BYTES,
      );
      boundedText(root.native_session_claim.identity.value.native_id, 'native session id', MAX_IDENTIFIER_BYTES);
    }
    const provenance = root.native_session_claim.identity.provenance as SemanticRevisionRef[];
    for (const reference of provenance) {
      parseNonzeroReference(reference.fact_revision_id, 'native session claim provenance');
    }
  }
  return root;
}

export function parseScopedActorEnvelopeContext(value: unknown): ScopedActorEnvelopeContext {
  const context = parseScopedUsageEnvelopeContext(value);
  validateRoot(context.root);
  for (const source of context.authorized_sources) {
    parseNonzeroReference(source.instance_key, 'authorized actor source instance key');
    parseNonzeroReference(source.stream_key, 'authorized actor source stream key');
    parseNonzeroReference(source.object_key, 'authorized actor source object key');
  }
  return context;
}

function parseActor(
  value: unknown,
  root: ScopedActorRoot,
  selection: ObservationContractSelection,
): ScopedActorContext {
  const input = record(value, 'actor context');
  const fields = [
    'root_session_key',
    'run_key',
    'role',
    'parent_run_key',
    'native_session_id',
    'native_actor_id',
    'native_actor_type',
  ];
  assertKnownFields(input, fields, 'actor context');
  assertRequiredFields(input, fields, 'actor context');
  if (input.role !== 'root' && input.role !== 'child') {
    throw new ContractValidationError('actor context has an unsupported role');
  }
  const actor: ScopedActorContext = {
    root_session_key: parseNonzeroReference(input.root_session_key, 'actor root session key'),
    run_key: parseNonzeroReference(input.run_key, 'actor run key'),
    role: input.role,
    parent_run_key:
      input.parent_run_key === null ? null : parseNonzeroReference(input.parent_run_key, 'actor parent run key'),
    native_session_id: nullableText(input.native_session_id, 'actor native_session_id'),
    native_actor_id: nullableText(input.native_actor_id, 'actor native_actor_id'),
    native_actor_type: nullableText(input.native_actor_type, 'actor native_actor_type'),
  };
  const expectedRole = actor.run_key === root.root_actor_run_key ? 'root' : 'child';
  if (actor.root_session_key !== root.session_key || actor.role !== expectedRole) {
    throw new ContractValidationError('actor context does not match the exact root');
  }
  const actorSelected = selection.contract_versions.fact_family_versions[ACTOR_RUN_FAMILY] === ACTOR_RUN_FAMILY_VERSION;
  if (actorSelected) {
    if (
      (actor.role === 'root' && actor.parent_run_key !== null) ||
      (actor.role === 'child' &&
        (actor.parent_run_key === actor.run_key ||
          (actor.parent_run_key === null && (actor.native_actor_id !== null || actor.native_actor_type !== null))))
    ) {
      throw new ContractValidationError('selected actor context has invalid parent lineage');
    }
  } else if (actor.parent_run_key !== null || actor.native_actor_id !== null || actor.native_actor_type !== null) {
    throw new ContractValidationError('actor enrichment requires selected runtime.actor-run@1');
  }
  const expectedNativeSession = root.native_session_claim?.identity.value?.native_id ?? null;
  if (actor.native_session_id !== expectedNativeSession) {
    throw new ContractValidationError('actor native session does not match the root claim');
  }
  return actor;
}

function parseAffiliations(
  value: unknown,
  actor: ScopedActorContext,
  selection: ObservationContractSelection,
): ScopedActorAffiliations {
  const input = record(value, 'actor affiliations');
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
  assertKnownFields(input, fields, 'actor affiliations');
  assertRequiredFields(input, fields, 'actor affiliations');
  if (!Array.isArray(input.derived_from_revision_refs)) {
    throw new ContractValidationError('actor affiliation revision refs must be an array');
  }
  if (input.derived_from_revision_refs.length > MAX_AFFILIATION_REVISIONS) {
    throw new ContractValidationError('actor affiliation revision refs exceed their bound');
  }
  const revisions = input.derived_from_revision_refs.map(parseSemanticRevisionRef);
  for (const reference of revisions) {
    parseNonzeroReference(reference.fact_revision_id, 'actor affiliation revision ref');
  }
  if (
    revisions.some(
      (reference) =>
        reference.semantic_reference_contract_version !==
        selection.contract_versions.semantic_revision_reference_version,
    ) ||
    revisions.some(
      (reference, index) =>
        index > 0 && compareReferences(revisions[index - 1]!.fact_revision_id, reference.fact_revision_id) >= 0,
    )
  ) {
    throw new ContractValidationError('actor affiliation revision refs are not canonical for the selection');
  }
  if (input.completeness !== 'partial' && input.completeness !== 'unknown') {
    throw new ContractValidationError('actor affiliations have unsupported completeness');
  }
  const result: ScopedActorAffiliations = {
    actor_run_key: parseNonzeroReference(input.actor_run_key, 'affiliation actor run key'),
    team_key: input.team_key === null ? null : parseNonzeroReference(input.team_key, 'affiliation team key'),
    native_team_id: nullableText(input.native_team_id, 'affiliation native_team_id'),
    team_name: nullableText(input.team_name, 'affiliation team_name'),
    member_key: input.member_key === null ? null : parseNonzeroReference(input.member_key, 'affiliation member key'),
    workflow_key:
      input.workflow_key === null ? null : parseNonzeroReference(input.workflow_key, 'affiliation workflow key'),
    native_workflow_id: nullableText(input.native_workflow_id, 'affiliation native_workflow_id'),
    completeness: input.completeness,
    derived_from_revision_refs: revisions,
  };
  if (result.actor_run_key !== actor.run_key) {
    throw new ContractValidationError('actor affiliation context targets a different actor');
  }
  const selected =
    selection.contract_versions.fact_family_versions[ACTOR_AFFILIATION_FAMILY] === ACTOR_AFFILIATION_FAMILY_VERSION;
  if (!selected) {
    if (
      result.team_key === null &&
      result.native_team_id === null &&
      result.team_name === null &&
      result.member_key === null &&
      result.workflow_key === null &&
      result.native_workflow_id === null &&
      result.completeness === 'unknown' &&
      result.derived_from_revision_refs.length === 0
    ) {
      return result;
    }
    throw new ContractValidationError('affiliation enrichment requires selected runtime.actor-affiliation@1');
  }
  if (
    result.team_name !== null ||
    (result.team_key === null && (result.native_team_id !== null || result.member_key !== null)) ||
    (result.workflow_key === null && result.native_workflow_id !== null) ||
    (result.completeness === 'partial' && result.derived_from_revision_refs.length === 0) ||
    (result.completeness === 'unknown' &&
      result.derived_from_revision_refs.length === 0 &&
      (result.team_key !== null ||
        result.native_team_id !== null ||
        result.member_key !== null ||
        result.workflow_key !== null ||
        result.native_workflow_id !== null))
  ) {
    throw new ContractValidationError('selected actor affiliation context is invalid');
  }
  return result;
}

function parseSource(value: unknown, context: ScopedActorEnvelopeContext): ScopedActorSource {
  const input = record(value, 'actor source');
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
  assertKnownFields(input, fields, 'actor source');
  assertRequiredFields(input, fields, 'actor source');
  const binding: ScopedActorSourceBinding = {
    instance_key: parseNonzeroReference(input.instance_key, 'actor source instance key'),
    stream_key: parseNonzeroReference(input.stream_key, 'actor source stream key'),
    object_key: parseNonzeroReference(input.object_key, 'actor source object key'),
  };
  if (
    !context.authorized_sources.some(
      (source) =>
        source.instance_key === binding.instance_key &&
        source.stream_key === binding.stream_key &&
        source.object_key === binding.object_key,
    )
  ) {
    throw new ContractValidationError('actor source is outside the caller-held authorized source set');
  }
  if (input.locator_id !== null) {
    throw new ContractValidationError('actor source cannot disclose a native locator');
  }
  const start = parseAppendCursor(input.cursor_start, 'actor cursor_start');
  const end = parseAppendCursor(input.cursor_end, 'actor cursor_end');
  const range = record(input.byte_range, 'actor byte range');
  assertKnownFields(range, ['start', 'end'], 'actor byte range');
  assertRequiredFields(range, ['start', 'end'], 'actor byte range');
  const rangeStart = nonnegativeSafeInteger(range.start, 'actor byte range start');
  const rangeEnd = nonnegativeSafeInteger(range.end, 'actor byte range end');
  if (start.offset > end.offset || rangeStart !== start.offset || rangeEnd !== end.offset) {
    throw new ContractValidationError('actor byte range does not match its append cursors');
  }
  const recordIndex = nonnegativeSafeInteger(input.record_index, 'actor record_index');
  if (recordIndex > MAX_U32) throw new ContractValidationError('actor record_index exceeds u32');
  return {
    ...binding,
    locator_id: null,
    generation: positiveSafeInteger(input.generation, 'actor source generation'),
    source_record_id: parseNonzeroReference(input.source_record_id, 'actor source record id'),
    record_index: recordIndex,
    cursor_start: start.wire,
    cursor_end: end.wire,
    byte_range: { start: rangeStart, end: rangeEnd },
  };
}

function parseRetraction(value: unknown): ScopedActorRetraction | null {
  if (value === null) return null;
  const input = record(value, 'actor retraction');
  if (input.kind === 'reset') {
    const fields = ['kind', 'old_generation', 'new_generation', 'reason'];
    assertKnownFields(input, fields, 'actor reset retraction');
    assertRequiredFields(input, fields, 'actor reset retraction');
    if (
      input.reason !== 'truncated' &&
      input.reason !== 'identity_changed' &&
      input.reason !== 'prefix_mismatch' &&
      input.reason !== 'contract_replay'
    ) {
      throw new ContractValidationError('actor reset retraction has an unsupported reason');
    }
    return {
      kind: 'reset',
      old_generation: positiveSafeInteger(input.old_generation, 'actor reset old_generation'),
      new_generation: positiveSafeInteger(input.new_generation, 'actor reset new_generation'),
      reason: input.reason,
    };
  }
  if (input.kind === 'source_deleted') {
    const fields = ['kind', 'generation'];
    assertKnownFields(input, fields, 'actor source-deleted retraction');
    assertRequiredFields(input, fields, 'actor source-deleted retraction');
    return {
      kind: 'source_deleted',
      generation: positiveSafeInteger(input.generation, 'actor deleted generation'),
    };
  }
  throw new ContractValidationError('actor retraction has an unsupported kind');
}

function assertActorRunRevisionShape(value: unknown): void {
  const input = record(value, 'actor-run revision');
  const fields = [
    'actor_run',
    'session',
    'role',
    'parent_actor_run',
    'native_session_id',
    'native_actor_id',
    'native_actor_type',
  ];
  assertKnownFields(input, fields, 'actor-run revision');
  assertRequiredFields(input, fields, 'actor-run revision');
}

function assertAffiliationRevisionShape(value: unknown): void {
  const input = record(value, 'actor-affiliation revision');
  const fields = [
    'affiliation',
    'actor_run',
    'session',
    'dimension',
    'target',
    'member',
    'native_target_id',
    'native_member_id',
    'state',
    'effective_at',
  ];
  assertKnownFields(input, fields, 'actor-affiliation revision');
  assertRequiredFields(input, fields, 'actor-affiliation revision');
  if (input.effective_at !== null) {
    const timestamp = record(input.effective_at, 'actor-affiliation effective_at');
    assertKnownFields(timestamp, ['value', 'quality'], 'actor-affiliation effective_at');
    assertRequiredFields(timestamp, ['value', 'quality'], 'actor-affiliation effective_at');
  }
}

function validateActorRunRevision(revision: ActorRunRevision): void {
  parseNonzeroReference(revision.actor_run, 'actor-run revision key');
  parseNonzeroReference(revision.session, 'actor-run session key');
  if (revision.parent_actor_run !== null) parseNonzeroReference(revision.parent_actor_run, 'actor-run parent key');
  for (const [label, value] of [
    ['native_session_id', revision.native_session_id],
    ['native_actor_id', revision.native_actor_id],
    ['native_actor_type', revision.native_actor_type],
  ] as const) {
    if (value !== null) boundedText(value, `actor-run ${label}`, MAX_RUNTIME_TEXT_BYTES);
  }
}

function validateAffiliationRevision(revision: ActorAffiliationRevision): void {
  for (const [label, value] of [
    ['affiliation key', revision.affiliation],
    ['affiliation actor-run key', revision.actor_run],
    ['affiliation session key', revision.session],
    ['affiliation target key', revision.target],
  ] as const) {
    parseNonzeroReference(value, label);
  }
  if (revision.member !== null) parseNonzeroReference(revision.member, 'affiliation member key');
  for (const [label, value] of [
    ['native_target_id', revision.native_target_id],
    ['native_member_id', revision.native_member_id],
  ] as const) {
    if (value !== null) boundedText(value, `actor-affiliation ${label}`, MAX_RUNTIME_TEXT_BYTES);
  }
  if (revision.effective_at !== null) {
    boundedText(revision.effective_at.value, 'actor-affiliation effective_at', MAX_RUNTIME_TEXT_BYTES);
  }
}

function parseEvent(value: unknown): ScopedActorEvent {
  const input = record(value, 'actor event');
  const fields = [
    'kind',
    'fact_family',
    'fact_family_contract_version',
    'fact_id',
    'operation',
    'retraction',
    'revision',
  ];
  assertKnownFields(input, fields, 'actor event');
  assertRequiredFields(input, fields, 'actor event');
  if (input.operation !== 'upsert' && input.operation !== 'retract') {
    throw new ContractValidationError('actor event has an unsupported operation');
  }
  const operation: 'upsert' | 'retract' = input.operation;
  const common = {
    fact_id: parseNonzeroReference(input.fact_id, 'actor fact id'),
    operation,
    retraction: parseRetraction(input.retraction),
  };
  if (
    input.kind === 'actor_run' &&
    input.fact_family === ACTOR_RUN_FAMILY &&
    input.fact_family_contract_version === ACTOR_RUN_FAMILY_VERSION
  ) {
    assertActorRunRevisionShape(input.revision);
    const revision = parseActorRunRevision(input.revision);
    validateActorRunRevision(revision);
    return {
      kind: 'actor_run',
      fact_family: ACTOR_RUN_FAMILY,
      fact_family_contract_version: ACTOR_RUN_FAMILY_VERSION,
      ...common,
      revision,
    };
  }
  if (
    input.kind === 'actor_affiliation' &&
    input.fact_family === ACTOR_AFFILIATION_FAMILY &&
    input.fact_family_contract_version === ACTOR_AFFILIATION_FAMILY_VERSION
  ) {
    assertAffiliationRevisionShape(input.revision);
    const revision = parseActorAffiliationRevision(input.revision);
    validateAffiliationRevision(revision);
    return {
      kind: 'actor_affiliation',
      fact_family: ACTOR_AFFILIATION_FAMILY,
      fact_family_contract_version: ACTOR_AFFILIATION_FAMILY_VERSION,
      ...common,
      revision,
    };
  }
  throw new ContractValidationError('actor event has an unsupported discriminator');
}

function parseEvidence(value: unknown, operation: 'upsert' | 'retract', nativeTime: QualifiedTimestamp | null) {
  const input = record(value, 'actor evidence');
  const fields = ['authority', 'quality', 'effective_at', 'completeness'];
  assertKnownFields(input, fields, 'actor evidence');
  assertRequiredFields(input, fields, 'actor evidence');
  if (
    (operation === 'upsert' && (input.authority !== 'native_record' || input.quality !== 'exact')) ||
    (operation === 'retract' && (input.authority !== 'common_reducer' || input.quality !== 'derived'))
  ) {
    throw new ContractValidationError('actor evidence does not match the event operation');
  }
  if (input.completeness !== 'complete') {
    throw new ContractValidationError('actor event evidence must be complete');
  }
  const effectiveAt = parseTimestamp(input.effective_at, 'actor evidence effective_at');
  if (JSON.stringify(effectiveAt) !== JSON.stringify(nativeTime)) {
    throw new ContractValidationError('actor evidence time does not match native_time');
  }
  return {
    authority: input.authority as 'native_record' | 'common_reducer',
    quality: input.quality as 'exact' | 'derived',
    effective_at: effectiveAt,
    completeness: 'complete' as const,
  };
}

function parseNativeEvidence(value: unknown) {
  const input = record(value, 'actor native evidence');
  const fields = ['kind', 'media_type', 'state', 'payload_hash', 'reason'];
  assertKnownFields(input, fields, 'actor native evidence');
  assertRequiredFields(input, fields, 'actor native evidence');
  if (input.kind !== 'withheld' || input.state !== 'present' || input.reason !== 'projection_boundary') {
    throw new ContractValidationError('actor native evidence has an unsupported projection');
  }
  return {
    kind: 'withheld' as const,
    media_type: boundedText(input.media_type, 'actor native evidence media_type', MAX_MEDIA_TYPE_BYTES),
    state: 'present' as const,
    payload_hash: parseFixedOpaque(input.payload_hash, 'actor native payload hash'),
    reason: 'projection_boundary' as const,
  };
}

export function parseScopedActorEnvelope(value: unknown, expectedContextInput: unknown): ScopedActorEnvelope {
  const context = parseScopedActorEnvelopeContext(expectedContextInput);
  const input = record(value, 'scoped actor envelope');
  const fields = [
    'scoped_actor_envelope_contract_version',
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
  assertKnownFields(input, fields, 'scoped actor envelope');
  assertRequiredFields(input, fields, 'scoped actor envelope');
  if (input.scoped_actor_envelope_contract_version !== SCOPED_ACTOR_ENVELOPE_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported scoped actor envelope contract version');
  }
  const selection = parseObservationContractSelectionForExpected(input.contract_selection, context.contract_selection);
  if (input.contract_version !== selection.envelope_contract_version) {
    throw new ContractValidationError('scoped actor envelope does not match the selected envelope contract');
  }
  const root = validateRoot(input.root);
  if (JSON.stringify(root) !== JSON.stringify(context.root)) {
    throw new ContractValidationError('scoped actor envelope does not match the caller-held root');
  }
  const actor = parseActor(input.actor, root, selection);
  if (input.actor_attribution !== 'derived_exact') {
    throw new ContractValidationError('current actor attribution must be derived_exact');
  }
  const affiliations = parseAffiliations(input.affiliations, actor, selection);
  const source = parseSource(input.source, context);
  const nativeTime = parseTimestamp(input.native_time, 'actor native_time');
  const event = parseEvent(input.event);
  if (selection.contract_versions.fact_family_versions[event.fact_family] !== event.fact_family_contract_version) {
    throw new ContractValidationError('actor event does not match its selected family contract');
  }
  const semanticRevisionRef = parseSemanticRevisionRef(input.semantic_revision_ref);
  parseNonzeroReference(semanticRevisionRef.fact_revision_id, 'actor semantic revision');
  if (
    semanticRevisionRef.semantic_reference_contract_version !==
    selection.contract_versions.semantic_revision_reference_version
  ) {
    throw new ContractValidationError('actor semantic revision does not match the selected reference contract');
  }
  if (event.revision.session !== root.session_key || event.revision.actor_run !== actor.run_key) {
    throw new ContractValidationError('actor event does not belong to the exact root and actor');
  }
  if (event.kind === 'actor_run') {
    if (
      event.revision.role !== actor.role ||
      event.revision.parent_actor_run !== actor.parent_run_key ||
      event.revision.native_session_id !== actor.native_session_id ||
      event.revision.native_actor_id !== actor.native_actor_id ||
      event.revision.native_actor_type !== actor.native_actor_type
    ) {
      throw new ContractValidationError('actor-run event context does not match its normalized revision');
    }
    if (nativeTime !== null) {
      throw new ContractValidationError('actor-run event cannot fabricate an effective timestamp');
    }
  } else {
    if (JSON.stringify(nativeTime) !== JSON.stringify(event.revision.effective_at)) {
      throw new ContractValidationError('actor-affiliation event time does not match its normalized revision');
    }
    if (
      !affiliations.derived_from_revision_refs.some(
        (reference) =>
          reference.semantic_reference_contract_version === semanticRevisionRef.semantic_reference_contract_version &&
          reference.fact_revision_id === semanticRevisionRef.fact_revision_id,
      )
    ) {
      throw new ContractValidationError('actor-affiliation context does not consume its own revision');
    }
  }
  if (
    (event.operation === 'upsert' && event.retraction !== null) ||
    (event.operation === 'retract' && event.retraction === null)
  ) {
    throw new ContractValidationError('actor operation and retraction cause are inconsistent');
  }
  if (event.retraction?.kind === 'reset') {
    if (
      event.retraction.old_generation !== source.generation ||
      event.retraction.new_generation !== event.retraction.old_generation + 1
    ) {
      throw new ContractValidationError('actor reset retraction has invalid generation lineage');
    }
  }
  if (event.retraction?.kind === 'source_deleted' && event.retraction.generation !== source.generation) {
    throw new ContractValidationError('actor deletion retraction targets a foreign generation');
  }
  if (input.phase !== 'bootstrap' && input.phase !== 'live' && input.phase !== 'correction') {
    throw new ContractValidationError('actor envelope has an unsupported phase');
  }
  const evidence = parseEvidence(input.evidence, event.operation, nativeTime);
  return {
    scoped_actor_envelope_contract_version: SCOPED_ACTOR_ENVELOPE_CONTRACT_VERSION,
    contract_version: selection.envelope_contract_version,
    contract_selection: selection,
    observer_sequence: positiveSafeInteger(input.observer_sequence, 'observer_sequence'),
    scope_epoch: positiveSafeInteger(input.scope_epoch, 'scope_epoch'),
    event_id: parseFixedOpaque(input.event_id, 'actor event_id'),
    semantic_revision_ref: semanticRevisionRef,
    root,
    actor,
    actor_attribution: 'derived_exact',
    affiliations,
    source,
    native_time: nativeTime,
    observed_at: safeInteger(input.observed_at, 'observed_at'),
    phase: input.phase,
    evidence,
    event,
    native_evidence: parseNativeEvidence(input.native_evidence),
  };
}
