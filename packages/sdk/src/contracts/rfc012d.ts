/** RFC 012D portable observation contract negotiation.
 *
 * This module selects exact transport contracts only. It does not create an
 * observer, access native sources, or define the still-incomplete event union.
 */

import {
  CONTRACT_VERSION_SELECTION_VERSION,
  ContractValidationError,
  EXTERNAL_ENTITY_REFERENCE_VERSION,
  parseContractVersionOffer,
  parseContractVersionRequest,
  parseContractVersionSelection,
  SEMANTIC_REFERENCE_CONTRACT_VERSION,
  selectContractVersions,
  SOURCE_COVERAGE_CONTRACT_VERSION,
  type ContractVersionOffer,
  type ContractVersionRequest,
  type ContractVersionSelection,
  type ContractCompleteness,
  type CompatibilityClass,
} from './rfc012a.js';

export const OBSERVATION_NEGOTIATION_CONTRACT_VERSION = 1 as const;
export const OBSERVATION_PROFILE_CONTRACT_VERSION = 1 as const;
export const OBSERVATION_ENVELOPE_CONTRACT_VERSION = 1 as const;
export const OBSERVATION_EVENT_CONTRACT_VERSION = 1 as const;
export const OBSERVATION_LIFECYCLE_CONTRACT_VERSION = 1 as const;
export const OBSERVATION_BASE_MODEL_MAJOR = 1 as const;

const MAX_VERSION_PREFERENCES = 16;
const MAX_FACT_FAMILIES = 64;
const MAX_CAPABILITY_FAMILIES = 64;
const MAX_FAMILY_IDENTIFIER_BYTES = 128;
const MAX_SUPPORT_RELEASE_ID_BYTES = 256;
const textEncoder = new TextEncoder();

type UnknownRecord = Record<string, unknown>;

export interface ObservationContractRequest {
  observation_negotiation_contract_version: typeof OBSERVATION_NEGOTIATION_CONTRACT_VERSION;
  contract_versions: ContractVersionRequest;
  envelope_contract_versions: number[];
  event_contract_versions: number[];
  lifecycle_contract_versions: number[];
}

export interface ObservationContractOffer {
  observation_negotiation_contract_version: typeof OBSERVATION_NEGOTIATION_CONTRACT_VERSION;
  contract_versions: ContractVersionOffer;
  envelope_contract_versions: number[];
  event_contract_versions: number[];
  lifecycle_contract_versions: number[];
}

export interface ObservationContractSelection {
  observation_negotiation_contract_version: typeof OBSERVATION_NEGOTIATION_CONTRACT_VERSION;
  contract_versions: ContractVersionSelection;
  envelope_contract_version: typeof OBSERVATION_ENVELOPE_CONTRACT_VERSION;
  event_contract_version: typeof OBSERVATION_EVENT_CONTRACT_VERSION;
  lifecycle_contract_version: typeof OBSERVATION_LIFECYCLE_CONTRACT_VERSION;
}

export type ObservationCompatibilityAxis =
  | 'base_model_major'
  | 'external_entity_reference_version'
  | 'semantic_revision_reference_version'
  | 'coverage_contract_version'
  | 'fact_family_version'
  | 'observation_profile_version'
  | 'envelope_contract_version'
  | 'event_contract_version'
  | 'lifecycle_contract_version';

export class IncompatibleObservationContractError extends ContractValidationError {
  readonly axis: ObservationCompatibilityAxis;

