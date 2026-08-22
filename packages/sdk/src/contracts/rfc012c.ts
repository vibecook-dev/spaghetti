/** RFC 012C v1 portable runtime value contracts.
 *
 * Rust derives opaque keys and usage semantic revision identities. This
 * module validates committed wire values and binds each revision to a
 * caller-held Rust-produced identity context. It does not implement
 * observer envelopes, epochs, or a second opaque-reference format.
 */

import {
  ContractValidationError,
  parseExternalEntityRef,
  parseOpaqueContractReference,
  parseQualifiedValue,
  parseSemanticRevisionRef,
  type ContractCompleteness,
  type ExternalEntityRef,
  type OpaqueContractReference,
  type QualifiedValue,
  type SemanticRevisionRef,
} from './rfc012a.js';
import {
  assertNoUnpairedUtf16Surrogates,
  assertSemanticFixtureGraph,
  hasSurroundingRustWhitespace,
  preflightSemanticFixtureJson,
} from './rfc012-semantic-json.js';

export const RUNTIME_SEMANTIC_CONTRACT_VERSION = 1 as const;
export const ACTOR_RUN_FAMILY = 'runtime.actor-run' as const;
export const ACTOR_AFFILIATION_FAMILY = 'runtime.actor-affiliation' as const;
export const USAGE_V2_FAMILY = 'runtime.usage-v2' as const;
export const EFFECTIVE_STATE_FAMILY = 'runtime.effective-state' as const;
export const USER_INPUT_FAMILY = 'runtime.user-input-request' as const;
export const MESSAGE_FAMILY = 'runtime.message' as const;
export const CONTENT_BLOCK_FAMILY = 'runtime.content-block' as const;
export const PLAN_FAMILY = 'runtime.plan' as const;
export const TASK_FAMILY = 'runtime.task' as const;
export const TOOL_FAMILY = 'runtime.tool' as const;
export const EFFECTIVE_STATE_FAMILY_VERSION = 1 as const;
export const USER_INPUT_FAMILY_VERSION = 1 as const;
export const MESSAGE_FAMILY_VERSION = 1 as const;
export const CONTENT_BLOCK_FAMILY_VERSION = 1 as const;
export const PLAN_FAMILY_VERSION = 1 as const;
export const TASK_FAMILY_VERSION = 1 as const;
export const TOOL_FAMILY_VERSION = 1 as const;
export const ACTOR_RUN_FAMILY_VERSION = 1 as const;
export const ACTOR_AFFILIATION_FAMILY_VERSION = 1 as const;
export const USAGE_V2_FAMILY_VERSION = 1 as const;

export type ActorRunRole = 'root' | 'child';
export type ActorAffiliationDimension = 'team' | 'workflow';
export type ActorAffiliationState = 'present' | 'removed' | 'unknown';
export type UsageResponseIdentity = 'native_message_id' | 'source_record_fallback';
export type UsageValueAuthority = 'native_response' | 'adapter_derived';
export type TimestampQuality = 'NativeExact' | 'NativeApproximate' | 'FileMetadataFallback' | 'Derived';

export interface QualifiedTimestamp {
  value: string;
  quality: TimestampQuality;
}

export interface UsageValueProvenance {
  native_field: string;
  normalization_contract_version: number;
}

export type UsageQualifiedValue<T> = QualifiedValue<T, UsageValueAuthority, UsageValueProvenance>;

export interface ActorRunRevision {
  actor_run: OpaqueContractReference;
  session: OpaqueContractReference;
  role: ActorRunRole;
  parent_actor_run: OpaqueContractReference | null;
  native_session_id: string | null;
  native_actor_id: string | null;
  native_actor_type: string | null;
}

export interface ActorAffiliationRevision {
  affiliation: OpaqueContractReference;
  actor_run: OpaqueContractReference;
  session: OpaqueContractReference;
  dimension: ActorAffiliationDimension;
  target: OpaqueContractReference;
  member: OpaqueContractReference | null;
  native_target_id: string | null;
  native_member_id: string | null;
  state: ActorAffiliationState;
  effective_at: QualifiedTimestamp | null;
}

export interface UsageBucketsV2 {
  input_tokens: UsageQualifiedValue<number>;
  output_tokens: UsageQualifiedValue<number>;
  cache_creation_input_tokens: UsageQualifiedValue<number>;
  cache_read_input_tokens: UsageQualifiedValue<number>;
}

export interface UsageRevisionV2 {
  session: OpaqueContractReference;
  actor_run: OpaqueContractReference;
  response_key: string;
  response_identity: UsageResponseIdentity;
  native_message_id: string | null;
  request_id: string | null;
  buckets: UsageBucketsV2;
  model: UsageQualifiedValue<string> | null;
  effort: UsageQualifiedValue<string> | null;
  source_time: QualifiedTimestamp | null;
}

export interface RuntimeFamilyVersion {
  family: string;
  version: number;
}

export interface RuntimeSessionIdentity {
  entity_key: OpaqueContractReference;
  external_ref: ExternalEntityRef;
  native_session_id: string;
}

export interface RuntimeSourceIdentity {
  adapter_id: string;
  source_instance_key: OpaqueContractReference;
  session: RuntimeSessionIdentity;
}

export interface ActorRunExample {
  family: typeof ACTOR_RUN_FAMILY;
  family_version: typeof ACTOR_RUN_FAMILY_VERSION;
  revision: ActorRunRevision;
  semantic_revision_key_hex: string;
  fact_id: OpaqueContractReference;
  source_record_id: OpaqueContractReference;
  semantic_revision_ref: SemanticRevisionRef;
}

export interface ActorAffiliationExample {
  family: typeof ACTOR_AFFILIATION_FAMILY;
  family_version: typeof ACTOR_AFFILIATION_FAMILY_VERSION;
  revision: ActorAffiliationRevision;
  semantic_revision_key_hex: string;
  fact_id: OpaqueContractReference;
  source_record_id: OpaqueContractReference;
  semantic_revision_ref: SemanticRevisionRef;
}

export interface UsageRevisionExample {
  family: typeof USAGE_V2_FAMILY;
  family_version: typeof USAGE_V2_FAMILY_VERSION;
  revision: UsageRevisionV2;
  semantic_revision_key_hex: string;
  fact_id: OpaqueContractReference;
  source_record_id: OpaqueContractReference;
  semantic_revision_ref: SemanticRevisionRef;
}

export interface RuntimeContractFixture {
  fixture_contract_version: typeof RUNTIME_SEMANTIC_CONTRACT_VERSION;
  runtime_semantic_contract_version: typeof RUNTIME_SEMANTIC_CONTRACT_VERSION;
  families: RuntimeFamilyVersion[];
  source: RuntimeSourceIdentity;
  actors: {
    root: ActorRunExample;
    child: ActorRunExample;
  };
  affiliations: {
    child_team_present: ActorAffiliationExample;
    child_workflow_present: ActorAffiliationExample;
    child_workflow_removed: ActorAffiliationExample;
  };
  usage: {
    native_message: UsageRevisionExample;
    source_record_fallback: UsageRevisionExample;
    response_revisions: {
      native_message_id: string;
      a: UsageRevisionExample;
      b: UsageRevisionExample;
      a_repeat: UsageRevisionExample;
    };
  };
}

type UnknownRecord = Record<string, unknown>;

const MAX_RUNTIME_SEMANTIC_TEXT_BYTES = 8 * 1024;
const MAX_ADAPTER_ID_BYTES = 128;
const MAX_INTERACTION_QUESTIONS = 32;
const MAX_INTERACTION_OPTIONS = 32;
const MAX_MESSAGE_CONTENT_BLOCKS = 32;
const MAX_USAGE_RESPONSE_KEY_BYTES = 8 * 1024;
const MAX_USAGE_PROVENANCE_FIELD_BYTES = 256;
const MAX_U32 = 0xffff_ffff;
const textEncoder = new TextEncoder();

function record(value: unknown, label: string): UnknownRecord {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new ContractValidationError(`${label} must be an object`);
  }
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== Object.prototype && prototype !== null) {
    throw new ContractValidationError(`${label} must be a plain object`);
  }
  return value as UnknownRecord;
}

function assertKnownFields(input: UnknownRecord, fields: readonly string[], label: string): void {
  const known = new Set(fields);
  for (const key of Object.keys(input)) {
    if (!known.has(key)) throw new ContractValidationError(`${label} contains unknown field ${key}`);
  }
}

function nonEmptyString(value: unknown, label: string): string {
  if (typeof value !== 'string' || value.length === 0) {
    throw new ContractValidationError(`${label} must be a non-empty canonical string`);
  }
  assertNoUnpairedUtf16Surrogates(value, label);
  if (hasSurroundingRustWhitespace(value)) {
    throw new ContractValidationError(`${label} must be a non-empty canonical string`);
  }
  return value;
}

function boundedText(value: unknown, label: string, maxBytes: number): string {
  if (typeof value !== 'string') {
    throw new ContractValidationError(`${label} must be a non-empty canonical string`);
  }
  if (value.length > maxBytes) {
    throw new ContractValidationError(`${label} exceeds ${maxBytes} UTF-8 bytes`);
  }
  const parsed = nonEmptyString(value, label);
  if (textEncoder.encode(parsed).byteLength > maxBytes) {
    throw new ContractValidationError(`${label} exceeds ${maxBytes} UTF-8 bytes`);
  }
  return parsed;
}

function boundedContentText(value: unknown, label: string): string {
  if (typeof value !== 'string') {
    throw new ContractValidationError(`${label} must be a string`);
  }
  if (value.length > MAX_RUNTIME_SEMANTIC_TEXT_BYTES) {
    throw new ContractValidationError(`${label} exceeds ${MAX_RUNTIME_SEMANTIC_TEXT_BYTES} UTF-8 bytes`);
  }
  assertNoUnpairedUtf16Surrogates(value, label);
  if (textEncoder.encode(value).byteLength > MAX_RUNTIME_SEMANTIC_TEXT_BYTES) {
    throw new ContractValidationError(`${label} exceeds ${MAX_RUNTIME_SEMANTIC_TEXT_BYTES} UTF-8 bytes`);
  }
  return value;
}

function optionalRuntimeSemanticText(value: unknown, label: string): string | null {
  if (value === undefined || value === null) return null;
  return boundedText(value, label, MAX_RUNTIME_SEMANTIC_TEXT_BYTES);
}

function optionalOpaque(value: unknown, label: string): OpaqueContractReference | null {
  if (value === undefined || value === null) return null;
  return parseOpaqueContractReference(value, label);
}

function positiveInteger(value: unknown, label: string): number {
  if (!Number.isSafeInteger(value) || (value as number) <= 0) {
    throw new ContractValidationError(`${label} must be a positive safe integer`);
  }
  return value as number;
}

function positiveU32(value: unknown, label: string): number {
  const result = positiveInteger(value, label);
  if (result > MAX_U32) {
    throw new ContractValidationError(`${label} exceeds u32`);
  }
  return result;
}

function nonNegativeU32(value: unknown, label: string): number {
  if (!Number.isInteger(value) || (value as number) < 0 || (value as number) > MAX_U32 || Object.is(value, -0)) {
    throw new ContractValidationError(`${label} must be a non-negative u32`);
  }
  return value as number;
}

function digest32(value: unknown, label: string): number[] {
  if (!Array.isArray(value) || value.length !== 32) {
    throw new ContractValidationError(`${label} must contain exactly 32 bytes`);
  }
  const result: number[] = [];
  for (let index = 0; index < 32; index += 1) {
    if (!Object.hasOwn(value, index)) {
      throw new ContractValidationError(`${label} must be a dense 32-byte array`);
    }
    const byte = value[index];
    if (!Number.isInteger(byte) || (byte as number) < 0 || (byte as number) > 255) {
      throw new ContractValidationError(`${label} must contain only bytes`);
    }
    result.push(byte as number);
  }
  if (result.every((byte) => byte === 0)) {
    throw new ContractValidationError(`${label} must be nonzero`);
  }
  return result;
}

function tokenCount(value: unknown, label: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0 || Object.is(value, -0)) {
    throw new ContractValidationError(`${label} must be a non-negative safe integer`);
  }
  return value;
}

const MAX_USAGE_RESPONSE_KEY_BASE64_CHARS = Math.ceil(MAX_USAGE_RESPONSE_KEY_BYTES / 3) * 4;

