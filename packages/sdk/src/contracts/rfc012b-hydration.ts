/** RFC 012B selected-session hydration command and scheduling-receipt wires.
 *
 * Hydration is an explicit command boundary, not a catalog query. This module
 * validates Rust-produced portable values against caller-held negotiated and
 * scheduling context; it does not read sources, schedule work, or expose the
 * native engine.
 */

import {
  ContractValidationError,
  parseExternalEntityRef,
  parseOpaqueContractReference,
  parseSemanticRevisionRef,
  type ExternalEntityRef,
  type OpaqueContractReference,
  type SemanticRevisionRef,
} from './rfc012a.js';
import {
  CATALOG_QUERY_PACK_CONTRACT_VERSION,
  parseCatalogQueryContractSelection,
  type CatalogQueryContractSelection,
  type CatalogSnapshotId,
} from './rfc012b.js';

export const CATALOG_HYDRATION_COMMAND_CONTRACT_VERSION = 1 as const;
export const CATALOG_HYDRATION_AUTHORIZATION_CONTRACT_VERSION = 1 as const;
export const CATALOG_HYDRATION_SCOPE_CONTRACT_VERSION = 1 as const;
export const CATALOG_SCHEDULING_RECEIPT_CONTRACT_VERSION = 1 as const;

const MAX_SCOPE_FACT_FAMILIES = 64;
const MAX_SCOPE_SOURCE_OBJECTS = 4_096;
const MAX_SCOPE_RECORDS = 1_000_000;
const MAX_SCOPE_BYTES = 256 * 1024 * 1024;
const MAX_PROVENANCE_REVISIONS = 64;
const MAX_RETRY_AFTER_MILLIS = 5 * 60 * 1_000;
const MAX_FAILURE_CODE_BYTES = 64;
const MAX_U32 = 0xffff_ffff;
const MAX_IDENTIFIER_BYTES = 256;
const textEncoder = new TextEncoder();

type UnknownRecord = Record<string, unknown>;

export interface CatalogHydrationEntityRef {
  kind: 'session';
  external_ref: ExternalEntityRef;
}

export interface CatalogHydrationCoverageSource {
  adapter_id: string;
  source_instance_key: OpaqueContractReference;
  support_release_id: string;
  catalog_declaration_digest: OpaqueContractReference;
  access_policy_digest: OpaqueContractReference;
}

export interface CatalogHydrationAttachHandoff {
  presentation_ref: CatalogHydrationEntityRef;
  member_refs: CatalogHydrationEntityRef[];
  relation_keys: OpaqueContractReference[];
  selected_base_session_ref: CatalogHydrationEntityRef;
  locator_claim_key: OpaqueContractReference;
}

export type CatalogHydrationLocatorKind = 'filesystem' | 'native_index' | 'repository' | 'opaque_native';
export type CatalogHydrationLocatorBasis =
  | 'native_project_index'
  | 'transcript_cwd'
  | 'session_directory'
  | 'rollout_header'
  | 'declared_derived_ancestor';
export type CatalogHydrationLocatorDisclosure = 'local_sensitive' | 'policy_shareable';

export interface CatalogHydrationLocatorAuthorization {
  hydration_authorization_contract_version: typeof CATALOG_HYDRATION_AUTHORIZATION_CONTRACT_VERSION;
  authorization_id: OpaqueContractReference;
  handoff: CatalogHydrationAttachHandoff;
  adapter_id: string;
  source_instance_key: OpaqueContractReference;
  support_release_id: string;
  catalog_declaration_digest: OpaqueContractReference;
  access_policy_digest: OpaqueContractReference;
  locator_source_generation: number;
  locator_kind: CatalogHydrationLocatorKind;
  locator_basis: CatalogHydrationLocatorBasis;
  locator_disclosure: CatalogHydrationLocatorDisclosure;
  locator_provenance: SemanticRevisionRef[];
}

export interface CatalogHydrationRequestedScope {
  hydration_scope_contract_version: typeof CATALOG_HYDRATION_SCOPE_CONTRACT_VERSION;
  fact_family_versions: Record<string, number>;
  max_source_objects_per_pass: number;
  max_records_per_pass: number;
  max_bytes_per_pass: number;
}

export interface CatalogHydrationCommandIdentity {
  request_key: OpaqueContractReference;
  command_id: OpaqueContractReference;
  coalescing_key: OpaqueContractReference;
}

export interface CatalogHydrationCommand extends CatalogHydrationCommandIdentity {
  hydration_command_contract_version: typeof CATALOG_HYDRATION_COMMAND_CONTRACT_VERSION;
  contract_selection: CatalogQueryContractSelection;
  snapshot_id: CatalogSnapshotId;
  source: CatalogHydrationCoverageSource;
  authorization: CatalogHydrationLocatorAuthorization;
  requested_scope: CatalogHydrationRequestedScope;
  reason: 'selected_session';
}