  constructor(axis: ObservationCompatibilityAxis) {
    super(`IncompatibleObservationContract: ${axis}`);
    this.name = 'IncompatibleObservationContract';
    this.axis = axis;
  }
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

function versionPreferences(value: unknown, label: string, requireNonempty = true): number[] {
  if (!Array.isArray(value)) throw new ContractValidationError(`${label} must be an array`);
  if ((requireNonempty && value.length === 0) || value.length > MAX_VERSION_PREFERENCES) {
    throw new ContractValidationError(
      `${label} must contain ${requireNonempty ? '1' : '0'}..=${MAX_VERSION_PREFERENCES} versions`,
    );
  }
  const versions = value.map((entry) => {
    if (!Number.isInteger(entry) || (entry as number) <= 0 || (entry as number) > 0xffff_ffff) {
      throw new ContractValidationError(`${label} contains an invalid u32 version`);
    }
    return entry as number;
  });
  if (new Set(versions).size !== versions.length) {
    throw new ContractValidationError(`${label} contains a duplicate version`);
  }
  return versions;
}

function validateFactFamilies(families: Record<string, number[]>, label: string): void {
  const entries = Object.entries(families);
  if (entries.length === 0 || entries.length > MAX_FACT_FAMILIES) {
    throw new ContractValidationError(`${label} requires 1..=${MAX_FACT_FAMILIES} fact families`);
  }
  for (const [family, versions] of entries) {
    validateFamilyIdentifier(family);
    versionPreferences(versions, `${label} fact-family versions for ${family}`);
  }
}

function validateFamilyIdentifier(family: string): void {
  if (!/^[a-z0-9][a-z0-9._-]*$/.test(family) || new TextEncoder().encode(family).byteLength > 128) {
    throw new ContractValidationError('observation fact-family identifier is not canonical');
  }
}

function validateSelectedFamilies(families: Record<string, number>): void {
  const entries = Object.entries(families);
  if (entries.length === 0 || entries.length > MAX_FACT_FAMILIES) {
    throw new ContractValidationError('observation selection has an invalid fact-family set');
  }
  for (const [family, version] of entries) {
    if (!Number.isInteger(version) || version <= 0 || version > 0xffff_ffff) {
      throw new ContractValidationError('observation selection has an invalid fact-family set');
    }
    validateFamilyIdentifier(family);
  }
}

export function parseObservationContractRequest(value: unknown): ObservationContractRequest {
  const input = record(value, 'observation contract request');
  assertKnownFields(
    input,
    [
      'observation_negotiation_contract_version',
      'contract_versions',
      'envelope_contract_versions',
      'event_contract_versions',
      'lifecycle_contract_versions',
    ],
    'observation contract request',
  );
  if (input.observation_negotiation_contract_version !== OBSERVATION_NEGOTIATION_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported observation negotiation request version');
  }
  const contractVersions = parseContractVersionRequest(input.contract_versions);
  if (contractVersions.query_pack_versions !== undefined) {
    throw new ContractValidationError('observation negotiation cannot request query-pack authority');
  }
  if (contractVersions.observation_contract_versions === undefined) {
    throw new ContractValidationError('observation negotiation requires observation-profile versions');
  }
  versionPreferences(contractVersions.coverage_contract_versions, 'requested coverage contract versions');
  versionPreferences(contractVersions.observation_contract_versions, 'requested observation profile versions');
  validateFactFamilies(contractVersions.fact_family_versions, 'requested observation');
  return {
    observation_negotiation_contract_version: OBSERVATION_NEGOTIATION_CONTRACT_VERSION,
    contract_versions: contractVersions,
    envelope_contract_versions: versionPreferences(
      input.envelope_contract_versions,
      'requested observation envelope versions',
    ),
    event_contract_versions: versionPreferences(input.event_contract_versions, 'requested observation event versions'),
    lifecycle_contract_versions: versionPreferences(
      input.lifecycle_contract_versions,
      'requested observation lifecycle versions',
    ),
  };
}

export function parseObservationContractOffer(value: unknown): ObservationContractOffer {
  const input = record(value, 'observation contract offer');
  assertKnownFields(
    input,
    [
      'observation_negotiation_contract_version',
      'contract_versions',
      'envelope_contract_versions',
      'event_contract_versions',
      'lifecycle_contract_versions',
    ],
    'observation contract offer',
  );
  if (input.observation_negotiation_contract_version !== OBSERVATION_NEGOTIATION_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported observation negotiation offer version');
  }
  const contractVersions = parseContractVersionOffer(input.contract_versions);
  if (contractVersions.query_pack_versions.length !== 0) {
    throw new ContractValidationError('observation negotiation offer cannot grant query-pack authority');
  }
  versionPreferences(contractVersions.external_entity_reference_versions, 'offered external entity reference versions');
  versionPreferences(
    contractVersions.semantic_revision_reference_versions,
    'offered semantic revision reference versions',
  );
  versionPreferences(contractVersions.coverage_contract_versions, 'offered coverage contract versions');
  versionPreferences(contractVersions.query_pack_versions, 'offered query pack versions', false);
  versionPreferences(contractVersions.observation_contract_versions, 'offered observation profile versions');
  validateFactFamilies(contractVersions.fact_family_versions, 'offered observation');
  return {
    observation_negotiation_contract_version: OBSERVATION_NEGOTIATION_CONTRACT_VERSION,
    contract_versions: contractVersions,
    envelope_contract_versions: versionPreferences(
      input.envelope_contract_versions,
      'offered observation envelope versions',
    ),
    event_contract_versions: versionPreferences(input.event_contract_versions, 'offered observation event versions'),
    lifecycle_contract_versions: versionPreferences(
      input.lifecycle_contract_versions,
      'offered observation lifecycle versions',
    ),
  };
}

function parseObservationContractSelectionShape(value: unknown): ObservationContractSelection {
  const input = record(value, 'observation contract selection');
  assertKnownFields(
    input,
    [
      'observation_negotiation_contract_version',
      'contract_versions',
      'envelope_contract_version',
      'event_contract_version',
      'lifecycle_contract_version',
    ],
    'observation contract selection',
  );
  const contractVersions = parseContractVersionSelection(input.contract_versions);
  if (
    input.observation_negotiation_contract_version !== OBSERVATION_NEGOTIATION_CONTRACT_VERSION ||
    contractVersions.selection_contract_version !== CONTRACT_VERSION_SELECTION_VERSION ||
    contractVersions.model_major !== OBSERVATION_BASE_MODEL_MAJOR ||
    contractVersions.external_entity_reference_version !== EXTERNAL_ENTITY_REFERENCE_VERSION ||
    contractVersions.semantic_revision_reference_version !== SEMANTIC_REFERENCE_CONTRACT_VERSION ||
    contractVersions.coverage_contract_version !== SOURCE_COVERAGE_CONTRACT_VERSION ||
    contractVersions.query_pack_version !== null ||
    contractVersions.observation_contract_version !== OBSERVATION_PROFILE_CONTRACT_VERSION ||
    input.envelope_contract_version !== OBSERVATION_ENVELOPE_CONTRACT_VERSION ||
    input.event_contract_version !== OBSERVATION_EVENT_CONTRACT_VERSION ||
    input.lifecycle_contract_version !== OBSERVATION_LIFECYCLE_CONTRACT_VERSION
  ) {
    throw new ContractValidationError('observation selection does not match the exact v1 contract profile');
  }
  validateSelectedFamilies(contractVersions.fact_family_versions);
  return {
    observation_negotiation_contract_version: OBSERVATION_NEGOTIATION_CONTRACT_VERSION,
    contract_versions: contractVersions,
    envelope_contract_version: OBSERVATION_ENVELOPE_CONTRACT_VERSION,
    event_contract_version: OBSERVATION_EVENT_CONTRACT_VERSION,
    lifecycle_contract_version: OBSERVATION_LIFECYCLE_CONTRACT_VERSION,
  };
}

function observationSelectionsEqual(left: ObservationContractSelection, right: ObservationContractSelection): boolean {
  const leftContracts = left.contract_versions;
  const rightContracts = right.contract_versions;
  const leftFamilies = Object.keys(leftContracts.fact_family_versions).sort();
  const rightFamilies = Object.keys(rightContracts.fact_family_versions).sort();
  return (
    left.observation_negotiation_contract_version === right.observation_negotiation_contract_version &&
    left.envelope_contract_version === right.envelope_contract_version &&
    left.event_contract_version === right.event_contract_version &&
    left.lifecycle_contract_version === right.lifecycle_contract_version &&
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
    )
  );
}