function parseCanonicalBase64(value: unknown, label: string): string {
  if (Array.isArray(value)) {
    throw new ContractValidationError(`${label} must not use the legacy byte-array form`);
  }
  if (typeof value !== 'string' || value.length === 0) {
    throw new ContractValidationError(`${label} must be non-empty canonical padded standard base64`);
  }
  if (value.length > MAX_USAGE_RESPONSE_KEY_BASE64_CHARS) {
    throw new ContractValidationError(`${label} exceeds the bounded encoded base64 maximum`);
  }
  if (!/^[A-Za-z0-9+/]+={0,2}$/.test(value) || value.length % 4 !== 0) {
    throw new ContractValidationError(`${label} is not canonical padded standard base64`);
  }
  const decoded = decodeBase64(value, label);
  if (decoded.byteLength === 0 || decoded.byteLength > MAX_USAGE_RESPONSE_KEY_BYTES) {
    throw new ContractValidationError(`${label} must decode to 1..=${MAX_USAGE_RESPONSE_KEY_BYTES} bytes`);
  }
  if (encodeBase64(decoded) !== value) {
    throw new ContractValidationError(`${label} is not canonical padded standard base64`);
  }
  return value;
}

function decodeBase64(value: string, label: string): Uint8Array {
  let binary: string;
  try {
    binary = atob(value);
  } catch {
    throw new ContractValidationError(`${label} is not canonical padded standard base64`);
  }
  return Uint8Array.from(binary, (character) => character.charCodeAt(0));
}

function encodeBase64(value: Uint8Array): string {
  let binary = '';
  for (const byte of value) binary += String.fromCharCode(byte);
  return btoa(binary);
}

function bytesEqual(left: Uint8Array, right: Uint8Array): boolean {
  return left.byteLength === right.byteLength && left.every((byte, index) => byte === right[index]);
}

function parseTimestampQuality(value: unknown, label: string): TimestampQuality {
  if (
    value !== 'NativeExact' &&
    value !== 'NativeApproximate' &&
    value !== 'FileMetadataFallback' &&
    value !== 'Derived'
  ) {
    throw new ContractValidationError(`${label} has an unsupported timestamp quality`);
  }
  return value;
}

function parseOptionalQualifiedTimestamp(value: unknown, label: string): QualifiedTimestamp | null {
  if (value === undefined || value === null) return null;
  const input = record(value, label);
  assertKnownFields(input, ['value', 'quality'], label);
  return {
    value: boundedText(input.value, `${label} value`, MAX_RUNTIME_SEMANTIC_TEXT_BYTES),
    quality: parseTimestampQuality(input.quality, label),
  };
}

function parseHexDigest(value: unknown, label: string): string {
  if (typeof value !== 'string' || value.length !== 64) {
    throw new ContractValidationError(`${label} must be 32 lowercase hex bytes`);
  }
  if (!/^[0-9a-f]{64}$/.test(value)) {
    throw new ContractValidationError(`${label} must be 32 lowercase hex bytes`);
  }
  return value;
}

function parseNativeField(value: unknown, label: string): string {
  const field = boundedText(value, label, MAX_USAGE_PROVENANCE_FIELD_BYTES);
  if (!/^[a-z][a-z0-9._-]*$/.test(field)) {
    throw new ContractValidationError(`${label} must be a closed machine field identifier`);
  }
  return field;
}

function parseUsageProvenance(value: unknown): UsageValueProvenance {
  const input = record(value, 'usage provenance');
  assertKnownFields(input, ['native_field', 'normalization_contract_version'], 'usage provenance');
  return {
    native_field: parseNativeField(input.native_field, 'usage provenance native_field'),
    normalization_contract_version: positiveU32(
      input.normalization_contract_version,
      'usage provenance normalization_contract_version',
    ),
  };
}

function parseUsageAuthority(value: unknown, label: string): UsageValueAuthority {
  if (value !== 'native_response' && value !== 'adapter_derived') {
    throw new ContractValidationError(`${label} must be native_response or adapter_derived`);
  }
  return value;
}

function parseUsageQualifiedValue<T>(
  value: unknown,
  label: string,
  parseKnown: (raw: unknown, field: string) => T,
): UsageQualifiedValue<T> {
  const parsed = parseQualifiedValue<T, UsageValueAuthority, UsageValueProvenance>(value, {
    parseKnownValue: parseKnown,
    parseAuthority: parseUsageAuthority,
    parseProvenance: parseUsageProvenance,
  });
  if (parsed.quality === 'unknown') {
    if (parsed.completeness === 'complete') {
      throw new ContractValidationError(`${label} cannot claim complete coverage while unknown`);
    }
  }
  return parsed;
}

function parseTokenValue(value: unknown, label: string): number {
  return tokenCount(value, label);
}

function parseTextValue(value: unknown, label: string): string {
  return boundedText(value, label, MAX_RUNTIME_SEMANTIC_TEXT_BYTES);
}

function parseOptionalUsageQualified<T>(
  value: unknown,
  label: string,
  parseKnown: (raw: unknown, field: string) => T,
): UsageQualifiedValue<T> | null {
  if (value === undefined || value === null) return null;
  return parseUsageQualifiedValue(value, label, parseKnown);
}

function parseFamilyVersion(value: unknown): RuntimeFamilyVersion {
  const input = record(value, 'runtime family');
  assertKnownFields(input, ['family', 'version'], 'runtime family');
  return {
    family: nonEmptyString(input.family, 'runtime family name'),
    version: positiveInteger(input.version, `runtime family ${String(input.family)} version`),
  };
}

const CANONICAL_RUNTIME_FAMILIES: ReadonlyArray<readonly [string, number]> = [
  [ACTOR_RUN_FAMILY, ACTOR_RUN_FAMILY_VERSION],
  [ACTOR_AFFILIATION_FAMILY, ACTOR_AFFILIATION_FAMILY_VERSION],
  [USAGE_V2_FAMILY, USAGE_V2_FAMILY_VERSION],
];

function parseCanonicalFamilies(value: unknown): RuntimeFamilyVersion[] {
  if (!Array.isArray(value)) {
    throw new ContractValidationError('runtime families must be an array');
  }
  const families = value.map(parseFamilyVersion);
  if (
    families.length !== CANONICAL_RUNTIME_FAMILIES.length ||
    families.some(
      (family, index) =>
        family.family !== CANONICAL_RUNTIME_FAMILIES[index]![0] ||
        family.version !== CANONICAL_RUNTIME_FAMILIES[index]![1],
    )
  ) {
    throw new ContractValidationError('runtime fixture families must be actor-run, actor-affiliation, and usage-v2 v1');
  }
  return families;
}

export function parseActorRunRevision(value: unknown): ActorRunRevision {
  const input = record(value, 'actor run revision');
  assertKnownFields(
    input,
    ['actor_run', 'session', 'role', 'parent_actor_run', 'native_session_id', 'native_actor_id', 'native_actor_type'],
    'actor run revision',
  );
  const role = input.role;
  if (role !== 'root' && role !== 'child') {
    throw new ContractValidationError('unsupported actor run role');
  }
  const actorRun = parseOpaqueContractReference(input.actor_run, 'actor run');
  const parent = optionalOpaque(input.parent_actor_run, 'parent actor run');
  if (role === 'root' && parent !== null) {
    throw new ContractValidationError('root actor run cannot declare a parent actor run');
  }
  if (role === 'child' && parent === null) {
    throw new ContractValidationError('child actor run must declare a parent actor run');
  }
  if (parent === actorRun) {
    throw new ContractValidationError('actor run cannot be its own parent');
  }
  return {
    actor_run: actorRun,
    session: parseOpaqueContractReference(input.session, 'actor session'),
    role,
    parent_actor_run: parent,
    native_session_id: optionalRuntimeSemanticText(input.native_session_id, 'native_session_id'),
    native_actor_id: optionalRuntimeSemanticText(input.native_actor_id, 'native_actor_id'),
    native_actor_type: optionalRuntimeSemanticText(input.native_actor_type, 'native_actor_type'),
  };
}

export function parseActorAffiliationRevision(value: unknown): ActorAffiliationRevision {
  const input = record(value, 'actor affiliation revision');
  assertKnownFields(
    input,
    [
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
    ],
    'actor affiliation revision',
  );
  if (input.dimension !== 'team' && input.dimension !== 'workflow') {
    throw new ContractValidationError('unsupported actor affiliation dimension');
  }
  if (input.state !== 'present' && input.state !== 'removed' && input.state !== 'unknown') {
    throw new ContractValidationError('unsupported actor affiliation state');
  }
  return {
    affiliation: parseOpaqueContractReference(input.affiliation, 'affiliation'),
    actor_run: parseOpaqueContractReference(input.actor_run, 'affiliated actor run'),
    session: parseOpaqueContractReference(input.session, 'affiliation session'),
    dimension: input.dimension,
    target: parseOpaqueContractReference(input.target, 'affiliation target'),
    member: optionalOpaque(input.member, 'affiliation member'),
    native_target_id: optionalRuntimeSemanticText(input.native_target_id, 'native_target_id'),
    native_member_id: optionalRuntimeSemanticText(input.native_member_id, 'native_member_id'),
    state: input.state,
    effective_at: parseOptionalQualifiedTimestamp(input.effective_at, 'affiliation effective_at'),
  };
}

export function parseUsageRevisionV2(value: unknown): UsageRevisionV2 {
  const input = record(value, 'usage revision');
  assertKnownFields(
    input,
    [
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
    ],
    'usage revision',
  );
  if (input.response_identity !== 'native_message_id' && input.response_identity !== 'source_record_fallback') {
    throw new ContractValidationError('unsupported usage response identity');
  }
  const responseKey = parseCanonicalBase64(input.response_key, 'response_key');
  const nativeMessageId = optionalRuntimeSemanticText(input.native_message_id, 'native_message_id');
  if (input.response_identity === 'native_message_id') {
    if (nativeMessageId === null) {
      throw new ContractValidationError('native usage response identity requires native_message_id');
    }
    if (!bytesEqual(decodeBase64(responseKey, 'response_key'), textEncoder.encode(nativeMessageId))) {
      throw new ContractValidationError('native usage response key must equal native_message_id');
    }
  } else if (nativeMessageId !== null) {
    throw new ContractValidationError('source-record usage fallback cannot claim a native_message_id');
  }
  const buckets = record(input.buckets, 'usage buckets');
  assertKnownFields(
    buckets,
    ['input_tokens', 'output_tokens', 'cache_creation_input_tokens', 'cache_read_input_tokens'],
    'usage buckets',
  );
  return {
    session: parseOpaqueContractReference(input.session, 'usage session'),
    actor_run: parseOpaqueContractReference(input.actor_run, 'usage actor run'),
    response_key: responseKey,
    response_identity: input.response_identity,
    native_message_id: nativeMessageId,
    request_id: optionalRuntimeSemanticText(input.request_id, 'request_id'),
    buckets: {
      input_tokens: parseUsageQualifiedValue(buckets.input_tokens, 'input_tokens', parseTokenValue),
      output_tokens: parseUsageQualifiedValue(buckets.output_tokens, 'output_tokens', parseTokenValue),
      cache_creation_input_tokens: parseUsageQualifiedValue(
        buckets.cache_creation_input_tokens,
        'cache_creation_input_tokens',
        parseTokenValue,
      ),
      cache_read_input_tokens: parseUsageQualifiedValue(
        buckets.cache_read_input_tokens,
        'cache_read_input_tokens',
        parseTokenValue,
      ),
    },
    model: parseOptionalUsageQualified(input.model, 'model', parseTextValue),
    effort: parseOptionalUsageQualified(input.effort, 'effort', parseTextValue),
    source_time: parseOptionalQualifiedTimestamp(input.source_time, 'usage source_time'),
  };
}

function parseActorExample(value: unknown): ActorRunExample {
  const input = record(value, 'actor run example');
  assertKnownFields(
    input,
    [
      'family',
      'family_version',
      'revision',
      'semantic_revision_key_hex',
      'fact_id',
      'source_record_id',
      'semantic_revision_ref',
    ],
    'actor run example',
  );
  if (input.family !== ACTOR_RUN_FAMILY) {
    throw new ContractValidationError('actor example family must be runtime.actor-run');
  }
  if (input.family_version !== ACTOR_RUN_FAMILY_VERSION) {
    throw new ContractValidationError('unsupported runtime.actor-run version');
  }
  return {
    family: ACTOR_RUN_FAMILY,
    family_version: ACTOR_RUN_FAMILY_VERSION,
    revision: parseActorRunRevision(input.revision),
    semantic_revision_key_hex: parseHexDigest(input.semantic_revision_key_hex, 'actor semantic_revision_key_hex'),
    fact_id: parseOpaqueContractReference(input.fact_id, 'actor fact id'),
    source_record_id: parseOpaqueContractReference(input.source_record_id, 'actor source record id'),
    semantic_revision_ref: parseSemanticRevisionRef(input.semantic_revision_ref),
  };
}

