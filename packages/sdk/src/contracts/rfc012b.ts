/** RFC 012B catalog query-pack negotiation and portable continuation wires.
 *
 * This module validates transport-neutral values only. It does not call the
 * native engine, retain snapshots, execute queries, or expose an N-API method.
 */

import {
  CONTRACT_VERSION_SELECTION_VERSION,
  ContractValidationError,
  EXTERNAL_ENTITY_REFERENCE_VERSION,
  parseContractVersionOffer,
  parseContractVersionRequest,
  parseContractVersionSelection,
  parseOpaqueContractReference,
  SEMANTIC_REFERENCE_CONTRACT_VERSION,
  selectContractVersions,
  SOURCE_COVERAGE_CONTRACT_VERSION,
  type ContractVersionOffer,
  type ContractVersionRequest,
  type ContractVersionSelection,
  type OpaqueContractReference,
} from './rfc012a.js';

export const CATALOG_QUERY_CONTRACT_VERSION = 1 as const;
export const CATALOG_QUERY_RESPONSE_CONTRACT_VERSION = 1 as const;
export const CATALOG_CONTINUATION_REQUEST_CONTRACT_VERSION = 1 as const;
export const CATALOG_TYPED_UNKNOWN_CONTRACT_VERSION = 1 as const;
export const CATALOG_QUERY_PACK_CONTRACT_VERSION = 1 as const;
export const CATALOG_BASE_MODEL_MAJOR = 1 as const;

const MAX_TYPED_UNKNOWN_PAYLOAD_BYTES = 64 * 1024;
const MAX_TYPED_UNKNOWN_DEPTH = 16;
const MAX_TYPED_UNKNOWN_NODES = 1_024;
const MAX_CONTINUATION_PAGE_SIZE = 1_000;
const MAX_IDENTIFIER_BYTES = 256;
const textEncoder = new TextEncoder();

type UnknownRecord = Record<string, unknown>;

export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
export type JsonObject = Record<string, JsonValue>;

export interface CatalogTypedUnknownCapability {
  typed_unknown_contract_version: number;
  preserves_unknown_fields: boolean;
  preserves_unknown_variants: boolean;
  max_payload_bytes: number;
}

export interface CatalogQueryContractRequest {
  catalog_query_contract_version: typeof CATALOG_QUERY_CONTRACT_VERSION;
  contract_versions: ContractVersionRequest;
  typed_unknown: CatalogTypedUnknownCapability;
}

export interface CatalogQueryContractOffer {
  catalog_query_contract_version: typeof CATALOG_QUERY_CONTRACT_VERSION;
  contract_versions: ContractVersionOffer;
  typed_unknown: CatalogTypedUnknownCapability;
}

export interface CatalogQueryContractSelection {
  catalog_query_contract_version: typeof CATALOG_QUERY_CONTRACT_VERSION;
  contract_versions: ContractVersionSelection;
  typed_unknown: CatalogTypedUnknownCapability;
}

export type CatalogQueryCompatibilityAxis =
  | 'base_model_major'
  | 'external_entity_reference_version'
  | 'semantic_revision_reference_version'
  | 'coverage_contract_version'
  | 'fact_family_version'
  | 'query_pack_version'
  | 'observation_contract_version'
  | 'typed_unknown_preservation';

export class IncompatibleCatalogContractError extends ContractValidationError {
  readonly axis: CatalogQueryCompatibilityAxis;

  constructor(axis: CatalogQueryCompatibilityAxis) {
    super(`IncompatibleCatalogContract: ${axis}`);
    this.name = 'IncompatibleCatalogContract';
    this.axis = axis;
  }
}

export type ParsedCatalogQueryContractResponse =
  | {
      kind: 'selected';
      selection: CatalogQueryContractSelection;
      additive_fields: JsonObject;
    }
  | {
      kind: 'typed_unknown';
      selection: CatalogQueryContractSelection;
      variant: string;
      payload: JsonObject;
    };

export interface CatalogSnapshotId {
  pack_contract_version: number;
  coverage_plan_id: OpaqueContractReference;
  readiness_epoch: number;
  complete_commit: number;
}