/**
 * Consumes a wire selection only when it exactly matches the result of the
 * caller-held request and host offer. This prevents a shape-valid selection
 * from adding, dropping, or downgrading a fact family.
 */
export function parseObservationContractSelection(
  value: unknown,
  requestInput: unknown,
  offerInput: unknown,
): ObservationContractSelection {
  const selection = parseObservationContractSelectionShape(value);
  const expected = negotiateObservationContract(requestInput, offerInput);
  if (!observationSelectionsEqual(selection, expected)) {
    throw new ContractValidationError('observation selection does not match the exact negotiated result');
  }
  return selection;
}

/** Consumes a repeated selection only when it equals caller-held negotiation state. */
export function parseObservationContractSelectionForExpected(
  value: unknown,
  expectedInput: unknown,
): ObservationContractSelection {
  const selection = parseObservationContractSelectionShape(value);
  const expected = parseObservationContractSelectionShape(expectedInput);
  if (!observationSelectionsEqual(selection, expected)) {
    throw new ContractValidationError('observation selection does not match the caller-held selection');
  }
  return selection;
}

function firstCommon(requested: number[], offered: number[]): number | undefined {
  return requested.find((version) => offered.includes(version));
}

function incompatible(axis: ObservationCompatibilityAxis): never {
  throw new IncompatibleObservationContractError(axis);
}