function parseAffiliationExample(value: unknown): ActorAffiliationExample {
  const input = record(value, 'actor affiliation example');
  assertKnownFields(
    input,
    [
      'family',
      'family_version',
      'revision',
      'semantic_revision_key_hex',
      'fact_id',
      'source_record_id',
      'semantic_revision_ref',
    ],
    'actor affiliation example',
  );
  if (input.family !== ACTOR_AFFILIATION_FAMILY) {
    throw new ContractValidationError('affiliation example family must be runtime.actor-affiliation');
  }
  if (input.family_version !== ACTOR_AFFILIATION_FAMILY_VERSION) {
    throw new ContractValidationError('unsupported runtime.actor-affiliation version');
  }
  return {
    family: ACTOR_AFFILIATION_FAMILY,
    family_version: ACTOR_AFFILIATION_FAMILY_VERSION,
    revision: parseActorAffiliationRevision(input.revision),
    semantic_revision_key_hex: parseHexDigest(input.semantic_revision_key_hex, 'affiliation semantic_revision_key_hex'),
    fact_id: parseOpaqueContractReference(input.fact_id, 'affiliation fact id'),
    source_record_id: parseOpaqueContractReference(input.source_record_id, 'affiliation source record id'),
    semantic_revision_ref: parseSemanticRevisionRef(input.semantic_revision_ref),
  };
}

function parseUsageExample(value: unknown): UsageRevisionExample {
  const input = record(value, 'usage example');
  assertKnownFields(
    input,
    [
      'family',
      'family_version',
      'revision',
      'semantic_revision_key_hex',
      'fact_id',
      'source_record_id',
      'semantic_revision_ref',
    ],
    'usage example',
  );
  if (input.family !== USAGE_V2_FAMILY) {
    throw new ContractValidationError('usage example family must be runtime.usage-v2');
  }
  if (input.family_version !== USAGE_V2_FAMILY_VERSION) {
    throw new ContractValidationError('unsupported runtime.usage-v2 version');
  }
  return {
    family: USAGE_V2_FAMILY,
    family_version: USAGE_V2_FAMILY_VERSION,
    revision: parseUsageRevisionV2(input.revision),
    semantic_revision_key_hex: parseHexDigest(input.semantic_revision_key_hex, 'semantic_revision_key_hex'),
    fact_id: parseOpaqueContractReference(input.fact_id, 'usage fact id'),
    source_record_id: parseOpaqueContractReference(input.source_record_id, 'usage source record id'),
    semantic_revision_ref: parseSemanticRevisionRef(input.semantic_revision_ref),
  };
}

function bindRevisionIdentity(
  label: string,
  parsed: {
    revision: unknown;
    semantic_revision_key_hex: string;
    fact_id: OpaqueContractReference;
    source_record_id: OpaqueContractReference;
    semantic_revision_ref: SemanticRevisionRef;
  },
  expected: {
    revision: unknown;
    semantic_revision_key_hex: string;
    fact_id: OpaqueContractReference;
    source_record_id: OpaqueContractReference;
    semantic_revision_ref: SemanticRevisionRef;
  },
): void {
  if (
    JSON.stringify(parsed.revision) !== JSON.stringify(expected.revision) ||
    parsed.semantic_revision_key_hex !== expected.semantic_revision_key_hex ||
    parsed.fact_id !== expected.fact_id ||
    parsed.source_record_id !== expected.source_record_id ||
    parsed.semantic_revision_ref.fact_revision_id !== expected.semantic_revision_ref.fact_revision_id ||
    parsed.semantic_revision_ref.semantic_reference_contract_version !==
      expected.semantic_revision_ref.semantic_reference_contract_version
  ) {
    throw new ContractValidationError(`${label} semantic content does not match the caller-held revision identity`);
  }
}

function parseRuntimeContractFixtureShape(value: unknown): RuntimeContractFixture {
  assertSemanticFixtureGraph(value);
  const input = record(value, 'runtime contract fixture');
  assertKnownFields(
    input,
    [
      'fixture_contract_version',
      'runtime_semantic_contract_version',
      'families',
      'source',
      'actors',
      'affiliations',
      'usage',
    ],
    'runtime contract fixture',
  );
  if (input.fixture_contract_version !== RUNTIME_SEMANTIC_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported runtime fixture contract version');
  }
  if (input.runtime_semantic_contract_version !== RUNTIME_SEMANTIC_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported runtime semantic contract version');
  }
  const families = parseCanonicalFamilies(input.families);

  const sourceInput = record(input.source, 'runtime source');
  assertKnownFields(sourceInput, ['adapter_id', 'source_instance_key', 'session'], 'runtime source');
  const sessionInput = record(sourceInput.session, 'runtime session');
  assertKnownFields(sessionInput, ['entity_key', 'external_ref', 'native_session_id'], 'runtime session');
  const source: RuntimeSourceIdentity = {
    adapter_id: boundedText(sourceInput.adapter_id, 'adapter_id', MAX_ADAPTER_ID_BYTES),
    source_instance_key: parseOpaqueContractReference(sourceInput.source_instance_key, 'source instance key'),
    session: {
      entity_key: parseOpaqueContractReference(sessionInput.entity_key, 'session entity key'),
      external_ref: parseExternalEntityRef(sessionInput.external_ref),
      native_session_id: boundedText(
        sessionInput.native_session_id,
        'native_session_id',
        MAX_RUNTIME_SEMANTIC_TEXT_BYTES,
      ),
    },
  };
  if (source.session.external_ref.entity_key !== source.session.entity_key) {
    throw new ContractValidationError('session external reference must match the session entity key');
  }

  const actorsInput = record(input.actors, 'runtime actors');
  assertKnownFields(actorsInput, ['root', 'child'], 'runtime actors');
  const actors = {
    root: parseActorExample(actorsInput.root),
    child: parseActorExample(actorsInput.child),
  };
  if (actors.root.revision.role !== 'root' || actors.child.revision.role !== 'child') {
    throw new ContractValidationError('runtime fixture must include one root actor and one child actor');
  }
  if (actors.child.revision.parent_actor_run !== actors.root.revision.actor_run) {
    throw new ContractValidationError('child actor must be parented to the fixture root actor');
  }
  if (
    actors.root.revision.session !== source.session.entity_key ||
    actors.child.revision.session !== source.session.entity_key
  ) {
    throw new ContractValidationError('fixture actors must reference the fixture session');
  }

  const affiliationsInput = record(input.affiliations, 'runtime affiliations');
  assertKnownFields(
    affiliationsInput,
    ['child_team_present', 'child_workflow_present', 'child_workflow_removed'],
    'runtime affiliations',
  );
  const affiliations = {
    child_team_present: parseAffiliationExample(affiliationsInput.child_team_present),
    child_workflow_present: parseAffiliationExample(affiliationsInput.child_workflow_present),
    child_workflow_removed: parseAffiliationExample(affiliationsInput.child_workflow_removed),
  };
  if (
    affiliations.child_team_present.revision.dimension !== 'team' ||
    affiliations.child_workflow_present.revision.dimension !== 'workflow' ||
    affiliations.child_workflow_removed.revision.dimension !== 'workflow'
  ) {
    throw new ContractValidationError('fixture affiliations must keep team and workflow dimensions orthogonal');
  }
  if (
    affiliations.child_team_present.revision.actor_run !== actors.child.revision.actor_run ||
    affiliations.child_workflow_present.revision.actor_run !== actors.child.revision.actor_run ||
    affiliations.child_workflow_removed.revision.actor_run !== actors.child.revision.actor_run
  ) {
    throw new ContractValidationError('team and workflow affiliations must attach to the same child actor');
  }
  if (
    affiliations.child_team_present.revision.session !== source.session.entity_key ||
    affiliations.child_workflow_present.revision.session !== source.session.entity_key ||
    affiliations.child_workflow_removed.revision.session !== source.session.entity_key
  ) {
    throw new ContractValidationError('fixture affiliations must reference the fixture session');
  }
  if (affiliations.child_workflow_removed.revision.state !== 'removed') {
    throw new ContractValidationError('fixture must include a removed affiliation revision');
  }
  if (
    affiliations.child_workflow_present.revision.affiliation !==
      affiliations.child_workflow_removed.revision.affiliation ||
    affiliations.child_workflow_present.revision.target !== affiliations.child_workflow_removed.revision.target ||
    affiliations.child_workflow_present.revision.member !== affiliations.child_workflow_removed.revision.member ||
    affiliations.child_workflow_present.fact_id !== affiliations.child_workflow_removed.fact_id
  ) {
    throw new ContractValidationError('workflow removal must revise the same affiliation identity');
  }
  if (
    affiliations.child_workflow_present.semantic_revision_ref.fact_revision_id ===
    affiliations.child_workflow_removed.semantic_revision_ref.fact_revision_id
  ) {
    throw new ContractValidationError('workflow removal must mint a distinct semantic revision');
  }

  const usageInput = record(input.usage, 'runtime usage');
  assertKnownFields(usageInput, ['native_message', 'source_record_fallback', 'response_revisions'], 'runtime usage');
  const revisionsInput = record(usageInput.response_revisions, 'usage response revisions');
  assertKnownFields(revisionsInput, ['native_message_id', 'a', 'b', 'a_repeat'], 'usage response revisions');
  const usage = {
    native_message: parseUsageExample(usageInput.native_message),
    source_record_fallback: parseUsageExample(usageInput.source_record_fallback),
    response_revisions: {
      native_message_id: nonEmptyString(revisionsInput.native_message_id, 'ABA native_message_id'),
      a: parseUsageExample(revisionsInput.a),
      b: parseUsageExample(revisionsInput.b),
      a_repeat: parseUsageExample(revisionsInput.a_repeat),
    },
  };
  if (usage.native_message.revision.response_identity !== 'native_message_id') {
    throw new ContractValidationError('native usage example must use a native message identity');
  }
  if (usage.source_record_fallback.revision.response_identity !== 'source_record_fallback') {
    throw new ContractValidationError('fallback usage example must use a source-record identity');
  }
  if (
    usage.response_revisions.a.semantic_revision_ref.fact_revision_id !==
      usage.response_revisions.a_repeat.semantic_revision_ref.fact_revision_id ||
    usage.response_revisions.a.semantic_revision_key_hex !== usage.response_revisions.a_repeat.semantic_revision_key_hex
  ) {
    throw new ContractValidationError('A and A-repeat usage revisions must share semantic identity');
  }
  if (
    usage.response_revisions.a.semantic_revision_ref.fact_revision_id ===
    usage.response_revisions.b.semantic_revision_ref.fact_revision_id
  ) {
    throw new ContractValidationError('A and B usage revisions must have distinct semantic identity');
  }
  if (
    usage.response_revisions.a.fact_id !== usage.response_revisions.b.fact_id ||
    usage.response_revisions.a.fact_id !== usage.response_revisions.a_repeat.fact_id
  ) {
    throw new ContractValidationError('A -> B -> A usage revisions must share one response fact identity');
  }
  if (
    usage.response_revisions.a.revision.native_message_id !== usage.response_revisions.native_message_id ||
    usage.response_revisions.b.revision.native_message_id !== usage.response_revisions.native_message_id ||
    usage.response_revisions.a_repeat.revision.native_message_id !== usage.response_revisions.native_message_id
  ) {
    throw new ContractValidationError('A -> B -> A revisions must use the declared native message ID');
  }
  for (const example of [
    usage.native_message,
    usage.source_record_fallback,
    usage.response_revisions.a,
    usage.response_revisions.b,
    usage.response_revisions.a_repeat,
  ]) {
    if (example.revision.session !== source.session.entity_key) {
      throw new ContractValidationError('fixture usage revisions must reference the fixture session');
    }
    if (
      example.revision.actor_run !== actors.child.revision.actor_run &&
      example.revision.actor_run !== actors.root.revision.actor_run
    ) {
      throw new ContractValidationError('fixture usage revisions must reference a fixture actor');
    }
  }

  const sourceRecordId = actors.root.source_record_id;
  for (const example of [
    actors.child,
    affiliations.child_team_present,
    affiliations.child_workflow_present,
    affiliations.child_workflow_removed,
    usage.native_message,
    usage.source_record_fallback,
    usage.response_revisions.a,
    usage.response_revisions.b,
    usage.response_revisions.a_repeat,
  ]) {
    if (example.source_record_id !== sourceRecordId) {
      throw new ContractValidationError('fixture examples must share one source-record identity');
    }
  }

  return {
    fixture_contract_version: RUNTIME_SEMANTIC_CONTRACT_VERSION,
    runtime_semantic_contract_version: RUNTIME_SEMANTIC_CONTRACT_VERSION,
    families,
    source,
    actors,
    affiliations,
    usage,
  };
}

