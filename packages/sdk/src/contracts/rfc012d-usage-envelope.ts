/** Strict RFC 012D wire projection for the currently implemented
 * `runtime.usage-v2` event family.
 *
 * This is not the complete observation event/lifecycle union. Consumption is
 * contextual: the repeated selection, root, and source coordinate must match
 * authority already retained by the caller. Opaque event/revision identities
 * remain Rust-derived; the native contextual parser recomputes them before a
 * value can cross this portable boundary.
 */

import {
  ContractValidationError,
  parseExternalEntityRef,
  parseNativeIdentityClaim,
  parseOpaqueContractReference,
  parseSemanticRevisionRef,
  type ExternalEntityRef,
  type NativeIdentityClaim,
  type OpaqueContractReference,
  type SemanticRevisionRef,
} from './rfc012a.js';
import { parseUsageRevisionV2, type QualifiedTimestamp, type UsageRevisionV2 } from './rfc012c.js';
import { parseObservationContractSelectionForExpected, type ObservationContractSelection } from './rfc012d.js';

export const SCOPED_USAGE_ENVELOPE_CONTRACT_VERSION = 1 as const;

const USAGE_FAMILY = 'runtime.usage-v2';
const USAGE_FAMILY_VERSION = 1;
const MAX_CURSOR_BYTES = 128;
const MAX_MEDIA_TYPE_BYTES = 256;
const MAX_RUNTIME_TEXT_BYTES = 8 * 1024;
const MAX_IDENTIFIER_BYTES = 256;
const MAX_AFFILIATION_REVISIONS = 64;
const MAX_AUTHORIZED_SOURCES = 1_000;
const MAX_U32 = 0xffff_ffff;
const textEncoder = new TextEncoder();

type UnknownRecord = Record<string, unknown>;

export interface ScopedUsageRoot {
  session_ref: ExternalEntityRef;
  session_key: OpaqueContractReference;
  root_actor_run_key: OpaqueContractReference;
  native_session_claim: NativeIdentityClaim | null;
}

export interface ScopedUsageSourceBinding {
  instance_key: OpaqueContractReference;
  stream_key: OpaqueContractReference;
  object_key: OpaqueContractReference;
}

export interface ScopedUsageEnvelopeContext {
  contract_selection: ObservationContractSelection;
  root: ScopedUsageRoot;
  authorized_sources: ScopedUsageSourceBinding[];
}

export interface ScopedUsageActor {
  root_session_key: OpaqueContractReference;
  run_key: OpaqueContractReference;
  role: 'root' | 'child';
  parent_run_key: OpaqueContractReference | null;
  native_session_id: string | null;
  native_actor_id: string | null;
  native_actor_type: string | null;
}

export interface ScopedUsageAffiliations {
  actor_run_key: OpaqueContractReference;
  team_key: OpaqueContractReference | null;
  native_team_id: string | null;
  team_name: string | null;
  member_key: OpaqueContractReference | null;
  workflow_key: OpaqueContractReference | null;
  native_workflow_id: string | null;
  completeness: 'unknown';
  derived_from_revision_refs: SemanticRevisionRef[];
}

export interface ScopedUsageSource extends ScopedUsageSourceBinding {
  locator_id: null;
  generation: number;
  source_record_id: OpaqueContractReference;
  record_index: number;
  cursor_start: string;
  cursor_end: string;
  byte_range: { start: number; end: number };
}

export type ScopedUsageRetraction =
  | {
      kind: 'reset';
      old_generation: number;
      new_generation: number;
      reason: 'truncated' | 'identity_changed' | 'prefix_mismatch' | 'contract_replay';
    }
  | { kind: 'source_deleted'; generation: number };

export interface ScopedUsageEvent {
  kind: 'usage_v2';
  fact_family: typeof USAGE_FAMILY;
  fact_family_contract_version: typeof USAGE_FAMILY_VERSION;
  fact_id: OpaqueContractReference;
  operation: 'upsert' | 'retract';
  retraction: ScopedUsageRetraction | null;
  revision: UsageRevisionV2;
}