export function negotiateObservationContract(requestInput: unknown, offerInput: unknown): ObservationContractSelection {
  const request = parseObservationContractRequest(requestInput);
  const offer = parseObservationContractOffer(offerInput);
  const requested = request.contract_versions;
  const offered = offer.contract_versions;

  if (
    requested.model_major !== OBSERVATION_BASE_MODEL_MAJOR ||
    offered.model_major !== OBSERVATION_BASE_MODEL_MAJOR ||
    requested.model_major !== offered.model_major
  ) {
    incompatible('base_model_major');
  }
  if (
    requested.external_entity_reference_version !== EXTERNAL_ENTITY_REFERENCE_VERSION ||
    !offered.external_entity_reference_versions.includes(requested.external_entity_reference_version)
  ) {
    incompatible('external_entity_reference_version');
  }
  if (
    requested.semantic_revision_reference_version !== SEMANTIC_REFERENCE_CONTRACT_VERSION ||
    !offered.semantic_revision_reference_versions.includes(requested.semantic_revision_reference_version)
  ) {
    incompatible('semantic_revision_reference_version');
  }
  if (
    firstCommon(requested.coverage_contract_versions, offered.coverage_contract_versions) !==
    SOURCE_COVERAGE_CONTRACT_VERSION
  ) {
    incompatible('coverage_contract_version');
  }
  for (const [family, requestedVersions] of Object.entries(requested.fact_family_versions)) {
    const offeredVersions = offered.fact_family_versions[family];
    if (offeredVersions === undefined || firstCommon(requestedVersions, offeredVersions) === undefined) {
      incompatible('fact_family_version');
    }
  }
  if (
    firstCommon(requested.observation_contract_versions!, offered.observation_contract_versions) !==
    OBSERVATION_PROFILE_CONTRACT_VERSION
  ) {
    incompatible('observation_profile_version');
  }
  if (
    firstCommon(request.envelope_contract_versions, offer.envelope_contract_versions) !==
    OBSERVATION_ENVELOPE_CONTRACT_VERSION
  ) {
    incompatible('envelope_contract_version');
  }
  if (
    firstCommon(request.event_contract_versions, offer.event_contract_versions) !== OBSERVATION_EVENT_CONTRACT_VERSION
  ) {
    incompatible('event_contract_version');
  }
  if (
    firstCommon(request.lifecycle_contract_versions, offer.lifecycle_contract_versions) !==
    OBSERVATION_LIFECYCLE_CONTRACT_VERSION
  ) {
    incompatible('lifecycle_contract_version');
  }

  return parseObservationContractSelectionShape({
    observation_negotiation_contract_version: OBSERVATION_NEGOTIATION_CONTRACT_VERSION,
    contract_versions: selectContractVersions(requested, offered),
    envelope_contract_version: OBSERVATION_ENVELOPE_CONTRACT_VERSION,
    event_contract_version: OBSERVATION_EVENT_CONTRACT_VERSION,
    lifecycle_contract_version: OBSERVATION_LIFECYCLE_CONTRACT_VERSION,
  });
}

export const OBSERVATION_CAPABILITIES_CONTRACT_VERSION = 1 as const;

export type ObservationCapabilityStatus = 'supported' | 'degraded' | 'unsupported';
export type ObservationCapabilityQuality = 'exact' | 'qualified' | 'unavailable';
export type ObservationCapabilityExpectedTiming = 'bootstrap_and_live' | 'never';
export type ObservationCapabilityLimitation =
  | 'scope_bound'
  | 'coverage_reported_separately'
  | 'range_supported_native_version'
  | 'not_negotiated';
export type ObservationCapabilitySupportEvidence = 'exact_promoted_release' | 'range_supported_release';

export type ObservationCapabilityEvidence =
  | {
      kind: 'promoted_support_release';
      support_release_id: string;
      support: ObservationCapabilitySupportEvidence;
    }
  | {
      kind: 'host_offer_not_selected';
      offered_versions: number[];
    };

