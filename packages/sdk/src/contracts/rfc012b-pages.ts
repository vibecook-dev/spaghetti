/** RFC 012B portable catalog pages, readiness, resolution, and expiration.
 *
 * These parsers validate Rust-produced contract values only. They do not run
 * queries, retain snapshots, read native sources, or authorize hydration.
 */

import {
  ContractValidationError,
  parseExternalEntityRef,
  parseOpaqueContractReference,
  parseQualifiedValue,
  parseSemanticRevisionRef,
  parseSourceCoverageSet,
  type ContractCompleteness,
  type ExternalEntityRef,
  type NativeIdentity,
  type OpaqueContractReference,
  type QualifiedUnknownReason,
  type QualifiedValue,
  type QualifiedValueQuality,
  type SemanticRevisionRef,
  type SourceCoverageSet,
} from './rfc012a.js';
import {
  parseCatalogContinuationRequest,
  parseCatalogQueryContractSelection,
  parseCatalogQueryContractSelectionForRequest,
  type CatalogContinuationRequest,
  type CatalogCursor,
  type CatalogQueryContractSelection,
  type CatalogSnapshotId,
  type JsonObject,
  type JsonValue,
} from './rfc012b.js';

export const CATALOG_PAGE_CONTRACT_VERSION = 1 as const;
export const CATALOG_READINESS_RESPONSE_CONTRACT_VERSION = 1 as const;
export const CATALOG_RESOLUTION_CONTRACT_VERSION = 1 as const;
export const CATALOG_SNAPSHOT_EXPIRATION_CONTRACT_VERSION = 1 as const;
export const CATALOG_COVERAGE_PLAN_CONTRACT_VERSION = 1 as const;
export const CATALOG_PORTABLE_COVERAGE_PLAN_CONTRACT_VERSION = 1 as const;
export const CATALOG_READINESS_CONTRACT_VERSION = 1 as const;

const MAX_U32 = 0xffff_ffff;
const MAX_PAGE_ROWS = 1_000;
const MAX_ROW_EVIDENCE_KEYS = 4_096;
const MAX_ASSOCIATION_EVIDENCE = 4_096;
const MAX_RESOLUTION_TARGETS = 4_096;
const MAX_PROVENANCE_REVISIONS = 64;
const MAX_PLAN_SOURCES = 4_096;
const MAX_SOURCE_COVERAGE_MEMBERS = 16_384;
const MAX_IDENTIFIER_BYTES = 256;
const MAX_READINESS_REASON_CODE_BYTES = 64;
const MAX_PORTABLE_COVERAGE_REASON_BYTES = 1_024;
const MAX_TEXT_BYTES = 16 * 1_024;
const MAX_TYPED_UNKNOWN_DEPTH = 16;
const MAX_TYPED_UNKNOWN_NODES = 1_024;
const textEncoder = new TextEncoder();

type UnknownRecord = Record<string, unknown>;
type CatalogEntityKind = 'project' | 'session';
export type CatalogQueryKind = 'projects' | 'sessions';

export interface CatalogEntityRef {
  kind: CatalogEntityKind;
  external_ref: ExternalEntityRef;
}

export interface CatalogFieldAuthority {
  class_id: string;
  precedence: number;
  native_times_comparable: boolean;
}

export interface CatalogPortableFieldSelection<T> {
  selected_assertion_key: OpaqueContractReference;
  field: QualifiedValue<T, CatalogFieldAuthority, SemanticRevisionRef[]>;
  conflicting_assertion_keys: OpaqueContractReference[];
}

export type CatalogOptionalField<T> =
  | { state: 'selected'; selection: CatalogPortableFieldSelection<T> }
  | { state: 'unknown'; reason: QualifiedUnknownReason };

export type CatalogAvailability =
  | { state: 'metadata_only' }
  | { state: 'transcript_discovered' }
  | { state: 'hydrating' }
  | { state: 'history_ready' }
  | { state: 'unavailable'; reason: string };

export interface CatalogEvidenceOwner {
  adapter_id: string;
  source_instance_key: OpaqueContractReference;
  stream_key: OpaqueContractReference;
  object_key: OpaqueContractReference;
  generation: number;
}

export type ProjectAssociationBasis =
  | 'native_project_index'
  | 'transcript_cwd'
  | 'session_directory'
  | 'rollout_header'
  | 'declared_derived_ancestor';

export interface CatalogAssociationFact {
  association_key: OpaqueContractReference;
  owner: CatalogEvidenceOwner;
  session_ref: CatalogEntityRef;
  project_ref: CatalogEntityRef;
  basis: ProjectAssociationBasis;
  declared_derivation_id: string | null;
  locator_claim_key: OpaqueContractReference | null;
  authority: CatalogFieldAuthority;
  quality: Exclude<QualifiedValueQuality, 'unknown'>;
  completeness: ContractCompleteness;
  effective_at: number | null;
  provenance: SemanticRevisionRef[];
}

export type CatalogAssociationCoverage =
  | { state: 'unknown' }
  | {
      state: 'available';
      selection: {
        association: CatalogAssociationFact;
        competing_associations: CatalogAssociationFact[];
        conflicting_association_keys: OpaqueContractReference[];
      };
    };

export interface CatalogPortableProjectRow {
  project_ref: CatalogEntityRef;
  native_identity: CatalogOptionalField<NativeIdentity>;
  root_identity: CatalogOptionalField<string>;
  display_path: CatalogOptionalField<string>;
  display_name: CatalogOptionalField<string>;
  native_time: CatalogOptionalField<number>;
  availability: CatalogPortableFieldSelection<CatalogAvailability>;
  assertion_keys: OpaqueContractReference[];
  additive_fields: JsonObject;
}

export interface CatalogPortableSessionRow {
  session_ref: CatalogEntityRef;
  project_association: CatalogAssociationCoverage;
  native_identity: CatalogOptionalField<NativeIdentity>;
  title: CatalogOptionalField<string>;
  first_user_summary: CatalogOptionalField<string>;
  native_created_at: CatalogOptionalField<number>;
  native_updated_at: CatalogOptionalField<number>;
  native_message_count: CatalogOptionalField<number>;
  transcript_locator_claim_keys: OpaqueContractReference[];
  availability: CatalogPortableFieldSelection<CatalogAvailability>;
  assertion_keys: OpaqueContractReference[];
  additive_fields: JsonObject;
}

export type CatalogCount = { state: 'known'; value: number } | { state: 'unknown'; reason: QualifiedUnknownReason };

export type CatalogCoverageScope = { kind: 'library' } | { kind: 'entity'; external_ref: ExternalEntityRef };

export interface CatalogPortableCoveragePlanSource {
  adapter_id: string;
  source_instance_key: OpaqueContractReference;
  support_release_id: string;
  catalog_declaration_digest: OpaqueContractReference;
  access_policy_digest: OpaqueContractReference;
  catalog_coverage_binding_digest: OpaqueContractReference;
}

export interface CatalogPortableCoveragePlan {
  catalog_portable_coverage_plan_contract_version: typeof CATALOG_PORTABLE_COVERAGE_PLAN_CONTRACT_VERSION;
  coverage_plan_contract_version: typeof CATALOG_COVERAGE_PLAN_CONTRACT_VERSION;
  coverage_plan_id: OpaqueContractReference;
  scope: CatalogCoverageScope;
  required_sources: CatalogPortableCoveragePlanSource[];
  optional_sources: CatalogPortableCoveragePlanSource[];
}

export type CatalogReadinessPhase = 'pending' | 'building' | 'partial' | 'ready' | 'degraded' | 'error';

export type CatalogReadinessReason =
  | { kind: 'source_retrying'; code: string }
  | { kind: 'terminal_source_unavailable'; code: string }
  | {
      kind: 'integrity_failure';
      code: string;
      snapshot_disposition: 'independently_safe' | 'discarded';
    };

export interface ProgressiveStartupView {
  catalogQueryReady: boolean;
  searchAvailable: boolean;
  selectedHydrationAvailable: boolean;
}

/** Catalog-first UX: last-complete/degraded catalog may query before FTS. */
export function progressiveStartupViewFromFlags(
  catalogQueryReady: boolean,
  searchPackComplete: boolean,
): ProgressiveStartupView {
  if (searchPackComplete && !catalogQueryReady) {
    throw new ContractValidationError('complete search cannot precede a queryable catalog');
  }
  return {
    catalogQueryReady,
    searchAvailable: catalogQueryReady && searchPackComplete,
    // The portable readiness projection cannot infer executable source-access
    // authority from a catalog snapshot.
    selectedHydrationAvailable: false,
  };
}

/** Catalog-first UX: last-complete/degraded catalog may query before FTS. */
export function progressiveStartupView(
  readiness: CatalogReadinessSnapshot,
  searchPackComplete: boolean,
): ProgressiveStartupView {
  const catalogQueryReady = readiness.state === 'ready' || readiness.state === 'degraded';
  return progressiveStartupViewFromFlags(catalogQueryReady, searchPackComplete);
}

export interface CatalogReadinessSnapshot {
  readiness_contract_version: typeof CATALOG_READINESS_CONTRACT_VERSION;
  scope: CatalogCoverageScope;
  coverage_plan_id: OpaqueContractReference;
  desired_contract_version: number;
  completed_contract_version?: number;
  epoch: number;
  attempt: number;
  state: CatalogReadinessPhase;
  complete_through_commit?: number;
  last_complete_snapshot?: CatalogSnapshotId;
  refreshing_from_snapshot?: CatalogSnapshotId;
  source_coverage: SourceCoverageSet[];
  reason?: CatalogReadinessReason;
}

export interface CatalogPageRequestBinding {
  contract_selection: CatalogQueryContractSelection;
  snapshot_id: CatalogSnapshotId;
  query_kind: CatalogQueryKind;
  query_fingerprint: OpaqueContractReference;
  sort_spec_version: number;
  page_size: number;
  after_cursor?: CatalogCursor;
}

export interface CatalogPageEntry<R> {
  sort_key: string;
  row: R;
}

export interface CatalogPage<R> {
  catalog_page_contract_version: typeof CATALOG_PAGE_CONTRACT_VERSION;
  request: CatalogPageRequestBinding;
  published_readiness: CatalogReadinessSnapshot;
  total_count: CatalogCount;
  has_more: boolean;
  rows: CatalogPageEntry<R>[];
  next_continuation?: CatalogContinuationRequest;
  additive_fields: JsonObject;
}

export type CatalogProjectPage = CatalogPage<CatalogPortableProjectRow>;
export type CatalogSessionPage = CatalogPage<CatalogPortableSessionRow>;

export interface CatalogReadinessResponse {
  catalog_readiness_response_contract_version: typeof CATALOG_READINESS_RESPONSE_CONTRACT_VERSION;
  contract_selection: CatalogQueryContractSelection;
  readiness: CatalogReadinessSnapshot;
  additive_fields: JsonObject;
}

export interface CatalogReadinessQueryResult {
  coverage_plan: CatalogPortableCoveragePlan;
  readiness: CatalogReadinessResponse;
}

