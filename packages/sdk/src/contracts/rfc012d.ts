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
} from './rfc012a.js';

export const OBSERVATION_NEGOTIATION_CONTRACT_VERSION = 1 as const;
export const OBSERVATION_PROFILE_CONTRACT_VERSION = 1 as const;
export const OBSERVATION_ENVELOPE_CONTRACT_VERSION = 1 as const;
export const OBSERVATION_EVENT_CONTRACT_VERSION = 1 as const;
export const OBSERVATION_LIFECYCLE_CONTRACT_VERSION = 1 as const;
export const OBSERVATION_BASE_MODEL_MAJOR = 1 as const;

const MAX_VERSION_PREFERENCES = 16;
const MAX_FACT_FAMILIES = 64;

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