export interface ObservationFactFamilyCapability {
  fact_family: string;
  selected_version?: number;
  status: ObservationCapabilityStatus;
  evidence: ObservationCapabilityEvidence;
  quality: ObservationCapabilityQuality;
  expected_timing: ObservationCapabilityExpectedTiming;
  /** Capability expectation only; current readiness remains coverage-owned. */
  expected_completeness: ContractCompleteness;
  limitations: ObservationCapabilityLimitation[];
}

export interface ObservationCapabilities {
  observation_capabilities_contract_version: typeof OBSERVATION_CAPABILITIES_CONTRACT_VERSION;
  selection: ObservationContractSelection;
  fact_families: ObservationFactFamilyCapability[];
}

const CAPABILITY_LIMITATION_ORDER: Record<ObservationCapabilityLimitation, number> = {
  scope_bound: 0,
  coverage_reported_separately: 1,
  range_supported_native_version: 2,
  not_negotiated: 3,
};

function u32Version(value: unknown, label: string): number {
  if (!Number.isInteger(value) || (value as number) <= 0 || (value as number) > 0xffff_ffff) {
    throw new ContractValidationError(`${label} must be a positive u32 version`);
  }
  return value as number;
}

function parseCapabilityStatus(value: unknown): ObservationCapabilityStatus {
  if (value !== 'supported' && value !== 'degraded' && value !== 'unsupported') {
    throw new ContractValidationError('observation capability status is invalid');
  }
  return value;
}

function parseCapabilityQuality(value: unknown): ObservationCapabilityQuality {
  if (value !== 'exact' && value !== 'qualified' && value !== 'unavailable') {
    throw new ContractValidationError('observation capability quality is invalid');
  }
  return value;
}

function parseCapabilityTiming(value: unknown): ObservationCapabilityExpectedTiming {
  if (value !== 'bootstrap_and_live' && value !== 'never') {
    throw new ContractValidationError('observation capability expected timing is invalid');
  }
  return value;
}

function parseCapabilityCompleteness(value: unknown): ContractCompleteness {
  if (value !== 'complete' && value !== 'partial' && value !== 'unknown') {
    throw new ContractValidationError('observation capability expected completeness is invalid');
  }
  return value;
}

function parseCapabilityLimitations(value: unknown): ObservationCapabilityLimitation[] {
  if (!Array.isArray(value) || value.length === 0 || value.length > 4) {
    throw new ContractValidationError('observation capability limitations must be nonempty and bounded');
  }
  const limitations = value.map((entry: unknown): ObservationCapabilityLimitation => {
    if (
      entry !== 'scope_bound' &&
      entry !== 'coverage_reported_separately' &&
      entry !== 'range_supported_native_version' &&
      entry !== 'not_negotiated'
    ) {
      throw new ContractValidationError('observation capability limitation is invalid');
    }
    return entry;
  });
  for (let index = 1; index < limitations.length; index += 1) {
    if (CAPABILITY_LIMITATION_ORDER[limitations[index - 1]!] >= CAPABILITY_LIMITATION_ORDER[limitations[index]!]) {
      throw new ContractValidationError('observation capability limitations must be strictly sorted and unique');
    }
  }
  return limitations;
}

function parseCapabilityFamilyIdentifier(value: unknown): string {
  if (
    typeof value !== 'string' ||
    !/^[a-z0-9][a-z0-9._-]*$/.test(value) ||
    textEncoder.encode(value).byteLength > MAX_FAMILY_IDENTIFIER_BYTES
  ) {
    throw new ContractValidationError('observation capability family is not a canonical bounded identifier');
  }
  return value;
}

function parseSupportReleaseId(value: unknown): string {
  if (
    typeof value !== 'string' ||
    !/^[a-z0-9][a-z0-9._@-]*$/.test(value) ||
    textEncoder.encode(value).byteLength > MAX_SUPPORT_RELEASE_ID_BYTES
  ) {
    throw new ContractValidationError('observation support release id is not a canonical bounded identifier');
  }
  return value;
}