export interface CatalogHydrationCommandBinding extends CatalogHydrationCommandIdentity {
  selected_base_session_ref: CatalogHydrationEntityRef;
  snapshot_id: CatalogSnapshotId;
}

export interface CatalogHydrationCommandExpectedContext {
  identity: CatalogHydrationCommandIdentity;
  contract_selection: unknown;
  snapshot_id: unknown;
  source: unknown;
  authorization: unknown;
  requested_scope: unknown;
  reason: 'selected_session';
}

export type CatalogHydrationFailure =
  | { disposition: 'retryable'; code: string; retry_after_millis: number }
  | { disposition: 'terminal'; code: string; retry_after_millis: null };

export type CatalogHydrationSchedulingOutcome =
  | { state: 'accepted' }
  | { state: 'already_satisfied' }
  | {
      state: 'in_progress';
      active_command_id: OpaqueContractReference;
      active_receipt_id: OpaqueContractReference;
    }
  | { state: 'rejected'; failure: CatalogHydrationFailure };

export interface CatalogSchedulingReceipt {
  scheduling_receipt_contract_version: typeof CATALOG_SCHEDULING_RECEIPT_CONTRACT_VERSION;
  receipt_id: OpaqueContractReference;
  request_key: OpaqueContractReference;
  command_id: OpaqueContractReference;
  coalescing_key: OpaqueContractReference;
  selected_base_session_ref: CatalogHydrationEntityRef;
  snapshot_id: CatalogSnapshotId;
  attempt: number;
  prior_receipt_id: OpaqueContractReference | null;
  emitted_at_commit: number;
  outcome: CatalogHydrationSchedulingOutcome;
}