export function parseRuntimeContractFixture(value: unknown, expectedContextInput: unknown): RuntimeContractFixture {
  const expected = parseRuntimeContractFixtureShape(expectedContextInput);
  const parsed = parseRuntimeContractFixtureShape(value);
  bindRevisionIdentity('root actor', parsed.actors.root, expected.actors.root);
  bindRevisionIdentity('child actor', parsed.actors.child, expected.actors.child);
  bindRevisionIdentity(
    'child team affiliation',
    parsed.affiliations.child_team_present,
    expected.affiliations.child_team_present,
  );
  bindRevisionIdentity(
    'child workflow present affiliation',
    parsed.affiliations.child_workflow_present,
    expected.affiliations.child_workflow_present,
  );
  bindRevisionIdentity(
    'child workflow removed affiliation',
    parsed.affiliations.child_workflow_removed,
    expected.affiliations.child_workflow_removed,
  );
  bindRevisionIdentity('native usage', parsed.usage.native_message, expected.usage.native_message);
  bindRevisionIdentity(
    'source-record usage fallback',
    parsed.usage.source_record_fallback,
    expected.usage.source_record_fallback,
  );
  bindRevisionIdentity('usage revision A', parsed.usage.response_revisions.a, expected.usage.response_revisions.a);
  bindRevisionIdentity('usage revision B', parsed.usage.response_revisions.b, expected.usage.response_revisions.b);
  bindRevisionIdentity(
    'usage revision A-repeat',
    parsed.usage.response_revisions.a_repeat,
    expected.usage.response_revisions.a_repeat,
  );
  return parsed;
}

export function parseRfc012cRuntimeV1Json(json: string, expectedContextInput: unknown): RuntimeContractFixture {
  return parseRuntimeContractFixture(preflightSemanticFixtureJson(json), expectedContextInput);
}

export type EffectiveStateDimension = 'model' | 'effort' | 'session_mode' | 'permission_mode';
export type EffectiveStateEvidenceKind = 'configured_intent' | 'response_observed' | 'native_transition';
export type EffectiveStateOperation = 'upsert' | 'retract';
export type EffectiveStateValueAuthority = 'native_configuration' | 'native_response' | 'native_transition';
export interface EffectiveStateValueProvenance {
  native_field: string;
  normalization_contract_version: number;
}
export type EffectiveStateQualifiedValue<T> = QualifiedValue<
  T,
  EffectiveStateValueAuthority,
  EffectiveStateValueProvenance
>;
export type UserInputKind = 'choice' | 'multi_choice' | 'free_text' | 'mixed';
export type UserInputLifecycleState = 'pending' | 'resolved' | 'failed' | 'cancelled';
export type UserInputOperation = 'upsert' | 'retract';

export interface EffectiveStateSlot {
  completeness: ContractCompleteness;
  evidence_kind: EffectiveStateEvidenceKind;
  operation: EffectiveStateOperation;
  semantic_revision_key_hex: string;
  semantic_revision_ref: SemanticRevisionRef;
  value: EffectiveStateQualifiedValue<string>;
}

export interface EffectiveStateFixture {
  adapter_id: string;
  family: typeof EFFECTIVE_STATE_FAMILY;
  family_version: typeof EFFECTIVE_STATE_FAMILY_VERSION;
  fixture_contract_version: typeof RUNTIME_SEMANTIC_CONTRACT_VERSION;
  runtime_semantic_contract_version: typeof RUNTIME_SEMANTIC_CONTRACT_VERSION;
  source_instance_key: OpaqueContractReference;
  session: OpaqueContractReference;
  actor_run: OpaqueContractReference;
  dimension: EffectiveStateDimension;
  fact_id: OpaqueContractReference;
  source_record_id: OpaqueContractReference;
  configured: EffectiveStateSlot;
  observed: EffectiveStateSlot;
  retract: EffectiveStateSlot;
}

export interface UserInputOption {
  label: string;
  description: string | null;
  preview: string | null;
}

export interface UserInputQuestion {
  header: string | null;
  prompt: string;
  options: UserInputOption[];
  multi_select: boolean;
}

export interface InteractionLifecycleSlot {
  completeness: ContractCompleteness;
  operation: UserInputOperation;
  result_reference: string | null;
  semantic_revision_key_hex: string;
  semantic_revision_ref: SemanticRevisionRef;
  state: UserInputLifecycleState;
}

export interface InteractionFixture {
  adapter_id: string;
  actor_run: OpaqueContractReference;
  family: typeof USER_INPUT_FAMILY;
  family_version: typeof USER_INPUT_FAMILY_VERSION;
  fixture_contract_version: typeof RUNTIME_SEMANTIC_CONTRACT_VERSION;
  kind: UserInputKind;
  native_tool_use_id: string;
  fact_id: OpaqueContractReference;
  source_record_id: OpaqueContractReference;
  pending: InteractionLifecycleSlot;
  resolved: InteractionLifecycleSlot;
  failed: InteractionLifecycleSlot;
  cancelled: InteractionLifecycleSlot;
  retract: InteractionLifecycleSlot;
  partial: InteractionLifecycleSlot;
  questions: UserInputQuestion[];
  runtime_semantic_contract_version: typeof RUNTIME_SEMANTIC_CONTRACT_VERSION;
  session: OpaqueContractReference;
  source_instance_key: OpaqueContractReference;
}

function parseContractCompleteness(value: unknown, label: string): ContractCompleteness {
  if (value !== 'complete' && value !== 'partial' && value !== 'unknown') {
    throw new ContractValidationError(`${label} must be complete, partial, or unknown`);
  }
  return value;
}

function bindSlotIdentity(
  label: string,
  parsed: { semantic_revision_key_hex: string; semantic_revision_ref: SemanticRevisionRef },
  expected: { semantic_revision_key_hex: string; semantic_revision_ref: SemanticRevisionRef },
): void {
  if (
    parsed.semantic_revision_key_hex !== expected.semantic_revision_key_hex ||
    parsed.semantic_revision_ref.fact_revision_id !== expected.semantic_revision_ref.fact_revision_id ||
    parsed.semantic_revision_ref.semantic_reference_contract_version !==
      expected.semantic_revision_ref.semantic_reference_contract_version
  ) {
    throw new ContractValidationError(`${label} semantic content does not match the caller-held revision identity`);
  }
}

function bindFixtureSemanticContext(label: string, parsed: unknown, expected: unknown): void {
  if (JSON.stringify(parsed) !== JSON.stringify(expected)) {
    throw new ContractValidationError(`${label} semantic content does not match the caller-held revision identity`);
  }
}

function parseEffectiveStateProvenance(value: unknown): EffectiveStateValueProvenance {
  const input = record(value, 'effective-state provenance');
  assertKnownFields(input, ['native_field', 'normalization_contract_version'], 'effective-state provenance');
  return {
    native_field: parseNativeField(input.native_field, 'effective-state provenance native_field'),
    normalization_contract_version: positiveU32(
      input.normalization_contract_version,
      'effective-state provenance normalization_contract_version',
    ),
  };
}

function parseEffectiveStateAuthority(value: unknown, label: string): EffectiveStateValueAuthority {
  if (value !== 'native_configuration' && value !== 'native_response' && value !== 'native_transition') {
    throw new ContractValidationError(`${label} must be native_configuration, native_response, or native_transition`);
  }
  return value;
}

function parseEffectiveStateQualifiedValue(value: unknown): EffectiveStateQualifiedValue<string> {
  return parseQualifiedValue<string, EffectiveStateValueAuthority, EffectiveStateValueProvenance>(value, {
    parseKnownValue: (raw, label) => boundedText(raw, label, MAX_RUNTIME_SEMANTIC_TEXT_BYTES),
    parseAuthority: parseEffectiveStateAuthority,
    parseProvenance: parseEffectiveStateProvenance,
  });
}

function parseEffectiveStateSlot(value: unknown, label: string): EffectiveStateSlot {
  const input = record(value, label);
  assertKnownFields(
    input,
    ['completeness', 'evidence_kind', 'operation', 'semantic_revision_key_hex', 'semantic_revision_ref', 'value'],
    label,
  );
  const evidence = input.evidence_kind;
  if (evidence !== 'configured_intent' && evidence !== 'response_observed' && evidence !== 'native_transition') {
    throw new ContractValidationError(`${label} evidence_kind is unsupported`);
  }
  const operation = input.operation;
  if (operation !== 'upsert' && operation !== 'retract') {
    throw new ContractValidationError(`${label} operation is unsupported`);
  }
  const completeness = parseContractCompleteness(input.completeness, `${label} completeness`);
  const qualifiedValue = parseEffectiveStateQualifiedValue(input.value);
  if (qualifiedValue.completeness !== completeness) {
    throw new ContractValidationError(`${label} value and revision completeness must match`);
  }
  const expectedAuthority: EffectiveStateValueAuthority =
    evidence === 'configured_intent'
      ? 'native_configuration'
      : evidence === 'response_observed'
        ? 'native_response'
        : 'native_transition';
  if (qualifiedValue.authority !== expectedAuthority) {
    throw new ContractValidationError(`${label} value authority does not match its evidence kind`);
  }
  return {
    completeness,
    evidence_kind: evidence,
    operation,
    semantic_revision_key_hex: parseHexDigest(input.semantic_revision_key_hex, `${label} semantic_revision_key_hex`),
    semantic_revision_ref: parseSemanticRevisionRef(input.semantic_revision_ref),
    value: qualifiedValue,
  };
}

function parseEffectiveStateFixtureShape(value: unknown): EffectiveStateFixture {
  assertSemanticFixtureGraph(value);
  const input = record(value, 'effective-state fixture');
  assertKnownFields(
    input,
    [
      'adapter_id',
      'family',
      'family_version',
      'fixture_contract_version',
      'runtime_semantic_contract_version',
      'source_instance_key',
      'session',
      'actor_run',
      'dimension',
      'fact_id',
      'source_record_id',
      'configured',
      'observed',
      'retract',
    ],
    'effective-state fixture',
  );
  if (input.fixture_contract_version !== RUNTIME_SEMANTIC_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported effective-state fixture contract version');
  }
  if (input.runtime_semantic_contract_version !== RUNTIME_SEMANTIC_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported runtime semantic contract version');
  }
  if (input.family !== EFFECTIVE_STATE_FAMILY || input.family_version !== EFFECTIVE_STATE_FAMILY_VERSION) {
    throw new ContractValidationError('effective-state fixture family must be runtime.effective-state@1');
  }
  const dimension = input.dimension;
  if (
    dimension !== 'model' &&
    dimension !== 'effort' &&
    dimension !== 'session_mode' &&
    dimension !== 'permission_mode'
  ) {
    throw new ContractValidationError('unsupported effective-state dimension');
  }
  const configured = parseEffectiveStateSlot(input.configured, 'configured');
  const observed = parseEffectiveStateSlot(input.observed, 'observed');
  const retract = parseEffectiveStateSlot(input.retract, 'retract');
  if (configured.evidence_kind !== 'configured_intent' || configured.operation !== 'upsert') {
    throw new ContractValidationError('effective-state configured slot must be configured_intent upsert');
  }
  const expectedObservedEvidence =
    dimension === 'model' || dimension === 'effort' ? 'response_observed' : 'native_transition';
  if (observed.evidence_kind !== expectedObservedEvidence || observed.operation !== 'upsert') {
    throw new ContractValidationError(
      `effective-state observed slot must be ${expectedObservedEvidence} upsert for ${dimension}`,
    );
  }
  if (retract.evidence_kind !== 'native_transition' || retract.operation !== 'retract') {
    throw new ContractValidationError('effective-state retract slot must be native_transition retract');
  }
  if (
    configured.completeness !== 'complete' ||
    observed.completeness !== 'complete' ||
    retract.completeness !== 'complete'
  ) {
    throw new ContractValidationError('effective-state configured, observed, and retract must be complete');
  }
  if (
    configured.semantic_revision_ref.fact_revision_id === observed.semantic_revision_ref.fact_revision_id ||
    configured.semantic_revision_ref.fact_revision_id === retract.semantic_revision_ref.fact_revision_id ||
    observed.semantic_revision_ref.fact_revision_id === retract.semantic_revision_ref.fact_revision_id
  ) {
    throw new ContractValidationError(
      'effective-state configured, observed, and retract revisions must have distinct semantic identity',
    );
  }
  return {
    adapter_id: boundedText(input.adapter_id, 'adapter_id', MAX_ADAPTER_ID_BYTES),
    family: EFFECTIVE_STATE_FAMILY,
    family_version: EFFECTIVE_STATE_FAMILY_VERSION,
    fixture_contract_version: RUNTIME_SEMANTIC_CONTRACT_VERSION,
    runtime_semantic_contract_version: RUNTIME_SEMANTIC_CONTRACT_VERSION,
    source_instance_key: parseOpaqueContractReference(input.source_instance_key, 'source instance key'),
    session: parseOpaqueContractReference(input.session, 'session'),
    actor_run: parseOpaqueContractReference(input.actor_run, 'actor run'),
    dimension,
    fact_id: parseOpaqueContractReference(input.fact_id, 'effective-state fact id'),
    source_record_id: parseOpaqueContractReference(input.source_record_id, 'effective-state source record id'),
    configured,
    observed,
    retract,
  };
}