function parseCapabilityEvidence(value: unknown): ObservationCapabilityEvidence {
  const input = record(value, 'observation capability evidence');
  if (input.kind === 'promoted_support_release') {
    assertKnownFields(input, ['kind', 'support_release_id', 'support'], 'promoted observation capability evidence');
    if (input.support !== 'exact_promoted_release' && input.support !== 'range_supported_release') {
      throw new ContractValidationError('observation capability support evidence is invalid');
    }
    return {
      kind: 'promoted_support_release',
      support_release_id: parseSupportReleaseId(input.support_release_id),
      support: input.support,
    };
  }
  if (input.kind === 'host_offer_not_selected') {
    assertKnownFields(input, ['kind', 'offered_versions'], 'unselected observation capability evidence');
    return {
      kind: 'host_offer_not_selected',
      offered_versions: versionPreferences(input.offered_versions, 'offered capability versions'),
    };
  }
  throw new ContractValidationError('observation capability evidence kind is invalid');
}

function parseFactFamilyCapability(value: unknown): ObservationFactFamilyCapability {
  const input = record(value, 'observation fact-family capability');
  assertKnownFields(
    input,
    [
      'fact_family',
      'selected_version',
      'status',
      'evidence',
      'quality',
      'expected_timing',
      'expected_completeness',
      'limitations',
    ],
    'observation fact-family capability',
  );
  return {
    fact_family: parseCapabilityFamilyIdentifier(input.fact_family),
    ...(input.selected_version === undefined
      ? {}
      : { selected_version: u32Version(input.selected_version, 'selected observation capability version') }),
    status: parseCapabilityStatus(input.status),
    evidence: parseCapabilityEvidence(input.evidence),
    quality: parseCapabilityQuality(input.quality),
    expected_timing: parseCapabilityTiming(input.expected_timing),
    expected_completeness: parseCapabilityCompleteness(input.expected_completeness),
    limitations: parseCapabilityLimitations(input.limitations),
  };
}

function validateSelectedCapability(capability: ObservationFactFamilyCapability): void {
  if (capability.evidence.kind !== 'promoted_support_release') {
    throw new ContractValidationError('selected capability requires promoted support-release evidence');
  }
  if (
    capability.expected_timing !== 'bootstrap_and_live' ||
    !capability.limitations.includes('scope_bound') ||
    !capability.limitations.includes('coverage_reported_separately')
  ) {
    throw new ContractValidationError('selected capability is missing its scope, coverage, or timing qualification');
  }
  if (capability.evidence.support === 'exact_promoted_release') {
    if (
      capability.status !== 'supported' ||
      capability.quality !== 'exact' ||
      capability.expected_completeness !== 'complete' ||
      capability.limitations.includes('range_supported_native_version')
    ) {
      throw new ContractValidationError('selected capability status does not match exact support evidence');
    }
  } else if (
    capability.status !== 'degraded' ||
    capability.quality !== 'qualified' ||
    capability.expected_completeness !== 'partial' ||
    !capability.limitations.includes('range_supported_native_version')
  ) {
    throw new ContractValidationError('selected capability status does not match range support evidence');
  }
}

function validateUnselectedCapability(capability: ObservationFactFamilyCapability): void {
  if (
    capability.evidence.kind !== 'host_offer_not_selected' ||
    capability.status !== 'unsupported' ||
    capability.quality !== 'unavailable' ||
    capability.expected_timing !== 'never' ||
    capability.expected_completeness !== 'unknown' ||
    capability.limitations.length !== 1 ||
    capability.limitations[0] !== 'not_negotiated'
  ) {
    throw new ContractValidationError('unselected capability must remain explicitly unsupported');
  }
}

/**
 * Parses a capability report only for the exact caller-held observation
 * selection. Static capability completeness never replaces source coverage.
 */