export interface CatalogResolutionRequestBinding {
  contract_selection: CatalogQueryContractSelection;
  snapshot_id: CatalogSnapshotId;
  external_ref: ExternalEntityRef;
}

export type CatalogPortableLiveRow =
  | { kind: 'project'; row: CatalogPortableProjectRow }
  | { kind: 'session'; row: CatalogPortableSessionRow };

export type CatalogEntityResolution =
  | { state: 'live'; external_ref: ExternalEntityRef; row: CatalogPortableLiveRow }
  | { state: 'tombstoned'; external_ref: ExternalEntityRef; provenance: SemanticRevisionRef[] }
  | {
      state: 'superseded';
      external_ref: ExternalEntityRef;
      target_refs: ExternalEntityRef[];
      provenance: SemanticRevisionRef[];
    }
  | {
      state: 'unknown';
      external_ref: ExternalEntityRef;
      reason: 'never_observed' | 'retracted_pending_publication' | 'related_identity_only';
    }
  | {
      state: 'typed_unknown';
      external_ref: ExternalEntityRef;
      variant: string;
      payload: JsonObject;
    };

export interface CatalogEntityResolutionResponse {
  catalog_resolution_contract_version: typeof CATALOG_RESOLUTION_CONTRACT_VERSION;
  request: CatalogResolutionRequestBinding;
  resolution: CatalogEntityResolution;
  additive_fields: JsonObject;
}