export function parseEffectiveStateFixture(value: unknown, expectedContextInput: unknown): EffectiveStateFixture {
  const expected = parseEffectiveStateFixtureShape(expectedContextInput);
  const parsed = parseEffectiveStateFixtureShape(value);
  if (
    parsed.fact_id !== expected.fact_id ||
    parsed.source_record_id !== expected.source_record_id ||
    parsed.session !== expected.session ||
    parsed.actor_run !== expected.actor_run
  ) {
    throw new ContractValidationError('effective-state identity does not match the caller-held revision identity');
  }
  bindSlotIdentity('configured', parsed.configured, expected.configured);
  bindSlotIdentity('observed', parsed.observed, expected.observed);
  bindSlotIdentity('retract', parsed.retract, expected.retract);
  bindFixtureSemanticContext('effective-state fixture', parsed, expected);
  return parsed;
}

export function parseRfc012cEffectiveStateV1Json(json: string, expectedContextInput: unknown): EffectiveStateFixture {
  return parseEffectiveStateFixture(preflightSemanticFixtureJson(json), expectedContextInput);
}

function parseUserInputOption(value: unknown, label: string): UserInputOption {
  const input = record(value, label);
  assertKnownFields(input, ['label', 'description', 'preview'], label);
  return {
    label: boundedText(input.label, `${label} label`, MAX_RUNTIME_SEMANTIC_TEXT_BYTES),
    description:
      input.description === null || input.description === undefined
        ? null
        : boundedText(input.description, `${label} description`, MAX_RUNTIME_SEMANTIC_TEXT_BYTES),
    preview:
      input.preview === null || input.preview === undefined
        ? null
        : boundedText(input.preview, `${label} preview`, MAX_RUNTIME_SEMANTIC_TEXT_BYTES),
  };
}

function parseUserInputQuestion(value: unknown, label: string): UserInputQuestion {
  const input = record(value, label);
  assertKnownFields(input, ['header', 'prompt', 'options', 'multi_select'], label);
  if (!Array.isArray(input.options) || input.options.length > MAX_INTERACTION_OPTIONS) {
    throw new ContractValidationError(`${label} options exceed ${MAX_INTERACTION_OPTIONS}`);
  }
  if (typeof input.multi_select !== 'boolean') {
    throw new ContractValidationError(`${label} multi_select must be a boolean`);
  }
  return {
    header:
      input.header === null || input.header === undefined
        ? null
        : boundedText(input.header, `${label} header`, MAX_RUNTIME_SEMANTIC_TEXT_BYTES),
    prompt: boundedText(input.prompt, `${label} prompt`, MAX_RUNTIME_SEMANTIC_TEXT_BYTES),
    options: input.options.map((option, index) => parseUserInputOption(option, `${label} option ${index}`)),
    multi_select: input.multi_select,
  };
}

function parseInteractionSlot(
  value: unknown,
  label: string,
  expectedState: UserInputLifecycleState,
  expectedOperation: UserInputOperation,
  expectedCompleteness: ContractCompleteness,
  requireResult: boolean,
): InteractionLifecycleSlot {
  const input = record(value, label);
  assertKnownFields(
    input,
    ['completeness', 'operation', 'result_reference', 'semantic_revision_key_hex', 'semantic_revision_ref', 'state'],
    label,
  );
  const state = input.state;
  if (state !== 'pending' && state !== 'resolved' && state !== 'failed' && state !== 'cancelled') {
    throw new ContractValidationError(`${label} state is unsupported`);
  }
  if (state !== expectedState) {
    throw new ContractValidationError('interaction lifecycle state does not match its fixture slot');
  }
  const operation = input.operation;
  if (operation !== 'upsert' && operation !== 'retract') {
    throw new ContractValidationError(`${label} operation is unsupported`);
  }
  if (operation !== expectedOperation) {
    throw new ContractValidationError(`interaction ${label} operation does not match its fixture slot`);
  }
  const completeness = parseContractCompleteness(input.completeness, `${label} completeness`);
  if (completeness !== expectedCompleteness) {
    throw new ContractValidationError(`interaction ${label} completeness does not match its fixture slot`);
  }
  const resultReference =
    input.result_reference === null || input.result_reference === undefined
      ? null
      : boundedText(input.result_reference, `${label} result_reference`, MAX_RUNTIME_SEMANTIC_TEXT_BYTES);
  if (requireResult && resultReference === null) {
    throw new ContractValidationError('resolved interaction requires a typed result_reference');
  }
  if (expectedState === 'pending' && resultReference !== null) {
    throw new ContractValidationError('pending interaction cannot carry a result_reference');
  }
  return {
    completeness,
    operation,
    result_reference: resultReference,
    semantic_revision_key_hex: parseHexDigest(input.semantic_revision_key_hex, `${label} semantic_revision_key_hex`),
    semantic_revision_ref: parseSemanticRevisionRef(input.semantic_revision_ref),
    state,
  };
}

function parseInteractionFixtureShape(value: unknown): InteractionFixture {
  assertSemanticFixtureGraph(value);
  const input = record(value, 'interaction fixture');
  assertKnownFields(
    input,
    [
      'adapter_id',
      'actor_run',
      'family',
      'family_version',
      'fixture_contract_version',
      'kind',
      'native_tool_use_id',
      'fact_id',
      'source_record_id',
      'pending',
      'resolved',
      'failed',
      'cancelled',
      'retract',
      'partial',
      'questions',
      'runtime_semantic_contract_version',
      'session',
      'source_instance_key',
    ],
    'interaction fixture',
  );
  if (input.fixture_contract_version !== RUNTIME_SEMANTIC_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported interaction fixture contract version');
  }
  if (input.runtime_semantic_contract_version !== RUNTIME_SEMANTIC_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported runtime semantic contract version');
  }
  if (input.family !== USER_INPUT_FAMILY || input.family_version !== USER_INPUT_FAMILY_VERSION) {
    throw new ContractValidationError('interaction fixture family must be runtime.user-input-request@1');
  }
  const kind = input.kind;
  if (kind !== 'choice' && kind !== 'multi_choice' && kind !== 'free_text' && kind !== 'mixed') {
    throw new ContractValidationError('unsupported interaction kind');
  }
  if (
    !Array.isArray(input.questions) ||
    input.questions.length === 0 ||
    input.questions.length > MAX_INTERACTION_QUESTIONS
  ) {
    throw new ContractValidationError(
      `interaction questions must contain 1..=${MAX_INTERACTION_QUESTIONS} typed questions`,
    );
  }
  const pending = parseInteractionSlot(input.pending, 'pending', 'pending', 'upsert', 'complete', false);
  const resolved = parseInteractionSlot(input.resolved, 'resolved', 'resolved', 'upsert', 'complete', true);
  const failed = parseInteractionSlot(input.failed, 'failed', 'failed', 'upsert', 'complete', false);
  const cancelled = parseInteractionSlot(input.cancelled, 'cancelled', 'cancelled', 'upsert', 'complete', false);
  const retract = parseInteractionSlot(input.retract, 'retract', 'pending', 'retract', 'complete', false);
  const partial = parseInteractionSlot(input.partial, 'partial', 'pending', 'upsert', 'partial', false);
  const refs = [
    pending.semantic_revision_ref.fact_revision_id,
    resolved.semantic_revision_ref.fact_revision_id,
    failed.semantic_revision_ref.fact_revision_id,
    cancelled.semantic_revision_ref.fact_revision_id,
    retract.semantic_revision_ref.fact_revision_id,
    partial.semantic_revision_ref.fact_revision_id,
  ];
  if (new Set(refs).size !== refs.length) {
    throw new ContractValidationError('interaction lifecycle slots must have distinct semantic identity');
  }
  return {
    adapter_id: boundedText(input.adapter_id, 'adapter_id', MAX_ADAPTER_ID_BYTES),
    actor_run: parseOpaqueContractReference(input.actor_run, 'actor run'),
    family: USER_INPUT_FAMILY,
    family_version: USER_INPUT_FAMILY_VERSION,
    fixture_contract_version: RUNTIME_SEMANTIC_CONTRACT_VERSION,
    kind,
    native_tool_use_id: boundedText(input.native_tool_use_id, 'native_tool_use_id', MAX_RUNTIME_SEMANTIC_TEXT_BYTES),
    fact_id: parseOpaqueContractReference(input.fact_id, 'interaction fact id'),
    source_record_id: parseOpaqueContractReference(input.source_record_id, 'interaction source record id'),
    pending,
    resolved,
    failed,
    cancelled,
    retract,
    partial,
    questions: input.questions.map((question, index) => parseUserInputQuestion(question, `question ${index}`)),
    runtime_semantic_contract_version: RUNTIME_SEMANTIC_CONTRACT_VERSION,
    session: parseOpaqueContractReference(input.session, 'session'),
    source_instance_key: parseOpaqueContractReference(input.source_instance_key, 'source instance key'),
  };
}

export function parseInteractionFixture(value: unknown, expectedContextInput: unknown): InteractionFixture {
  const expected = parseInteractionFixtureShape(expectedContextInput);
  const parsed = parseInteractionFixtureShape(value);
  if (
    parsed.fact_id !== expected.fact_id ||
    parsed.source_record_id !== expected.source_record_id ||
    parsed.session !== expected.session ||
    parsed.actor_run !== expected.actor_run
  ) {
    throw new ContractValidationError('interaction identity does not match the caller-held revision identity');
  }
  bindSlotIdentity('pending', parsed.pending, expected.pending);
  bindSlotIdentity('resolved', parsed.resolved, expected.resolved);
  bindSlotIdentity('failed', parsed.failed, expected.failed);
  bindSlotIdentity('cancelled', parsed.cancelled, expected.cancelled);
  bindSlotIdentity('retract', parsed.retract, expected.retract);
  bindSlotIdentity('partial', parsed.partial, expected.partial);
  bindFixtureSemanticContext('interaction fixture', parsed, expected);
  return parsed;
}

export function parseRfc012cInteractionV1Json(json: string, expectedContextInput: unknown): InteractionFixture {
  return parseInteractionFixture(preflightSemanticFixtureJson(json), expectedContextInput);
}

export type MessageRevisionRole = 'user' | 'assistant' | 'system';
export type TaskLifecycleState = 'created' | 'updated' | 'completed' | 'failed' | 'cancelled' | 'removed';

export interface MessageRevisionSlot {
  completeness: ContractCompleteness;
  operation: UserInputOperation;
  ordered_content_block_keys: string[];
  semantic_revision_key_hex: string;
  semantic_revision_ref: SemanticRevisionRef;
}

export type ContentBlockRevisionValue =
  | { kind: 'text'; text: string }
  | { kind: 'thinking'; text: string; redacted: boolean }
  | { kind: 'tool_call'; tool_name: string; input_digest: number[] }
  | { kind: 'tool_result'; content_digest: number[]; is_error: boolean }
  | { kind: 'image'; media_type: string; data_hash: number[] }
  | { kind: 'document'; media_type: string; data_hash: number[] }
  | { kind: 'native_extension'; native_kind: string; value_digest: number[] };

export interface ContentBlockRevisionSlot {
  ordinal: number;
  content: ContentBlockRevisionValue;
  native_tool_call_or_result_id: string | null;
  completeness: ContractCompleteness;
  operation: UserInputOperation;
  semantic_revision_key_hex: string;
  semantic_revision_ref: SemanticRevisionRef;
}

export interface ContentBlockFixture {
  family: typeof CONTENT_BLOCK_FAMILY;
  family_version: typeof CONTENT_BLOCK_FAMILY_VERSION;
  native_content_block_id: string;
  fact_id: OpaqueContractReference;
  current: ContentBlockRevisionSlot;
  correction: ContentBlockRevisionSlot;
  retract: ContentBlockRevisionSlot;
  partial_retract: ContentBlockRevisionSlot;
}

export interface MessageFixture {
  adapter_id: string;
  actor_run: OpaqueContractReference;
  family: typeof MESSAGE_FAMILY;
  family_version: typeof MESSAGE_FAMILY_VERSION;
  fixture_contract_version: typeof RUNTIME_SEMANTIC_CONTRACT_VERSION;
  native_message_id: string;
  fact_id: OpaqueContractReference;
  source_record_id: OpaqueContractReference;
  role: MessageRevisionRole;
  content_block?: ContentBlockFixture;
  current: MessageRevisionSlot;
  correction: MessageRevisionSlot;
  complete_blocks: MessageRevisionSlot;
  partial_blocks: MessageRevisionSlot;
  retract: MessageRevisionSlot;
  partial: MessageRevisionSlot;
  runtime_semantic_contract_version: typeof RUNTIME_SEMANTIC_CONTRACT_VERSION;
  session: OpaqueContractReference;
  source_instance_key: OpaqueContractReference;
}