export interface CatalogHydrationActiveSchedule {
  command: CatalogHydrationCommandBinding;
  receipt: CatalogSchedulingReceipt;
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

function rejectUnknown(input: UnknownRecord, allowed: ReadonlySet<string>, label: string): void {
  for (const key of Object.keys(input)) {
    if (!allowed.has(key)) throw new ContractValidationError(`${label} contains unknown field ${key}`);
  }
}

function canonicalString(value: unknown, label: string): string {
  if (
    typeof value !== 'string' ||
    value.length === 0 ||
    value.trim() !== value ||
    textEncoder.encode(value).byteLength > MAX_IDENTIFIER_BYTES
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

function opaqueBytes(reference: OpaqueContractReference): Uint8Array {
  const payload = reference.slice(3).replace(/-/g, '+').replace(/_/g, '/');
  const decoded = atob(`${payload}=`);
  return Uint8Array.from(decoded, (character) => character.charCodeAt(0));
}

function compareOpaque(left: OpaqueContractReference, right: OpaqueContractReference): number {
  const leftBytes = opaqueBytes(left);
  const rightBytes = opaqueBytes(right);
  for (let index = 0; index < leftBytes.length; index += 1) {
    const difference = leftBytes[index]! - rightBytes[index]!;
    if (difference !== 0) return difference;
  }
  return 0;
}

function parseEntityRef(value: unknown, label: string): CatalogHydrationEntityRef {
  const input = record(value, label);
  rejectUnknown(input, new Set(['kind', 'external_ref']), label);
  if (input.kind !== 'session') throw new ContractValidationError(`${label} must be a base session reference`);
  return { kind: 'session', external_ref: parseExternalEntityRef(input.external_ref) };
}

export function parseCatalogHydrationEntityRef(value: unknown): CatalogHydrationEntityRef {
  return parseEntityRef(value, 'catalog hydration session reference');
}

function entityRefsEqual(left: CatalogHydrationEntityRef, right: CatalogHydrationEntityRef): boolean {
  return (
    left.kind === right.kind &&
    left.external_ref.external_entity_reference_version === right.external_ref.external_entity_reference_version &&
    left.external_ref.entity_key === right.external_ref.entity_key
  );
}

function snapshotsEqual(left: CatalogSnapshotId, right: CatalogSnapshotId): boolean {
  return (
    left.pack_contract_version === right.pack_contract_version &&
    left.coverage_plan_id === right.coverage_plan_id &&
    left.readiness_epoch === right.readiness_epoch &&
    left.complete_commit === right.complete_commit
  );
}

function parseSnapshot(value: unknown): CatalogSnapshotId {
  const input = record(value, 'catalog hydration snapshot');
  rejectUnknown(
    input,
    new Set(['pack_contract_version', 'coverage_plan_id', 'readiness_epoch', 'complete_commit']),
    'catalog hydration snapshot',
  );
  const snapshot = {
    pack_contract_version: positiveInteger(input.pack_contract_version, 'catalog hydration snapshot pack version'),
    coverage_plan_id: parseOpaqueContractReference(input.coverage_plan_id, 'catalog hydration coverage plan id'),
    readiness_epoch: positiveInteger(input.readiness_epoch, 'catalog hydration snapshot epoch'),
    complete_commit: positiveInteger(input.complete_commit, 'catalog hydration snapshot commit'),
  };
  if (snapshot.pack_contract_version !== CATALOG_QUERY_PACK_CONTRACT_VERSION) {
    throw new ContractValidationError('catalog hydration snapshot uses an unsupported query pack');
  }
  return snapshot;
}

function selectionsEqual(left: CatalogQueryContractSelection, right: CatalogQueryContractSelection): boolean {
  const leftContracts = left.contract_versions;
  const rightContracts = right.contract_versions;
  const leftFamilies = Object.keys(leftContracts.fact_family_versions).sort();
  const rightFamilies = Object.keys(rightContracts.fact_family_versions).sort();
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
      (family, index) =>
        family === rightFamilies[index] &&
        leftContracts.fact_family_versions[family] === rightContracts.fact_family_versions[family],
    ) &&
    left.typed_unknown.typed_unknown_contract_version === right.typed_unknown.typed_unknown_contract_version &&
    left.typed_unknown.preserves_unknown_fields === right.typed_unknown.preserves_unknown_fields &&
    left.typed_unknown.preserves_unknown_variants === right.typed_unknown.preserves_unknown_variants &&
    left.typed_unknown.max_payload_bytes === right.typed_unknown.max_payload_bytes
  );
}

function parseSource(value: unknown): CatalogHydrationCoverageSource {
  const input = record(value, 'catalog hydration coverage source');
  rejectUnknown(
    input,
    new Set([
      'adapter_id',
      'source_instance_key',
      'support_release_id',
      'catalog_declaration_digest',
      'access_policy_digest',
    ]),
    'catalog hydration coverage source',
  );
  return {
    adapter_id: canonicalString(input.adapter_id, 'catalog hydration adapter id'),
    source_instance_key: parseOpaqueContractReference(input.source_instance_key, 'catalog hydration source instance'),
    support_release_id: canonicalString(input.support_release_id, 'catalog hydration support release'),
    catalog_declaration_digest: parseOpaqueContractReference(
      input.catalog_declaration_digest,
      'catalog hydration declaration digest',
    ),
    access_policy_digest: parseOpaqueContractReference(
      input.access_policy_digest,
      'catalog hydration access-policy digest',
    ),
  };
}

function sourcesEqual(left: CatalogHydrationCoverageSource, right: CatalogHydrationCoverageSource): boolean {
  return (
    left.adapter_id === right.adapter_id &&
    left.source_instance_key === right.source_instance_key &&
    left.support_release_id === right.support_release_id &&
    left.catalog_declaration_digest === right.catalog_declaration_digest &&
    left.access_policy_digest === right.access_policy_digest
  );
}

function parseHandoff(value: unknown): CatalogHydrationAttachHandoff {
  const input = record(value, 'catalog hydration attach handoff');
  rejectUnknown(
    input,
    new Set(['presentation_ref', 'member_refs', 'relation_keys', 'selected_base_session_ref', 'locator_claim_key']),
    'catalog hydration attach handoff',
  );
  if (!Array.isArray(input.member_refs) || input.member_refs.length === 0 || input.member_refs.length > 4_096) {
    throw new ContractValidationError('catalog hydration handoff requires bounded base-session members');
  }
  if (!Array.isArray(input.relation_keys) || input.relation_keys.length > 4_096) {
    throw new ContractValidationError('catalog hydration handoff relation keys must be an array');
  }
  const presentationRef = parseEntityRef(input.presentation_ref, 'catalog hydration presentation reference');
  const selectedBaseSessionRef = parseEntityRef(
    input.selected_base_session_ref,
    'catalog hydration selected base session',
  );
  const memberRefs = input.member_refs.map((member, index) =>
    parseEntityRef(member, `catalog hydration member ${index}`),
  );
  const relationKeys = input.relation_keys.map((key, index) =>
    parseOpaqueContractReference(key, `catalog hydration relation key ${index}`),
  );
  for (let index = 1; index < memberRefs.length; index += 1) {
    const previous = memberRefs[index - 1]!;
    const current = memberRefs[index]!;
    if (compareOpaque(previous.external_ref.entity_key, current.external_ref.entity_key) >= 0) {
      throw new ContractValidationError('catalog hydration handoff members must be strictly canonical');
    }
  }
  for (let index = 1; index < relationKeys.length; index += 1) {
    if (compareOpaque(relationKeys[index - 1]!, relationKeys[index]!) >= 0) {
      throw new ContractValidationError('catalog hydration relation keys must be strictly canonical');
    }
  }
  if (
    !memberRefs.some((member) => entityRefsEqual(member, presentationRef)) ||
    !memberRefs.some((member) => entityRefsEqual(member, selectedBaseSessionRef)) ||
    (memberRefs.length > 1 && relationKeys.length === 0)
  ) {
    throw new ContractValidationError('catalog hydration must select a proven concrete base session');
  }
  return {
    presentation_ref: presentationRef,
    member_refs: memberRefs,
    relation_keys: relationKeys,
    selected_base_session_ref: selectedBaseSessionRef,
    locator_claim_key: parseOpaqueContractReference(input.locator_claim_key, 'catalog hydration locator claim key'),
  };
}

function parseAuthorization(value: unknown): CatalogHydrationLocatorAuthorization {
  const input = record(value, 'catalog hydration locator authorization');
  rejectUnknown(
    input,
    new Set([
      'hydration_authorization_contract_version',
      'authorization_id',
      'handoff',
      'adapter_id',
      'source_instance_key',
      'support_release_id',
      'catalog_declaration_digest',
      'access_policy_digest',
      'locator_source_generation',
      'locator_kind',
      'locator_basis',
      'locator_disclosure',
      'locator_provenance',
    ]),
    'catalog hydration locator authorization',
  );
  if (input.hydration_authorization_contract_version !== CATALOG_HYDRATION_AUTHORIZATION_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported catalog hydration authorization version');
  }
  const locatorKind = input.locator_kind;
  if (!['filesystem', 'native_index', 'repository', 'opaque_native'].includes(locatorKind as string)) {
    throw new ContractValidationError('catalog hydration authorization has an unsupported locator kind');
  }
  const locatorBasis = input.locator_basis;
  if (
    ![
      'native_project_index',
      'transcript_cwd',
      'session_directory',
      'rollout_header',
      'declared_derived_ancestor',
    ].includes(locatorBasis as string)
  ) {
    throw new ContractValidationError('catalog hydration authorization has an unsupported locator basis');
  }
  const locatorDisclosure = input.locator_disclosure;
  if (!['local_sensitive', 'policy_shareable'].includes(locatorDisclosure as string)) {
    throw new ContractValidationError('catalog hydration authorization has an invalid locator disclosure class');
  }
  if (
    !Array.isArray(input.locator_provenance) ||
    input.locator_provenance.length === 0 ||
    input.locator_provenance.length > MAX_PROVENANCE_REVISIONS
  ) {
    throw new ContractValidationError('catalog hydration authorization requires bounded semantic provenance');
  }
  const locatorProvenance = input.locator_provenance.map(parseSemanticRevisionRef);
  if (new Set(locatorProvenance.map((reference) => reference.fact_revision_id)).size !== locatorProvenance.length) {
    throw new ContractValidationError('catalog hydration authorization contains duplicate provenance');
  }
  for (let index = 1; index < locatorProvenance.length; index += 1) {
    if (
      compareOpaque(locatorProvenance[index - 1]!.fact_revision_id, locatorProvenance[index]!.fact_revision_id) >= 0
    ) {
      throw new ContractValidationError('catalog hydration authorization provenance must be strictly canonical');
    }
  }
  return {
    hydration_authorization_contract_version: CATALOG_HYDRATION_AUTHORIZATION_CONTRACT_VERSION,
    authorization_id: parseOpaqueContractReference(input.authorization_id, 'catalog hydration authorization id'),
    handoff: parseHandoff(input.handoff),
    adapter_id: canonicalString(input.adapter_id, 'catalog hydration authorization adapter'),
    source_instance_key: parseOpaqueContractReference(
      input.source_instance_key,
      'catalog hydration authorization source instance',
    ),
    support_release_id: canonicalString(input.support_release_id, 'catalog hydration authorization support release'),
    catalog_declaration_digest: parseOpaqueContractReference(
      input.catalog_declaration_digest,
      'catalog hydration authorization declaration digest',
    ),
    access_policy_digest: parseOpaqueContractReference(
      input.access_policy_digest,
      'catalog hydration authorization policy digest',
    ),
    locator_source_generation: positiveInteger(
      input.locator_source_generation,
      'catalog hydration locator source generation',
    ),
    locator_kind: locatorKind as CatalogHydrationLocatorKind,
    locator_basis: locatorBasis as CatalogHydrationLocatorBasis,
    locator_disclosure: locatorDisclosure as CatalogHydrationLocatorDisclosure,
    locator_provenance: locatorProvenance,
  };
}

function authorizationsEqual(
  left: CatalogHydrationLocatorAuthorization,
  right: CatalogHydrationLocatorAuthorization,
): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

function parseScope(value: unknown, selection: CatalogQueryContractSelection): CatalogHydrationRequestedScope {
  const input = record(value, 'catalog hydration requested scope');
  rejectUnknown(
    input,
    new Set([
      'hydration_scope_contract_version',
      'fact_family_versions',
      'max_source_objects_per_pass',
      'max_records_per_pass',
      'max_bytes_per_pass',
    ]),
    'catalog hydration requested scope',
  );
  if (input.hydration_scope_contract_version !== CATALOG_HYDRATION_SCOPE_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported catalog hydration scope version');
  }
  const families = record(input.fact_family_versions, 'catalog hydration fact-family scope');
  const entries = Object.entries(families);
  if (entries.length === 0 || entries.length > MAX_SCOPE_FACT_FAMILIES) {
    throw new ContractValidationError('catalog hydration scope requires bounded fact families');
  }
  const parsedEntries = entries.map(([family, rawVersion]): [string, number] => {
    canonicalString(family, 'catalog hydration fact family');
    if (family === '__proto__' || family === 'prototype' || family === 'constructor') {
      throw new ContractValidationError(`catalog hydration fact family ${family} is reserved`);
    }
    const version = positiveInteger(rawVersion, `catalog hydration ${family} version`);
    if (version > MAX_U32) {
      throw new ContractValidationError(`catalog hydration ${family} version exceeds u32`);
    }
    if (selection.contract_versions.fact_family_versions[family] !== version) {
      throw new ContractValidationError(`catalog hydration fact family ${family} is outside the negotiated selection`);
    }
    return [family, version];
  });
  const factFamilyVersions = Object.fromEntries(parsedEntries);
  const maxSourceObjects = positiveInteger(input.max_source_objects_per_pass, 'catalog hydration object bound');
  const maxRecords = positiveInteger(input.max_records_per_pass, 'catalog hydration record bound');
  const maxBytes = positiveInteger(input.max_bytes_per_pass, 'catalog hydration byte bound');
  if (maxSourceObjects > MAX_SCOPE_SOURCE_OBJECTS || maxRecords > MAX_SCOPE_RECORDS || maxBytes > MAX_SCOPE_BYTES) {
    throw new ContractValidationError('catalog hydration requested scope exceeds its hard bounds');
  }
  return {
    hydration_scope_contract_version: CATALOG_HYDRATION_SCOPE_CONTRACT_VERSION,
    fact_family_versions: factFamilyVersions,
    max_source_objects_per_pass: maxSourceObjects,
    max_records_per_pass: maxRecords,
    max_bytes_per_pass: maxBytes,
  };
}

function parseCommandIdentity(value: unknown): CatalogHydrationCommandIdentity {
  const input = record(value, 'catalog hydration command identity');
  return {
    request_key: parseOpaqueContractReference(input.request_key, 'catalog hydration request key'),
    command_id: parseOpaqueContractReference(input.command_id, 'catalog hydration command id'),
    coalescing_key: parseOpaqueContractReference(input.coalescing_key, 'catalog hydration coalescing key'),
  };
}

function identitiesEqual(left: CatalogHydrationCommandIdentity, right: CatalogHydrationCommandIdentity): boolean {
  return (
    left.request_key === right.request_key &&
    left.command_id === right.command_id &&
    left.coalescing_key === right.coalescing_key
  );
}

function scopesEqual(left: CatalogHydrationRequestedScope, right: CatalogHydrationRequestedScope): boolean {
  const leftFamilies = Object.keys(left.fact_family_versions).sort();
  const rightFamilies = Object.keys(right.fact_family_versions).sort();
  return (
    left.hydration_scope_contract_version === right.hydration_scope_contract_version &&
    left.max_source_objects_per_pass === right.max_source_objects_per_pass &&
    left.max_records_per_pass === right.max_records_per_pass &&
    left.max_bytes_per_pass === right.max_bytes_per_pass &&
    leftFamilies.length === rightFamilies.length &&
    leftFamilies.every(
      (family, index) =>
        family === rightFamilies[index] && left.fact_family_versions[family] === right.fact_family_versions[family],
    )
  );
}

export function parseCatalogHydrationCommand(
  value: unknown,
  expectedContext: CatalogHydrationCommandExpectedContext,
): CatalogHydrationCommand {
  const input = record(value, 'catalog hydration command');
  rejectUnknown(
    input,
    new Set([
      'hydration_command_contract_version',
      'request_key',
      'command_id',
      'coalescing_key',
      'contract_selection',
      'snapshot_id',
      'source',
      'authorization',
      'requested_scope',
      'reason',
    ]),
    'catalog hydration command',
  );
  if (input.hydration_command_contract_version !== CATALOG_HYDRATION_COMMAND_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported catalog hydration command version');
  }
  const identity = parseCommandIdentity(input);
  const expectedIdentity = parseCommandIdentity(expectedContext.identity);
  const contractSelection = parseCatalogQueryContractSelection(input.contract_selection);
  const expectedSelection = parseCatalogQueryContractSelection(expectedContext.contract_selection);
  const snapshotId = parseSnapshot(input.snapshot_id);
  const expectedSnapshot = parseSnapshot(expectedContext.snapshot_id);
  const source = parseSource(input.source);
  const expectedSource = parseSource(expectedContext.source);
  const authorization = parseAuthorization(input.authorization);
  const expectedAuthorization = parseAuthorization(expectedContext.authorization);
  const requestedScope = parseScope(input.requested_scope, contractSelection);
  const expectedScope = parseScope(expectedContext.requested_scope, expectedSelection);
  if (
    !identitiesEqual(identity, expectedIdentity) ||
    !selectionsEqual(contractSelection, expectedSelection) ||
    !snapshotsEqual(snapshotId, expectedSnapshot) ||
    !sourcesEqual(source, expectedSource) ||
    !authorizationsEqual(authorization, expectedAuthorization) ||
    !scopesEqual(requestedScope, expectedScope) ||
    input.reason !== expectedContext.reason
  ) {
    throw new ContractValidationError('catalog hydration command does not match its retained authority context');
  }
  if (
    snapshotId.coverage_plan_id !== expectedSnapshot.coverage_plan_id ||
    snapshotId.pack_contract_version !== contractSelection.contract_versions.query_pack_version ||
    !sourcesEqual(source, {
      adapter_id: authorization.adapter_id,
      source_instance_key: authorization.source_instance_key,
      support_release_id: authorization.support_release_id,
      catalog_declaration_digest: authorization.catalog_declaration_digest,
      access_policy_digest: authorization.access_policy_digest,
    })
  ) {
    throw new ContractValidationError('catalog hydration command has inconsistent source or contract authority');
  }
  if (input.reason !== 'selected_session') {
    throw new ContractValidationError('catalog hydration command has an unsupported reason');
  }
  return {
    hydration_command_contract_version: CATALOG_HYDRATION_COMMAND_CONTRACT_VERSION,
    ...identity,
    contract_selection: contractSelection,
    snapshot_id: snapshotId,
    source,
    authorization,
    requested_scope: requestedScope,
    reason: 'selected_session',
  };
}

/**
 * Strictly parses a native-produced command and its internal bindings. This
 * does not replace caller-held request/snapshot/plan validation.
 */
export function parseCatalogHydrationCommandShape(value: unknown): CatalogHydrationCommand {
  const input = record(value, 'catalog hydration command');
  return parseCatalogHydrationCommand(input, {
    identity: parseCommandIdentity(input),
    contract_selection: input.contract_selection,
    snapshot_id: input.snapshot_id,
    source: input.source,
    authorization: input.authorization,
    requested_scope: input.requested_scope,
    reason: 'selected_session',
  });
}

export function catalogHydrationCommandBinding(command: CatalogHydrationCommand): CatalogHydrationCommandBinding {
  return {
    request_key: command.request_key,
    command_id: command.command_id,
    coalescing_key: command.coalescing_key,
    selected_base_session_ref: command.authorization.handoff.selected_base_session_ref,
    snapshot_id: command.snapshot_id,
  };
}

export function catalogHydrationCommandsCoalesce(
  left: CatalogHydrationCommandBinding,
  right: CatalogHydrationCommandBinding,
): boolean {
  return left.coalescing_key === right.coalescing_key;
}

export function parseCatalogHydrationCommandBinding(value: unknown): CatalogHydrationCommandBinding {
  const input = record(value, 'catalog hydration command binding');
  rejectUnknown(
    input,
    new Set(['request_key', 'command_id', 'coalescing_key', 'selected_base_session_ref', 'snapshot_id']),
    'catalog hydration command binding',
  );
  return {
    ...parseCommandIdentity(input),
    selected_base_session_ref: parseEntityRef(
      input.selected_base_session_ref,
      'catalog hydration binding selected session',
    ),
    snapshot_id: parseSnapshot(input.snapshot_id),
  };
}

function parseFailure(value: unknown): CatalogHydrationFailure {
  const input = record(value, 'catalog hydration scheduling failure');
  rejectUnknown(input, new Set(['disposition', 'code', 'retry_after_millis']), 'catalog hydration scheduling failure');
  const code = canonicalString(input.code, 'catalog hydration failure code');
  if (textEncoder.encode(code).byteLength > MAX_FAILURE_CODE_BYTES || !/^[a-z][a-z0-9_]*$/.test(code)) {
    throw new ContractValidationError('catalog hydration failure code must be a bounded lowercase ASCII machine code');
  }
  if (input.disposition === 'retryable') {
    const retryAfter = positiveInteger(input.retry_after_millis, 'catalog hydration retry delay');
    if (retryAfter > MAX_RETRY_AFTER_MILLIS) {
      throw new ContractValidationError('catalog hydration retry delay exceeds its hard bound');
    }
    return { disposition: 'retryable', code, retry_after_millis: retryAfter };
  }
  if (input.disposition === 'terminal' && input.retry_after_millis === null) {
    return { disposition: 'terminal', code, retry_after_millis: null };
  }
  throw new ContractValidationError('catalog hydration failure has inconsistent retry disposition');
}

function parseOutcome(value: unknown): CatalogHydrationSchedulingOutcome {
  const input = record(value, 'catalog hydration scheduling outcome');
  if (input.state === 'accepted' || input.state === 'already_satisfied') {
    rejectUnknown(input, new Set(['state']), 'catalog hydration scheduling outcome');
    return { state: input.state };
  }
  if (input.state === 'in_progress') {
    rejectUnknown(
      input,
      new Set(['state', 'active_command_id', 'active_receipt_id']),
      'catalog hydration scheduling outcome',
    );
    return {
      state: 'in_progress',
      active_command_id: parseOpaqueContractReference(input.active_command_id, 'active hydration command id'),
      active_receipt_id: parseOpaqueContractReference(input.active_receipt_id, 'active scheduling receipt id'),
    };
  }
  if (input.state === 'rejected') {
    rejectUnknown(input, new Set(['state', 'failure']), 'catalog hydration scheduling outcome');
    return { state: 'rejected', failure: parseFailure(input.failure) };
  }
  throw new ContractValidationError('catalog hydration scheduling outcome has an unsupported state');
}

function retryable(outcome: CatalogHydrationSchedulingOutcome): boolean {
  return outcome.state === 'rejected' && outcome.failure.disposition === 'retryable';
}

function terminal(outcome: CatalogHydrationSchedulingOutcome): boolean {
  return (
    outcome.state === 'already_satisfied' ||
    (outcome.state === 'rejected' && outcome.failure.disposition === 'terminal')
  );
}

function receiptMatchesCommand(receipt: CatalogSchedulingReceipt, command: CatalogHydrationCommandBinding): boolean {
  return (
    receipt.request_key === command.request_key &&
    receipt.command_id === command.command_id &&
    receipt.coalescing_key === command.coalescing_key &&
    entityRefsEqual(receipt.selected_base_session_ref, command.selected_base_session_ref) &&
    snapshotsEqual(receipt.snapshot_id, command.snapshot_id)
  );
}

/**
 * Strictly parses a receipt and binds it to one command without claiming
 * prior-receipt or active-coalescing lineage.
 */
export function parseCatalogSchedulingReceiptShape(
  value: unknown,
  expectedCommandInput: unknown,
): CatalogSchedulingReceipt {
  const input = record(value, 'catalog scheduling receipt');
  rejectUnknown(
    input,
    new Set([
      'scheduling_receipt_contract_version',
      'receipt_id',
      'request_key',
      'command_id',
      'coalescing_key',
      'selected_base_session_ref',
      'snapshot_id',
      'attempt',
      'prior_receipt_id',
      'emitted_at_commit',
      'outcome',
    ]),
    'catalog scheduling receipt',
  );
  if (input.scheduling_receipt_contract_version !== CATALOG_SCHEDULING_RECEIPT_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported catalog scheduling receipt version');
  }
  const command = parseCatalogHydrationCommandBinding(expectedCommandInput);
  const attempt = positiveInteger(input.attempt, 'catalog scheduling receipt attempt');
  if (attempt > MAX_U32) {
    throw new ContractValidationError('catalog scheduling receipt attempt exceeds u32');
  }
  const priorReceiptId =
    input.prior_receipt_id === null
      ? null
      : parseOpaqueContractReference(input.prior_receipt_id, 'prior scheduling receipt id');
  const receipt: CatalogSchedulingReceipt = {
    scheduling_receipt_contract_version: CATALOG_SCHEDULING_RECEIPT_CONTRACT_VERSION,
    receipt_id: parseOpaqueContractReference(input.receipt_id, 'catalog scheduling receipt id'),
    ...parseCommandIdentity(input),
    selected_base_session_ref: parseEntityRef(input.selected_base_session_ref, 'receipt selected base session'),
    snapshot_id: parseSnapshot(input.snapshot_id),
    attempt,
    prior_receipt_id: priorReceiptId,
    emitted_at_commit: positiveInteger(input.emitted_at_commit, 'catalog scheduling receipt commit'),
    outcome: parseOutcome(input.outcome),
  };
  if (!receiptMatchesCommand(receipt, command) || receipt.emitted_at_commit < receipt.snapshot_id.complete_commit) {
    throw new ContractValidationError('catalog scheduling receipt does not match its hydration command');
  }
  return receipt;
}

export function parseCatalogHydrationActiveScheduleShape(value: unknown): CatalogHydrationActiveSchedule {
  const input = record(value, 'active catalog hydration schedule');
  rejectUnknown(input, new Set(['command', 'receipt']), 'active catalog hydration schedule');
  const command = parseCatalogHydrationCommandBinding(input.command);
  const receipt = parseCatalogSchedulingReceiptShape(input.receipt, command);
  if (receipt.outcome.state !== 'accepted') {
    throw new ContractValidationError('active catalog hydration schedule requires an accepted receipt');
  }
  return { command, receipt };
}

export function parseCatalogSchedulingReceipt(
  value: unknown,
  expectedCommandInput: unknown,
  expectedPrior: CatalogSchedulingReceipt | null,
  expectedActive: CatalogHydrationActiveSchedule | null,
): CatalogSchedulingReceipt {
  const command = parseCatalogHydrationCommandBinding(expectedCommandInput);
  const receipt = parseCatalogSchedulingReceiptShape(value, command);
  if (expectedPrior === null) {
    if (receipt.prior_receipt_id !== null || receipt.attempt !== 1) {
      throw new ContractValidationError('initial catalog scheduling receipt cannot claim prior lineage');
    }
  } else {
    if (
      !receiptMatchesCommand(expectedPrior, command) ||
      receipt.prior_receipt_id !== expectedPrior.receipt_id ||
      receipt.emitted_at_commit < expectedPrior.emitted_at_commit ||
      terminal(expectedPrior.outcome)
    ) {
      throw new ContractValidationError('catalog scheduling receipt has impossible or terminal prior lineage');
    }
    if (retryable(expectedPrior.outcome) && expectedPrior.attempt === MAX_U32) {
      throw new ContractValidationError('catalog hydration receipt attempt overflow');
    }
    const expectedAttempt = expectedPrior.attempt + (retryable(expectedPrior.outcome) ? 1 : 0);
    if (receipt.attempt !== expectedAttempt) {
      throw new ContractValidationError('catalog scheduling receipt attempt does not follow prior outcome');
    }
    if (
      !retryable(expectedPrior.outcome) &&
      !(
        (expectedPrior.outcome.state === 'accepted' || expectedPrior.outcome.state === 'in_progress') &&
        (receipt.outcome.state === 'already_satisfied' || receipt.outcome.state === 'rejected')
      )
    ) {
      throw new ContractValidationError('catalog scheduling receipt outcome cannot follow its prior state');
    }
  }
  if (receipt.outcome.state === 'in_progress') {
    const activeCommand = expectedActive === null ? null : parseCatalogHydrationCommandBinding(expectedActive.command);
    const activeReceipt = expectedActive?.receipt ?? null;
    if (
      activeCommand === null ||
      activeReceipt === null ||
      !receiptMatchesCommand(activeReceipt, activeCommand) ||
      activeReceipt.outcome.state !== 'accepted' ||
      activeCommand.command_id === receipt.command_id ||
      activeCommand.coalescing_key !== receipt.coalescing_key ||
      !snapshotsEqual(activeCommand.snapshot_id, receipt.snapshot_id) ||
      !entityRefsEqual(activeCommand.selected_base_session_ref, receipt.selected_base_session_ref) ||
      receipt.emitted_at_commit < activeReceipt.emitted_at_commit ||
      receipt.outcome.active_command_id !== activeCommand.command_id ||
      receipt.outcome.active_receipt_id !== activeReceipt.receipt_id
    ) {
      throw new ContractValidationError('catalog in-progress receipt does not bind an accepted coalesced command');
    }
  } else if (expectedActive !== null) {
    throw new ContractValidationError('only an in-progress receipt may carry active coalescing context');
  }
  return receipt;
}