export interface CatalogSnapshotExpired {
  catalog_snapshot_expiration_contract_version: typeof CATALOG_SNAPSHOT_EXPIRATION_CONTRACT_VERSION;
  contract_selection: CatalogQueryContractSelection;
  scope: CatalogCoverageScope;
  request: CatalogContinuationRequest;
  latest_snapshot: CatalogSnapshotId;
  additive_fields: JsonObject;
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

function utf8Bytes(value: string): number {
  return textEncoder.encode(value).byteLength;
}

function canonicalString(value: unknown, label: string, maxBytes = MAX_IDENTIFIER_BYTES): string {
  if (typeof value !== 'string' || value.length === 0 || value.trim() !== value || utf8Bytes(value) > maxBytes) {
    throw new ContractValidationError(`${label} must be non-empty bounded canonical text`);
  }
  return value;
}

function safeInteger(value: unknown, label: string): number {
  if (!Number.isSafeInteger(value)) throw new ContractValidationError(`${label} must be a safe integer`);
  return value as number;
}

function nonNegativeInteger(value: unknown, label: string): number {
  const parsed = safeInteger(value, label);
  if (parsed < 0) throw new ContractValidationError(`${label} must be non-negative`);
  return parsed;
}

function positiveInteger(value: unknown, label: string): number {
  const parsed = nonNegativeInteger(value, label);
  if (parsed === 0) throw new ContractValidationError(`${label} must be positive`);
  return parsed;
}

function u32(value: unknown, label: string): number {
  const parsed = positiveInteger(value, label);
  if (parsed > MAX_U32) throw new ContractValidationError(`${label} exceeds u32`);
  return parsed;
}

function booleanValue(value: unknown, label: string): boolean {
  if (typeof value !== 'boolean') throw new ContractValidationError(`${label} must be boolean`);
  return value;
}

function optional<T>(value: unknown, parse: (entry: unknown) => T): T | undefined {
  return value === undefined || value === null ? undefined : parse(value);
}

function decodeBase64Url(value: string, label: string): Uint8Array {
  const payload = value.startsWith('v1:') ? value.slice(3) : value;
  if (!/^[A-Za-z0-9_-]+$/.test(payload)) {
    throw new ContractValidationError(`${label} must be canonical unpadded base64url`);
  }
  const standard = payload.replace(/-/g, '+').replace(/_/g, '/');
  const padded = standard + '='.repeat((4 - (standard.length % 4)) % 4);
  let decoded: string;
  try {
    decoded = atob(padded);
  } catch {
    throw new ContractValidationError(`${label} must be canonical unpadded base64url`);
  }
  const bytes = Uint8Array.from(decoded, (character) => character.charCodeAt(0));
  const canonical = btoa(decoded).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  if (canonical !== payload) throw new ContractValidationError(`${label} is not canonical base64url`);
  return bytes;
}

function compareBytes(left: Uint8Array, right: Uint8Array): number {
  const count = Math.min(left.length, right.length);
  for (let index = 0; index < count; index += 1) {
    if (left[index]! < right[index]!) return -1;
    if (left[index]! > right[index]!) return 1;
  }
  return left.length - right.length;
}

function compareText(left: string, right: string): number {
  return compareBytes(textEncoder.encode(left), textEncoder.encode(right));
}

function compareOpaque(left: string, right: string): number {
  return compareBytes(decodeBase64Url(left, 'opaque reference'), decodeBase64Url(right, 'opaque reference'));
}

function strictlyIncreasing<T>(values: T[], compare: (left: T, right: T) => number, label: string, max: number): void {
  if (values.length > max) throw new ContractValidationError(`${label} exceeds ${max} entries`);
  for (let index = 1; index < values.length; index += 1) {
    if (compare(values[index - 1]!, values[index]!) >= 0) {
      throw new ContractValidationError(`${label} must be canonical and duplicate-free`);
    }
  }
}

function parseOpaqueList(
  value: unknown,
  label: string,
  max: number,
  requireNonempty = false,
): OpaqueContractReference[] {
  if (!Array.isArray(value) || (requireNonempty && value.length === 0)) {
    throw new ContractValidationError(`${label} must be ${requireNonempty ? 'a non-empty' : 'an'} array`);
  }
  if (value.length > max) throw new ContractValidationError(`${label} exceeds ${max} entries`);
  const parsed = value.map((entry) => parseOpaqueContractReference(entry, label));
  strictlyIncreasing(parsed, compareOpaque, label, max);
  return parsed;
}

function parseProvenance(value: unknown, label: string): SemanticRevisionRef[] {
  if (!Array.isArray(value) || value.length === 0 || value.length > MAX_PROVENANCE_REVISIONS) {
    throw new ContractValidationError(`${label} requires 1..=${MAX_PROVENANCE_REVISIONS} revisions`);
  }
  const parsed = value.map(parseSemanticRevisionRef);
  strictlyIncreasing(
    parsed,
    (left, right) => compareOpaque(left.fact_revision_id, right.fact_revision_id),
    label,
    MAX_PROVENANCE_REVISIONS,
  );
  return parsed;
}

function parseEntityRef(value: unknown, expectedKind?: CatalogEntityKind): CatalogEntityRef {
  const input = record(value, 'catalog entity reference');
  assertKnownFields(input, ['kind', 'external_ref'], 'catalog entity reference');
  if (input.kind !== 'project' && input.kind !== 'session') {
    throw new ContractValidationError('catalog entity reference has unsupported kind');
  }
  if (expectedKind !== undefined && input.kind !== expectedKind) {
    throw new ContractValidationError(`catalog entity reference must be ${expectedKind}`);
  }
  return { kind: input.kind, external_ref: parseExternalEntityRef(input.external_ref) };
}

function externalRefsEqual(left: ExternalEntityRef, right: ExternalEntityRef): boolean {
  return (
    left.external_entity_reference_version === right.external_entity_reference_version &&
    left.entity_key === right.entity_key
  );
}

function entityRefsEqual(left: CatalogEntityRef, right: CatalogEntityRef): boolean {
  return left.kind === right.kind && externalRefsEqual(left.external_ref, right.external_ref);
}

function parseSnapshot(value: unknown): CatalogSnapshotId {
  const input = record(value, 'catalog snapshot id');
  assertKnownFields(
    input,
    ['pack_contract_version', 'coverage_plan_id', 'readiness_epoch', 'complete_commit'],
    'catalog snapshot id',
  );
  return {
    pack_contract_version: u32(input.pack_contract_version, 'catalog snapshot pack version'),
    coverage_plan_id: parseOpaqueContractReference(input.coverage_plan_id, 'catalog coverage-plan id'),
    readiness_epoch: positiveInteger(input.readiness_epoch, 'catalog readiness epoch'),
    complete_commit: positiveInteger(input.complete_commit, 'catalog complete commit'),
  };
}

function snapshotsEqual(left: CatalogSnapshotId, right: CatalogSnapshotId): boolean {
  return (
    left.pack_contract_version === right.pack_contract_version &&
    left.coverage_plan_id === right.coverage_plan_id &&
    left.readiness_epoch === right.readiness_epoch &&
    left.complete_commit === right.complete_commit
  );
}

function selectionsEqual(left: CatalogQueryContractSelection, right: CatalogQueryContractSelection): boolean {
  const leftFamilies = Object.keys(left.contract_versions.fact_family_versions).sort();
  const rightFamilies = Object.keys(right.contract_versions.fact_family_versions).sort();
  return (
    left.catalog_query_contract_version === right.catalog_query_contract_version &&
    left.contract_versions.selection_contract_version === right.contract_versions.selection_contract_version &&
    left.contract_versions.model_major === right.contract_versions.model_major &&
    left.contract_versions.external_entity_reference_version ===
      right.contract_versions.external_entity_reference_version &&
    left.contract_versions.semantic_revision_reference_version ===
      right.contract_versions.semantic_revision_reference_version &&
    left.contract_versions.coverage_contract_version === right.contract_versions.coverage_contract_version &&
    left.contract_versions.query_pack_version === right.contract_versions.query_pack_version &&
    left.contract_versions.observation_contract_version === right.contract_versions.observation_contract_version &&
    leftFamilies.length === rightFamilies.length &&
    leftFamilies.every(
      (family, index) =>
        family === rightFamilies[index] &&
        left.contract_versions.fact_family_versions[family] === right.contract_versions.fact_family_versions[family],
    ) &&
    left.typed_unknown.typed_unknown_contract_version === right.typed_unknown.typed_unknown_contract_version &&
    left.typed_unknown.preserves_unknown_fields === right.typed_unknown.preserves_unknown_fields &&
    left.typed_unknown.preserves_unknown_variants === right.typed_unknown.preserves_unknown_variants &&
    left.typed_unknown.max_payload_bytes === right.typed_unknown.max_payload_bytes
  );
}

function parseAdditiveFields(
  input: UnknownRecord,
  knownFields: readonly string[],
  selection: CatalogQueryContractSelection,
): JsonObject {
  const known = new Set(knownFields);
  const budget = {
    bytes: 1,
    nodes: 1,
    maxBytes: selection.typed_unknown.max_payload_bytes,
  };
  const addBytes = (bytes: number): void => {
    budget.bytes += bytes;
    if (!Number.isSafeInteger(budget.bytes) || budget.bytes > budget.maxBytes) {
      throw new ContractValidationError(
        `catalog typed-unknown payload exceeds the negotiated ${budget.maxBytes} byte bound`,
      );
    }
  };
  const addNode = (): void => {
    budget.nodes += 1;
    if (budget.nodes > MAX_TYPED_UNKNOWN_NODES) {
      throw new ContractValidationError(`catalog typed-unknown payload exceeds ${MAX_TYPED_UNKNOWN_NODES} nodes`);
    }
    addBytes(1);
  };
  const key = (raw: string, label: string): string => {
    const parsed = canonicalString(raw, label);
    if (parsed === '__proto__' || parsed === 'prototype' || parsed === 'constructor') {
      throw new ContractValidationError(`${label} uses a reserved object key`);
    }
    return parsed;
  };
  const cloneBounded = (value: unknown, depth: number): JsonValue => {
    if (depth > MAX_TYPED_UNKNOWN_DEPTH) {
      throw new ContractValidationError(`catalog typed-unknown payload exceeds depth ${MAX_TYPED_UNKNOWN_DEPTH}`);
    }
    addNode();
    if (value === null) return null;
    if (typeof value === 'boolean') {
      addBytes(1);
      return value;
    }
    if (typeof value === 'number') {
      if (!Number.isSafeInteger(value)) {
        throw new ContractValidationError('catalog typed-unknown numbers must be JavaScript-safe integers');
      }
      addBytes(8);
      return value;
    }
    if (typeof value === 'string') {
      addBytes(utf8Bytes(value));
      return value;
    }
    if (Array.isArray(value)) return value.map((entry) => cloneBounded(entry, depth + 1));
    const object = record(value, 'catalog typed-unknown object');
    const entries = Object.entries(object).map(([rawKey, entry]) => {
      const parsedKey = key(rawKey, 'catalog typed-unknown object key');
      addBytes(utf8Bytes(parsedKey));
      return [parsedKey, cloneBounded(entry, depth + 1)] as const;
    });
    return Object.fromEntries(entries);
  };
  const entries = Object.entries(input)
    .filter(([rawKey]) => !known.has(rawKey))
    .map(([rawKey, value]) => {
      const parsedKey = key(rawKey, 'catalog additive field');
      addBytes(utf8Bytes(parsedKey));
      return [parsedKey, cloneBounded(value, 1)] as const;
    });
  return Object.fromEntries(entries);
}

export function parseCatalogCoverageScope(value: unknown): CatalogCoverageScope {
  const input = record(value, 'catalog coverage scope');
  switch (input.kind) {
    case 'library':
      assertKnownFields(input, ['kind'], 'library catalog coverage scope');
      return { kind: 'library' };
    case 'entity':
      assertKnownFields(input, ['kind', 'external_ref'], 'entity catalog coverage scope');
      return { kind: 'entity', external_ref: parseExternalEntityRef(input.external_ref) };
    default:
      throw new ContractValidationError('catalog coverage scope has unsupported kind');
  }
}

function scopesEqual(left: CatalogCoverageScope, right: CatalogCoverageScope): boolean {
  return (
    left.kind === right.kind &&
    (left.kind === 'library' || (right.kind === 'entity' && externalRefsEqual(left.external_ref, right.external_ref)))
  );
}

function parsePlanSource(value: unknown): CatalogPortableCoveragePlanSource {
  const input = record(value, 'catalog coverage-plan source');
  assertKnownFields(
    input,
    [
      'adapter_id',
      'source_instance_key',
      'support_release_id',
      'catalog_declaration_digest',
      'access_policy_digest',
      'catalog_coverage_binding_digest',
    ],
    'catalog coverage-plan source',
  );
  return {
    adapter_id: canonicalString(input.adapter_id, 'catalog adapter id'),
    source_instance_key: parseOpaqueContractReference(input.source_instance_key, 'catalog source-instance key'),
    support_release_id: canonicalString(input.support_release_id, 'catalog support release id'),
    catalog_declaration_digest: parseOpaqueContractReference(
      input.catalog_declaration_digest,
      'catalog declaration digest',
    ),
    access_policy_digest: parseOpaqueContractReference(input.access_policy_digest, 'catalog access-policy digest'),
    catalog_coverage_binding_digest: parseOpaqueContractReference(
      input.catalog_coverage_binding_digest,
      'catalog coverage binding digest',
    ),
  };
}

function comparePlanSource(left: CatalogPortableCoveragePlanSource, right: CatalogPortableCoveragePlanSource): number {
  return (
    compareText(left.adapter_id, right.adapter_id) ||
    compareOpaque(left.source_instance_key, right.source_instance_key) ||
    compareText(left.support_release_id, right.support_release_id) ||
    compareOpaque(left.catalog_declaration_digest, right.catalog_declaration_digest) ||
    compareOpaque(left.access_policy_digest, right.access_policy_digest) ||
    compareOpaque(left.catalog_coverage_binding_digest, right.catalog_coverage_binding_digest)
  );
}

export function parseCatalogPortableCoveragePlan(value: unknown): CatalogPortableCoveragePlan {
  const input = record(value, 'portable catalog coverage plan');
  assertKnownFields(
    input,
    [
      'catalog_portable_coverage_plan_contract_version',
      'coverage_plan_contract_version',
      'coverage_plan_id',
      'scope',
      'required_sources',
      'optional_sources',
    ],
    'portable catalog coverage plan',
  );
  if (input.catalog_portable_coverage_plan_contract_version !== CATALOG_PORTABLE_COVERAGE_PLAN_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported portable catalog coverage-plan contract version');
  }
  if (input.coverage_plan_contract_version !== CATALOG_COVERAGE_PLAN_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported catalog coverage-plan contract version');
  }
  if (!Array.isArray(input.required_sources) || !Array.isArray(input.optional_sources)) {
    throw new ContractValidationError('catalog coverage-plan sources must be arrays');
  }
  if (input.required_sources.length + input.optional_sources.length > MAX_PLAN_SOURCES) {
    throw new ContractValidationError(`catalog coverage plan exceeds ${MAX_PLAN_SOURCES} sources`);
  }
  const requiredSources = input.required_sources.map(parsePlanSource);
  const optionalSources = input.optional_sources.map(parsePlanSource);
  strictlyIncreasing(requiredSources, comparePlanSource, 'required catalog sources', MAX_PLAN_SOURCES);
  strictlyIncreasing(optionalSources, comparePlanSource, 'optional catalog sources', MAX_PLAN_SOURCES);
  const coordinates = new Set<string>();
  for (const source of [...requiredSources, ...optionalSources]) {
    const key = `${source.adapter_id}\0${source.source_instance_key}`;
    if (coordinates.has(key)) throw new ContractValidationError('catalog source has duplicate optionality assignment');
    coordinates.add(key);
  }
  return {
    catalog_portable_coverage_plan_contract_version: CATALOG_PORTABLE_COVERAGE_PLAN_CONTRACT_VERSION,
    coverage_plan_contract_version: CATALOG_COVERAGE_PLAN_CONTRACT_VERSION,
    coverage_plan_id: parseOpaqueContractReference(input.coverage_plan_id, 'catalog coverage-plan id'),
    scope: parseCatalogCoverageScope(input.scope),
    required_sources: requiredSources,
    optional_sources: optionalSources,
  };
}

function parseReadinessReason(value: unknown): CatalogReadinessReason {
  const input = record(value, 'catalog readiness reason');
  if (
    typeof input.code !== 'string' ||
    input.code.length === 0 ||
    input.code.length > MAX_READINESS_REASON_CODE_BYTES ||
    !/^[a-z][a-z0-9_]*$/.test(input.code)
  ) {
    throw new ContractValidationError(
      `catalog readiness reason code must be lowercase ASCII machine code of at most ${MAX_READINESS_REASON_CODE_BYTES} bytes`,
    );
  }
  const code = input.code;
  switch (input.kind) {
    case 'source_retrying':
      assertKnownFields(input, ['kind', 'code'], 'source-retrying readiness reason');
      return { kind: 'source_retrying', code };
    case 'terminal_source_unavailable':
      assertKnownFields(input, ['kind', 'code'], 'terminal-source readiness reason');
      return { kind: 'terminal_source_unavailable', code };
    case 'integrity_failure':
      assertKnownFields(input, ['kind', 'code', 'snapshot_disposition'], 'integrity readiness reason');
      if (input.snapshot_disposition !== 'independently_safe' && input.snapshot_disposition !== 'discarded') {
        throw new ContractValidationError('catalog integrity reason has unsupported snapshot disposition');
      }
      return { kind: 'integrity_failure', code, snapshot_disposition: input.snapshot_disposition };
    default:
      throw new ContractValidationError('catalog readiness reason has unsupported kind');
  }
}

function assertCoverageDomainShape(value: unknown): void {
  const input = record(value, 'catalog coverage domain');
  switch (input.kind) {
    case 'decode':
      assertKnownFields(input, ['kind'], 'decode coverage domain');
      break;
    case 'fact_family':
      assertKnownFields(input, ['kind', 'family', 'version'], 'fact-family coverage domain');
      break;
    case 'projection_pack':
      assertKnownFields(input, ['kind', 'pack', 'version'], 'projection-pack coverage domain');
      break;
    default:
      throw new ContractValidationError('catalog coverage domain has unsupported kind');
  }
}

function assertCoverageMemberShapes(
  input: UnknownRecord,
  points: unknown[],
  absences: unknown[],
  errors: unknown[],
): void {
  assertKnownFields(
    input,
    [
      'coverage_set_contract_version',
      'coverage_domain',
      'scope',
      'membership_revision',
      'points',
      'explicit_absence_or_deletion',
      'explicit_errors',
      'completeness',
    ],
    'catalog source coverage set',
  );
  assertCoverageDomainShape(input.coverage_domain);
  const scope = record(input.scope, 'catalog coverage scope');
  assertKnownFields(
    scope,
    [
      'adapter_id',
      'source_instance_key',
      'root_entity_key',
      'support_release_id',
      'source_or_scope_declaration_digest',
    ],
    'catalog coverage scope',
  );
  for (const value of points) {
    const point = record(value, 'catalog coverage point');
    assertKnownFields(
      point,
      [
        'coverage_contract_version',
        'coverage_domain',
        'adapter_id',
        'source_instance_key',
        'stream_key',
        'object_key',
        'generation',
        'position',
        'status',
        'provenance',
      ],
      'catalog coverage point',
    );
    assertCoverageDomainShape(point.coverage_domain);
    if (point.position !== undefined && point.position !== null) {
      assertKnownFields(
        record(point.position, 'catalog coverage position'),
        ['kind', 'opaque', 'monotonic_order'],
        'catalog coverage position',
      );
    }
    const status = record(point.status, 'catalog coverage status');
    assertKnownFields(status, status.kind === 'unavailable' ? ['kind', 'reason'] : ['kind'], 'catalog coverage status');
    const provenance = record(point.provenance, 'catalog coverage provenance');
    assertKnownFields(
      provenance,
      ['source_record_id', 'semantic_revision_ref', 'observed_at'],
      'catalog coverage provenance',
    );
    if (provenance.semantic_revision_ref !== undefined && provenance.semantic_revision_ref !== null) {
      assertKnownFields(
        record(provenance.semantic_revision_ref, 'catalog coverage semantic revision'),
        ['semantic_reference_contract_version', 'fact_revision_id'],
        'catalog coverage semantic revision',
      );
    }
  }
  for (const value of absences) {
    assertKnownFields(
      record(value, 'catalog coverage absence'),
      ['stream_key', 'object_key', 'generation', 'kind'],
      'catalog coverage absence',
    );
  }
  for (const value of errors) {
    assertKnownFields(
      record(value, 'catalog coverage error'),
      ['stream_key', 'object_key', 'code'],
      'catalog coverage error',
    );
  }
}

function validateCoveragePortable(value: unknown): SourceCoverageSet {
  const input = record(value, 'catalog source coverage set');
  const pointInput = input.points;
  const absenceInput = input.explicit_absence_or_deletion;
  const errorInput = input.explicit_errors;
  if (!Array.isArray(pointInput) || !Array.isArray(absenceInput) || !Array.isArray(errorInput)) {
    throw new ContractValidationError('catalog source coverage members must be arrays');
  }
  if (
    pointInput.length > MAX_SOURCE_COVERAGE_MEMBERS ||
    absenceInput.length > MAX_SOURCE_COVERAGE_MEMBERS ||
    errorInput.length > MAX_SOURCE_COVERAGE_MEMBERS ||
    pointInput.length + absenceInput.length + errorInput.length > MAX_SOURCE_COVERAGE_MEMBERS
  ) {
    throw new ContractValidationError(`catalog source coverage set exceeds ${MAX_SOURCE_COVERAGE_MEMBERS} members`);
  }
  assertCoverageMemberShapes(input, pointInput, absenceInput, errorInput);
  const coverage = parseSourceCoverageSet(value);
  for (const point of coverage.points) {
    positiveInteger(point.generation, 'catalog coverage generation');
    if (point.status.kind === 'unavailable') {
      canonicalString(point.status.reason, 'catalog coverage unavailable reason', MAX_PORTABLE_COVERAGE_REASON_BYTES);
    }
    if (point.position?.monotonic_order !== undefined) {
      nonNegativeInteger(point.position.monotonic_order, 'catalog coverage monotonic order');
    }
    if (point.provenance.observed_at !== undefined) {
      safeInteger(point.provenance.observed_at, 'catalog coverage observation time');
    }
  }
  for (const absence of coverage.explicit_absence_or_deletion) {
    positiveInteger(absence.generation, 'catalog coverage absence generation');
  }
  for (const error of coverage.explicit_errors) {
    canonicalString(error.code, 'catalog coverage error code', MAX_IDENTIFIER_BYTES);
  }
  strictlyIncreasing(
    coverage.points,
    (left, right) =>
      compareOpaque(left.stream_key, right.stream_key) ||
      compareOpaque(left.object_key, right.object_key) ||
      left.generation - right.generation,
    'catalog coverage points',
    MAX_SOURCE_COVERAGE_MEMBERS,
  );
  strictlyIncreasing(
    coverage.explicit_absence_or_deletion,
    (left, right) =>
      compareOpaque(left.stream_key, right.stream_key) ||
      compareOpaque(left.object_key, right.object_key) ||
      left.generation - right.generation ||
      compareText(left.kind, right.kind),
    'catalog coverage absences',
    MAX_SOURCE_COVERAGE_MEMBERS,
  );
  const compareOptionalOpaque = (left: string | undefined, right: string | undefined): number => {
    if (left === undefined) return right === undefined ? 0 : -1;
    if (right === undefined) return 1;
    return compareOpaque(left, right);
  };
  strictlyIncreasing(
    coverage.explicit_errors,
    (left, right) =>
      compareOptionalOpaque(left.stream_key, right.stream_key) ||
      compareOptionalOpaque(left.object_key, right.object_key) ||
      compareText(left.code, right.code),
    'catalog coverage errors',
    MAX_SOURCE_COVERAGE_MEMBERS,
  );
  return coverage;
}

function sourceMatchesCoverage(source: CatalogPortableCoveragePlanSource, coverage: SourceCoverageSet): boolean {
  return (
    source.adapter_id === coverage.scope.adapter_id &&
    source.source_instance_key === coverage.scope.source_instance_key &&
    source.support_release_id === coverage.scope.support_release_id &&
    source.catalog_coverage_binding_digest === coverage.scope.source_or_scope_declaration_digest
  );
}

function coveragePresent(source: CatalogPortableCoveragePlanSource, coverage: SourceCoverageSet[]): boolean {
  return coverage.some((set) => sourceMatchesCoverage(source, set));
}

function coverageComplete(source: CatalogPortableCoveragePlanSource, coverage: SourceCoverageSet[]): boolean {
  return coverage.some((set) => sourceMatchesCoverage(source, set) && set.completeness === 'complete');
}

export function parseCatalogReadinessSnapshot(value: unknown, expectedPlanInput: unknown): CatalogReadinessSnapshot {
  const expectedPlan = parseCatalogPortableCoveragePlan(expectedPlanInput);
  const input = record(value, 'catalog readiness snapshot');
  assertKnownFields(
    input,
    [
      'readiness_contract_version',
      'scope',
      'coverage_plan_id',
      'desired_contract_version',
      'completed_contract_version',
      'epoch',
      'attempt',
      'state',
      'complete_through_commit',
      'last_complete_snapshot',
      'refreshing_from_snapshot',
      'source_coverage',
      'reason',
    ],
    'catalog readiness snapshot',
  );
  if (input.readiness_contract_version !== CATALOG_READINESS_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported catalog readiness contract version');
  }
  const scope = parseCatalogCoverageScope(input.scope);
  const coveragePlanId = parseOpaqueContractReference(input.coverage_plan_id, 'catalog readiness coverage-plan id');
  if (!scopesEqual(scope, expectedPlan.scope) || coveragePlanId !== expectedPlan.coverage_plan_id) {
    throw new ContractValidationError('catalog readiness belongs to a different scope or coverage plan');
  }
  const desiredContractVersion = u32(input.desired_contract_version, 'desired catalog contract version');
  const completedContractVersion = optional(input.completed_contract_version, (entry) =>
    u32(entry, 'completed catalog contract version'),
  );
  if (completedContractVersion !== undefined && completedContractVersion > desiredContractVersion) {
    throw new ContractValidationError('completed catalog version cannot exceed desired version');
  }
  const epoch = positiveInteger(input.epoch, 'catalog readiness epoch');
  const attempt = positiveInteger(input.attempt, 'catalog readiness attempt');
  const supportedStates: CatalogReadinessPhase[] = ['pending', 'building', 'partial', 'ready', 'degraded', 'error'];
  if (!supportedStates.includes(input.state as CatalogReadinessPhase)) {
    throw new ContractValidationError('catalog readiness has unsupported state');
  }
  const state = input.state as CatalogReadinessPhase;
  const completeThroughCommit = optional(input.complete_through_commit, (entry) =>
    positiveInteger(entry, 'catalog complete-through commit'),
  );
  const lastCompleteSnapshot = optional(input.last_complete_snapshot, parseSnapshot);
  const refreshingFromSnapshot = optional(input.refreshing_from_snapshot, parseSnapshot);
  if (!Array.isArray(input.source_coverage)) {
    throw new ContractValidationError('catalog readiness source coverage must be an array');
  }
  const sourceBound = expectedPlan.required_sources.length + expectedPlan.optional_sources.length;
  if (input.source_coverage.length > sourceBound || input.source_coverage.length > MAX_PLAN_SOURCES) {
    throw new ContractValidationError('catalog readiness source coverage exceeds its frozen plan');
  }
  const sourceCoverage = input.source_coverage.map(validateCoveragePortable);
  const memberCount = sourceCoverage.reduce(
    (count, set) => count + set.points.length + set.explicit_absence_or_deletion.length + set.explicit_errors.length,
    0,
  );
  if (memberCount > MAX_SOURCE_COVERAGE_MEMBERS) {
    throw new ContractValidationError(`catalog source coverage exceeds ${MAX_SOURCE_COVERAGE_MEMBERS} members`);
  }
  const reason = optional(input.reason, parseReadinessReason);

  strictlyIncreasing(
    sourceCoverage,
    (left, right) =>
      compareText(left.scope.adapter_id, right.scope.adapter_id) ||
      compareOpaque(left.scope.source_instance_key, right.scope.source_instance_key),
    'catalog readiness source coverage',
    MAX_PLAN_SOURCES,
  );

  if (
    (state === 'degraded' && reason?.kind !== 'terminal_source_unavailable') ||
    (state === 'error' && reason?.kind !== 'integrity_failure') ||
    (state !== 'degraded' && reason?.kind === 'terminal_source_unavailable') ||
    (state !== 'error' && reason?.kind === 'integrity_failure')
  ) {
    throw new ContractValidationError('catalog readiness reason does not match its state');
  }
  if ((completedContractVersion === undefined) !== (lastCompleteSnapshot === undefined)) {
    throw new ContractValidationError('completed catalog version and last snapshot must agree');
  }
  if (
    completedContractVersion !== undefined &&
    lastCompleteSnapshot !== undefined &&
    completedContractVersion !== lastCompleteSnapshot.pack_contract_version
  ) {
    throw new ContractValidationError('completed catalog version does not match the retained snapshot');
  }
  if (lastCompleteSnapshot !== undefined && lastCompleteSnapshot.readiness_epoch > epoch) {
    throw new ContractValidationError('last catalog snapshot cannot come from a future epoch');
  }
  if (completeThroughCommit !== undefined && !['ready', 'degraded', 'error'].includes(state)) {
    throw new ContractValidationError('only ready, degraded, or safe-error readiness may be current-complete');
  }
  if (completeThroughCommit !== undefined) {
    if (
      lastCompleteSnapshot === undefined ||
      lastCompleteSnapshot.coverage_plan_id !== coveragePlanId ||
      lastCompleteSnapshot.readiness_epoch !== epoch ||
      lastCompleteSnapshot.complete_commit !== completeThroughCommit
    ) {
      throw new ContractValidationError('current complete commit does not identify the current epoch snapshot');
    }
  }
  if (refreshingFromSnapshot !== undefined) {
    if (
      state !== 'ready' ||
      lastCompleteSnapshot === undefined ||
      !snapshotsEqual(refreshingFromSnapshot, lastCompleteSnapshot) ||
      completeThroughCommit !== refreshingFromSnapshot.complete_commit
    ) {
      throw new ContractValidationError('catalog refresh does not retain the exact current snapshot');
    }
  }
  if (reason?.kind === 'integrity_failure') {
    if (reason.snapshot_disposition === 'discarded') {
      if (
        completedContractVersion !== undefined ||
        completeThroughCommit !== undefined ||
        lastCompleteSnapshot !== undefined
      ) {
        throw new ContractValidationError('discarded integrity failure cannot retain snapshot state');
      }
    } else {
      if (completedContractVersion === undefined || lastCompleteSnapshot === undefined) {
        throw new ContractValidationError('independently-safe integrity failure must retain a complete snapshot');
      }
      const expectedCommit =
        lastCompleteSnapshot.coverage_plan_id === coveragePlanId && lastCompleteSnapshot.readiness_epoch === epoch
          ? lastCompleteSnapshot.complete_commit
          : undefined;
      if (completeThroughCommit !== expectedCommit) {
        throw new ContractValidationError('independently-safe integrity failure has false current completeness');
      }
    }
  }

  const expectedRoot = scope.kind === 'entity' ? scope.external_ref.entity_key : undefined;
  const seenSources = new Set<string>();
  for (const coverage of sourceCoverage) {
    if (
      coverage.coverage_domain.kind !== 'projection_pack' ||
      coverage.coverage_domain.pack !== 'library.catalog' ||
      coverage.coverage_domain.version !== desiredContractVersion ||
      coverage.scope.root_entity_key !== expectedRoot
    ) {
      throw new ContractValidationError('catalog readiness coverage has the wrong pack or scope');
    }
    const source = [...expectedPlan.required_sources, ...expectedPlan.optional_sources].find((candidate) =>
      sourceMatchesCoverage(candidate, coverage),
    );
    if (source === undefined)
      throw new ContractValidationError('catalog readiness coverage is outside its frozen plan');
    const sourceKey = `${source.adapter_id}\0${source.source_instance_key}`;
    if (seenSources.has(sourceKey)) throw new ContractValidationError('catalog readiness repeats source coverage');
    seenSources.add(sourceKey);
  }
  const everyRequiredPresent = expectedPlan.required_sources.every((source) => coveragePresent(source, sourceCoverage));
  const everyRequiredComplete = expectedPlan.required_sources.every((source) =>
    coverageComplete(source, sourceCoverage),
  );
  const anyRequiredPresent = expectedPlan.required_sources.some((source) => coveragePresent(source, sourceCoverage));

  if (state === 'ready') {
    if (
      lastCompleteSnapshot === undefined ||
      lastCompleteSnapshot.coverage_plan_id !== coveragePlanId ||
      lastCompleteSnapshot.readiness_epoch !== epoch ||
      lastCompleteSnapshot.pack_contract_version !== desiredContractVersion ||
      completedContractVersion !== desiredContractVersion ||
      completeThroughCommit !== lastCompleteSnapshot.complete_commit ||
      !((reason === undefined && everyRequiredComplete) || (reason?.kind === 'source_retrying' && everyRequiredPresent))
    ) {
      throw new ContractValidationError('ready catalog state does not identify one complete snapshot');
    }
  }
  if (state === 'degraded' && !everyRequiredPresent) {
    throw new ContractValidationError('degraded catalog state must cover every required source');
  }
  if (state === 'partial' && !anyRequiredPresent) {
    throw new ContractValidationError('partial catalog state requires required-source coverage');
  }

  return {
    readiness_contract_version: CATALOG_READINESS_CONTRACT_VERSION,
    scope,
    coverage_plan_id: coveragePlanId,
    desired_contract_version: desiredContractVersion,
    ...(completedContractVersion === undefined ? {} : { completed_contract_version: completedContractVersion }),
    epoch,
    attempt,
    state,
    ...(completeThroughCommit === undefined ? {} : { complete_through_commit: completeThroughCommit }),
    ...(lastCompleteSnapshot === undefined ? {} : { last_complete_snapshot: lastCompleteSnapshot }),
    ...(refreshingFromSnapshot === undefined ? {} : { refreshing_from_snapshot: refreshingFromSnapshot }),
    source_coverage: sourceCoverage,
    ...(reason === undefined ? {} : { reason }),
  };
}

function validatePublishedReadiness(
  value: unknown,
  snapshot: CatalogSnapshotId,
  expectedPlan: CatalogPortableCoveragePlan,
): CatalogReadinessSnapshot {
  const readiness = parseCatalogReadinessSnapshot(value, expectedPlan);
  if (
    readiness.state !== 'ready' ||
    readiness.last_complete_snapshot === undefined ||
    !snapshotsEqual(readiness.last_complete_snapshot, snapshot) ||
    readiness.complete_through_commit !== snapshot.complete_commit ||
    readiness.coverage_plan_id !== snapshot.coverage_plan_id ||
    readiness.epoch !== snapshot.readiness_epoch ||
    readiness.completed_contract_version !== snapshot.pack_contract_version ||
    readiness.reason !== undefined ||
    readiness.refreshing_from_snapshot !== undefined
  ) {
    throw new ContractValidationError('catalog page readiness does not describe its immutable published snapshot');
  }
  return readiness;
}

export function parseCatalogReadinessResponse(
  value: unknown,
  expectedSelectionInput: unknown,
  expectedPlanInput: unknown,
): CatalogReadinessResponse {
  const expectedSelection = parseCatalogQueryContractSelection(expectedSelectionInput);
  const expectedPlan = parseCatalogPortableCoveragePlan(expectedPlanInput);
  const input = record(value, 'catalog readiness response');
  if (input.catalog_readiness_response_contract_version !== CATALOG_READINESS_RESPONSE_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported catalog readiness response contract version');
  }
  const selection = parseCatalogQueryContractSelection(input.contract_selection);
  if (!selectionsEqual(selection, expectedSelection)) {
    throw new ContractValidationError('catalog readiness response does not match the negotiated selection');
  }
  const readiness = parseCatalogReadinessSnapshot(input.readiness, expectedPlan);
  if (selection.contract_versions.query_pack_version !== readiness.desired_contract_version) {
    throw new ContractValidationError('catalog readiness desired pack does not match the negotiated query pack');
  }
  return {
    catalog_readiness_response_contract_version: CATALOG_READINESS_RESPONSE_CONTRACT_VERSION,
    contract_selection: selection,
    readiness,
    additive_fields: parseAdditiveFields(
      input,
      ['catalog_readiness_response_contract_version', 'contract_selection', 'readiness'],
      selection,
    ),
  };
}

/** Parse the native/IPC readiness envelope against the original client offer. */
export function parseCatalogReadinessQueryResult(
  value: unknown,
  expectedRequestInput: unknown,
): CatalogReadinessQueryResult {
  const input = record(value, 'catalog readiness query result');
  assertKnownFields(input, ['coverage_plan', 'readiness'], 'catalog readiness query result');
  const plan = parseCatalogPortableCoveragePlan(input.coverage_plan);
  const readinessInput = record(input.readiness, 'catalog readiness response');
  const selection = parseCatalogQueryContractSelectionForRequest(
    readinessInput.contract_selection,
    expectedRequestInput,
  );
  return {
    coverage_plan: plan,
    readiness: parseCatalogReadinessResponse(readinessInput, selection, plan),
  };
}

function parseAuthority(value: unknown): CatalogFieldAuthority {
  const input = record(value, 'catalog field authority');
  assertKnownFields(input, ['class_id', 'precedence', 'native_times_comparable'], 'catalog field authority');
  const precedence = positiveInteger(input.precedence, 'catalog authority precedence');
  if (precedence > 0xffff) throw new ContractValidationError('catalog authority precedence exceeds u16');
  return {
    class_id: canonicalString(input.class_id, 'catalog authority class'),
    precedence,
    native_times_comparable: booleanValue(input.native_times_comparable, 'native-times comparability'),
  };
}

function parseNativeIdentity(value: unknown): NativeIdentity {
  const input = record(value, 'catalog native identity');
  assertKnownFields(input, ['native_namespace', 'native_id'], 'catalog native identity');
  return {
    native_namespace: canonicalString(input.native_namespace, 'native identity namespace'),
    native_id: canonicalString(input.native_id, 'native identity', MAX_TEXT_BYTES),
  };
}

function parseText(value: unknown, label: string): string {
  return canonicalString(value, label, MAX_TEXT_BYTES);
}

function parseAvailability(value: unknown): CatalogAvailability {
  const input = record(value, 'catalog availability');
  switch (input.state) {
    case 'metadata_only':
    case 'transcript_discovered':
    case 'hydrating':
    case 'history_ready':
      assertKnownFields(input, ['state'], 'catalog availability');
      return { state: input.state };
    case 'unavailable':
      assertKnownFields(input, ['state', 'reason'], 'unavailable catalog availability');
      return { state: 'unavailable', reason: canonicalString(input.reason, 'catalog unavailable reason') };
    default:
      throw new ContractValidationError('catalog availability has unsupported state');
  }
}

function parseQualifiedField<T>(
  value: unknown,
  label: string,
  parseValue: (entry: unknown) => T,
): QualifiedValue<T, CatalogFieldAuthority, SemanticRevisionRef[]> {
  const input = record(value, label);
  assertKnownFields(
    input,
    ['value', 'quality', 'authority', 'completeness', 'unknown_reason', 'effective_at', 'provenance'],
    label,
  );
  const parsed = parseQualifiedValue<T, CatalogFieldAuthority, SemanticRevisionRef[]>(value);
  const concrete = parsed.value === null ? null : parseValue(parsed.value);
  const authority = parseAuthority(input.authority);
  const provenance = parseProvenance(input.provenance, `${label} provenance`);
  const effectiveAt = optional(input.effective_at, (entry) => safeInteger(entry, `${label} effective time`));
  return {
    value: concrete,
    quality: parsed.quality,
    authority,
    completeness: parsed.completeness,
    ...(parsed.unknown_reason === undefined ? {} : { unknown_reason: parsed.unknown_reason }),
    ...(effectiveAt === undefined ? {} : { effective_at: effectiveAt }),
    provenance,
  };
}

function parseFieldSelection<T>(
  value: unknown,
  label: string,
  parseValue: (entry: unknown) => T,
): CatalogPortableFieldSelection<T> {
  const input = record(value, `${label} selection`);
  assertKnownFields(input, ['selected_assertion_key', 'field', 'conflicting_assertion_keys'], `${label} selection`);
  const selected = parseOpaqueContractReference(input.selected_assertion_key, `${label} selected assertion`);
  const conflicts = parseOpaqueList(
    input.conflicting_assertion_keys,
    `${label} conflicting assertions`,
    MAX_ROW_EVIDENCE_KEYS,
  );
  if (conflicts.includes(selected)) {
    throw new ContractValidationError(`${label} selected assertion cannot also conflict`);
  }
  return {
    selected_assertion_key: selected,
    field: parseQualifiedField(input.field, label, parseValue),
    conflicting_assertion_keys: conflicts,
  };
}

function parseUnknownReason(value: unknown, label: string): QualifiedUnknownReason {
  const supported: QualifiedUnknownReason[] = [
    'missing',
    'unsupported',
    'withheld',
    'not_yet_observed',
    'ambiguous',
    'malformed',
  ];
  if (!supported.includes(value as QualifiedUnknownReason)) {
    throw new ContractValidationError(`${label} has unsupported unknown reason`);
  }
  return value as QualifiedUnknownReason;
}

function parseOptionalField<T>(
  value: unknown,
  label: string,
  parseValue: (entry: unknown) => T,
): CatalogOptionalField<T> {
  const input = record(value, label);
  switch (input.state) {
    case 'selected':
      assertKnownFields(input, ['state', 'selection'], label);
      return { state: 'selected', selection: parseFieldSelection(input.selection, label, parseValue) };
    case 'unknown':
      assertKnownFields(input, ['state', 'reason'], label);
      return { state: 'unknown', reason: parseUnknownReason(input.reason, label) };
    default:
      throw new ContractValidationError(`${label} has unsupported state`);
  }
}

function fieldEvidenceKeys<T>(field: CatalogOptionalField<T>): OpaqueContractReference[] {
  return field.state === 'selected'
    ? [field.selection.selected_assertion_key, ...field.selection.conflicting_assertion_keys]
    : [];
}

function selectionEvidenceKeys<T>(selection: CatalogPortableFieldSelection<T>): OpaqueContractReference[] {
  return [selection.selected_assertion_key, ...selection.conflicting_assertion_keys];
}

function assertEvidenceMembership(
  assertionKeys: OpaqueContractReference[],
  evidenceKeys: OpaqueContractReference[],
): void {
  for (const evidence of evidenceKeys) {
    if (!assertionKeys.includes(evidence)) {
      throw new ContractValidationError('catalog field evidence is outside its row assertion membership');
    }
  }
}

function parseOwner(value: unknown): CatalogEvidenceOwner {
  const input = record(value, 'catalog evidence owner');
  assertKnownFields(
    input,
    ['adapter_id', 'source_instance_key', 'stream_key', 'object_key', 'generation'],
    'catalog evidence owner',
  );
  return {
    adapter_id: canonicalString(input.adapter_id, 'catalog evidence adapter'),
    source_instance_key: parseOpaqueContractReference(input.source_instance_key, 'catalog owner source'),
    stream_key: parseOpaqueContractReference(input.stream_key, 'catalog owner stream'),
    object_key: parseOpaqueContractReference(input.object_key, 'catalog owner object'),
    generation: positiveInteger(input.generation, 'catalog owner generation'),
  };
}

function parseAssociationFact(value: unknown): CatalogAssociationFact {
  const input = record(value, 'catalog association fact');
  assertKnownFields(
    input,
    [
      'association_key',
      'owner',
      'session_ref',
      'project_ref',
      'basis',
      'declared_derivation_id',
      'locator_claim_key',
      'authority',
      'quality',
      'completeness',
      'effective_at',
      'provenance',
    ],
    'catalog association fact',
  );
  const bases: ProjectAssociationBasis[] = [
    'native_project_index',
    'transcript_cwd',
    'session_directory',
    'rollout_header',
    'declared_derived_ancestor',
  ];
  if (!bases.includes(input.basis as ProjectAssociationBasis)) {
    throw new ContractValidationError('catalog association has unsupported basis');
  }
  const basis = input.basis as ProjectAssociationBasis;
  const declaredDerivation =
    input.declared_derivation_id === null || input.declared_derivation_id === undefined
      ? null
      : canonicalString(input.declared_derivation_id, 'catalog declared derivation');
  if ((basis === 'declared_derived_ancestor') !== (declaredDerivation !== null)) {
    throw new ContractValidationError('catalog association derivation does not match its basis');
  }
  const qualities: Array<Exclude<QualifiedValueQuality, 'unknown'>> = [
    'exact',
    'native_claimed',
    'derived',
    'estimated',
  ];
  if (!qualities.includes(input.quality as Exclude<QualifiedValueQuality, 'unknown'>)) {
    throw new ContractValidationError('unknown evidence cannot assert a catalog association');
  }
  const completeness: ContractCompleteness[] = ['complete', 'partial', 'unknown'];
  if (!completeness.includes(input.completeness as ContractCompleteness)) {
    throw new ContractValidationError('catalog association has unsupported completeness');
  }
  const effectiveAt = optional(input.effective_at, (entry) => safeInteger(entry, 'catalog association effective time'));
  return {
    association_key: parseOpaqueContractReference(input.association_key, 'catalog association key'),
    owner: parseOwner(input.owner),
    session_ref: parseEntityRef(input.session_ref, 'session'),
    project_ref: parseEntityRef(input.project_ref, 'project'),
    basis,
    declared_derivation_id: declaredDerivation,
    locator_claim_key:
      input.locator_claim_key === null || input.locator_claim_key === undefined
        ? null
        : parseOpaqueContractReference(input.locator_claim_key, 'catalog locator-claim key'),
    authority: parseAuthority(input.authority),
    quality: input.quality as Exclude<QualifiedValueQuality, 'unknown'>,
    completeness: input.completeness as ContractCompleteness,
    effective_at: effectiveAt ?? null,
    provenance: parseProvenance(input.provenance, 'catalog association provenance'),
  };
}

function parseAssociationCoverage(value: unknown, sessionRef: CatalogEntityRef): CatalogAssociationCoverage {
  const input = record(value, 'catalog association coverage');
  switch (input.state) {
    case 'unknown':
      assertKnownFields(input, ['state'], 'unknown catalog association coverage');
      return { state: 'unknown' };
    case 'available': {
      assertKnownFields(input, ['state', 'selection'], 'available catalog association coverage');
      const selectionInput = record(input.selection, 'catalog association selection');
      assertKnownFields(
        selectionInput,
        ['association', 'competing_associations', 'conflicting_association_keys'],
        'catalog association selection',
      );
      const association = parseAssociationFact(selectionInput.association);
      if (!entityRefsEqual(association.session_ref, sessionRef)) {
        throw new ContractValidationError('selected catalog association belongs to another session');
      }
      if (!Array.isArray(selectionInput.competing_associations)) {
        throw new ContractValidationError('competing catalog associations must be an array');
      }
      if (selectionInput.competing_associations.length > MAX_ASSOCIATION_EVIDENCE) {
        throw new ContractValidationError(`competing catalog associations exceeds ${MAX_ASSOCIATION_EVIDENCE} entries`);
      }
      const competitors = selectionInput.competing_associations.map(parseAssociationFact);
      strictlyIncreasing(
        competitors,
        (left, right) => compareOpaque(left.association_key, right.association_key),
        'competing catalog associations',
        MAX_ASSOCIATION_EVIDENCE,
      );
      for (const competitor of competitors) {
        if (
          !entityRefsEqual(competitor.session_ref, sessionRef) ||
          competitor.association_key === association.association_key
        ) {
          throw new ContractValidationError('competing association is not independent evidence for this session');
        }
      }
      const conflicts = parseOpaqueList(
        selectionInput.conflicting_association_keys,
        'conflicting catalog association keys',
        MAX_ASSOCIATION_EVIDENCE,
      );
      const expectedConflicts = competitors
        .filter(
          (competitor) =>
            competitor.authority.class_id === association.authority.class_id &&
            competitor.authority.precedence === association.authority.precedence &&
            competitor.authority.native_times_comparable === association.authority.native_times_comparable &&
            !entityRefsEqual(competitor.project_ref, association.project_ref),
        )
        .map((competitor) => competitor.association_key);
      if (
        conflicts.length !== expectedConflicts.length ||
        conflicts.some((key, index) => key !== expectedConflicts[index])
      ) {
        throw new ContractValidationError(
          'catalog association conflicts do not exactly identify equal-authority different-project evidence',
        );
      }
      return {
        state: 'available',
        selection: {
          association,
          competing_associations: competitors,
          conflicting_association_keys: conflicts,
        },
      };
    }
    default:
      throw new ContractValidationError('catalog association coverage has unsupported state');
  }
}

function parseProjectRow(value: unknown, selection: CatalogQueryContractSelection): CatalogPortableProjectRow {
  const input = record(value, 'catalog project row');
  const known = [
    'project_ref',
    'native_identity',
    'root_identity',
    'display_path',
    'display_name',
    'native_time',
    'availability',
    'assertion_keys',
  ] as const;
  const projectRef = parseEntityRef(input.project_ref, 'project');
  const nativeIdentity = parseOptionalField(input.native_identity, 'project native identity', parseNativeIdentity);
  const rootIdentity = parseOptionalField(input.root_identity, 'project root identity', (entry) =>
    parseText(entry, 'project root identity'),
  );
  const displayPath = parseOptionalField(input.display_path, 'project display path', (entry) =>
    parseText(entry, 'project display path'),
  );
  const displayName = parseOptionalField(input.display_name, 'project display name', (entry) =>
    parseText(entry, 'project display name'),
  );
  const nativeTime = parseOptionalField(input.native_time, 'project native time', (entry) =>
    safeInteger(entry, 'project native time'),
  );
  const availability = parseFieldSelection(input.availability, 'project availability', parseAvailability);
  const assertionKeys = parseOpaqueList(input.assertion_keys, 'project assertion keys', MAX_ROW_EVIDENCE_KEYS, true);
  assertEvidenceMembership(assertionKeys, [
    ...fieldEvidenceKeys(nativeIdentity),
    ...fieldEvidenceKeys(rootIdentity),
    ...fieldEvidenceKeys(displayPath),
    ...fieldEvidenceKeys(displayName),
    ...fieldEvidenceKeys(nativeTime),
    ...selectionEvidenceKeys(availability),
  ]);
  return {
    project_ref: projectRef,
    native_identity: nativeIdentity,
    root_identity: rootIdentity,
    display_path: displayPath,
    display_name: displayName,
    native_time: nativeTime,
    availability,
    assertion_keys: assertionKeys,
    additive_fields: parseAdditiveFields(input, known, selection),
  };
}

function parseSessionRow(value: unknown, selection: CatalogQueryContractSelection): CatalogPortableSessionRow {
  const input = record(value, 'catalog session row');
  const known = [
    'session_ref',
    'project_association',
    'native_identity',
    'title',
    'first_user_summary',
    'native_created_at',
    'native_updated_at',
    'native_message_count',
    'transcript_locator_claim_keys',
    'availability',
    'assertion_keys',
  ] as const;
  const sessionRef = parseEntityRef(input.session_ref, 'session');
  const association = parseAssociationCoverage(input.project_association, sessionRef);
  const nativeIdentity = parseOptionalField(input.native_identity, 'session native identity', parseNativeIdentity);
  const title = parseOptionalField(input.title, 'session title', (entry) => parseText(entry, 'session title'));
  const firstUserSummary = parseOptionalField(input.first_user_summary, 'session first-user summary', (entry) =>
    parseText(entry, 'session first-user summary'),
  );
  const nativeCreatedAt = parseOptionalField(input.native_created_at, 'session native creation time', (entry) =>
    safeInteger(entry, 'session native creation time'),
  );
  const nativeUpdatedAt = parseOptionalField(input.native_updated_at, 'session native update time', (entry) =>
    safeInteger(entry, 'session native update time'),
  );
  const nativeMessageCount = parseOptionalField(input.native_message_count, 'session native message count', (entry) =>
    nonNegativeInteger(entry, 'session native message count'),
  );
  const locatorKeys = parseOpaqueList(
    input.transcript_locator_claim_keys,
    'session transcript locator-claim keys',
    MAX_ROW_EVIDENCE_KEYS,
  );
  const availability = parseFieldSelection(input.availability, 'session availability', parseAvailability);
  const assertionKeys = parseOpaqueList(input.assertion_keys, 'session assertion keys', MAX_ROW_EVIDENCE_KEYS, true);
  assertEvidenceMembership(assertionKeys, [
    ...fieldEvidenceKeys(nativeIdentity),
    ...fieldEvidenceKeys(title),
    ...fieldEvidenceKeys(firstUserSummary),
    ...fieldEvidenceKeys(nativeCreatedAt),
    ...fieldEvidenceKeys(nativeUpdatedAt),
    ...fieldEvidenceKeys(nativeMessageCount),
    ...selectionEvidenceKeys(availability),
  ]);
  return {
    session_ref: sessionRef,
    project_association: association,
    native_identity: nativeIdentity,
    title,
    first_user_summary: firstUserSummary,
    native_created_at: nativeCreatedAt,
    native_updated_at: nativeUpdatedAt,
    native_message_count: nativeMessageCount,
    transcript_locator_claim_keys: locatorKeys,
    availability,
    assertion_keys: assertionKeys,
    additive_fields: parseAdditiveFields(input, known, selection),
  };
}

function parseSortKey(value: unknown): string {
  if (typeof value !== 'string' || !value.startsWith('v1:')) {
    throw new ContractValidationError('catalog sort key must be versioned base64url');
  }
  const decoded = decodeBase64Url(value, 'catalog sort key');
  if (decoded.length === 0 || decoded.length > 64 * 1_024) {
    throw new ContractValidationError('catalog sort key must contain 1..=65536 bytes');
  }
  return value;
}

function parseCursor(value: unknown): CatalogCursor {
  const input = record(value, 'catalog cursor');
  assertKnownFields(
    input,
    [
      'cursor_contract_version',
      'snapshot_id',
      'query_fingerprint',
      'sort_spec_version',
      'last_sort_key',
      'last_entity_key',
    ],
    'catalog cursor',
  );
  if (input.cursor_contract_version !== 1) {
    throw new ContractValidationError('unsupported catalog cursor contract version');
  }
  return {
    cursor_contract_version: 1,
    snapshot_id: parseSnapshot(input.snapshot_id),
    query_fingerprint: parseOpaqueContractReference(input.query_fingerprint, 'catalog query fingerprint'),
    sort_spec_version: u32(input.sort_spec_version, 'catalog cursor sort version'),
    last_sort_key: parseSortKey(input.last_sort_key),
    last_entity_key: parseOpaqueContractReference(input.last_entity_key, 'catalog cursor entity key'),
  };
}

function cursorsEqual(left: CatalogCursor, right: CatalogCursor): boolean {
  return (
    snapshotsEqual(left.snapshot_id, right.snapshot_id) &&
    left.query_fingerprint === right.query_fingerprint &&
    left.sort_spec_version === right.sort_spec_version &&
    left.last_sort_key === right.last_sort_key &&
    left.last_entity_key === right.last_entity_key
  );
}

export function parseCatalogPageRequestBinding(
  value: unknown,
  expectedSelectionInput?: unknown,
): CatalogPageRequestBinding {
  const input = record(value, 'catalog page request binding');
  assertKnownFields(
    input,
    [
      'contract_selection',
      'snapshot_id',
      'query_kind',
      'query_fingerprint',
      'sort_spec_version',
      'page_size',
      'after_cursor',
    ],
    'catalog page request binding',
  );
  const selection = parseCatalogQueryContractSelection(input.contract_selection);
  if (expectedSelectionInput !== undefined) {
    const expected = parseCatalogQueryContractSelection(expectedSelectionInput);
    if (!selectionsEqual(selection, expected)) {
      throw new ContractValidationError('catalog page request does not match the negotiated selection');
    }
  }
  const snapshot = parseSnapshot(input.snapshot_id);
  if (snapshot.pack_contract_version !== selection.contract_versions.query_pack_version) {
    throw new ContractValidationError('catalog page snapshot does not match the selected query pack');
  }
  if (input.query_kind !== 'projects' && input.query_kind !== 'sessions') {
    throw new ContractValidationError('catalog page request has unsupported query kind');
  }
  const queryFingerprint = parseOpaqueContractReference(input.query_fingerprint, 'catalog query fingerprint');
  const sortSpecVersion = u32(input.sort_spec_version, 'catalog sort specification version');
  const pageSize = positiveInteger(input.page_size, 'catalog page size');
  if (pageSize > MAX_PAGE_ROWS) throw new ContractValidationError(`catalog page size exceeds ${MAX_PAGE_ROWS}`);
  const afterCursor = optional(input.after_cursor, parseCursor);
  if (
    afterCursor !== undefined &&
    (!snapshotsEqual(afterCursor.snapshot_id, snapshot) ||
      afterCursor.query_fingerprint !== queryFingerprint ||
      afterCursor.sort_spec_version !== sortSpecVersion)
  ) {
    throw new ContractValidationError('catalog page after-cursor has a foreign snapshot, query, or sort binding');
  }
  return {
    contract_selection: selection,
    snapshot_id: snapshot,
    query_kind: input.query_kind,
    query_fingerprint: queryFingerprint,
    sort_spec_version: sortSpecVersion,
    page_size: pageSize,
    ...(afterCursor === undefined ? {} : { after_cursor: afterCursor }),
  };
}

function pageRequestsEqual(left: CatalogPageRequestBinding, right: CatalogPageRequestBinding): boolean {
  return (
    selectionsEqual(left.contract_selection, right.contract_selection) &&
    snapshotsEqual(left.snapshot_id, right.snapshot_id) &&
    left.query_kind === right.query_kind &&
    left.query_fingerprint === right.query_fingerprint &&
    left.sort_spec_version === right.sort_spec_version &&
    left.page_size === right.page_size &&
    ((left.after_cursor === undefined && right.after_cursor === undefined) ||
      (left.after_cursor !== undefined &&
        right.after_cursor !== undefined &&
        cursorsEqual(left.after_cursor, right.after_cursor)))
  );
}

function parseCount(value: unknown): CatalogCount {
  const input = record(value, 'catalog count');
  switch (input.state) {
    case 'known':
      assertKnownFields(input, ['state', 'value'], 'known catalog count');
      return { state: 'known', value: nonNegativeInteger(input.value, 'catalog count') };
    case 'unknown':
      assertKnownFields(input, ['state', 'reason'], 'unknown catalog count');
      return { state: 'unknown', reason: parseUnknownReason(input.reason, 'catalog count') };
    default:
      throw new ContractValidationError('catalog count has unsupported state');
  }
}

function comparePageKey(
  left: { sort_key: string; entity_key: OpaqueContractReference },
  right: { sort_key: string; entity_key: OpaqueContractReference },
): number {
  return (
    compareBytes(
      decodeBase64Url(left.sort_key, 'catalog sort key'),
      decodeBase64Url(right.sort_key, 'catalog sort key'),
    ) || compareOpaque(left.entity_key, right.entity_key)
  );
}

function parsePage<R extends CatalogPortableProjectRow | CatalogPortableSessionRow>(
  value: unknown,
  expectedRequestInput: unknown,
  expectedPlanInput: unknown,
  queryKind: CatalogQueryKind,
  parseRow: (row: unknown, selection: CatalogQueryContractSelection) => R,
  rowEntity: (row: R) => CatalogEntityRef,
): CatalogPage<R> {
  const expectedRequest = parseCatalogPageRequestBinding(expectedRequestInput);
  if (expectedRequest.query_kind !== queryKind) {
    throw new ContractValidationError(`caller-held request must be a ${queryKind} query`);
  }
  const expectedPlan = parseCatalogPortableCoveragePlan(expectedPlanInput);
  const input = record(value, `${queryKind} catalog page`);
  if (input.catalog_page_contract_version !== CATALOG_PAGE_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported catalog page contract version');
  }
  const request = parseCatalogPageRequestBinding(input.request, expectedRequest.contract_selection);
  if (!pageRequestsEqual(request, expectedRequest)) {
    throw new ContractValidationError('catalog page response does not match the exact caller-held request');
  }
  const publishedReadiness = validatePublishedReadiness(input.published_readiness, request.snapshot_id, expectedPlan);
  const totalCount = parseCount(input.total_count);
  const hasMore = booleanValue(input.has_more, 'catalog page has_more');
  if (!Array.isArray(input.rows) || input.rows.length > request.page_size || input.rows.length > MAX_PAGE_ROWS) {
    throw new ContractValidationError('catalog page rows exceed the caller-held page bound');
  }
  const rows = input.rows.map((entryInput): CatalogPageEntry<R> => {
    const entry = record(entryInput, 'catalog page entry');
    assertKnownFields(entry, ['sort_key', 'row'], 'catalog page entry');
    return { sort_key: parseSortKey(entry.sort_key), row: parseRow(entry.row, request.contract_selection) };
  });
  const pageKeys = rows.map((entry) => ({
    sort_key: entry.sort_key,
    entity_key: rowEntity(entry.row).external_ref.entity_key,
  }));
  strictlyIncreasing(pageKeys, comparePageKey, 'catalog page row keys', MAX_PAGE_ROWS);
  const entityKeys = new Set(pageKeys.map((key) => key.entity_key));
  if (entityKeys.size !== pageKeys.length) {
    throw new ContractValidationError('catalog page cannot repeat an entity under a different sort key');
  }
  if (request.after_cursor !== undefined && pageKeys[0] !== undefined) {
    const afterKey = {
      sort_key: request.after_cursor.last_sort_key,
      entity_key: request.after_cursor.last_entity_key,
    };
    if (comparePageKey(afterKey, pageKeys[0]) >= 0) {
      throw new ContractValidationError('catalog continuation rows do not begin after the caller-held cursor');
    }
  }
  if (
    totalCount.state === 'known' &&
    (totalCount.value < rows.length ||
      ((hasMore || request.after_cursor !== undefined) && totalCount.value <= rows.length))
  ) {
    throw new ContractValidationError('catalog total count is inconsistent with page progress');
  }
  const next = optional(input.next_continuation, (entry) =>
    parseCatalogContinuationRequest(entry, request.contract_selection),
  );
  if (hasMore !== (next !== undefined)) {
    throw new ContractValidationError('catalog next continuation must be present exactly when more rows exist');
  }
  if (next !== undefined) {
    const finalEntry = rows.at(-1);
    if (finalEntry === undefined) throw new ContractValidationError('an empty catalog page cannot continue');
    const finalRef = rowEntity(finalEntry.row);
    if (
      !snapshotsEqual(next.snapshot_id, request.snapshot_id) ||
      next.query_fingerprint !== request.query_fingerprint ||
      next.sort_spec_version !== request.sort_spec_version ||
      next.page_size !== request.page_size ||
      next.cursor.last_sort_key !== finalEntry.sort_key ||
      next.cursor.last_entity_key !== finalRef.external_ref.entity_key
    ) {
      throw new ContractValidationError('catalog next continuation is not bound to the canonical final row');
    }
  }
  return {
    catalog_page_contract_version: CATALOG_PAGE_CONTRACT_VERSION,
    request,
    published_readiness: publishedReadiness,
    total_count: totalCount,
    has_more: hasMore,
    rows,
    ...(next === undefined ? {} : { next_continuation: next }),
    additive_fields: parseAdditiveFields(
      input,
      [
        'catalog_page_contract_version',
        'request',
        'published_readiness',
        'total_count',
        'has_more',
        'rows',
        'next_continuation',
      ],
      request.contract_selection,
    ),
  };
}

export function parseCatalogProjectPage(
  value: unknown,
  expectedRequestInput: unknown,
  expectedPlanInput: unknown,
): CatalogProjectPage {
  return parsePage(
    value,
    expectedRequestInput,
    expectedPlanInput,
    'projects',
    parseProjectRow,
    (row) => row.project_ref,
  );
}

export function parseCatalogSessionPage(
  value: unknown,
  expectedRequestInput: unknown,
  expectedPlanInput: unknown,
): CatalogSessionPage {
  return parsePage(
    value,
    expectedRequestInput,
    expectedPlanInput,
    'sessions',
    parseSessionRow,
    (row) => row.session_ref,
  );
}

export function parseCatalogResolutionRequestBinding(
  value: unknown,
  expectedSelectionInput?: unknown,
): CatalogResolutionRequestBinding {
  const input = record(value, 'catalog resolution request binding');
  assertKnownFields(input, ['contract_selection', 'snapshot_id', 'external_ref'], 'catalog resolution request binding');
  const selection = parseCatalogQueryContractSelection(input.contract_selection);
  if (expectedSelectionInput !== undefined) {
    const expected = parseCatalogQueryContractSelection(expectedSelectionInput);
    if (!selectionsEqual(selection, expected)) {
      throw new ContractValidationError('catalog resolution request does not match the negotiated selection');
    }
  }
  const snapshot = parseSnapshot(input.snapshot_id);
  if (snapshot.pack_contract_version !== selection.contract_versions.query_pack_version) {
    throw new ContractValidationError('catalog resolution snapshot does not match the selected query pack');
  }
  return {
    contract_selection: selection,
    snapshot_id: snapshot,
    external_ref: parseExternalEntityRef(input.external_ref),
  };
}

function resolutionRequestsEqual(
  left: CatalogResolutionRequestBinding,
  right: CatalogResolutionRequestBinding,
): boolean {
  return (
    selectionsEqual(left.contract_selection, right.contract_selection) &&
    snapshotsEqual(left.snapshot_id, right.snapshot_id) &&
    externalRefsEqual(left.external_ref, right.external_ref)
  );
}

function parseLiveRow(value: unknown, selection: CatalogQueryContractSelection): CatalogPortableLiveRow {
  const input = record(value, 'catalog live row');
  assertKnownFields(input, ['kind', 'row'], 'catalog live row');
  switch (input.kind) {
    case 'project':
      return { kind: 'project', row: parseProjectRow(input.row, selection) };
    case 'session':
      return { kind: 'session', row: parseSessionRow(input.row, selection) };
    default:
      throw new ContractValidationError('catalog live row has unsupported kind');
  }
}

function parseResolution(value: unknown, request: CatalogResolutionRequestBinding): CatalogEntityResolution {
  const input = record(value, 'catalog entity resolution');
  const externalRef = parseExternalEntityRef(input.external_ref);
  if (!externalRefsEqual(externalRef, request.external_ref)) {
    throw new ContractValidationError('catalog resolution changed the requested external reference');
  }
  switch (input.state) {
    case 'live': {
      assertKnownFields(input, ['state', 'external_ref', 'row'], 'live catalog resolution');
      const row = parseLiveRow(input.row, request.contract_selection);
      const rowRef = row.kind === 'project' ? row.row.project_ref.external_ref : row.row.session_ref.external_ref;
      if (!externalRefsEqual(rowRef, externalRef)) {
        throw new ContractValidationError('live catalog resolution row has a different identity');
      }
      return { state: 'live', external_ref: externalRef, row };
    }
    case 'tombstoned':
      assertKnownFields(input, ['state', 'external_ref', 'provenance'], 'tombstoned catalog resolution');
      return {
        state: 'tombstoned',
        external_ref: externalRef,
        provenance: parseProvenance(input.provenance, 'catalog tombstone provenance'),
      };
    case 'superseded': {
      assertKnownFields(input, ['state', 'external_ref', 'target_refs', 'provenance'], 'superseded catalog resolution');
      if (!Array.isArray(input.target_refs) || input.target_refs.length === 0) {
        throw new ContractValidationError('superseded catalog resolution requires replacement targets');
      }
      if (input.target_refs.length > MAX_RESOLUTION_TARGETS) {
        throw new ContractValidationError(`superseded catalog targets exceeds ${MAX_RESOLUTION_TARGETS} entries`);
      }
      const targetRefs = input.target_refs.map(parseExternalEntityRef);
      strictlyIncreasing(
        targetRefs,
        (left, right) => compareOpaque(left.entity_key, right.entity_key),
        'superseded catalog targets',
        MAX_RESOLUTION_TARGETS,
      );
      if (targetRefs.some((target) => externalRefsEqual(target, externalRef))) {
        throw new ContractValidationError('superseded catalog target cannot reuse the prior identity');
      }
      return {
        state: 'superseded',
        external_ref: externalRef,
        target_refs: targetRefs,
        provenance: parseProvenance(input.provenance, 'superseded catalog provenance'),
      };
    }
    case 'unknown': {
      assertKnownFields(input, ['state', 'external_ref', 'reason'], 'unknown catalog resolution');
      const reasons = ['never_observed', 'retracted_pending_publication', 'related_identity_only'] as const;
      if (!reasons.includes(input.reason as (typeof reasons)[number])) {
        throw new ContractValidationError('catalog resolution has unsupported unknown reason');
      }
      return {
        state: 'unknown',
        external_ref: externalRef,
        reason: input.reason as (typeof reasons)[number],
      };
    }
    case 'typed_unknown': {
      assertKnownFields(input, ['state', 'external_ref', 'variant', 'payload'], 'typed-unknown catalog resolution');
      const variant = canonicalString(input.variant, 'catalog typed-unknown resolution variant');
      if (['live', 'tombstoned', 'superseded', 'unknown', 'typed_unknown'].includes(variant)) {
        throw new ContractValidationError('typed-unknown catalog resolution shadows a known state');
      }
      const payloadInput = record(input.payload, 'catalog typed-unknown resolution payload');
      return {
        state: 'typed_unknown',
        external_ref: externalRef,
        variant,
        payload: parseAdditiveFields(payloadInput, [], request.contract_selection),
      };
    }
    default:
      throw new ContractValidationError('catalog resolution has unsupported state');
  }
}

export function parseCatalogEntityResolutionResponse(
  value: unknown,
  expectedRequestInput: unknown,
): CatalogEntityResolutionResponse {
  const expectedRequest = parseCatalogResolutionRequestBinding(expectedRequestInput);
  const input = record(value, 'catalog entity resolution response');
  if (input.catalog_resolution_contract_version !== CATALOG_RESOLUTION_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported catalog resolution contract version');
  }
  const request = parseCatalogResolutionRequestBinding(input.request, expectedRequest.contract_selection);
  if (!resolutionRequestsEqual(request, expectedRequest)) {
    throw new ContractValidationError('catalog resolution response does not match the caller-held request');
  }
  return {
    catalog_resolution_contract_version: CATALOG_RESOLUTION_CONTRACT_VERSION,
    request,
    resolution: parseResolution(input.resolution, request),
    additive_fields: parseAdditiveFields(
      input,
      ['catalog_resolution_contract_version', 'request', 'resolution'],
      request.contract_selection,
    ),
  };
}

function continuationsEqual(left: CatalogContinuationRequest, right: CatalogContinuationRequest): boolean {
  return (
    left.catalog_continuation_request_contract_version === right.catalog_continuation_request_contract_version &&
    selectionsEqual(left.contract_selection, right.contract_selection) &&
    snapshotsEqual(left.snapshot_id, right.snapshot_id) &&
    left.query_fingerprint === right.query_fingerprint &&
    left.sort_spec_version === right.sort_spec_version &&
    left.page_size === right.page_size &&
    cursorsEqual(left.cursor, right.cursor)
  );
}

export function parseCatalogSnapshotExpired(
  value: unknown,
  expectedContinuationInput: unknown,
  expectedSelectionInput: unknown,
  expectedScopeInput: unknown,
): CatalogSnapshotExpired {
  const expectedSelection = parseCatalogQueryContractSelection(expectedSelectionInput);
  const expectedContinuation = parseCatalogContinuationRequest(expectedContinuationInput, expectedSelection);
  const expectedScope = parseCatalogCoverageScope(expectedScopeInput);
  const input = record(value, 'catalog snapshot-expiration response');
  if (input.catalog_snapshot_expiration_contract_version !== CATALOG_SNAPSHOT_EXPIRATION_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported catalog snapshot-expiration contract version');
  }
  const selection = parseCatalogQueryContractSelection(input.contract_selection);
  if (!selectionsEqual(selection, expectedSelection)) {
    throw new ContractValidationError('catalog expiration does not match the negotiated selection');
  }
  const scope = parseCatalogCoverageScope(input.scope);
  if (!scopesEqual(scope, expectedScope)) {
    throw new ContractValidationError('catalog expiration belongs to a different caller-held scope');
  }
  const request = parseCatalogContinuationRequest(input.request, expectedSelection);
  if (!continuationsEqual(request, expectedContinuation)) {
    throw new ContractValidationError('catalog expiration does not match the exact caller-held continuation');
  }
  const latestSnapshot = parseSnapshot(input.latest_snapshot);
  const expired = request.snapshot_id;
  if (
    latestSnapshot.pack_contract_version !== expired.pack_contract_version ||
    latestSnapshot.complete_commit <= expired.complete_commit ||
    latestSnapshot.readiness_epoch < expired.readiness_epoch ||
    (latestSnapshot.readiness_epoch === expired.readiness_epoch &&
      latestSnapshot.coverage_plan_id !== expired.coverage_plan_id)
  ) {
    throw new ContractValidationError('latest catalog snapshot is not newer in the same scope and pack lineage');
  }
  return {
    catalog_snapshot_expiration_contract_version: CATALOG_SNAPSHOT_EXPIRATION_CONTRACT_VERSION,
    contract_selection: selection,
    scope,
    request,
    latest_snapshot: latestSnapshot,
    additive_fields: parseAdditiveFields(
      input,
      ['catalog_snapshot_expiration_contract_version', 'contract_selection', 'scope', 'request', 'latest_snapshot'],
      selection,
    ),
  };
}