export interface ScopedUsageEnvelope {
  scoped_usage_envelope_contract_version: typeof SCOPED_USAGE_ENVELOPE_CONTRACT_VERSION;
  contract_version: number;
  contract_selection: ObservationContractSelection;
  observer_sequence: number;
  scope_epoch: number;
  event_id: string;
  semantic_revision_ref: SemanticRevisionRef;
  root: ScopedUsageRoot;
  actor: ScopedUsageActor;
  actor_attribution: 'derived_exact';
  affiliations: ScopedUsageAffiliations;
  source: ScopedUsageSource;
  native_time: QualifiedTimestamp | null;
  observed_at: number;
  phase: 'bootstrap' | 'live' | 'correction';
  evidence: {
    authority: 'native_record' | 'common_reducer';
    quality: 'exact' | 'derived';
    effective_at: QualifiedTimestamp | null;
    completeness: 'complete';
  };
  event: ScopedUsageEvent;
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
    value.trim() !== value ||
    textEncoder.encode(value).byteLength > maxBytes
  ) {
    throw new ContractValidationError(`${label} is not bounded canonical text`);
  }
  return value;
}

function nullableText(value: unknown, label: string): string | null {
  if (value === null) return null;
  return boundedText(value, label, MAX_RUNTIME_TEXT_BYTES);
}

function nullableReference(value: unknown, label: string): OpaqueContractReference | null {
  if (value === null) return null;
  return parseOpaqueContractReference(value, label);
}

function assertTimestampShape(value: unknown, label: string): void {
  if (value === null) return;
  const input = record(value, label);
  assertKnownFields(input, ['value', 'quality'], label);
  assertRequiredFields(input, ['value', 'quality'], label);
}

function assertQualifiedShape(value: unknown, label: string): UnknownRecord {
  const input = record(value, label);
  assertKnownFields(
    input,
    ['value', 'quality', 'authority', 'completeness', 'unknown_reason', 'effective_at', 'provenance'],
    label,
  );
  assertRequiredFields(input, ['value', 'quality', 'authority', 'completeness', 'provenance'], label);
  return input;
}

function assertNativeClaimShape(value: unknown): void {
  if (value === null) return;
  const claim = record(value, 'native session claim');
  assertKnownFields(claim, ['entity_ref', 'identity'], 'native session claim');
  assertRequiredFields(claim, ['entity_ref', 'identity'], 'native session claim');
  const identity = assertQualifiedShape(claim.identity, 'native session qualified identity');
  if (identity.value !== null) {
    const nativeIdentity = record(identity.value, 'native session identity');
    assertKnownFields(nativeIdentity, ['native_namespace', 'native_id'], 'native session identity');
    assertRequiredFields(nativeIdentity, ['native_namespace', 'native_id'], 'native session identity');
  }
  if (!Array.isArray(identity.provenance)) {
    throw new ContractValidationError('native session claim provenance must be an array');
  }
  if (identity.provenance.length > MAX_AFFILIATION_REVISIONS) {
    throw new ContractValidationError('native session claim provenance exceeds its bound');
  }
  for (const reference of identity.provenance) parseSemanticRevisionRef(reference);
}

function assertUsageQualifiedShape(value: unknown, label: string): void {
  const qualified = assertQualifiedShape(value, label);
  const provenance = record(qualified.provenance, `${label} provenance`);
  assertKnownFields(provenance, ['native_field', 'normalization_contract_version'], `${label} provenance`);
  assertRequiredFields(provenance, ['native_field', 'normalization_contract_version'], `${label} provenance`);
}

