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

function parseUsageProvenance(value: unknown): UsageValueProvenance {
  const input = record(value, 'usage provenance');
  assertKnownFields(input, ['native_field', 'normalization_contract_version'], 'usage provenance');
  return {
    native_field: boundedText(input.native_field, 'usage provenance native_field', MAX_USAGE_PROVENANCE_FIELD_BYTES),
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