export interface TaskRevisionSlot {
  completeness: ContractCompleteness;
  operation: UserInputOperation;
  owned_set: string[] | null;
  semantic_revision_key_hex: string;
  semantic_revision_ref: SemanticRevisionRef;
  state: TaskLifecycleState;
}

export interface TaskFixture {
  adapter_id: string;
  actor_run: OpaqueContractReference;
  family: typeof TASK_FAMILY;
  family_version: typeof TASK_FAMILY_VERSION;
  fixture_contract_version: typeof RUNTIME_SEMANTIC_CONTRACT_VERSION;
  native_task_id: string;
  peer_native_task_id: string;
  fact_id: OpaqueContractReference;
  peer_fact_id: OpaqueContractReference;
  source_record_id: OpaqueContractReference;
  subject: string;
  created: TaskRevisionSlot;
  updated: TaskRevisionSlot;
  completed: TaskRevisionSlot;
  retract: TaskRevisionSlot;
  partial: TaskRevisionSlot;
  collection_omit: TaskRevisionSlot;
  runtime_semantic_contract_version: typeof RUNTIME_SEMANTIC_CONTRACT_VERSION;
  session: OpaqueContractReference;
  source_instance_key: OpaqueContractReference;
}

const MESSAGE_SLOT_FIELDS = [
  'completeness',
  'operation',
  'ordered_content_block_keys',
  'semantic_revision_key_hex',
  'semantic_revision_ref',
] as const;