export interface CatalogCursor {
  cursor_contract_version: 1;
  snapshot_id: CatalogSnapshotId;
  query_fingerprint: OpaqueContractReference;
  sort_spec_version: number;
  last_sort_key: string;
  last_entity_key: OpaqueContractReference;
}

export interface CatalogContinuationRequest {
  catalog_continuation_request_contract_version: typeof CATALOG_CONTINUATION_REQUEST_CONTRACT_VERSION;
  contract_selection: CatalogQueryContractSelection;
  snapshot_id: CatalogSnapshotId;
  query_fingerprint: OpaqueContractReference;
  sort_spec_version: number;
  cursor: CatalogCursor;
  page_size: number;
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

function utf8Bytes(value: string): number {
  return textEncoder.encode(value).byteLength;
}

function canonicalString(value: unknown, label: string): string {
  if (
    typeof value !== 'string' ||
    value.length === 0 ||
    value.trim() !== value ||
    utf8Bytes(value) > MAX_IDENTIFIER_BYTES
  ) {
    throw new ContractValidationError(`${label} must be a non-empty bounded canonical string`);
  }
  return value;
}

function positiveInteger(value: unknown, label: string): number {
  if (!Number.isSafeInteger(value) || (value as number) <= 0) {
    throw new ContractValidationError(`${label} must be a positive safe integer`);
  }
  return value as number;
}

function booleanValue(value: unknown, label: string): boolean {
  if (typeof value !== 'boolean') throw new ContractValidationError(`${label} must be boolean`);
  return value;
}

function parseTypedUnknownCapability(value: unknown): CatalogTypedUnknownCapability {
  const input = record(value, 'catalog typed-unknown capability');
  const maxPayloadBytes = positiveInteger(input.max_payload_bytes, 'catalog typed-unknown payload bound');
  if (maxPayloadBytes > MAX_TYPED_UNKNOWN_PAYLOAD_BYTES) {
    throw new ContractValidationError(
      `catalog typed-unknown payload bound must be at most ${MAX_TYPED_UNKNOWN_PAYLOAD_BYTES} bytes`,
    );
  }
  return {
    typed_unknown_contract_version: positiveInteger(
      input.typed_unknown_contract_version,
      'catalog typed-unknown contract version',
    ),
    preserves_unknown_fields: booleanValue(input.preserves_unknown_fields, 'catalog unknown-field preservation'),
    preserves_unknown_variants: booleanValue(input.preserves_unknown_variants, 'catalog unknown-variant preservation'),
    max_payload_bytes: maxPayloadBytes,
  };
}

export function parseCatalogQueryContractRequest(value: unknown): CatalogQueryContractRequest {
  const input = record(value, 'catalog query contract request');
  if (input.catalog_query_contract_version !== CATALOG_QUERY_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported catalog query contract request version');
  }
  const contractVersions = parseContractVersionRequest(input.contract_versions);
  if (contractVersions.query_pack_versions === undefined) {
    throw new ContractValidationError('catalog query negotiation must explicitly request a query-pack version');
  }
  return {
    catalog_query_contract_version: CATALOG_QUERY_CONTRACT_VERSION,
    contract_versions: contractVersions,
    typed_unknown: parseTypedUnknownCapability(input.typed_unknown),
  };
}

export function parseCatalogQueryContractOffer(value: unknown): CatalogQueryContractOffer {
  const input = record(value, 'catalog query contract offer');
  if (input.catalog_query_contract_version !== CATALOG_QUERY_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported catalog query contract offer version');
  }
  return {
    catalog_query_contract_version: CATALOG_QUERY_CONTRACT_VERSION,
    contract_versions: parseContractVersionOffer(input.contract_versions),
    typed_unknown: parseTypedUnknownCapability(input.typed_unknown),
  };
}

function firstCommon(requested: number[], offered: number[]): number | undefined {
  return requested.find((version) => offered.includes(version));
}

function incompatible(axis: CatalogQueryCompatibilityAxis): never {
  throw new IncompatibleCatalogContractError(axis);
}

export function negotiateCatalogQueryContract(
  requestInput: unknown,
  offerInput: unknown,
): CatalogQueryContractSelection {
  const request = parseCatalogQueryContractRequest(requestInput);
  const offer = parseCatalogQueryContractOffer(offerInput);
  const requested = request.contract_versions;
  const offered = offer.contract_versions;

  if (
    requested.model_major !== CATALOG_BASE_MODEL_MAJOR ||
    offered.model_major !== CATALOG_BASE_MODEL_MAJOR ||
    requested.model_major !== offered.model_major
  ) {
    return incompatible('base_model_major');
  }
  if (
    requested.external_entity_reference_version !== EXTERNAL_ENTITY_REFERENCE_VERSION ||
    !offered.external_entity_reference_versions.includes(requested.external_entity_reference_version)
  ) {
    return incompatible('external_entity_reference_version');
  }
  if (
    requested.semantic_revision_reference_version !== SEMANTIC_REFERENCE_CONTRACT_VERSION ||
    !offered.semantic_revision_reference_versions.includes(requested.semantic_revision_reference_version)
  ) {
    return incompatible('semantic_revision_reference_version');
  }
  if (
    firstCommon(requested.coverage_contract_versions, offered.coverage_contract_versions) !==
    SOURCE_COVERAGE_CONTRACT_VERSION
  ) {
    return incompatible('coverage_contract_version');
  }
  for (const [family, versions] of Object.entries(requested.fact_family_versions)) {
    const offeredVersions = offered.fact_family_versions[family];
    if (offeredVersions === undefined || firstCommon(versions, offeredVersions) === undefined) {
      return incompatible('fact_family_version');
    }
  }
  const requestedQueryPacks = requested.query_pack_versions;
  if (requestedQueryPacks === undefined) {
    throw new ContractValidationError('catalog query negotiation requires query-pack versions');
  }
  if (firstCommon(requestedQueryPacks, offered.query_pack_versions) !== CATALOG_QUERY_PACK_CONTRACT_VERSION) {
    return incompatible('query_pack_version');
  }
  if (
    requested.observation_contract_versions !== undefined &&
    firstCommon(requested.observation_contract_versions, offered.observation_contract_versions) === undefined
  ) {
    return incompatible('observation_contract_version');
  }
  if (
    request.typed_unknown.typed_unknown_contract_version !== CATALOG_TYPED_UNKNOWN_CONTRACT_VERSION ||
    offer.typed_unknown.typed_unknown_contract_version !== CATALOG_TYPED_UNKNOWN_CONTRACT_VERSION ||
    !request.typed_unknown.preserves_unknown_fields ||
    !request.typed_unknown.preserves_unknown_variants ||
    !offer.typed_unknown.preserves_unknown_fields ||
    !offer.typed_unknown.preserves_unknown_variants
  ) {
    return incompatible('typed_unknown_preservation');
  }

  return parseCatalogQueryContractSelection({
    catalog_query_contract_version: CATALOG_QUERY_CONTRACT_VERSION,
    contract_versions: selectContractVersions(requested, offered),
    typed_unknown: {
      typed_unknown_contract_version: CATALOG_TYPED_UNKNOWN_CONTRACT_VERSION,
      preserves_unknown_fields: true,
      preserves_unknown_variants: true,
      max_payload_bytes: Math.min(request.typed_unknown.max_payload_bytes, offer.typed_unknown.max_payload_bytes),
    },
  });
}

export function parseCatalogQueryContractSelection(value: unknown): CatalogQueryContractSelection {
  const input = record(value, 'catalog query contract selection');
  if (input.catalog_query_contract_version !== CATALOG_QUERY_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported selected catalog query contract version');
  }
  const contracts = parseContractVersionSelection(input.contract_versions);
  const typedUnknown = parseTypedUnknownCapability(input.typed_unknown);
  for (const family of Object.keys(contracts.fact_family_versions)) {
    canonicalString(family, 'selected catalog fact family');
  }
  if (
    contracts.selection_contract_version !== CONTRACT_VERSION_SELECTION_VERSION ||
    contracts.model_major !== CATALOG_BASE_MODEL_MAJOR ||
    contracts.external_entity_reference_version !== EXTERNAL_ENTITY_REFERENCE_VERSION ||
    contracts.semantic_revision_reference_version !== SEMANTIC_REFERENCE_CONTRACT_VERSION ||
    contracts.coverage_contract_version !== SOURCE_COVERAGE_CONTRACT_VERSION ||
    contracts.query_pack_version !== CATALOG_QUERY_PACK_CONTRACT_VERSION
  ) {
    throw new ContractValidationError('selected catalog query contract has an incompatible base or query-pack version');
  }
  if (
    typedUnknown.typed_unknown_contract_version !== CATALOG_TYPED_UNKNOWN_CONTRACT_VERSION ||
    !typedUnknown.preserves_unknown_fields ||
    !typedUnknown.preserves_unknown_variants
  ) {
    throw new ContractValidationError(
      'selected catalog query contract must preserve bounded unknown fields and variants',
    );
  }
  return {
    catalog_query_contract_version: CATALOG_QUERY_CONTRACT_VERSION,
    contract_versions: contracts,
    typed_unknown: typedUnknown,
  };
}

function catalogQuerySelectionsEqual(
  left: CatalogQueryContractSelection,
  right: CatalogQueryContractSelection,
): boolean {
  const leftContracts = left.contract_versions;
  const rightContracts = right.contract_versions;
  const leftFamilies = Object.keys(leftContracts.fact_family_versions);
  const rightFamilies = Object.keys(rightContracts.fact_family_versions);
  return (
    left.catalog_query_contract_version === right.catalog_query_contract_version &&
    leftContracts.selection_contract_version === rightContracts.selection_contract_version &&
    leftContracts.model_major === rightContracts.model_major &&
    leftContracts.external_entity_reference_version === rightContracts.external_entity_reference_version &&
    leftContracts.semantic_revision_reference_version === rightContracts.semantic_revision_reference_version &&
    leftContracts.coverage_contract_version === rightContracts.coverage_contract_version &&
    leftContracts.query_pack_version === rightContracts.query_pack_version &&
    leftContracts.observation_contract_version === rightContracts.observation_contract_version &&
    leftFamilies.length === rightFamilies.length &&
    leftFamilies.every(
      (family) => leftContracts.fact_family_versions[family] === rightContracts.fact_family_versions[family],
    ) &&
    left.typed_unknown.typed_unknown_contract_version === right.typed_unknown.typed_unknown_contract_version &&
    left.typed_unknown.preserves_unknown_fields === right.typed_unknown.preserves_unknown_fields &&
    left.typed_unknown.preserves_unknown_variants === right.typed_unknown.preserves_unknown_variants &&
    left.typed_unknown.max_payload_bytes === right.typed_unknown.max_payload_bytes
  );
}

interface UnknownBudget {
  bytes: number;
  nodes: number;
  maxBytes: number;
}

function addBudgetBytes(budget: UnknownBudget, bytes: number): void {
  budget.bytes += bytes;
  if (!Number.isSafeInteger(budget.bytes) || budget.bytes > budget.maxBytes) {
    throw new ContractValidationError(
      `catalog typed-unknown payload exceeds the negotiated ${budget.maxBytes} byte bound`,
    );
  }
}

function addBudgetNode(budget: UnknownBudget): void {
  budget.nodes += 1;
  if (budget.nodes > MAX_TYPED_UNKNOWN_NODES) {
    throw new ContractValidationError(`catalog typed-unknown payload exceeds ${MAX_TYPED_UNKNOWN_NODES} nodes`);
  }
  addBudgetBytes(budget, 1);
}

function validateObjectKey(value: string, label: string): string {
  const key = canonicalString(value, label);
  if (key === '__proto__' || key === 'prototype' || key === 'constructor') {
    throw new ContractValidationError(`${label} uses a reserved object key`);
  }
  return key;
}

function cloneBoundedJson(value: unknown, depth: number, budget: UnknownBudget): JsonValue {
  if (depth > MAX_TYPED_UNKNOWN_DEPTH) {
    throw new ContractValidationError(`catalog typed-unknown payload exceeds depth ${MAX_TYPED_UNKNOWN_DEPTH}`);
  }
  addBudgetNode(budget);
  if (value === null) return null;
  if (typeof value === 'boolean') {
    addBudgetBytes(budget, 1);
    return value;
  }
  if (typeof value === 'number') {
    if (!Number.isSafeInteger(value)) {
      throw new ContractValidationError('catalog typed-unknown numbers must be JavaScript-safe integers');
    }
    addBudgetBytes(budget, 8);
    return value;
  }
  if (typeof value === 'string') {
    addBudgetBytes(budget, utf8Bytes(value));
    return value;
  }
  if (Array.isArray(value)) {
    return value.map((entry) => cloneBoundedJson(entry, depth + 1, budget));
  }
  if (typeof value === 'object') {
    const prototype = Object.getPrototypeOf(value);
    if (prototype !== Object.prototype && prototype !== null) {
      throw new ContractValidationError('catalog typed-unknown payload contains a non-JSON object');
    }
    const result: JsonObject = {};
    for (const [rawKey, entry] of Object.entries(value)) {
      const key = validateObjectKey(rawKey, 'catalog typed-unknown object key');
      addBudgetBytes(budget, utf8Bytes(key));
      result[key] = cloneBoundedJson(entry, depth + 1, budget);
    }
    return result;
  }
  throw new ContractValidationError('catalog typed-unknown payload contains a non-JSON value');
}

function parseUnknownFields(
  input: UnknownRecord,
  known: ReadonlySet<string>,
  selection: CatalogQueryContractSelection,
): JsonObject {
  const prototype = Object.getPrototypeOf(input);
  if (prototype !== Object.prototype && prototype !== null) {
    throw new ContractValidationError('catalog typed-unknown fields must be a plain JSON object');
  }
  const budget: UnknownBudget = { bytes: 0, nodes: 0, maxBytes: selection.typed_unknown.max_payload_bytes };
  addBudgetNode(budget);
  const result: JsonObject = {};
  for (const [rawKey, value] of Object.entries(input)) {
    if (known.has(rawKey)) continue;
    const key = validateObjectKey(rawKey, 'catalog additive field');
    addBudgetBytes(budget, utf8Bytes(key));
    result[key] = cloneBoundedJson(value, 1, budget);
  }
  return result;
}

const responseKnownFields = new Set(['catalog_query_response_contract_version', 'contract_selection', 'kind']);

export function parseCatalogQueryContractResponse(
  value: unknown,
  expectedSelectionInput: unknown,
): ParsedCatalogQueryContractResponse {
  const input = record(value, 'catalog query contract response');
  if (input.catalog_query_response_contract_version !== CATALOG_QUERY_RESPONSE_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported catalog query response contract version');
  }
  const expectedSelection = parseCatalogQueryContractSelection(expectedSelectionInput);
  const selection = parseCatalogQueryContractSelection(input.contract_selection);
  if (!catalogQuerySelectionsEqual(selection, expectedSelection)) {
    throw new ContractValidationError('catalog query response does not match the negotiated contract selection');
  }
  const variant = canonicalString(input.kind, 'catalog query response variant');
  const payload = parseUnknownFields(input, responseKnownFields, selection);
  if (variant === 'selected') {
    return { kind: 'selected', selection, additive_fields: payload };
  }
  return { kind: 'typed_unknown', selection, variant, payload };
}

export function serializeCatalogQueryContractResponse(response: ParsedCatalogQueryContractResponse): JsonObject {
  const selection = parseCatalogQueryContractSelection(response.selection);
  const rawPayload = response.kind === 'selected' ? response.additive_fields : response.payload;
  for (const key of Object.keys(rawPayload)) {
    if (responseKnownFields.has(key)) {
      throw new ContractValidationError(`catalog additive field cannot replace response contract field ${key}`);
    }
  }
  const payload = parseUnknownFields(rawPayload, new Set(), selection);
  const variant =
    response.kind === 'selected' ? 'selected' : canonicalString(response.variant, 'catalog query response variant');
  return {
    catalog_query_response_contract_version: CATALOG_QUERY_RESPONSE_CONTRACT_VERSION,
    contract_selection: selection as unknown as JsonValue,
    kind: variant,
    ...payload,
  };
}

function parseCatalogSnapshotId(value: unknown): CatalogSnapshotId {
  const input = record(value, 'catalog snapshot id');
  return {
    pack_contract_version: positiveInteger(input.pack_contract_version, 'catalog snapshot pack contract version'),
    coverage_plan_id: parseOpaqueContractReference(input.coverage_plan_id, 'catalog coverage plan id'),
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

function parseCatalogSortKey(value: unknown): string {
  if (typeof value !== 'string' || !/^v1:[A-Za-z0-9_-]+$/.test(value)) {
    throw new ContractValidationError('catalog sort key must be canonical unpadded base64url');
  }
  const payload = value.slice(3);
  const standard = payload.replace(/-/g, '+').replace(/_/g, '/');
  const padded = standard + '='.repeat((4 - (standard.length % 4)) % 4);
  let decoded: string;
  try {
    decoded = atob(padded);
  } catch {
    throw new ContractValidationError('catalog sort key must be canonical unpadded base64url');
  }
  if (decoded.length === 0 || decoded.length > 64 * 1024) {
    throw new ContractValidationError('catalog sort key has an unsupported decoded length');
  }
  const canonical = btoa(decoded).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  if (canonical !== payload) {
    throw new ContractValidationError('catalog sort key must be canonical unpadded base64url');
  }
  return value;
}

function parseCatalogCursor(value: unknown): CatalogCursor {
  const input = record(value, 'catalog cursor');
  if (input.cursor_contract_version !== 1) {
    throw new ContractValidationError('unsupported catalog cursor contract version');
  }
  return {
    cursor_contract_version: 1,
    snapshot_id: parseCatalogSnapshotId(input.snapshot_id),
    query_fingerprint: parseOpaqueContractReference(input.query_fingerprint, 'catalog query fingerprint'),
    sort_spec_version: positiveInteger(input.sort_spec_version, 'catalog cursor sort specification version'),
    last_sort_key: parseCatalogSortKey(input.last_sort_key),
    last_entity_key: parseOpaqueContractReference(input.last_entity_key, 'catalog cursor last entity key'),
  };
}

export function parseCatalogContinuationRequest(
  value: unknown,
  expectedSelectionInput: unknown,
): CatalogContinuationRequest {
  const input = record(value, 'catalog continuation request');
  if (input.catalog_continuation_request_contract_version !== CATALOG_CONTINUATION_REQUEST_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported catalog continuation request contract version');
  }
  const expectedSelection = parseCatalogQueryContractSelection(expectedSelectionInput);
  const contractSelection = parseCatalogQueryContractSelection(input.contract_selection);
  if (!catalogQuerySelectionsEqual(contractSelection, expectedSelection)) {
    throw new ContractValidationError('catalog continuation does not match the negotiated contract selection');
  }
  const snapshotId = parseCatalogSnapshotId(input.snapshot_id);
  const queryFingerprint = parseOpaqueContractReference(input.query_fingerprint, 'catalog query fingerprint');
  const sortSpecVersion = positiveInteger(input.sort_spec_version, 'catalog sort specification version');
  const cursor = parseCatalogCursor(input.cursor);
  const pageSize = positiveInteger(input.page_size, 'catalog continuation page size');
  if (pageSize > MAX_CONTINUATION_PAGE_SIZE) {
    throw new ContractValidationError(`catalog continuation page size must be at most ${MAX_CONTINUATION_PAGE_SIZE}`);
  }
  if (snapshotId.pack_contract_version !== contractSelection.contract_versions.query_pack_version) {
    throw new ContractValidationError('catalog continuation snapshot uses a different selected query pack');
  }
  if (!snapshotsEqual(cursor.snapshot_id, snapshotId)) {
    throw new ContractValidationError('catalog cursor is bound to a different retained snapshot');
  }
  if (cursor.query_fingerprint !== queryFingerprint) {
    throw new ContractValidationError('catalog cursor is bound to a different query fingerprint');
  }
  if (cursor.sort_spec_version !== sortSpecVersion) {
    throw new ContractValidationError('catalog cursor is bound to a different sort specification');
  }
  return {
    catalog_continuation_request_contract_version: CATALOG_CONTINUATION_REQUEST_CONTRACT_VERSION,
    contract_selection: contractSelection,
    snapshot_id: snapshotId,
    query_fingerprint: queryFingerprint,
    sort_spec_version: sortSpecVersion,
    cursor,
    page_size: pageSize,
  };
}