export function parseObservationCapabilities(
  value: unknown,
  expectedSelectionInput: unknown,
  expectedOfferInput: unknown,
  expectedCompatibility: CompatibilityClass,
  expectedSupportReleaseIdInput: unknown,
): ObservationCapabilities {
  const input = record(value, 'observation capabilities');
  assertKnownFields(
    input,
    ['observation_capabilities_contract_version', 'selection', 'fact_families'],
    'observation capabilities',
  );
  if (input.observation_capabilities_contract_version !== OBSERVATION_CAPABILITIES_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported observation capabilities contract version');
  }
  const selection = parseObservationContractSelectionForExpected(input.selection, expectedSelectionInput);
  const offer = parseObservationContractOffer(expectedOfferInput);
  if (expectedCompatibility !== 'ExactSupported' && expectedCompatibility !== 'RangeSupported') {
    throw new ContractValidationError('observation capabilities require authorized support context');
  }
  const expectedSupportReleaseId = parseSupportReleaseId(expectedSupportReleaseIdInput);
  const selectedContracts = selection.contract_versions;
  const offeredContracts = offer.contract_versions;
  const selectionIsOffered =
    selectedContracts.model_major === offeredContracts.model_major &&
    offeredContracts.external_entity_reference_versions.includes(selectedContracts.external_entity_reference_version) &&
    offeredContracts.semantic_revision_reference_versions.includes(
      selectedContracts.semantic_revision_reference_version,
    ) &&
    offeredContracts.coverage_contract_versions.includes(selectedContracts.coverage_contract_version) &&
    Object.entries(selectedContracts.fact_family_versions).every(([family, version]) =>
      offeredContracts.fact_family_versions[family]?.includes(version),
    ) &&
    selectedContracts.query_pack_version === null &&
    selectedContracts.observation_contract_version !== null &&
    offeredContracts.observation_contract_versions.includes(selectedContracts.observation_contract_version) &&
    offer.envelope_contract_versions.includes(selection.envelope_contract_version) &&
    offer.event_contract_versions.includes(selection.event_contract_version) &&
    offer.lifecycle_contract_versions.includes(selection.lifecycle_contract_version);
  if (!selectionIsOffered) {
    throw new ContractValidationError('observation selection is not contained in the caller-held host offer');
  }
  if (
    !Array.isArray(input.fact_families) ||
    input.fact_families.length === 0 ||
    input.fact_families.length > MAX_CAPABILITY_FAMILIES
  ) {
    throw new ContractValidationError(`observation capabilities require 1..=${MAX_CAPABILITY_FAMILIES} family reports`);
  }
  const factFamilies = input.fact_families.map(parseFactFamilyCapability);
  const selected = selection.contract_versions.fact_family_versions;
  const offered = offer.contract_versions.fact_family_versions;
  const offeredFamilies = Object.keys(offered).sort();
  if (
    factFamilies.length !== offeredFamilies.length ||
    factFamilies.some((capability, index) => capability.fact_family !== offeredFamilies[index])
  ) {
    throw new ContractValidationError('observation capability families do not match the caller-held host offer');
  }
  const observedSelected = new Set<string>();
  let previousFamily: string | undefined;
  let attachmentSupport: string | undefined;
  for (const capability of factFamilies) {
    if (previousFamily !== undefined && previousFamily >= capability.fact_family) {
      throw new ContractValidationError('observation capability families must be strictly sorted and unique');
    }
    previousFamily = capability.fact_family;
    if (capability.selected_version !== undefined) {
      if (
        selected[capability.fact_family] !== capability.selected_version ||
        observedSelected.has(capability.fact_family)
      ) {
        throw new ContractValidationError('selected capability does not match the negotiated family version');
      }
      observedSelected.add(capability.fact_family);
      validateSelectedCapability(capability);
      const evidence = capability.evidence;
      if (evidence.kind !== 'promoted_support_release') throw new ContractValidationError('invalid selected evidence');
      const expectedSupport =
        expectedCompatibility === 'ExactSupported' ? 'exact_promoted_release' : 'range_supported_release';
      if (evidence.support !== expectedSupport || evidence.support_release_id !== expectedSupportReleaseId) {
        throw new ContractValidationError(
          'selected capability evidence does not match the caller-held support context',
        );
      }
      const supportKey = `${evidence.support}\u0000${evidence.support_release_id}`;
      if (attachmentSupport !== undefined && attachmentSupport !== supportKey) {
        throw new ContractValidationError('selected capabilities must share one support-release evidence source');
      }
      attachmentSupport = supportKey;
    } else {
      validateUnselectedCapability(capability);
      const evidence = capability.evidence;
      if (
        evidence.kind !== 'host_offer_not_selected' ||
        JSON.stringify(evidence.offered_versions) !== JSON.stringify(offered[capability.fact_family])
      ) {
        throw new ContractValidationError('unselected capability evidence does not match the caller-held host offer');
      }
    }
  }
  if (observedSelected.size !== Object.keys(selected).length) {
    throw new ContractValidationError('observation capabilities omit a negotiated fact family');
  }
  return {
    observation_capabilities_contract_version: OBSERVATION_CAPABILITIES_CONTRACT_VERSION,
    selection,
    fact_families: factFamilies,
  };
}