function parseContentMediaType(value: unknown, label: string): string {
  if (typeof value !== 'string' || !/^[a-z0-9!#$&^_.+-]{1,127}\/[a-z0-9!#$&^_.+-]{1,127}$/.test(value)) {
    throw new ContractValidationError(`${label} must be a canonical MIME type`);
  }
  return value;
}

function parseContentBlockValue(value: unknown): ContentBlockRevisionValue {
  const input = record(value, 'content-block content');
  switch (input.kind) {
    case 'text':
      assertKnownFields(input, ['kind', 'text'], 'content-block text');
      return { kind: 'text', text: boundedContentText(input.text, 'content-block text') };
    case 'thinking':
      assertKnownFields(input, ['kind', 'text', 'redacted'], 'content-block thinking');
      if (typeof input.redacted !== 'boolean') {
        throw new ContractValidationError('content-block thinking redacted must be boolean');
      }
      return {
        kind: 'thinking',
        text: boundedContentText(input.text, 'content-block thinking text'),
        redacted: input.redacted,
      };
    case 'tool_call':
      assertKnownFields(input, ['kind', 'tool_name', 'input_digest'], 'content-block tool call');
      return {
        kind: 'tool_call',
        tool_name: boundedText(input.tool_name, 'content-block tool name', MAX_RUNTIME_SEMANTIC_TEXT_BYTES),
        input_digest: digest32(input.input_digest, 'content-block tool-call input digest'),
      };
    case 'tool_result':
      assertKnownFields(input, ['kind', 'content_digest', 'is_error'], 'content-block tool result');
      if (typeof input.is_error !== 'boolean') {
        throw new ContractValidationError('content-block tool result is_error must be boolean');
      }
      return {
        kind: 'tool_result',
        content_digest: digest32(input.content_digest, 'content-block tool-result content digest'),
        is_error: input.is_error,
      };
    case 'image':
    case 'document': {
      assertKnownFields(input, ['kind', 'media_type', 'data_hash'], `content-block ${input.kind}`);
      return {
        kind: input.kind,
        media_type: parseContentMediaType(input.media_type, `content-block ${input.kind} media_type`),
        data_hash: digest32(input.data_hash, `content-block ${input.kind} data hash`),
      };
    }
    case 'native_extension':
      assertKnownFields(input, ['kind', 'native_kind', 'value_digest'], 'content-block native extension');
      return {
        kind: 'native_extension',
        native_kind: parseNativeField(input.native_kind, 'content-block native kind'),
        value_digest: digest32(input.value_digest, 'content-block native extension digest'),
      };
    default:
      throw new ContractValidationError('content-block content kind is unsupported');
  }
}

function parseContentBlockSlot(
  value: unknown,
  label: string,
  operation: UserInputOperation,
  completeness: ContractCompleteness,
): ContentBlockRevisionSlot {
  const input = record(value, label);
  assertKnownFields(
    input,
    [
      'ordinal',
      'content',
      'native_tool_call_or_result_id',
      'completeness',
      'operation',
      'semantic_revision_key_hex',
      'semantic_revision_ref',
    ],
    label,
  );
  if (input.operation !== operation || input.completeness !== completeness) {
    throw new ContractValidationError(`${label} operation/completeness does not match its fixture slot`);
  }
  if (!Object.hasOwn(input, 'native_tool_call_or_result_id')) {
    throw new ContractValidationError(`${label} must declare native_tool_call_or_result_id`);
  }
  const content = parseContentBlockValue(input.content);
  const nativeToolId = optionalRuntimeSemanticText(
    input.native_tool_call_or_result_id,
    `${label} native_tool_call_or_result_id`,
  );
  const isToolContent = content.kind === 'tool_call' || content.kind === 'tool_result';
  if (nativeToolId !== null && !isToolContent) {
    throw new ContractValidationError(`${label} tool identity may be present only for tool call/result content`);
  }
  return {
    ordinal: nonNegativeU32(input.ordinal, `${label} ordinal`),
    content,
    native_tool_call_or_result_id: nativeToolId,
    completeness,
    operation,
    semantic_revision_key_hex: parseHexDigest(input.semantic_revision_key_hex, `${label} semantic_revision_key_hex`),
    semantic_revision_ref: parseSemanticRevisionRef(input.semantic_revision_ref),
  };
}

function parseContentBlockFixture(value: unknown): ContentBlockFixture {
  const input = record(value, 'content-block fixture');
  assertKnownFields(
    input,
    [
      'family',
      'family_version',
      'native_content_block_id',
      'fact_id',
      'current',
      'correction',
      'retract',
      'partial_retract',
    ],
    'content-block fixture',
  );
  if (input.family !== CONTENT_BLOCK_FAMILY || input.family_version !== CONTENT_BLOCK_FAMILY_VERSION) {
    throw new ContractValidationError('content-block fixture family must be runtime.content-block@1');
  }
  const current = parseContentBlockSlot(input.current, 'content-block current', 'upsert', 'complete');
  const correction = parseContentBlockSlot(input.correction, 'content-block correction', 'upsert', 'complete');
  const retract = parseContentBlockSlot(input.retract, 'content-block retract', 'retract', 'complete');
  const partialRetract = parseContentBlockSlot(
    input.partial_retract,
    'content-block partial_retract',
    'retract',
    'partial',
  );
  if (JSON.stringify(current.content) === JSON.stringify(correction.content)) {
    throw new ContractValidationError('content-block correction must change the normalized content');
  }
  if (
    JSON.stringify(retract.content) !== JSON.stringify(correction.content) ||
    JSON.stringify(partialRetract.content) !== JSON.stringify(correction.content) ||
    current.ordinal !== correction.ordinal ||
    current.ordinal !== retract.ordinal ||
    current.ordinal !== partialRetract.ordinal ||
    retract.native_tool_call_or_result_id !== correction.native_tool_call_or_result_id ||
    partialRetract.native_tool_call_or_result_id !== correction.native_tool_call_or_result_id
  ) {
    throw new ContractValidationError(
      'content-block replacement slots must retain the corrected entity value and ordinal',
    );
  }
  const revisionIds = [current, correction, retract, partialRetract].map(
    (slot) => slot.semantic_revision_ref.fact_revision_id,
  );
  if (new Set(revisionIds).size !== revisionIds.length) {
    throw new ContractValidationError('content-block revision slots must have distinct semantic identity');
  }
  return {
    family: CONTENT_BLOCK_FAMILY,
    family_version: CONTENT_BLOCK_FAMILY_VERSION,
    native_content_block_id: boundedText(
      input.native_content_block_id,
      'native_content_block_id',
      MAX_RUNTIME_SEMANTIC_TEXT_BYTES,
    ),
    fact_id: parseOpaqueContractReference(input.fact_id, 'content-block fact id'),
    current,
    correction,
    retract,
    partial_retract: partialRetract,
  };
}

function parseMessageSlot(
  value: unknown,
  label: string,
  operation: UserInputOperation,
  completeness: ContractCompleteness,
  expectedKeys: readonly string[],
): MessageRevisionSlot {
  const input = record(value, label);
  assertKnownFields(input, MESSAGE_SLOT_FIELDS, label);
  if (input.operation !== operation) {
    throw new ContractValidationError(`${label} operation does not match its fixture slot`);
  }
  const parsedCompleteness = parseContractCompleteness(input.completeness, `${label} completeness`);
  if (parsedCompleteness !== completeness) {
    throw new ContractValidationError(`${label} completeness does not match its fixture slot`);
  }
  if (
    !Array.isArray(input.ordered_content_block_keys) ||
    input.ordered_content_block_keys.length === 0 ||
    input.ordered_content_block_keys.length > MAX_MESSAGE_CONTENT_BLOCKS
  ) {
    throw new ContractValidationError(
      `${label} ordered_content_block_keys must contain 1..=${MAX_MESSAGE_CONTENT_BLOCKS} keys`,
    );
  }
  const keys = input.ordered_content_block_keys.map((key, index) =>
    boundedText(key, `${label} block ${index}`, MAX_RUNTIME_SEMANTIC_TEXT_BYTES),
  );
  if (keys.join('\0') !== expectedKeys.join('\0')) {
    throw new ContractValidationError(`${label} ordered_content_block_keys do not match the declared snapshot`);
  }
  return {
    completeness: parsedCompleteness,
    operation,
    ordered_content_block_keys: keys,
    semantic_revision_key_hex: parseHexDigest(input.semantic_revision_key_hex, `${label} semantic_revision_key_hex`),
    semantic_revision_ref: parseSemanticRevisionRef(input.semantic_revision_ref),
  };
}

function parseMessageFixtureShape(value: unknown): MessageFixture {
  const input = record(value, 'message fixture');
  assertKnownFields(
    input,
    [
      'adapter_id',
      'actor_run',
      'family',
      'family_version',
      'fixture_contract_version',
      'native_message_id',
      'fact_id',
      'source_record_id',
      'role',
      'content_block',
      'current',
      'correction',
      'complete_blocks',
      'partial_blocks',
      'retract',
      'partial',
      'runtime_semantic_contract_version',
      'session',
      'source_instance_key',
    ],
    'message fixture',
  );
  if (input.family !== MESSAGE_FAMILY || input.family_version !== MESSAGE_FAMILY_VERSION) {
    throw new ContractValidationError('message fixture family must be runtime.message@1');
  }
  const role = input.role;
  if (role !== 'user' && role !== 'assistant' && role !== 'system') {
    throw new ContractValidationError('unsupported message role');
  }
  const current = parseMessageSlot(input.current, 'current', 'upsert', 'complete', ['block-a', 'block-b']);
  const contentBlock = Object.hasOwn(input, 'content_block')
    ? parseContentBlockFixture(input.content_block)
    : undefined;
  const correction = parseMessageSlot(input.correction, 'correction', 'upsert', 'complete', ['block-a', 'block-c']);
  const completeBlocks = parseMessageSlot(input.complete_blocks, 'complete_blocks', 'upsert', 'complete', ['block-a']);
  const partialBlocks = parseMessageSlot(input.partial_blocks, 'partial_blocks', 'upsert', 'partial', ['block-a']);
  const retract = parseMessageSlot(input.retract, 'retract', 'retract', 'complete', ['block-a', 'block-b']);
  const partial = parseMessageSlot(input.partial, 'partial', 'upsert', 'partial', ['block-a', 'block-b']);
  if (
    current.completeness !== 'complete' ||
    correction.completeness !== 'complete' ||
    completeBlocks.completeness !== 'complete'
  ) {
    throw new ContractValidationError('complete message snapshots must declare complete coverage');
  }
  if (partialBlocks.completeness !== 'partial' || partial.completeness !== 'partial') {
    throw new ContractValidationError('partial message snapshots must declare partial coverage');
  }
  const messageSnapshots = [current, correction, completeBlocks, partialBlocks, retract, partial];
  if (
    contentBlock !== undefined &&
    !messageSnapshots.every((slot) => slot.ordered_content_block_keys.includes(contentBlock.native_content_block_id))
  ) {
    throw new ContractValidationError('content-block fixture must belong to every declared parent message snapshot');
  }
  return {
    adapter_id: boundedText(input.adapter_id, 'adapter_id', MAX_ADAPTER_ID_BYTES),
    actor_run: parseOpaqueContractReference(input.actor_run, 'actor run'),
    family: MESSAGE_FAMILY,
    family_version: MESSAGE_FAMILY_VERSION,
    fixture_contract_version: RUNTIME_SEMANTIC_CONTRACT_VERSION,
    native_message_id: boundedText(input.native_message_id, 'native_message_id', MAX_RUNTIME_SEMANTIC_TEXT_BYTES),
    fact_id: parseOpaqueContractReference(input.fact_id, 'message fact id'),
    source_record_id: parseOpaqueContractReference(input.source_record_id, 'message source record id'),
    role,
    ...(contentBlock === undefined ? {} : { content_block: contentBlock }),
    current,
    correction,
    complete_blocks: completeBlocks,
    partial_blocks: partialBlocks,
    retract,
    partial,
    runtime_semantic_contract_version: RUNTIME_SEMANTIC_CONTRACT_VERSION,
    session: parseOpaqueContractReference(input.session, 'session'),
    source_instance_key: parseOpaqueContractReference(input.source_instance_key, 'source instance key'),
  };
}

export function parseMessageFixture(value: unknown, expectedContextInput: unknown): MessageFixture {
  const expected = parseMessageFixtureShape(expectedContextInput);
  const parsed = parseMessageFixtureShape(value);
  if (parsed.fact_id !== expected.fact_id || parsed.session !== expected.session) {
    throw new ContractValidationError('message identity does not match the caller-held revision identity');
  }
  bindSlotIdentity('current', parsed.current, expected.current);
  bindSlotIdentity('correction', parsed.correction, expected.correction);
  bindSlotIdentity('complete_blocks', parsed.complete_blocks, expected.complete_blocks);
  bindSlotIdentity('partial_blocks', parsed.partial_blocks, expected.partial_blocks);
  bindSlotIdentity('retract', parsed.retract, expected.retract);
  bindSlotIdentity('partial', parsed.partial, expected.partial);
  if ((parsed.content_block === undefined) !== (expected.content_block === undefined)) {
    throw new ContractValidationError('content-block identity does not match the caller-held revision identity');
  }
  if (parsed.content_block !== undefined && expected.content_block !== undefined) {
    if (parsed.content_block.fact_id !== expected.content_block.fact_id) {
      throw new ContractValidationError('content-block identity does not match the caller-held revision identity');
    }
    bindSlotIdentity('content-block current', parsed.content_block.current, expected.content_block.current);
    bindSlotIdentity('content-block correction', parsed.content_block.correction, expected.content_block.correction);
    bindSlotIdentity('content-block retract', parsed.content_block.retract, expected.content_block.retract);
    bindSlotIdentity(
      'content-block partial_retract',
      parsed.content_block.partial_retract,
      expected.content_block.partial_retract,
    );
  }
  bindFixtureSemanticContext('message fixture', parsed, expected);
  return parsed;
}

export function parseRfc012cMessageV1Json(json: string, expectedContextInput: unknown): MessageFixture {
  return parseMessageFixture(preflightSemanticFixtureJson(json), expectedContextInput);
}

function parseTaskSlot(
  value: unknown,
  label: string,
  state: TaskLifecycleState,
  operation: UserInputOperation,
  completeness: ContractCompleteness,
  ownedSet: string[] | null,
): TaskRevisionSlot {
  const input = record(value, label);
  assertKnownFields(
    input,
    ['completeness', 'operation', 'owned_set', 'semantic_revision_key_hex', 'semantic_revision_ref', 'state'],
    label,
  );
  if (input.state !== state || input.operation !== operation) {
    throw new ContractValidationError(`${label} lifecycle does not match its fixture slot`);
  }
  let parsedOwned: string[] | null = null;
  if (input.owned_set !== null) {
    if (!Array.isArray(input.owned_set) || input.owned_set.length === 0) {
      throw new ContractValidationError(`${label} owned_set must be null or a non-empty array`);
    }
    parsedOwned = input.owned_set.map((member, index) =>
      boundedText(member, `${label} owned_set ${index}`, MAX_RUNTIME_SEMANTIC_TEXT_BYTES),
    );
  }
  if ((parsedOwned ?? null) === null && ownedSet !== null) {
    throw new ContractValidationError(`${label} owned_set does not match the declared snapshot`);
  }
  if (parsedOwned && ownedSet && parsedOwned.join('\0') !== ownedSet.join('\0')) {
    throw new ContractValidationError(`${label} owned_set does not match the declared snapshot`);
  }
  if (parsedOwned && ownedSet === null) {
    throw new ContractValidationError(`${label} owned_set does not match the declared snapshot`);
  }
  return {
    completeness: parseContractCompleteness(input.completeness, `${label} completeness`),
    operation,
    owned_set: parsedOwned,
    semantic_revision_key_hex: parseHexDigest(input.semantic_revision_key_hex, `${label} semantic_revision_key_hex`),
    semantic_revision_ref: parseSemanticRevisionRef(input.semantic_revision_ref),
    state,
  };
}

function parseTaskFixtureShape(value: unknown): TaskFixture {
  const input = record(value, 'task fixture');
  assertKnownFields(
    input,
    [
      'adapter_id',
      'actor_run',
      'family',
      'family_version',
      'fixture_contract_version',
      'native_task_id',
      'peer_native_task_id',
      'fact_id',
      'peer_fact_id',
      'source_record_id',
      'subject',
      'created',
      'updated',
      'completed',
      'retract',
      'partial',
      'collection_omit',
      'runtime_semantic_contract_version',
      'session',
      'source_instance_key',
    ],
    'task fixture',
  );
  if (input.family !== TASK_FAMILY || input.family_version !== TASK_FAMILY_VERSION) {
    throw new ContractValidationError('task fixture family must be runtime.task@1');
  }
  const created = parseTaskSlot(input.created, 'created', 'created', 'upsert', 'complete', null);
  const updated = parseTaskSlot(input.updated, 'updated', 'updated', 'upsert', 'complete', null);
  const completed = parseTaskSlot(input.completed, 'completed', 'completed', 'upsert', 'complete', null);
  const retract = parseTaskSlot(input.retract, 'retract', 'created', 'retract', 'complete', null);
  const partial = parseTaskSlot(input.partial, 'partial', 'created', 'upsert', 'partial', null);
  const collectionOmit = parseTaskSlot(input.collection_omit, 'collection_omit', 'created', 'upsert', 'complete', [
    'fixture-task-2',
  ]);
  if (
    created.completeness !== 'complete' ||
    collectionOmit.completeness !== 'complete' ||
    partial.completeness !== 'partial'
  ) {
    throw new ContractValidationError('task completeness does not match its fixture slot');
  }
  return {
    adapter_id: boundedText(input.adapter_id, 'adapter_id', MAX_ADAPTER_ID_BYTES),
    actor_run: parseOpaqueContractReference(input.actor_run, 'actor run'),
    family: TASK_FAMILY,
    family_version: TASK_FAMILY_VERSION,
    fixture_contract_version: RUNTIME_SEMANTIC_CONTRACT_VERSION,
    native_task_id: boundedText(input.native_task_id, 'native_task_id', MAX_RUNTIME_SEMANTIC_TEXT_BYTES),
    peer_native_task_id: boundedText(input.peer_native_task_id, 'peer_native_task_id', MAX_RUNTIME_SEMANTIC_TEXT_BYTES),
    fact_id: parseOpaqueContractReference(input.fact_id, 'task fact id'),
    peer_fact_id: parseOpaqueContractReference(input.peer_fact_id, 'peer task fact id'),
    source_record_id: parseOpaqueContractReference(input.source_record_id, 'task source record id'),
    subject: boundedText(input.subject, 'subject', MAX_RUNTIME_SEMANTIC_TEXT_BYTES),
    created,
    updated,
    completed,
    retract,
    partial,
    collection_omit: collectionOmit,
    runtime_semantic_contract_version: RUNTIME_SEMANTIC_CONTRACT_VERSION,
    session: parseOpaqueContractReference(input.session, 'session'),
    source_instance_key: parseOpaqueContractReference(input.source_instance_key, 'source instance key'),
  };
}

export function parseTaskFixture(value: unknown, expectedContextInput: unknown): TaskFixture {
  const expected = parseTaskFixtureShape(expectedContextInput);
  const parsed = parseTaskFixtureShape(value);
  if (parsed.fact_id !== expected.fact_id || parsed.peer_fact_id !== expected.peer_fact_id) {
    throw new ContractValidationError('task identity does not match the caller-held revision identity');
  }
  bindSlotIdentity('created', parsed.created, expected.created);
  bindSlotIdentity('updated', parsed.updated, expected.updated);
  bindSlotIdentity('completed', parsed.completed, expected.completed);
  bindSlotIdentity('retract', parsed.retract, expected.retract);
  bindSlotIdentity('partial', parsed.partial, expected.partial);
  bindSlotIdentity('collection_omit', parsed.collection_omit, expected.collection_omit);
  bindFixtureSemanticContext('task fixture', parsed, expected);
  return parsed;
}

export function parseRfc012cTaskV1Json(json: string, expectedContextInput: unknown): TaskFixture {
  return parseTaskFixture(preflightSemanticFixtureJson(json), expectedContextInput);
}

export interface PlanRevisionSlot {
  completeness: ContractCompleteness;
  operation: UserInputOperation;
  ordered_step_keys: string[];
  owned_set: string[] | null;
  semantic_revision_key_hex: string;
  semantic_revision_ref: SemanticRevisionRef;
}

export interface PlanFixture {
  adapter_id: string;
  actor_run: OpaqueContractReference;
  family: typeof PLAN_FAMILY;
  family_version: typeof PLAN_FAMILY_VERSION;
  fixture_contract_version: typeof RUNTIME_SEMANTIC_CONTRACT_VERSION;
  native_plan_id: string;
  peer_native_plan_id: string;
  fact_id: OpaqueContractReference;
  peer_fact_id: OpaqueContractReference;
  source_record_id: OpaqueContractReference;
  subject: string;
  current: PlanRevisionSlot;
  complete_steps: PlanRevisionSlot;
  partial_steps: PlanRevisionSlot;
  retract: PlanRevisionSlot;
  partial: PlanRevisionSlot;
  collection_omit: PlanRevisionSlot;
  runtime_semantic_contract_version: typeof RUNTIME_SEMANTIC_CONTRACT_VERSION;
  session: OpaqueContractReference;
  source_instance_key: OpaqueContractReference;
}

const PLAN_SLOT_FIELDS = [
  'completeness',
  'operation',
  'ordered_step_keys',
  'owned_set',
  'semantic_revision_key_hex',
  'semantic_revision_ref',
] as const;

function parsePlanSlot(
  value: unknown,
  label: string,
  operation: UserInputOperation,
  completeness: ContractCompleteness,
  expectedKeys: readonly string[],
  ownedSet: string[] | null,
): PlanRevisionSlot {
  const input = record(value, label);
  assertKnownFields(input, PLAN_SLOT_FIELDS, label);
  if (input.operation !== operation) {
    throw new ContractValidationError(`${label} operation does not match its fixture slot`);
  }
  if (input.completeness !== completeness) {
    throw new ContractValidationError(`${label} completeness does not match its fixture slot`);
  }
  if (!Array.isArray(input.ordered_step_keys) || input.ordered_step_keys.length === 0) {
    throw new ContractValidationError(`${label} ordered_step_keys must be a non-empty array`);
  }
  const keys = input.ordered_step_keys.map((key, index) =>
    boundedText(key, `${label} step ${index}`, MAX_RUNTIME_SEMANTIC_TEXT_BYTES),
  );
  if (keys.join('\0') !== expectedKeys.join('\0')) {
    throw new ContractValidationError(`${label} ordered_step_keys do not match the declared snapshot`);
  }
  let parsedOwned: string[] | null = null;
  if (input.owned_set !== null) {
    if (!Array.isArray(input.owned_set) || input.owned_set.length === 0) {
      throw new ContractValidationError(`${label} owned_set must be null or a non-empty array`);
    }
    parsedOwned = input.owned_set.map((member, index) =>
      boundedText(member, `${label} owned_set ${index}`, MAX_RUNTIME_SEMANTIC_TEXT_BYTES),
    );
  }
  if ((parsedOwned ?? null) === null && ownedSet !== null) {
    throw new ContractValidationError(`${label} owned_set does not match the declared snapshot`);
  }
  if (parsedOwned && ownedSet && parsedOwned.join('\0') !== ownedSet.join('\0')) {
    throw new ContractValidationError(`${label} owned_set does not match the declared snapshot`);
  }
  if (parsedOwned && ownedSet === null) {
    throw new ContractValidationError(`${label} owned_set does not match the declared snapshot`);
  }
  return {
    completeness: parseContractCompleteness(input.completeness, `${label} completeness`),
    operation,
    ordered_step_keys: keys,
    owned_set: parsedOwned,
    semantic_revision_key_hex: parseHexDigest(input.semantic_revision_key_hex, `${label} semantic_revision_key_hex`),
    semantic_revision_ref: parseSemanticRevisionRef(input.semantic_revision_ref),
  };
}

function parsePlanFixtureShape(value: unknown): PlanFixture {
  const input = record(value, 'plan fixture');
  assertKnownFields(
    input,
    [
      'adapter_id',
      'actor_run',
      'family',
      'family_version',
      'fixture_contract_version',
      'native_plan_id',
      'peer_native_plan_id',
      'fact_id',
      'peer_fact_id',
      'source_record_id',
      'subject',
      'current',
      'complete_steps',
      'partial_steps',
      'retract',
      'partial',
      'collection_omit',
      'runtime_semantic_contract_version',
      'session',
      'source_instance_key',
    ],
    'plan fixture',
  );
  if (input.family !== PLAN_FAMILY || input.family_version !== PLAN_FAMILY_VERSION) {
    throw new ContractValidationError('plan fixture family must be runtime.plan@1');
  }
  const current = parsePlanSlot(input.current, 'current', 'upsert', 'complete', ['step-a', 'step-b'], null);
  const completeSteps = parsePlanSlot(input.complete_steps, 'complete_steps', 'upsert', 'complete', ['step-a'], null);
  const partialSteps = parsePlanSlot(input.partial_steps, 'partial_steps', 'upsert', 'partial', ['step-a'], null);
  const retract = parsePlanSlot(input.retract, 'retract', 'retract', 'complete', ['step-a', 'step-b'], null);
  const partial = parsePlanSlot(input.partial, 'partial', 'upsert', 'partial', ['step-a', 'step-b'], null);
  const collectionOmit = parsePlanSlot(
    input.collection_omit,
    'collection_omit',
    'upsert',
    'complete',
    ['step-a'],
    ['fixture-plan-2'],
  );
  if (
    current.completeness !== 'complete' ||
    completeSteps.completeness !== 'complete' ||
    partialSteps.completeness !== 'partial' ||
    partial.completeness !== 'partial'
  ) {
    throw new ContractValidationError('plan completeness does not match its fixture slot');
  }
  return {
    adapter_id: boundedText(input.adapter_id, 'adapter_id', MAX_ADAPTER_ID_BYTES),
    actor_run: parseOpaqueContractReference(input.actor_run, 'actor run'),
    family: PLAN_FAMILY,
    family_version: PLAN_FAMILY_VERSION,
    fixture_contract_version: RUNTIME_SEMANTIC_CONTRACT_VERSION,
    native_plan_id: boundedText(input.native_plan_id, 'native_plan_id', MAX_RUNTIME_SEMANTIC_TEXT_BYTES),
    peer_native_plan_id: boundedText(input.peer_native_plan_id, 'peer_native_plan_id', MAX_RUNTIME_SEMANTIC_TEXT_BYTES),
    fact_id: parseOpaqueContractReference(input.fact_id, 'plan fact id'),
    peer_fact_id: parseOpaqueContractReference(input.peer_fact_id, 'peer plan fact id'),
    source_record_id: parseOpaqueContractReference(input.source_record_id, 'plan source record id'),
    subject: boundedText(input.subject, 'subject', MAX_RUNTIME_SEMANTIC_TEXT_BYTES),
    current,
    complete_steps: completeSteps,
    partial_steps: partialSteps,
    retract,
    partial,
    collection_omit: collectionOmit,
    runtime_semantic_contract_version: RUNTIME_SEMANTIC_CONTRACT_VERSION,
    session: parseOpaqueContractReference(input.session, 'session'),
    source_instance_key: parseOpaqueContractReference(input.source_instance_key, 'source instance key'),
  };
}

export function parsePlanFixture(value: unknown, expectedContextInput: unknown): PlanFixture {
  const expected = parsePlanFixtureShape(expectedContextInput);
  const parsed = parsePlanFixtureShape(value);
  if (parsed.fact_id !== expected.fact_id || parsed.peer_fact_id !== expected.peer_fact_id) {
    throw new ContractValidationError('plan identity does not match the caller-held revision identity');
  }
  bindSlotIdentity('current', parsed.current, expected.current);
  bindSlotIdentity('complete_steps', parsed.complete_steps, expected.complete_steps);
  bindSlotIdentity('partial_steps', parsed.partial_steps, expected.partial_steps);
  bindSlotIdentity('retract', parsed.retract, expected.retract);
  bindSlotIdentity('partial', parsed.partial, expected.partial);
  bindSlotIdentity('collection_omit', parsed.collection_omit, expected.collection_omit);
  bindFixtureSemanticContext('plan fixture', parsed, expected);
  return parsed;
}

export function parseRfc012cPlanV1Json(json: string, expectedContextInput: unknown): PlanFixture {
  return parsePlanFixture(preflightSemanticFixtureJson(json), expectedContextInput);
}

export type ToolRevisionKind = 'call' | 'result';

export interface ToolRevisionSlot {
  completeness: ContractCompleteness;
  correlated_native_id: string | null;
  kind: ToolRevisionKind;
  operation: UserInputOperation;
  semantic_revision_key_hex: string;
  semantic_revision_ref: SemanticRevisionRef;
}

export interface ToolFixture {
  adapter_id: string;
  actor_run: OpaqueContractReference;
  family: typeof TOOL_FAMILY;
  family_version: typeof TOOL_FAMILY_VERSION;
  fixture_contract_version: typeof RUNTIME_SEMANTIC_CONTRACT_VERSION;
  native_call_id: string;
  native_result_id: string;
  fact_id: OpaqueContractReference;
  result_fact_id: OpaqueContractReference;
  source_record_id: OpaqueContractReference;
  tool_name: string;
  call: ToolRevisionSlot;
  unmatched_result: ToolRevisionSlot;
  correlated_call: ToolRevisionSlot;
  correlated_result: ToolRevisionSlot;
  retract: ToolRevisionSlot;
  partial: ToolRevisionSlot;
  runtime_semantic_contract_version: typeof RUNTIME_SEMANTIC_CONTRACT_VERSION;
  session: OpaqueContractReference;
  source_instance_key: OpaqueContractReference;
}

const TOOL_SLOT_FIELDS = [
  'completeness',
  'correlated_native_id',
  'kind',
  'operation',
  'semantic_revision_key_hex',
  'semantic_revision_ref',
] as const;

function parseToolSlot(
  value: unknown,
  label: string,
  kind: ToolRevisionKind,
  operation: UserInputOperation,
  completeness: ContractCompleteness,
  correlatedNativeId: string | null,
): ToolRevisionSlot {
  const input = record(value, label);
  assertKnownFields(input, TOOL_SLOT_FIELDS, label);
  if (input.kind !== kind) {
    throw new ContractValidationError(`${label} kind does not match its fixture slot`);
  }
  if (input.operation !== operation) {
    throw new ContractValidationError(`${label} operation does not match its fixture slot`);
  }
  if (input.completeness !== completeness) {
    throw new ContractValidationError(`${label} completeness does not match its fixture slot`);
  }
  let parsedCorrelated: string | null = null;
  if (input.correlated_native_id !== null) {
    parsedCorrelated = boundedText(
      input.correlated_native_id,
      `${label} correlated_native_id`,
      MAX_RUNTIME_SEMANTIC_TEXT_BYTES,
    );
  }
  if (parsedCorrelated !== correlatedNativeId) {
    throw new ContractValidationError(`${label} correlation does not match the declared snapshot`);
  }
  return {
    completeness: parseContractCompleteness(input.completeness, `${label} completeness`),
    correlated_native_id: parsedCorrelated,
    kind,
    operation,
    semantic_revision_key_hex: parseHexDigest(input.semantic_revision_key_hex, `${label} semantic_revision_key_hex`),
    semantic_revision_ref: parseSemanticRevisionRef(input.semantic_revision_ref),
  };
}

function parseToolFixtureShape(value: unknown): ToolFixture {
  const input = record(value, 'tool fixture');
  assertKnownFields(
    input,
    [
      'adapter_id',
      'actor_run',
      'family',
      'family_version',
      'fixture_contract_version',
      'native_call_id',
      'native_result_id',
      'fact_id',
      'result_fact_id',
      'source_record_id',
      'tool_name',
      'call',
      'unmatched_result',
      'correlated_call',
      'correlated_result',
      'retract',
      'partial',
      'runtime_semantic_contract_version',
      'session',
      'source_instance_key',
    ],
    'tool fixture',
  );
  if (input.family !== TOOL_FAMILY || input.family_version !== TOOL_FAMILY_VERSION) {
    throw new ContractValidationError('tool fixture family must be runtime.tool@1');
  }
  const nativeCallId = boundedText(input.native_call_id, 'native_call_id', MAX_RUNTIME_SEMANTIC_TEXT_BYTES);
  const nativeResultId = boundedText(input.native_result_id, 'native_result_id', MAX_RUNTIME_SEMANTIC_TEXT_BYTES);
  if (nativeCallId === nativeResultId) {
    throw new ContractValidationError('tool fixture call and result must be distinct identities');
  }
  const toolName = boundedText(input.tool_name, 'tool_name', MAX_RUNTIME_SEMANTIC_TEXT_BYTES);
  if (toolName !== 'read') {
    throw new ContractValidationError('tool fixture tool_name must be the declared bounded name');
  }
  const call = parseToolSlot(input.call, 'call', 'call', 'upsert', 'complete', null);
  const unmatchedResult = parseToolSlot(
    input.unmatched_result,
    'unmatched_result',
    'result',
    'upsert',
    'complete',
    null,
  );
  const correlatedCall = parseToolSlot(
    input.correlated_call,
    'correlated_call',
    'call',
    'upsert',
    'complete',
    nativeResultId,
  );
  const correlatedResult = parseToolSlot(
    input.correlated_result,
    'correlated_result',
    'result',
    'upsert',
    'complete',
    nativeCallId,
  );
  const retract = parseToolSlot(input.retract, 'retract', 'call', 'retract', 'complete', null);
  const partial = parseToolSlot(input.partial, 'partial', 'call', 'upsert', 'partial', null);
  return {
    adapter_id: boundedText(input.adapter_id, 'adapter_id', MAX_ADAPTER_ID_BYTES),
    actor_run: parseOpaqueContractReference(input.actor_run, 'actor run'),
    family: TOOL_FAMILY,
    family_version: TOOL_FAMILY_VERSION,
    fixture_contract_version: RUNTIME_SEMANTIC_CONTRACT_VERSION,
    native_call_id: nativeCallId,
    native_result_id: nativeResultId,
    fact_id: parseOpaqueContractReference(input.fact_id, 'tool call fact id'),
    result_fact_id: parseOpaqueContractReference(input.result_fact_id, 'tool result fact id'),
    source_record_id: parseOpaqueContractReference(input.source_record_id, 'tool source record id'),
    tool_name: toolName,
    call,
    unmatched_result: unmatchedResult,
    correlated_call: correlatedCall,
    correlated_result: correlatedResult,
    retract,
    partial,
    runtime_semantic_contract_version: RUNTIME_SEMANTIC_CONTRACT_VERSION,
    session: parseOpaqueContractReference(input.session, 'session'),
    source_instance_key: parseOpaqueContractReference(input.source_instance_key, 'source instance key'),
  };
}

export function parseToolFixture(value: unknown, expectedContextInput: unknown): ToolFixture {
  const expected = parseToolFixtureShape(expectedContextInput);
  const parsed = parseToolFixtureShape(value);
  if (parsed.fact_id !== expected.fact_id || parsed.result_fact_id !== expected.result_fact_id) {
    throw new ContractValidationError('tool identity does not match the caller-held revision identity');
  }
  bindSlotIdentity('call', parsed.call, expected.call);
  bindSlotIdentity('unmatched_result', parsed.unmatched_result, expected.unmatched_result);
  bindSlotIdentity('correlated_call', parsed.correlated_call, expected.correlated_call);
  bindSlotIdentity('correlated_result', parsed.correlated_result, expected.correlated_result);
  bindSlotIdentity('retract', parsed.retract, expected.retract);
  bindSlotIdentity('partial', parsed.partial, expected.partial);
  bindFixtureSemanticContext('tool fixture', parsed, expected);
  return parsed;
}

export function parseRfc012cToolV1Json(json: string, expectedContextInput: unknown): ToolFixture {
  return parseToolFixture(preflightSemanticFixtureJson(json), expectedContextInput);
}