function assertUsageRevisionShape(value: unknown): UnknownRecord {
  const revision = record(value, 'usage revision');
  const fields = [
    'session',
    'actor_run',
    'response_key',
    'response_identity',
    'native_message_id',
    'request_id',
    'buckets',
    'model',
    'effort',
    'source_time',
  ];
  assertKnownFields(revision, fields, 'usage revision');
  assertRequiredFields(revision, fields, 'usage revision');
  const buckets = record(revision.buckets, 'usage buckets');
  const bucketFields = ['input_tokens', 'output_tokens', 'cache_creation_input_tokens', 'cache_read_input_tokens'];
  assertKnownFields(buckets, bucketFields, 'usage buckets');
  assertRequiredFields(buckets, bucketFields, 'usage buckets');
  for (const field of bucketFields) assertUsageQualifiedShape(buckets[field], `usage ${field}`);
  for (const field of ['model', 'effort']) {
    if (revision[field] !== null) assertUsageQualifiedShape(revision[field], `usage ${field}`);
  }
  assertTimestampShape(revision.source_time, 'usage source_time');
  return revision;
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

function decodeOpaqueBytes(value: unknown, label: string, maxBytes: number): Uint8Array {
  if (typeof value !== 'string' || !/^v1:[A-Za-z0-9_-]+$/.test(value)) {
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

function parseFixedOpaque(value: unknown, label: string): string {
  const bytes = decodeOpaqueBytes(value, label, 32);
  if (bytes.byteLength !== 32) throw new ContractValidationError(`${label} must contain 32 bytes`);
  return value as string;
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

export function parseScopedUsageRoot(value: unknown): ScopedUsageRoot {
  const input = record(value, 'usage root');
  const fields = ['session_ref', 'session_key', 'root_actor_run_key', 'native_session_claim'];
  assertKnownFields(input, fields, 'usage root');
  assertRequiredFields(input, fields, 'usage root');
  const sessionRef = parseExternalEntityRef(input.session_ref);
  const sessionKey = parseOpaqueContractReference(input.session_key, 'usage root session key');
  if (sessionRef.entity_key !== sessionKey) {
    throw new ContractValidationError('usage root external reference does not match its session key');
  }
  assertNativeClaimShape(input.native_session_claim);
  const claim = input.native_session_claim === null ? null : parseNativeIdentityClaim(input.native_session_claim);
  if (claim !== null) {
    if (claim.entity_ref.entity_key !== sessionKey) {
      throw new ContractValidationError('native session claim retargets the usage root');
    }
    if (
      typeof claim.identity.authority !== 'string' ||
      claim.identity.authority.length === 0 ||
      claim.identity.authority.trim() !== claim.identity.authority ||
      textEncoder.encode(claim.identity.authority).byteLength > MAX_IDENTIFIER_BYTES ||
      !Array.isArray(claim.identity.provenance) ||
      claim.identity.provenance.length > MAX_AFFILIATION_REVISIONS
    ) {
      throw new ContractValidationError('native session claim has invalid bounded evidence');
    }
    if (claim.identity.value !== null) {
      boundedText(claim.identity.value.native_namespace, 'native identity namespace', MAX_IDENTIFIER_BYTES);
      boundedText(claim.identity.value.native_id, 'native identity', MAX_IDENTIFIER_BYTES);
    }
    const provenance = claim.identity.provenance.map(parseSemanticRevisionRef);
    if (
      provenance.some(
        (reference, index) => index > 0 && provenance[index - 1]!.fact_revision_id >= reference.fact_revision_id,
      )
    ) {
      throw new ContractValidationError('native session claim provenance is not canonical');
    }
    if (claim.identity.effective_at !== undefined) safeInteger(claim.identity.effective_at, 'claim effective_at');
  }
  return {
    session_ref: sessionRef,
    session_key: sessionKey,
    root_actor_run_key: parseOpaqueContractReference(input.root_actor_run_key, 'root actor run key'),
    native_session_claim: claim,
  };
}

function parseSourceBinding(value: unknown, label: string): ScopedUsageSourceBinding {
  const input = record(value, label);
  const fields = ['instance_key', 'stream_key', 'object_key'];
  assertKnownFields(input, fields, label);
  assertRequiredFields(input, fields, label);
  return {
    instance_key: parseOpaqueContractReference(input.instance_key, `${label} instance key`),
    stream_key: parseOpaqueContractReference(input.stream_key, `${label} stream key`),
    object_key: parseOpaqueContractReference(input.object_key, `${label} object key`),
  };
}

export function parseScopedUsageEnvelopeContext(value: unknown): ScopedUsageEnvelopeContext {
  const input = record(value, 'scoped usage envelope context');
  const fields = ['contract_selection', 'root', 'authorized_sources'];
  assertKnownFields(input, fields, 'scoped usage envelope context');
  assertRequiredFields(input, fields, 'scoped usage envelope context');
  const selection = parseObservationContractSelectionForExpected(input.contract_selection, input.contract_selection);
  if (
    !Array.isArray(input.authorized_sources) ||
    input.authorized_sources.length === 0 ||
    input.authorized_sources.length > MAX_AUTHORIZED_SOURCES
  ) {
    throw new ContractValidationError(`scoped usage context requires 1..=${MAX_AUTHORIZED_SOURCES} authorized sources`);
  }
  const sources = input.authorized_sources.map((source, index) =>
    parseSourceBinding(source, `authorized source ${index}`),
  );
  const keys = sources.map((source) => `${source.instance_key}\0${source.stream_key}\0${source.object_key}`);
  if (new Set(keys).size !== keys.length) {
    throw new ContractValidationError('scoped usage context contains duplicate authorized sources');
  }
  return { contract_selection: selection, root: parseScopedUsageRoot(input.root), authorized_sources: sources };
}

function parseActor(value: unknown, root: ScopedUsageRoot): ScopedUsageActor {
  const input = record(value, 'usage actor');
  const fields = [
    'root_session_key',
    'run_key',
    'role',
    'parent_run_key',
    'native_session_id',
    'native_actor_id',
    'native_actor_type',
  ];
  assertKnownFields(input, fields, 'usage actor');
  assertRequiredFields(input, fields, 'usage actor');
  if (input.role !== 'root' && input.role !== 'child') {
    throw new ContractValidationError('usage actor has an unsupported role');
  }
  const actor: ScopedUsageActor = {
    root_session_key: parseOpaqueContractReference(input.root_session_key, 'actor root session key'),
    run_key: parseOpaqueContractReference(input.run_key, 'actor run key'),
    role: input.role,
    parent_run_key: nullableReference(input.parent_run_key, 'actor parent run key'),
    native_session_id: nullableText(input.native_session_id, 'actor native_session_id'),
    native_actor_id: nullableText(input.native_actor_id, 'actor native_actor_id'),
    native_actor_type: nullableText(input.native_actor_type, 'actor native_actor_type'),
  };
  const expectedRole = actor.run_key === root.root_actor_run_key ? 'root' : 'child';
  if (actor.root_session_key !== root.session_key || actor.role !== expectedRole) {
    throw new ContractValidationError('usage actor does not match the exact root');
  }
  if (actor.parent_run_key !== null || actor.native_actor_id !== null || actor.native_actor_type !== null) {
    throw new ContractValidationError('current usage actor contains unsupported enrichment');
  }
  const claimValue = root.native_session_claim?.identity.value;
  const expectedNativeSession =
    claimValue !== null && claimValue !== undefined && typeof claimValue === 'object'
      ? (claimValue as { native_id?: unknown }).native_id
      : null;
  if (actor.native_session_id !== expectedNativeSession) {
    throw new ContractValidationError('usage actor native session does not match the root claim');
  }
  return actor;
}

function parseAffiliations(value: unknown, actor: ScopedUsageActor): ScopedUsageAffiliations {
  const input = record(value, 'usage affiliations');
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
  assertKnownFields(input, fields, 'usage affiliations');
  assertRequiredFields(input, fields, 'usage affiliations');
  if (!Array.isArray(input.derived_from_revision_refs)) {
    throw new ContractValidationError('usage affiliation revision refs must be an array');
  }
  if (input.derived_from_revision_refs.length !== 0) {
    throw new ContractValidationError('current usage affiliation projection is not explicitly unknown');
  }
  const result: ScopedUsageAffiliations = {
    actor_run_key: parseOpaqueContractReference(input.actor_run_key, 'affiliation actor run key'),
    team_key: nullableReference(input.team_key, 'affiliation team key'),
    native_team_id: nullableText(input.native_team_id, 'affiliation native_team_id'),
    team_name: nullableText(input.team_name, 'affiliation team_name'),
    member_key: nullableReference(input.member_key, 'affiliation member key'),
    workflow_key: nullableReference(input.workflow_key, 'affiliation workflow key'),
    native_workflow_id: nullableText(input.native_workflow_id, 'affiliation native_workflow_id'),
    completeness:
      input.completeness === 'unknown'
        ? 'unknown'
        : (() => {
            throw new ContractValidationError('current usage affiliations must remain explicitly unknown');
          })(),
    derived_from_revision_refs: input.derived_from_revision_refs.map(parseSemanticRevisionRef),
  };
  if (
    result.actor_run_key !== actor.run_key ||
    result.team_key !== null ||
    result.native_team_id !== null ||
    result.team_name !== null ||
    result.member_key !== null ||
    result.workflow_key !== null ||
    result.native_workflow_id !== null ||
    result.derived_from_revision_refs.length !== 0
  ) {
    throw new ContractValidationError('current usage affiliation projection is not explicitly unknown');
  }
  return result;
}

function parseSource(value: unknown, context: ScopedUsageEnvelopeContext): ScopedUsageSource {
  const input = record(value, 'usage source');
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
  assertKnownFields(input, fields, 'usage source');
  assertRequiredFields(input, fields, 'usage source');
  const binding: ScopedUsageSourceBinding = {
    instance_key: parseOpaqueContractReference(input.instance_key, 'usage source instance key'),
    stream_key: parseOpaqueContractReference(input.stream_key, 'usage source stream key'),
    object_key: parseOpaqueContractReference(input.object_key, 'usage source object key'),
  };
  if (
    !context.authorized_sources.some(
      (source) =>
        source.instance_key === binding.instance_key &&
        source.stream_key === binding.stream_key &&
        source.object_key === binding.object_key,
    )
  ) {
    throw new ContractValidationError('usage source is outside the caller-held authorized source set');
  }
  if (input.locator_id !== null) {
    throw new ContractValidationError('usage source cannot disclose a native locator');
  }
  const start = parseAppendCursor(input.cursor_start, 'usage cursor_start');
  const end = parseAppendCursor(input.cursor_end, 'usage cursor_end');
  const range = record(input.byte_range, 'usage byte range');
  assertKnownFields(range, ['start', 'end'], 'usage byte range');
  assertRequiredFields(range, ['start', 'end'], 'usage byte range');
  const rangeStart = nonnegativeSafeInteger(range.start, 'usage byte range start');
  const rangeEnd = nonnegativeSafeInteger(range.end, 'usage byte range end');
  if (start.offset > end.offset || rangeStart !== start.offset || rangeEnd !== end.offset) {
    throw new ContractValidationError('usage byte range does not match its append cursors');
  }
  const recordIndex = nonnegativeSafeInteger(input.record_index, 'usage record_index');
  if (recordIndex > MAX_U32) throw new ContractValidationError('usage record_index exceeds u32');
  return {
    ...binding,
    locator_id: null,
    generation: positiveSafeInteger(input.generation, 'usage source generation'),
    source_record_id: parseOpaqueContractReference(input.source_record_id, 'usage source record id'),
    record_index: recordIndex,
    cursor_start: start.wire,
    cursor_end: end.wire,
    byte_range: { start: rangeStart, end: rangeEnd },
  };
}

function parseRetraction(value: unknown): ScopedUsageRetraction | null {
  if (value === null) return null;
  const input = record(value, 'usage retraction');
  if (input.kind === 'reset') {
    const fields = ['kind', 'old_generation', 'new_generation', 'reason'];
    assertKnownFields(input, fields, 'usage reset retraction');
    assertRequiredFields(input, fields, 'usage reset retraction');
    if (
      input.reason !== 'truncated' &&
      input.reason !== 'identity_changed' &&
      input.reason !== 'prefix_mismatch' &&
      input.reason !== 'contract_replay'
    ) {
      throw new ContractValidationError('usage reset retraction has an unsupported reason');
    }
    return {
      kind: 'reset',
      old_generation: positiveSafeInteger(input.old_generation, 'reset old_generation'),
      new_generation: positiveSafeInteger(input.new_generation, 'reset new_generation'),
      reason: input.reason,
    };
  }
  if (input.kind === 'source_deleted') {
    const fields = ['kind', 'generation'];
    assertKnownFields(input, fields, 'source-deleted usage retraction');
    assertRequiredFields(input, fields, 'source-deleted usage retraction');
    return {
      kind: 'source_deleted',
      generation: positiveSafeInteger(input.generation, 'deleted generation'),
    };
  }
  throw new ContractValidationError('usage retraction has an unsupported kind');
}

function parseEvent(value: unknown): ScopedUsageEvent {
  const input = record(value, 'usage event');
  const fields = [
    'kind',
    'fact_family',
    'fact_family_contract_version',
    'fact_id',
    'operation',
    'retraction',
    'revision',
  ];
  assertKnownFields(input, fields, 'usage event');
  assertRequiredFields(input, fields, 'usage event');
  if (
    input.kind !== 'usage_v2' ||
    input.fact_family !== USAGE_FAMILY ||
    input.fact_family_contract_version !== USAGE_FAMILY_VERSION
  ) {
    throw new ContractValidationError('usage event has an unsupported discriminator');
  }
  if (input.operation !== 'upsert' && input.operation !== 'retract') {
    throw new ContractValidationError('usage event has an unsupported operation');
  }
  const revisionInput = assertUsageRevisionShape(input.revision);
  const revision = parseUsageRevisionV2(revisionInput);
  validateUsageRevisionForEnvelope(revision);
  return {
    kind: 'usage_v2',
    fact_family: USAGE_FAMILY,
    fact_family_contract_version: USAGE_FAMILY_VERSION,
    fact_id: parseOpaqueContractReference(input.fact_id, 'usage fact id'),
    operation: input.operation,
    retraction: parseRetraction(input.retraction),
    revision,
  };
}

function validateUsageRevisionForEnvelope(revision: UsageRevisionV2): void {
  const values = [
    revision.buckets.input_tokens,
    revision.buckets.output_tokens,
    revision.buckets.cache_creation_input_tokens,
    revision.buckets.cache_read_input_tokens,
    revision.model,
    revision.effort,
  ];
  for (const value of values) {
    if (value === null) continue;
    if (value.provenance.normalization_contract_version !== 1) {
      throw new ContractValidationError('usage value has an unsupported normalization contract');
    }
  }
  for (const [label, value] of [
    ['native_message_id', revision.native_message_id],
    ['request_id', revision.request_id],
  ] as const) {
    if (value !== null) boundedText(value, `usage ${label}`, MAX_RUNTIME_TEXT_BYTES);
  }
  for (const [label, value] of [
    ['model', revision.model?.value],
    ['effort', revision.effort?.value],
  ] as const) {
    if (value !== null && value !== undefined) {
      boundedText(value, `usage ${label}`, MAX_RUNTIME_TEXT_BYTES);
    }
  }
}

function parseEvidence(value: unknown, operation: 'upsert' | 'retract', nativeTime: QualifiedTimestamp | null) {
  const input = record(value, 'usage evidence');
  const fields = ['authority', 'quality', 'effective_at', 'completeness'];
  assertKnownFields(input, fields, 'usage evidence');
  assertRequiredFields(input, fields, 'usage evidence');
  const authority = input.authority;
  const quality = input.quality;
  if (
    (operation === 'upsert' && (authority !== 'native_record' || quality !== 'exact')) ||
    (operation === 'retract' && (authority !== 'common_reducer' || quality !== 'derived'))
  ) {
    throw new ContractValidationError('usage evidence does not match the event operation');
  }
  if (input.completeness !== 'complete') {
    throw new ContractValidationError('usage event evidence must be complete');
  }
  const effectiveAt = parseTimestamp(input.effective_at, 'usage evidence effective_at');
  if (JSON.stringify(effectiveAt) !== JSON.stringify(nativeTime)) {
    throw new ContractValidationError('usage evidence time does not match native_time');
  }
  return {
    authority: authority as 'native_record' | 'common_reducer',
    quality: quality as 'exact' | 'derived',
    effective_at: effectiveAt,
    completeness: 'complete' as const,
  };
}

function parseNativeEvidence(value: unknown) {
  const input = record(value, 'usage native evidence');
  const fields = ['kind', 'media_type', 'state', 'payload_hash', 'reason'];
  assertKnownFields(input, fields, 'usage native evidence');
  assertRequiredFields(input, fields, 'usage native evidence');
  if (input.kind !== 'withheld' || input.state !== 'present' || input.reason !== 'projection_boundary') {
    throw new ContractValidationError('usage native evidence has an unsupported projection');
  }
  const mediaType = boundedText(input.media_type, 'usage native evidence media_type', MAX_MEDIA_TYPE_BYTES);
  if ([...mediaType].some((character) => /\p{Cc}/u.test(character))) {
    throw new ContractValidationError('usage native evidence media_type contains control characters');
  }
  return {
    kind: 'withheld' as const,
    media_type: mediaType,
    state: 'present' as const,
    payload_hash: parseFixedOpaque(input.payload_hash, 'usage native payload hash'),
    reason: 'projection_boundary' as const,
  };
}

export function parseScopedUsageEnvelope(value: unknown, expectedContextInput: unknown): ScopedUsageEnvelope {
  const context = parseScopedUsageEnvelopeContext(expectedContextInput);
  const input = record(value, 'scoped usage envelope');
  const fields = [
    'scoped_usage_envelope_contract_version',
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
  assertKnownFields(input, fields, 'scoped usage envelope');
  assertRequiredFields(input, fields, 'scoped usage envelope');
  if (input.scoped_usage_envelope_contract_version !== SCOPED_USAGE_ENVELOPE_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported scoped usage envelope contract version');
  }
  const selection = parseObservationContractSelectionForExpected(input.contract_selection, context.contract_selection);
  if (
    input.contract_version !== selection.envelope_contract_version ||
    selection.contract_versions.fact_family_versions[USAGE_FAMILY] !== USAGE_FAMILY_VERSION
  ) {
    throw new ContractValidationError('scoped usage envelope does not match the selected usage contract');
  }
  const root = parseScopedUsageRoot(input.root);
  if (JSON.stringify(root) !== JSON.stringify(context.root)) {
    throw new ContractValidationError('scoped usage envelope does not match the caller-held root');
  }
  const actor = parseActor(input.actor, root);
  if (input.actor_attribution !== 'derived_exact') {
    throw new ContractValidationError('current usage actor attribution must be derived_exact');
  }
  const affiliations = parseAffiliations(input.affiliations, actor);
  const source = parseSource(input.source, context);
  const nativeTime = parseTimestamp(input.native_time, 'usage native_time');
  const event = parseEvent(input.event);
  if (event.revision.session !== root.session_key || event.revision.actor_run !== actor.run_key) {
    throw new ContractValidationError('usage revision does not match the exact root and actor');
  }
  if (JSON.stringify(event.revision.source_time) !== JSON.stringify(nativeTime)) {
    throw new ContractValidationError('usage revision source_time does not match native_time');
  }
  if (
    (event.operation === 'upsert' && event.retraction !== null) ||
    (event.operation === 'retract' && event.retraction === null)
  ) {
    throw new ContractValidationError('usage operation and retraction cause are inconsistent');
  }
  if (event.retraction?.kind === 'reset') {
    if (
      event.retraction.old_generation !== source.generation ||
      event.retraction.new_generation !== event.retraction.old_generation + 1
    ) {
      throw new ContractValidationError('usage reset retraction has invalid generation lineage');
    }
  }
  if (event.retraction?.kind === 'source_deleted' && event.retraction.generation !== source.generation) {
    throw new ContractValidationError('usage deletion retraction targets a foreign generation');
  }
  if (input.phase !== 'bootstrap' && input.phase !== 'live' && input.phase !== 'correction') {
    throw new ContractValidationError('usage envelope has an unsupported phase');
  }
  const evidence = parseEvidence(input.evidence, event.operation, nativeTime);
  return {
    scoped_usage_envelope_contract_version: SCOPED_USAGE_ENVELOPE_CONTRACT_VERSION,
    contract_version: selection.envelope_contract_version,
    contract_selection: selection,
    observer_sequence: positiveSafeInteger(input.observer_sequence, 'observer_sequence'),
    scope_epoch: positiveSafeInteger(input.scope_epoch, 'scope_epoch'),
    event_id: parseFixedOpaque(input.event_id, 'usage event_id'),
    semantic_revision_ref: parseSemanticRevisionRef(input.semantic_revision_ref),
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
