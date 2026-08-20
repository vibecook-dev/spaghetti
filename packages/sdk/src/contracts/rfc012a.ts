/** RFC 012A v1 wire validation and coverage comparison.
 *
 * Rust derives opaque references. Portable consumers validate and compare
 * them but never decode, reorder, or synthesize native cursor values.
 */

export const EXTERNAL_ENTITY_REFERENCE_VERSION = 1 as const;
export const SEMANTIC_REFERENCE_CONTRACT_VERSION = 1 as const;
export const SOURCE_COVERAGE_CONTRACT_VERSION = 1 as const;
export const SOURCE_COVERAGE_SET_CONTRACT_VERSION = 1 as const;
export const SUPPORT_SELECTION_CONTRACT_VERSION = 1 as const;
export const CONTRACT_VERSION_SELECTION_VERSION = 1 as const;
export const ACCESS_REQUEST_CONTRACT_VERSION = 1 as const;
export const ACCESS_REPORT_RETRIEVAL_CONTRACT_VERSION = 1 as const;

import {
  assertNoUnpairedUtf16Surrogates,
  assertSemanticFixtureGraph,
  ContractValidationError,
  hasSurroundingRustWhitespace,
  preflightSemanticFixtureJson,
} from './rfc012-semantic-json.js';

export { ContractValidationError } from './rfc012-semantic-json.js';

const MAX_U32 = 0xffff_ffff;
const MAX_IDENTIFIER_BYTES = 256;
const MAX_COVERAGE_POINTS_PER_SET = 250_000;
const MAX_COVERAGE_ABSENCES_PER_SET = 250_000;
const MAX_COVERAGE_ERRORS_PER_SET = 4_096;
const MAX_COVERAGE_UNAVAILABLE_REASON_BYTES = 1_024;
const MAX_COVERAGE_ERROR_CODE_BYTES = 64;
const UTF8_ENCODER = new TextEncoder();

export type SupportReleaseStatus = 'candidate' | 'promoted' | 'retired';
export type SupportCapabilityTopology = 'catalog' | 'durable' | 'scoped';
export type SupportCapabilityLevel = 'supported' | 'degraded' | 'unsupported';

export interface SupportCapabilityDeclaration {
  capability_id: string;
  topology: SupportCapabilityTopology;
  level: SupportCapabilityLevel;
}

export interface ArtifactVersionRange {
  minimum: string;
  minimum_inclusive: boolean;
  maximum: string;
  maximum_inclusive: boolean;
}

export interface ArtifactCompatibilityDeclaration {
  family: string;
  platforms: string[];
  exact_versions: string[];
  ranges: ArtifactVersionRange[];
  required_markers: string[];
  forward_catalog_only: boolean;
}

export interface SupportReleaseDescriptor {
  support_release_id: string;
  status: SupportReleaseStatus;
  capabilities: SupportCapabilityDeclaration[];
  artifact_compatibility: ArtifactCompatibilityDeclaration;
}

export interface NativeArtifactProbe {
  family: string;
  platform: string;
  version: string | null;
  markers: string[];
  contradictory_markers: boolean;
}

export type CompatibilityClass = 'ExactSupported' | 'RangeSupported' | 'RecognizedUnverified' | 'UnknownOrIncompatible';

export type CompatibilityReason =
  | 'exact_promoted_version'
  | 'fixture_backed_range'
  | 'promoted_forward_catalog_only'
  | 'no_matching_promoted_release'
  | 'required_native_marker_absent'
  | 'platform_not_declared'
  | 'unrecognized_artifact_family'
  | 'contradictory_native_markers'
  | 'ambiguous_promoted_release';

export interface OperationPermissions {
  version_probe: boolean;
  catalog: boolean;
  durable: boolean;
  scoped_observation: boolean;
  bounded_drift: boolean;
}

export interface CompatibilityDecision {
  support_selection_contract_version: typeof SUPPORT_SELECTION_CONTRACT_VERSION;
  compatibility_class: CompatibilityClass;
  support_release_id: string | null;
  reason: CompatibilityReason;
  permissions: OperationPermissions;
}

export interface ContractVersionRequest {
  selection_contract_version: typeof CONTRACT_VERSION_SELECTION_VERSION;
  model_major: number;
  external_entity_reference_version: number;
  semantic_revision_reference_version: number;
  coverage_contract_versions: number[];
  fact_family_versions: Record<string, number[]>;
  query_pack_versions?: number[];
  observation_contract_versions?: number[];
}

export interface ContractVersionOffer {
  selection_contract_version: typeof CONTRACT_VERSION_SELECTION_VERSION;
  model_major: number;
  external_entity_reference_versions: number[];
  semantic_revision_reference_versions: number[];
  coverage_contract_versions: number[];
  fact_family_versions: Record<string, number[]>;
  query_pack_versions: number[];
  observation_contract_versions: number[];
}

export interface ContractVersionSelection {
  selection_contract_version: typeof CONTRACT_VERSION_SELECTION_VERSION;
  model_major: number;
  external_entity_reference_version: number;
  semantic_revision_reference_version: number;
  coverage_contract_version: number;
  fact_family_versions: Record<string, number>;
  query_pack_version: number | null;
  observation_contract_version: number | null;
}

declare const opaqueReferenceBrand: unique symbol;
export type OpaqueContractReference = string & {
  readonly [opaqueReferenceBrand]: true;
};

export interface ExternalEntityRef {
  external_entity_reference_version: typeof EXTERNAL_ENTITY_REFERENCE_VERSION;
  entity_key: OpaqueContractReference;
}

export interface SemanticRevisionRef {
  semantic_reference_contract_version: typeof SEMANTIC_REFERENCE_CONTRACT_VERSION;
  fact_revision_id: OpaqueContractReference;
}

export interface NativeIdentity {
  native_namespace: string;
  native_id: string;
}

export type QualifiedValueQuality = 'exact' | 'native_claimed' | 'derived' | 'estimated' | 'unknown';
export type ContractCompleteness = 'complete' | 'partial' | 'unknown';
export type QualifiedUnknownReason =
  | 'missing'
  | 'unsupported'
  | 'withheld'
  | 'not_yet_observed'
  | 'ambiguous'
  | 'malformed';

export interface QualifiedValue<T = unknown, A = unknown, P = unknown> {
  value: T | null;
  quality: QualifiedValueQuality;
  authority: A;
  completeness: ContractCompleteness;
  unknown_reason?: QualifiedUnknownReason;
  effective_at?: number;
  provenance: P;
}

export interface NativeIdentityClaim {
  entity_ref: ExternalEntityRef;
  identity: QualifiedValue<NativeIdentity, string, SemanticRevisionRef[]>;
}

export interface QualifiedValueDecoders<T, A, P> {
  parseKnownValue?: (value: unknown, label: string) => T;
  parseAuthority?: (value: unknown, label: string) => A;
  parseProvenance?: (value: unknown, label: string) => P;
}

export const RFC012A_FIXTURE_CONTRACT_VERSION = 1 as const;

export interface Rfc012aCoverageExpected {
  dominant_vs_baseline: CoverageComparison;
  baseline_vs_dominant: CoverageComparison;
  reset_vs_baseline: CoverageComparison;
}

export interface Rfc012aV1Fixture {
  fixture_contract_version: typeof RFC012A_FIXTURE_CONTRACT_VERSION;
  canonical_source_instance_key: OpaqueContractReference;
  external_entity_ref: ExternalEntityRef;
  native_identity_claim: NativeIdentityClaim;
  semantic_revision_ref: SemanticRevisionRef;
  qualified_known_zero: QualifiedValue<number, string, SemanticRevisionRef[]>;
  qualified_unknown: QualifiedValue<string, string, SemanticRevisionRef[]>;
  coverage: {
    baseline: SourceCoverageSet;
    dominant: SourceCoverageSet;
    reset: SourceCoverageSet;
    expected: Rfc012aCoverageExpected;
  };
}

export type CoverageDomain =
  | { kind: 'decode' }
  | { kind: 'fact_family'; family: string; version: number }
  | { kind: 'projection_pack'; pack: string; version: number };

export type CoveragePositionKind =
  | 'append_cursor'
  | 'document_revision'
  | 'snapshot_revision'
  | 'database_watermark'
  | 'key_range_token';

export interface CoveragePosition {
  kind: CoveragePositionKind;
  opaque: OpaqueContractReference;
  monotonic_order?: number;
}

export type CoverageStatus =
  | { kind: 'complete_through' }
  | { kind: 'exact_snapshot' }
  | { kind: 'partial' }
  | { kind: 'unavailable'; reason: string };

export interface CoverageProvenance {
  source_record_id?: OpaqueContractReference;
  semantic_revision_ref?: SemanticRevisionRef;
  observed_at?: number;
}

export interface SourceCoveragePoint {
  coverage_contract_version: typeof SOURCE_COVERAGE_CONTRACT_VERSION;
  coverage_domain: CoverageDomain;
  adapter_id: string;
  source_instance_key: OpaqueContractReference;
  stream_key: OpaqueContractReference;
  object_key: OpaqueContractReference;
  generation: number;
  position?: CoveragePosition;
  status: CoverageStatus;
  provenance: CoverageProvenance;
}

export interface CoverageScope {
  adapter_id: string;
  source_instance_key: OpaqueContractReference;
  root_entity_key?: OpaqueContractReference;
  support_release_id: string;
  source_or_scope_declaration_digest: OpaqueContractReference;
}

export interface CoverageAbsence {
  stream_key: OpaqueContractReference;
  object_key: OpaqueContractReference;
  generation: number;
  kind: 'absent' | 'deleted';
}

export interface CoverageError {
  stream_key?: OpaqueContractReference;
  object_key?: OpaqueContractReference;
  code: string;
}

export type CoverageSetCompleteness = 'complete' | 'partial' | 'unavailable';

export interface SourceCoverageSet {
  coverage_set_contract_version: typeof SOURCE_COVERAGE_SET_CONTRACT_VERSION;
  coverage_domain: CoverageDomain;
  scope: CoverageScope;
  membership_revision: OpaqueContractReference;
  points: SourceCoveragePoint[];
  explicit_absence_or_deletion: CoverageAbsence[];
  explicit_errors: CoverageError[];
  completeness: CoverageSetCompleteness;
}

export type CoverageComparison = 'equal' | 'dominates' | 'behind' | 'incomparable';

type UnknownRecord = Record<string, unknown>;

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

function hasOwn(input: UnknownRecord, field: string): boolean {
  return Object.prototype.hasOwnProperty.call(input, field);
}

function assertKnownFields(input: UnknownRecord, fields: readonly string[], label: string): void {
  const known = new Set(fields);
  for (const key of Object.keys(input)) {
    if (!known.has(key)) throw new ContractValidationError(`${label} contains an unknown field`);
  }
}

function assertRequiredFields(input: UnknownRecord, fields: readonly string[], label: string): void {
  for (const field of fields) {
    if (!hasOwn(input, field)) {
      throw new ContractValidationError(`${label} is missing a required field`);
    }
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

function boundedCanonicalString(value: unknown, label: string, maxBytes: number): string {
  if (typeof value !== 'string') {
    throw new ContractValidationError(`${label} must be a non-empty canonical string`);
  }
  if (value.length > maxBytes) {
    throw new ContractValidationError(`${label} exceeds the bounded maximum of ${maxBytes} UTF-8 bytes`);
  }
  const result = nonEmptyString(value, label);
  if (UTF8_ENCODER.encode(result).length > maxBytes) {
    throw new ContractValidationError(`${label} exceeds the bounded maximum of ${maxBytes} UTF-8 bytes`);
  }
  return result;
}

function coverageIdentifier(value: unknown, label: string): string {
  return boundedCanonicalString(value, label, MAX_IDENTIFIER_BYTES);
}

function coverageErrorCode(value: unknown): string {
  const result = boundedCanonicalString(value, 'coverage error code', MAX_COVERAGE_ERROR_CODE_BYTES);
  if (!/^[a-z][a-z0-9_]*$/.test(result)) {
    throw new ContractValidationError('coverage error code must be a lowercase ASCII machine code');
  }
  return result;
}

function nonNegativeInteger(value: unknown, label: string): number {
  const result = safeInteger(value, label);
  if (result < 0) {
    throw new ContractValidationError(`${label} must be a non-negative safe integer`);
  }
  return result;
}

function safeInteger(value: unknown, label: string): number {
  if (!Number.isSafeInteger(value) || Object.is(value, -0)) {
    throw new ContractValidationError(`${label} must be a safe integer`);
  }
  return value as number;
}

function positiveInteger(value: unknown, label: string): number {
  const result = nonNegativeInteger(value, label);
  if (result === 0) throw new ContractValidationError(`${label} must be greater than zero`);
  return result;
}

function contractVersion(value: unknown, label: string): number {
  const result = positiveInteger(value, label);
  if (result > MAX_U32) {
    throw new ContractValidationError(`${label} exceeds u32`);
  }
  return result;
}

function canonicalStringList(value: unknown, label: string, requireNonempty: boolean): string[] {
  if (!Array.isArray(value) || (requireNonempty && value.length === 0)) {
    throw new ContractValidationError(`${label} must be ${requireNonempty ? 'a non-empty' : 'an'} array`);
  }
  const result = value.map((entry) => nonEmptyString(entry, label));
  if (new Set(result).size !== result.length) {
    throw new ContractValidationError(`${label} contains duplicate values`);
  }
  return result;
}

function versionList(value: unknown, label: string, requireNonempty: boolean): number[] {
  if (!Array.isArray(value) || (requireNonempty && value.length === 0)) {
    throw new ContractValidationError(`${label} must be ${requireNonempty ? 'a non-empty' : 'an'} version array`);
  }
  const result = value.map((entry) => contractVersion(entry, label));
  if (new Set(result).size !== result.length) {
    throw new ContractValidationError(`${label} contains duplicate versions`);
  }
  return result;
}

function parseDottedVersion(value: string): bigint[] {
  if (value.length > 128 || !/^[0-9]+(?:\.[0-9]+)*$/.test(value)) {
    throw new ContractValidationError(`artifact range version ${JSON.stringify(value)} is not dotted numeric`);
  }
  const parts = value.split('.');
  if (parts.length > 16) {
    throw new ContractValidationError('dotted artifact version exceeds 16 components');
  }
  return parts.map((part) => BigInt(part));
}

function compareDottedVersions(left: bigint[], right: bigint[]): number {
  const count = Math.max(left.length, right.length);
  for (let index = 0; index < count; index += 1) {
    const leftPart = left[index] ?? 0n;
    const rightPart = right[index] ?? 0n;
    if (leftPart < rightPart) return -1;
    if (leftPart > rightPart) return 1;
  }
  return 0;
}

function parseArtifactRange(value: unknown): ArtifactVersionRange {
  const input = record(value, 'artifact version range');
  const minimum = nonEmptyString(input.minimum, 'artifact range minimum');
  const maximum = nonEmptyString(input.maximum, 'artifact range maximum');
  if (typeof input.minimum_inclusive !== 'boolean' || typeof input.maximum_inclusive !== 'boolean') {
    throw new ContractValidationError('artifact range inclusivity must be boolean');
  }
  const order = compareDottedVersions(parseDottedVersion(minimum), parseDottedVersion(maximum));
  if (order > 0) throw new ContractValidationError('artifact version range minimum exceeds maximum');
  if (order === 0 && !(input.minimum_inclusive && input.maximum_inclusive)) {
    throw new ContractValidationError('artifact version range is empty');
  }
  return {
    minimum,
    minimum_inclusive: input.minimum_inclusive,
    maximum,
    maximum_inclusive: input.maximum_inclusive,
  };
}

function parseSupportRelease(value: unknown): SupportReleaseDescriptor {
  const input = record(value, 'support release');
  if (!['candidate', 'promoted', 'retired'].includes(input.status as string)) {
    throw new ContractValidationError('support release has an unsupported status');
  }
  const compatibility = record(input.artifact_compatibility, 'artifact compatibility');
  if (!Array.isArray(compatibility.ranges)) {
    throw new ContractValidationError('artifact compatibility ranges must be an array');
  }
  if (typeof compatibility.forward_catalog_only !== 'boolean') {
    throw new ContractValidationError('forward_catalog_only must be boolean');
  }
  if (!Array.isArray(input.capabilities) || input.capabilities.length === 0) {
    throw new ContractValidationError('support release capabilities must be a non-empty array');
  }
  const capabilities = input.capabilities.map((value, index): SupportCapabilityDeclaration => {
    const capability = record(value, `support capability ${index}`);
    assertKnownFields(capability, ['capability_id', 'topology', 'level', 'notes'], `support capability ${index}`);
    const capabilityId = nonEmptyString(capability.capability_id, `support capability ${index} id`);
    if (new TextEncoder().encode(capabilityId).length > 128) {
      throw new ContractValidationError(`support capability ${index} id exceeds 128 bytes`);
    }
    if (!['catalog', 'durable', 'scoped'].includes(capability.topology as string)) {
      throw new ContractValidationError(`support capability ${index} has an unsupported topology`);
    }
    if (!['supported', 'degraded', 'unsupported'].includes(capability.level as string)) {
      throw new ContractValidationError(`support capability ${index} has an unsupported level`);
    }
    if (capability.notes !== undefined && capability.notes !== null && typeof capability.notes !== 'string') {
      throw new ContractValidationError(`support capability ${index} notes must be a string or null`);
    }
    return {
      capability_id: capabilityId,
      topology: capability.topology as SupportCapabilityTopology,
      level: capability.level as SupportCapabilityLevel,
    };
  });
  if (new Set(capabilities.map((capability) => capability.capability_id)).size !== capabilities.length) {
    throw new ContractValidationError('duplicate support capability id');
  }
  const exactVersions = canonicalStringList(compatibility.exact_versions, 'exact artifact versions', false);
  if (exactVersions.some((version) => version.length > 128)) {
    throw new ContractValidationError('exact artifact version exceeds 128 bytes');
  }
  return {
    support_release_id: nonEmptyString(input.support_release_id, 'support release id'),
    status: input.status as SupportReleaseStatus,
    capabilities,
    artifact_compatibility: {
      family: nonEmptyString(compatibility.family, 'artifact family'),
      platforms: canonicalStringList(compatibility.platforms, 'artifact platforms', true),
      exact_versions: exactVersions,
      ranges: compatibility.ranges.map(parseArtifactRange),
      required_markers: canonicalStringList(compatibility.required_markers, 'required native markers', false),
      forward_catalog_only: compatibility.forward_catalog_only,
    },
  };
}

function parseNativeArtifactProbe(value: unknown): NativeArtifactProbe {
  const input = record(value, 'native artifact probe');
  const rawVersion = input.version;
  let version: string | null;
  if (rawVersion === null || rawVersion === undefined) {
    version = null;
  } else if (typeof rawVersion === 'string') {
    version = rawVersion;
  } else {
    throw new ContractValidationError('probed artifact version must be a string or null');
  }
  if (typeof input.contradictory_markers !== 'boolean') {
    throw new ContractValidationError('contradictory_markers must be boolean');
  }
  if (
    typeof version === 'string' &&
    (version.length === 0 || version.length > 128 || hasSurroundingRustWhitespace(version))
  ) {
    throw new ContractValidationError('probed artifact version must be non-empty and canonical');
  }
  return {
    family: nonEmptyString(input.family, 'probed artifact family'),
    platform: nonEmptyString(input.platform, 'probed artifact platform'),
    version,
    markers: canonicalStringList(input.markers, 'probed native markers', false),
    contradictory_markers: input.contradictory_markers,
  };
}

const recognizedPermissions: OperationPermissions = {
  version_probe: true,
  catalog: false,
  durable: false,
  scoped_observation: false,
  bounded_drift: true,
};

function declaredOperationPermissions(release: SupportReleaseDescriptor): OperationPermissions {
  const topologyIsFullySupported = (topology: SupportCapabilityTopology): boolean => {
    const matching = release.capabilities.filter((capability) => capability.topology === topology);
    return matching.length > 0 && matching.every((capability) => capability.level === 'supported');
  };
  return {
    version_probe: true,
    catalog: topologyIsFullySupported('catalog'),
    durable: topologyIsFullySupported('durable'),
    scoped_observation: topologyIsFullySupported('scoped'),
    bounded_drift: true,
  };
}

const incompatiblePermissions: OperationPermissions = {
  version_probe: true,
  catalog: false,
  durable: false,
  scoped_observation: false,
  bounded_drift: false,
};

function compatibilityDecision(
  compatibilityClass: CompatibilityClass,
  supportReleaseId: string | null,
  reason: CompatibilityReason,
  permissions: OperationPermissions,
): CompatibilityDecision {
  return {
    support_selection_contract_version: SUPPORT_SELECTION_CONTRACT_VERSION,
    compatibility_class: compatibilityClass,
    support_release_id: supportReleaseId,
    reason,
    permissions: { ...permissions },
  };
}

function rangeContains(range: ArtifactVersionRange, version: string): boolean {
  let candidate: bigint[];
  try {
    candidate = parseDottedVersion(version);
  } catch {
    return false;
  }
  const lower = compareDottedVersions(candidate, parseDottedVersion(range.minimum));
  const upper = compareDottedVersions(candidate, parseDottedVersion(range.maximum));
  return (
    (lower > 0 || (lower === 0 && range.minimum_inclusive)) && (upper < 0 || (upper === 0 && range.maximum_inclusive))
  );
}

export function classifyRuntimeSupport(probeInput: unknown, releasesInput: unknown): CompatibilityDecision {
  const probe = parseNativeArtifactProbe(probeInput);
  if (!Array.isArray(releasesInput)) {
    throw new ContractValidationError('support releases must be an array');
  }
  const releases = releasesInput.map(parseSupportRelease);
  const releaseIds = releases.map((release) => release.support_release_id);
  if (new Set(releaseIds).size !== releaseIds.length) {
    throw new ContractValidationError('duplicate support release id');
  }

  const familyEntries = releases.filter((release) => release.artifact_compatibility.family === probe.family);
  if (familyEntries.length === 0) {
    return compatibilityDecision(
      'UnknownOrIncompatible',
      null,
      'unrecognized_artifact_family',
      incompatiblePermissions,
    );
  }
  if (probe.contradictory_markers) {
    return compatibilityDecision(
      'UnknownOrIncompatible',
      null,
      'contradictory_native_markers',
      incompatiblePermissions,
    );
  }
  if (!familyEntries.some((release) => release.artifact_compatibility.platforms.includes(probe.platform))) {
    return compatibilityDecision('UnknownOrIncompatible', null, 'platform_not_declared', incompatiblePermissions);
  }

  const promotedOnPlatform = familyEntries.filter(
    (release) => release.status === 'promoted' && release.artifact_compatibility.platforms.includes(probe.platform),
  );
  const probeMarkers = new Set(probe.markers);
  const markerCompatible = promotedOnPlatform.filter((release) =>
    release.artifact_compatibility.required_markers.every((marker) => probeMarkers.has(marker)),
  );
  const matches: Array<[SupportReleaseDescriptor, 'ExactSupported' | 'RangeSupported']> = [];
  if (probe.version !== null) {
    for (const release of markerCompatible) {
      const declaration = release.artifact_compatibility;
      if (declaration.exact_versions.includes(probe.version)) {
        matches.push([release, 'ExactSupported']);
      } else if (declaration.ranges.some((range) => rangeContains(range, probe.version!))) {
        matches.push([release, 'RangeSupported']);
      }
    }
  }
  if (matches.length > 1) {
    return compatibilityDecision('UnknownOrIncompatible', null, 'ambiguous_promoted_release', incompatiblePermissions);
  }
  const match = matches[0];
  if (match !== undefined) {
    const [release, compatibilityClass] = match;
    return compatibilityDecision(
      compatibilityClass,
      release.support_release_id,
      compatibilityClass === 'ExactSupported' ? 'exact_promoted_version' : 'fixture_backed_range',
      declaredOperationPermissions(release),
    );
  }
  if (promotedOnPlatform.length > 0 && markerCompatible.length === 0) {
    return compatibilityDecision(
      'UnknownOrIncompatible',
      null,
      'required_native_marker_absent',
      incompatiblePermissions,
    );
  }
  const forwardCatalog = markerCompatible.filter(
    (release) => release.artifact_compatibility.forward_catalog_only && declaredOperationPermissions(release).catalog,
  );
  if (forwardCatalog.length > 1) {
    return compatibilityDecision('UnknownOrIncompatible', null, 'ambiguous_promoted_release', incompatiblePermissions);
  }
  if (forwardCatalog[0] !== undefined) {
    return compatibilityDecision(
      'RecognizedUnverified',
      forwardCatalog[0].support_release_id,
      'promoted_forward_catalog_only',
      { ...recognizedPermissions, catalog: true },
    );
  }
  return compatibilityDecision('RecognizedUnverified', null, 'no_matching_promoted_release', recognizedPermissions);
}

function parseFactFamilyVersions(value: unknown, label: string): Record<string, number[]> {
  const input = record(value, `${label} fact families`);
  return Object.fromEntries(
    Object.entries(input).map(([family, versions]) => [
      nonEmptyString(family, `${label} fact family`),
      versionList(versions, `${label} fact family ${family}`, true),
    ]),
  );
}

export function parseContractVersionRequest(value: unknown): ContractVersionRequest {
  const input = record(value, 'contract version request');
  assertKnownFields(
    input,
    [
      'selection_contract_version',
      'model_major',
      'external_entity_reference_version',
      'semantic_revision_reference_version',
      'coverage_contract_versions',
      'fact_family_versions',
      'query_pack_versions',
      'observation_contract_versions',
    ],
    'contract version request',
  );
  if (input.selection_contract_version !== CONTRACT_VERSION_SELECTION_VERSION) {
    throw new ContractValidationError('unsupported contract-version selection request version');
  }
  const result: ContractVersionRequest = {
    selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
    model_major: contractVersion(input.model_major, 'requested model major'),
    external_entity_reference_version: contractVersion(
      input.external_entity_reference_version,
      'requested external entity reference version',
    ),
    semantic_revision_reference_version: contractVersion(
      input.semantic_revision_reference_version,
      'requested semantic revision reference version',
    ),
    coverage_contract_versions: versionList(
      input.coverage_contract_versions,
      'requested coverage contract versions',
      true,
    ),
    fact_family_versions: parseFactFamilyVersions(input.fact_family_versions, 'requested'),
  };
  if (input.query_pack_versions !== undefined && input.query_pack_versions !== null) {
    result.query_pack_versions = versionList(input.query_pack_versions, 'requested query pack versions', true);
  }
  if (input.observation_contract_versions !== undefined && input.observation_contract_versions !== null) {
    result.observation_contract_versions = versionList(
      input.observation_contract_versions,
      'requested observation contract versions',
      true,
    );
  }
  return result;
}

export function parseContractVersionOffer(value: unknown): ContractVersionOffer {
  const input = record(value, 'contract version offer');
  assertKnownFields(
    input,
    [
      'selection_contract_version',
      'model_major',
      'external_entity_reference_versions',
      'semantic_revision_reference_versions',
      'coverage_contract_versions',
      'fact_family_versions',
      'query_pack_versions',
      'observation_contract_versions',
    ],
    'contract version offer',
  );
  if (input.selection_contract_version !== CONTRACT_VERSION_SELECTION_VERSION) {
    throw new ContractValidationError('unsupported contract-version offer version');
  }
  return {
    selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
    model_major: contractVersion(input.model_major, 'offered model major'),
    external_entity_reference_versions: versionList(
      input.external_entity_reference_versions,
      'offered external entity reference versions',
      true,
    ),
    semantic_revision_reference_versions: versionList(
      input.semantic_revision_reference_versions,
      'offered semantic revision reference versions',
      true,
    ),
    coverage_contract_versions: versionList(
      input.coverage_contract_versions,
      'offered coverage contract versions',
      true,
    ),
    fact_family_versions: parseFactFamilyVersions(input.fact_family_versions, 'offered'),
    query_pack_versions: versionList(input.query_pack_versions, 'offered query pack versions', false),
    observation_contract_versions: versionList(
      input.observation_contract_versions,
      'offered observation contract versions',
      false,
    ),
  };
}

export function parseContractVersionSelection(value: unknown): ContractVersionSelection {
  const input = record(value, 'contract version selection');
  assertKnownFields(
    input,
    [
      'selection_contract_version',
      'model_major',
      'external_entity_reference_version',
      'semantic_revision_reference_version',
      'coverage_contract_version',
      'fact_family_versions',
      'query_pack_version',
      'observation_contract_version',
    ],
    'contract version selection',
  );
  if (input.selection_contract_version !== CONTRACT_VERSION_SELECTION_VERSION) {
    throw new ContractValidationError('unsupported contract-version selection version');
  }
  const families = record(input.fact_family_versions, 'selected fact families');
  const factFamilyVersions = Object.fromEntries(
    Object.entries(families).map(([family, version]) => [
      nonEmptyString(family, 'selected fact family'),
      contractVersion(version, `selected fact family ${family}`),
    ]),
  );
  const optionalVersion = (version: unknown, label: string): number | null => {
    if (version === null || version === undefined) return null;
    return contractVersion(version, label);
  };
  return {
    selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
    model_major: contractVersion(input.model_major, 'selected model major'),
    external_entity_reference_version: contractVersion(
      input.external_entity_reference_version,
      'selected external entity reference version',
    ),
    semantic_revision_reference_version: contractVersion(
      input.semantic_revision_reference_version,
      'selected semantic revision reference version',
    ),
    coverage_contract_version: contractVersion(input.coverage_contract_version, 'selected coverage contract version'),
    fact_family_versions: factFamilyVersions,
    query_pack_version: optionalVersion(input.query_pack_version, 'selected query pack version'),
    observation_contract_version: optionalVersion(
      input.observation_contract_version,
      'selected observation contract version',
    ),
  };
}

function selectPreferred(label: string, requested: number[], offered: number[]): number {
  const selected = requested.find((version) => offered.includes(version));
  if (selected === undefined) throw new ContractValidationError(`no compatible ${label} version`);
  return selected;
}

export function selectContractVersions(requestInput: unknown, offerInput: unknown): ContractVersionSelection {
  const request = parseContractVersionRequest(requestInput);
  const offer = parseContractVersionOffer(offerInput);
  if (request.model_major !== offer.model_major) {
    throw new ContractValidationError('incompatible base model major');
  }
  if (!offer.external_entity_reference_versions.includes(request.external_entity_reference_version)) {
    throw new ContractValidationError(
      `unsupported external entity reference version ${request.external_entity_reference_version}`,
    );
  }
  if (!offer.semantic_revision_reference_versions.includes(request.semantic_revision_reference_version)) {
    throw new ContractValidationError(
      `unsupported semantic revision reference version ${request.semantic_revision_reference_version}`,
    );
  }
  const factFamilyVersions: Record<string, number> = {};
  for (const [family, requested] of Object.entries(request.fact_family_versions)) {
    const offered = offer.fact_family_versions[family];
    if (offered === undefined) {
      throw new ContractValidationError(`required fact family is absent: ${family}`);
    }
    factFamilyVersions[family] = selectPreferred(`fact family ${family}`, requested, offered);
  }
  return {
    selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
    model_major: request.model_major,
    external_entity_reference_version: request.external_entity_reference_version,
    semantic_revision_reference_version: request.semantic_revision_reference_version,
    coverage_contract_version: selectPreferred(
      'coverage contract',
      request.coverage_contract_versions,
      offer.coverage_contract_versions,
    ),
    fact_family_versions: factFamilyVersions,
    query_pack_version:
      request.query_pack_versions === undefined
        ? null
        : selectPreferred('query pack', request.query_pack_versions, offer.query_pack_versions),
    observation_contract_version:
      request.observation_contract_versions === undefined
        ? null
        : selectPreferred(
            'observation contract',
            request.observation_contract_versions,
            offer.observation_contract_versions,
          ),
  };
}

export function parseOpaqueContractReference(value: unknown, label = 'opaque reference'): OpaqueContractReference {
  if (typeof value !== 'string' || !/^v1:[A-Za-z0-9_-]{43}$/.test(value)) {
    throw new ContractValidationError(`${label} is not an RFC 012A v1 opaque reference`);
  }
  return value as OpaqueContractReference;
}

export function parseExternalEntityRef(value: unknown): ExternalEntityRef {
  const input = record(value, 'external entity reference');
  assertKnownFields(input, ['external_entity_reference_version', 'entity_key'], 'external entity reference');
  if (input.external_entity_reference_version !== EXTERNAL_ENTITY_REFERENCE_VERSION) {
    throw new ContractValidationError('unsupported external entity reference version');
  }
  return {
    external_entity_reference_version: EXTERNAL_ENTITY_REFERENCE_VERSION,
    entity_key: parseOpaqueContractReference(input.entity_key, 'entity key'),
  };
}

export function parseSemanticRevisionRef(value: unknown): SemanticRevisionRef {
  const input = record(value, 'semantic revision reference');
  assertKnownFields(input, ['semantic_reference_contract_version', 'fact_revision_id'], 'semantic revision reference');
  if (input.semantic_reference_contract_version !== SEMANTIC_REFERENCE_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported semantic reference contract version');
  }
  return {
    semantic_reference_contract_version: SEMANTIC_REFERENCE_CONTRACT_VERSION,
    fact_revision_id: parseOpaqueContractReference(input.fact_revision_id, 'fact revision id'),
  };
}

export function parseQualifiedValue<T = unknown, A = unknown, P = unknown>(
  value: unknown,
  decoders?: QualifiedValueDecoders<T, A, P>,
): QualifiedValue<T, A, P> {
  const input = record(value, 'qualified value');
  assertKnownFields(
    input,
    ['value', 'quality', 'authority', 'completeness', 'unknown_reason', 'effective_at', 'provenance'],
    'qualified value',
  );
  const quality = input.quality;
  if (!['exact', 'native_claimed', 'derived', 'estimated', 'unknown'].includes(quality as string)) {
    throw new ContractValidationError('qualified value has an unsupported quality');
  }
  const completeness = input.completeness;
  if (!['complete', 'partial', 'unknown'].includes(completeness as string)) {
    throw new ContractValidationError('qualified value has an unsupported completeness');
  }
  if (!Object.hasOwn(input, 'value') || !Object.hasOwn(input, 'authority') || !Object.hasOwn(input, 'provenance')) {
    throw new ContractValidationError('qualified value is missing value, authority, or provenance');
  }
  if (input.authority === null || input.provenance === null) {
    throw new ContractValidationError('qualified value authority and provenance cannot be null');
  }
  const isUnknown = quality === 'unknown';
  if (isUnknown !== (input.value === null)) {
    throw new ContractValidationError('quality is unknown if and only if value is null');
  }
  if (Object.hasOwn(input, 'unknown_reason') && input.unknown_reason === null) {
    throw new ContractValidationError('qualified value unknown_reason cannot be explicit null');
  }
  if (Object.hasOwn(input, 'effective_at') && input.effective_at === null) {
    throw new ContractValidationError('qualified value effective_at cannot be explicit null');
  }
  const unknownReason = input.unknown_reason;
  const hasUnknownReason = unknownReason !== undefined;
  if (isUnknown !== hasUnknownReason) {
    throw new ContractValidationError('unknown_reason is present if and only if quality is unknown');
  }
  if (
    hasUnknownReason &&
    (typeof unknownReason !== 'string' ||
      !['missing', 'unsupported', 'withheld', 'not_yet_observed', 'ambiguous', 'malformed'].includes(unknownReason))
  ) {
    throw new ContractValidationError('qualified value has an unsupported unknown_reason');
  }
  const effectiveAt = input.effective_at === undefined ? undefined : safeInteger(input.effective_at, 'effective_at');
  const authority = decoders?.parseAuthority
    ? decoders.parseAuthority(input.authority, 'qualified value authority')
    : (input.authority as A);
  const provenance = decoders?.parseProvenance
    ? decoders.parseProvenance(input.provenance, 'qualified value provenance')
    : (input.provenance as P);
  const parsedValue = isUnknown
    ? null
    : decoders?.parseKnownValue
      ? decoders.parseKnownValue(input.value, 'qualified value value')
      : (input.value as T);
  return {
    value: parsedValue,
    quality: quality as QualifiedValueQuality,
    authority,
    completeness: completeness as ContractCompleteness,
    ...(hasUnknownReason ? { unknown_reason: unknownReason as QualifiedUnknownReason } : {}),
    ...(effectiveAt === undefined ? {} : { effective_at: effectiveAt }),
    provenance,
  };
}

function parseNativeIdentityValue(value: unknown, label: string): NativeIdentity {
  const nativeIdentity = record(value, label);
  assertKnownFields(nativeIdentity, ['native_namespace', 'native_id'], label);
  return {
    native_namespace: boundedCanonicalString(
      nativeIdentity.native_namespace,
      `${label} namespace`,
      MAX_IDENTIFIER_BYTES,
    ),
    native_id: boundedCanonicalString(nativeIdentity.native_id, `${label} id`, MAX_IDENTIFIER_BYTES),
  };
}

function parseStringAuthority(value: unknown, label: string): string {
  return boundedCanonicalString(value, label, MAX_IDENTIFIER_BYTES);
}

function parseSemanticRevisionProvenance(value: unknown, label: string): SemanticRevisionRef[] {
  if (!Array.isArray(value)) {
    throw new ContractValidationError(`${label} must be an array of semantic revision references`);
  }
  return value.map((item) => parseSemanticRevisionRef(item));
}

export function parseNativeIdentityClaim(value: unknown): NativeIdentityClaim {
  const input = record(value, 'native identity claim');
  assertKnownFields(input, ['entity_ref', 'identity'], 'native identity claim');
  return {
    entity_ref: parseExternalEntityRef(input.entity_ref),
    identity: parseQualifiedValue<NativeIdentity, string, SemanticRevisionRef[]>(input.identity, {
      parseKnownValue: parseNativeIdentityValue,
      parseAuthority: parseStringAuthority,
      parseProvenance: parseSemanticRevisionProvenance,
    }),
  };
}

function parseCoverageDomain(value: unknown): CoverageDomain {
  const input = record(value, 'coverage domain');
  switch (input.kind) {
    case 'decode':
      assertKnownFields(input, ['kind'], 'decode coverage domain');
      return { kind: 'decode' };
    case 'fact_family':
      assertKnownFields(input, ['kind', 'family', 'version'], 'fact-family coverage domain');
      return {
        kind: 'fact_family',
        family: coverageIdentifier(input.family, 'fact family'),
        version: contractVersion(input.version, 'fact-family version'),
      };
    case 'projection_pack':
      assertKnownFields(input, ['kind', 'pack', 'version'], 'projection-pack coverage domain');
      return {
        kind: 'projection_pack',
        pack: coverageIdentifier(input.pack, 'projection pack'),
        version: contractVersion(input.version, 'projection-pack version'),
      };
    default:
      throw new ContractValidationError('unsupported coverage domain');
  }
}

function domainKey(domain: CoverageDomain): string {
  switch (domain.kind) {
    case 'decode':
      return 'decode';
    case 'fact_family':
      return `fact_family\0${domain.family}\0${domain.version}`;
    case 'projection_pack':
      return `projection_pack\0${domain.pack}\0${domain.version}`;
  }
}

function parseCoveragePosition(value: unknown): CoveragePosition {
  const input = record(value, 'coverage position');
  assertKnownFields(input, ['kind', 'opaque', 'monotonic_order'], 'coverage position');
  if (
    !['append_cursor', 'document_revision', 'snapshot_revision', 'database_watermark', 'key_range_token'].includes(
      input.kind as string,
    )
  ) {
    throw new ContractValidationError('unsupported coverage position kind');
  }
  const result: CoveragePosition = {
    kind: input.kind as CoveragePositionKind,
    opaque: parseOpaqueContractReference(input.opaque, 'coverage position'),
  };
  if (input.monotonic_order !== undefined) {
    result.monotonic_order = nonNegativeInteger(input.monotonic_order, 'monotonic order');
  }
  return result;
}

function parseCoverageStatus(value: unknown): CoverageStatus {
  const input = record(value, 'coverage status');
  switch (input.kind) {
    case 'complete_through':
    case 'exact_snapshot':
    case 'partial':
      assertKnownFields(input, ['kind'], `${input.kind} coverage status`);
      return { kind: input.kind };
    case 'unavailable':
      assertKnownFields(input, ['kind', 'reason'], 'unavailable coverage status');
      return {
        kind: 'unavailable',
        reason: boundedCanonicalString(input.reason, 'unavailable reason', MAX_COVERAGE_UNAVAILABLE_REASON_BYTES),
      };
    default:
      throw new ContractValidationError('unsupported coverage status');
  }
}

function parseCoverageProvenance(value: unknown): CoverageProvenance {
  const input = record(value, 'coverage provenance');
  assertKnownFields(input, ['source_record_id', 'semantic_revision_ref', 'observed_at'], 'coverage provenance');
  const result: CoverageProvenance = {};
  if (input.source_record_id !== undefined) {
    result.source_record_id = parseOpaqueContractReference(input.source_record_id, 'source record id');
  }
  if (input.semantic_revision_ref !== undefined) {
    result.semantic_revision_ref = parseSemanticRevisionRef(input.semantic_revision_ref);
  }
  if (input.observed_at !== undefined) {
    result.observed_at = safeInteger(input.observed_at, 'coverage observed_at');
  }
  return result;
}

function parseCoveragePoint(value: unknown): SourceCoveragePoint {
  const input = record(value, 'source coverage point');
  assertKnownFields(
    input,
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
    'source coverage point',
  );
  if (input.coverage_contract_version !== SOURCE_COVERAGE_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported source coverage contract version');
  }
  const position = input.position === undefined ? undefined : parseCoveragePosition(input.position);
  const status = parseCoverageStatus(input.status);
  if (status.kind === 'complete_through') {
    if (position === undefined || !['append_cursor', 'database_watermark', 'key_range_token'].includes(position.kind)) {
      throw new ContractValidationError('complete-through coverage requires an ordered position');
    }
  }
  if (status.kind === 'exact_snapshot') {
    if (position === undefined || !['document_revision', 'snapshot_revision'].includes(position.kind)) {
      throw new ContractValidationError('exact-snapshot coverage requires a snapshot position');
    }
  }
  return {
    coverage_contract_version: SOURCE_COVERAGE_CONTRACT_VERSION,
    coverage_domain: parseCoverageDomain(input.coverage_domain),
    adapter_id: coverageIdentifier(input.adapter_id, 'coverage adapter id'),
    source_instance_key: parseOpaqueContractReference(input.source_instance_key, 'source instance key'),
    stream_key: parseOpaqueContractReference(input.stream_key, 'stream key'),
    object_key: parseOpaqueContractReference(input.object_key, 'object key'),
    generation: positiveInteger(input.generation, 'coverage generation'),
    ...(position === undefined ? {} : { position }),
    status,
    provenance: parseCoverageProvenance(input.provenance),
  };
}

function parseCoverageScope(value: unknown): CoverageScope {
  const input = record(value, 'coverage scope');
  assertKnownFields(
    input,
    [
      'adapter_id',
      'source_instance_key',
      'root_entity_key',
      'support_release_id',
      'source_or_scope_declaration_digest',
    ],
    'coverage scope',
  );
  const result: CoverageScope = {
    adapter_id: coverageIdentifier(input.adapter_id, 'coverage scope adapter id'),
    source_instance_key: parseOpaqueContractReference(input.source_instance_key, 'scope source instance key'),
    support_release_id: coverageIdentifier(input.support_release_id, 'support release id'),
    source_or_scope_declaration_digest: parseOpaqueContractReference(
      input.source_or_scope_declaration_digest,
      'scope declaration digest',
    ),
  };
  if (input.root_entity_key !== undefined) {
    result.root_entity_key = parseOpaqueContractReference(input.root_entity_key, 'root entity key');
  }
  return result;
}

function parseCoverageAbsence(value: unknown): CoverageAbsence {
  const input = record(value, 'coverage absence');
  assertKnownFields(input, ['stream_key', 'object_key', 'generation', 'kind'], 'coverage absence');
  if (input.kind !== 'absent' && input.kind !== 'deleted') {
    throw new ContractValidationError('coverage absence has an unsupported kind');
  }
  return {
    stream_key: parseOpaqueContractReference(input.stream_key, 'absence stream key'),
    object_key: parseOpaqueContractReference(input.object_key, 'absence object key'),
    generation: positiveInteger(input.generation, 'absence generation'),
    kind: input.kind,
  };
}

function parseCoverageError(value: unknown): CoverageError {
  const input = record(value, 'coverage error');
  assertKnownFields(input, ['stream_key', 'object_key', 'code'], 'coverage error');
  const result: CoverageError = { code: coverageErrorCode(input.code) };
  if (input.stream_key !== undefined) {
    result.stream_key = parseOpaqueContractReference(input.stream_key, 'error stream key');
  }
  if (input.object_key !== undefined) {
    if (result.stream_key === undefined) {
      throw new ContractValidationError('coverage error object key requires a stream key');
    }
    result.object_key = parseOpaqueContractReference(input.object_key, 'error object key');
  }
  return result;
}

function coordinate(value: {
  stream_key: OpaqueContractReference;
  object_key: OpaqueContractReference;
  generation: number;
}): string {
  return `${value.stream_key}\0${value.object_key}\0${value.generation}`;
}

export function parseSourceCoverageSet(value: unknown): SourceCoverageSet {
  const input = record(value, 'source coverage set');
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
    'source coverage set',
  );
  if (input.coverage_set_contract_version !== SOURCE_COVERAGE_SET_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported source coverage set contract version');
  }
  const coverageDomain = parseCoverageDomain(input.coverage_domain);
  const scope = parseCoverageScope(input.scope);
  if (!Array.isArray(input.points)) throw new ContractValidationError('coverage points must be an array');
  if (!Array.isArray(input.explicit_absence_or_deletion)) {
    throw new ContractValidationError('coverage absences must be an array');
  }
  if (!Array.isArray(input.explicit_errors)) {
    throw new ContractValidationError('coverage errors must be an array');
  }
  if (input.points.length > MAX_COVERAGE_POINTS_PER_SET) {
    throw new ContractValidationError(`coverage set exceeds ${MAX_COVERAGE_POINTS_PER_SET} points`);
  }
  if (input.explicit_absence_or_deletion.length > MAX_COVERAGE_ABSENCES_PER_SET) {
    throw new ContractValidationError(`coverage set exceeds ${MAX_COVERAGE_ABSENCES_PER_SET} absences`);
  }
  if (input.explicit_errors.length > MAX_COVERAGE_ERRORS_PER_SET) {
    throw new ContractValidationError(`coverage set exceeds ${MAX_COVERAGE_ERRORS_PER_SET} errors`);
  }
  if (!['complete', 'partial', 'unavailable'].includes(input.completeness as string)) {
    throw new ContractValidationError('unsupported coverage-set completeness');
  }
  const points = input.points.map(parseCoveragePoint);
  const absences = input.explicit_absence_or_deletion.map(parseCoverageAbsence);
  const errors = input.explicit_errors.map(parseCoverageError);
  const coordinates = new Set<string>();
  for (const point of points) {
    if (
      domainKey(point.coverage_domain) !== domainKey(coverageDomain) ||
      point.adapter_id !== scope.adapter_id ||
      point.source_instance_key !== scope.source_instance_key
    ) {
      throw new ContractValidationError('coverage point does not belong to its set domain and scope');
    }
    const key = coordinate(point);
    if (coordinates.has(key)) {
      throw new ContractValidationError('coverage set contains a duplicate object generation');
    }
    coordinates.add(key);
  }
  for (const absence of absences) {
    const key = coordinate(absence);
    if (coordinates.has(key)) {
      throw new ContractValidationError('coverage absence conflicts with a point or duplicate absence');
    }
    coordinates.add(key);
  }
  const explicitErrors = new Set<string>();
  for (const error of errors) {
    const key = JSON.stringify([error.stream_key ?? null, error.object_key ?? null, error.code]);
    if (explicitErrors.has(key)) {
      throw new ContractValidationError('coverage set contains a duplicate explicit error');
    }
    explicitErrors.add(key);
  }
  const completeness = input.completeness as CoverageSetCompleteness;
  if (
    completeness === 'complete' &&
    (errors.length > 0 ||
      points.some((point) => point.status.kind === 'partial' || point.status.kind === 'unavailable'))
  ) {
    throw new ContractValidationError('complete coverage cannot contain errors, partial points, or unavailable points');
  }
  return {
    coverage_set_contract_version: SOURCE_COVERAGE_SET_CONTRACT_VERSION,
    coverage_domain: coverageDomain,
    scope,
    membership_revision: parseOpaqueContractReference(input.membership_revision, 'membership revision'),
    points,
    explicit_absence_or_deletion: absences,
    explicit_errors: errors,
    completeness,
  };
}

function scopeKey(scope: CoverageScope): string {
  return [
    scope.adapter_id,
    scope.source_instance_key,
    scope.root_entity_key ?? '',
    scope.support_release_id,
    scope.source_or_scope_declaration_digest,
  ].join('\0');
}

function comparePositions(candidate: CoveragePosition, baseline: CoveragePosition): CoverageComparison {
  if (candidate.kind !== baseline.kind) return 'incomparable';
  if (candidate.opaque === baseline.opaque) return 'equal';
  if (candidate.monotonic_order === undefined || baseline.monotonic_order === undefined) {
    return 'incomparable';
  }
  if (candidate.monotonic_order > baseline.monotonic_order) return 'dominates';
  if (candidate.monotonic_order < baseline.monotonic_order) return 'behind';
  return 'incomparable';
}

function pointDominates(candidate: SourceCoveragePoint, baseline: SourceCoveragePoint): boolean {
  if (coordinate(candidate) !== coordinate(baseline)) return false;
  if (
    candidate.status.kind === baseline.status.kind &&
    candidate.position?.kind === baseline.position?.kind &&
    candidate.position?.opaque === baseline.position?.opaque &&
    candidate.position?.monotonic_order === baseline.position?.monotonic_order
  ) {
    return true;
  }
  const candidateComplete = candidate.status.kind === 'complete_through' || candidate.status.kind === 'exact_snapshot';
  if (!candidateComplete) return false;
  const statusCompatible =
    candidate.status.kind === baseline.status.kind ||
    baseline.status.kind === 'partial' ||
    baseline.status.kind === 'unavailable';
  if (!statusCompatible) return false;
  if (candidate.position !== undefined && baseline.position === undefined) return true;
  if (candidate.position === undefined || baseline.position === undefined) return false;
  const comparison = comparePositions(candidate.position, baseline.position);
  return comparison === 'equal' || comparison === 'dominates';
}

function setDominates(candidate: SourceCoverageSet, baseline: SourceCoverageSet): boolean {
  if (candidate.completeness !== 'complete') return false;
  const candidatePoints = new Map(candidate.points.map((point) => [coordinate(point), point]));
  if (
    !baseline.points.every((point) => {
      const candidatePoint = candidatePoints.get(coordinate(point));
      return candidatePoint !== undefined && pointDominates(candidatePoint, point);
    })
  ) {
    return false;
  }
  const candidateAbsences = new Set(
    candidate.explicit_absence_or_deletion.map((absence) => `${coordinate(absence)}\0${absence.kind}`),
  );
  return baseline.explicit_absence_or_deletion.every((absence) =>
    candidateAbsences.has(`${coordinate(absence)}\0${absence.kind}`),
  );
}

function coverageSemanticallyEqual(candidate: SourceCoverageSet, baseline: SourceCoverageSet): boolean {
  if (
    candidate.completeness !== baseline.completeness ||
    candidate.points.length !== baseline.points.length ||
    candidate.explicit_absence_or_deletion.length !== baseline.explicit_absence_or_deletion.length ||
    candidate.explicit_errors.length !== baseline.explicit_errors.length
  ) {
    return false;
  }
  const baselinePoints = new Map(baseline.points.map((point) => [coordinate(point), point]));
  const pointsEqual = candidate.points.every((point) => {
    const other = baselinePoints.get(coordinate(point));
    return (
      other !== undefined &&
      domainKey(point.coverage_domain) === domainKey(other.coverage_domain) &&
      point.adapter_id === other.adapter_id &&
      point.source_instance_key === other.source_instance_key &&
      JSON.stringify(point.position) === JSON.stringify(other.position) &&
      JSON.stringify(point.status) === JSON.stringify(other.status)
    );
  });
  const valuesEqual = (left: unknown[], right: unknown[]): boolean => {
    const normalized = (values: unknown[]) => values.map((value) => JSON.stringify(value)).sort();
    return JSON.stringify(normalized(left)) === JSON.stringify(normalized(right));
  };
  return (
    pointsEqual &&
    valuesEqual(candidate.explicit_absence_or_deletion, baseline.explicit_absence_or_deletion) &&
    valuesEqual(candidate.explicit_errors, baseline.explicit_errors)
  );
}

export function compareCoverage(
  candidateInput: SourceCoverageSet,
  baselineInput: SourceCoverageSet,
): CoverageComparison {
  const candidate = parseSourceCoverageSet(candidateInput);
  const baseline = parseSourceCoverageSet(baselineInput);
  if (
    domainKey(candidate.coverage_domain) !== domainKey(baseline.coverage_domain) ||
    scopeKey(candidate.scope) !== scopeKey(baseline.scope) ||
    candidate.membership_revision !== baseline.membership_revision
  ) {
    return 'incomparable';
  }
  if (coverageSemanticallyEqual(candidate, baseline)) return 'equal';
  const candidateDominates = setDominates(candidate, baseline);
  const baselineDominates = setDominates(baseline, candidate);
  if (candidateDominates && baselineDominates) return 'equal';
  if (candidateDominates) return 'dominates';
  if (baselineDominates) return 'behind';
  return 'incomparable';
}

function parseCoverageComparison(value: unknown, label: string): CoverageComparison {
  if (value !== 'equal' && value !== 'dominates' && value !== 'behind' && value !== 'incomparable') {
    throw new ContractValidationError(`${label} has an unsupported coverage comparison`);
  }
  return value;
}

function parseKnownNonNegativeInteger(value: unknown, label: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0 || Object.is(value, -0)) {
    throw new ContractValidationError(`${label} must be a non-negative safe integer`);
  }
  return value;
}

const RFC012A_STRING_QUALIFIED_DECODERS = {
  parseAuthority: parseStringAuthority,
  parseProvenance: parseSemanticRevisionProvenance,
} as const;

export function parseRfc012aV1Json(json: string): Rfc012aV1Fixture {
  return parseRfc012aV1Fixture(preflightSemanticFixtureJson(json));
}

export function parseRfc012aV1Fixture(value: unknown): Rfc012aV1Fixture {
  assertSemanticFixtureGraph(value);
  const input = record(value, 'RFC 012A fixture');
  assertKnownFields(
    input,
    [
      'fixture_contract_version',
      'canonical_source_instance_key',
      'external_entity_ref',
      'native_identity_claim',
      'semantic_revision_ref',
      'qualified_known_zero',
      'qualified_unknown',
      'coverage',
    ],
    'RFC 012A fixture',
  );
  if (input.fixture_contract_version !== RFC012A_FIXTURE_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported RFC 012A fixture contract version');
  }
  const canonicalSourceInstanceKey = parseOpaqueContractReference(
    input.canonical_source_instance_key,
    'canonical source instance key',
  );
  const coverageInput = record(input.coverage, 'RFC 012A coverage');
  assertKnownFields(coverageInput, ['baseline', 'dominant', 'reset', 'expected'], 'RFC 012A coverage');
  const baseline = parseSourceCoverageSet(coverageInput.baseline);
  const dominant = parseSourceCoverageSet(coverageInput.dominant);
  const reset = parseSourceCoverageSet(coverageInput.reset);
  if (
    canonicalSourceInstanceKey !== baseline.scope.source_instance_key ||
    canonicalSourceInstanceKey !== dominant.scope.source_instance_key ||
    canonicalSourceInstanceKey !== reset.scope.source_instance_key
  ) {
    throw new ContractValidationError('coverage scopes must use the fixture source instance');
  }
  const expectedInput = record(coverageInput.expected, 'RFC 012A coverage expected');
  assertKnownFields(
    expectedInput,
    ['dominant_vs_baseline', 'baseline_vs_dominant', 'reset_vs_baseline'],
    'RFC 012A coverage expected',
  );
  const expected: Rfc012aCoverageExpected = {
    dominant_vs_baseline: parseCoverageComparison(expectedInput.dominant_vs_baseline, 'dominant_vs_baseline'),
    baseline_vs_dominant: parseCoverageComparison(expectedInput.baseline_vs_dominant, 'baseline_vs_dominant'),
    reset_vs_baseline: parseCoverageComparison(expectedInput.reset_vs_baseline, 'reset_vs_baseline'),
  };
  if (compareCoverage(dominant, baseline) !== expected.dominant_vs_baseline) {
    throw new ContractValidationError('coverage comparison outcomes do not match the fixture');
  }
  if (compareCoverage(baseline, dominant) !== expected.baseline_vs_dominant) {
    throw new ContractValidationError('coverage comparison outcomes do not match the fixture');
  }
  if (compareCoverage(reset, baseline) !== expected.reset_vs_baseline) {
    throw new ContractValidationError('coverage comparison outcomes do not match the fixture');
  }

  const externalEntityRef = parseExternalEntityRef(input.external_entity_ref);
  const nativeIdentityClaim = parseNativeIdentityClaim(input.native_identity_claim);
  if (
    nativeIdentityClaim.entity_ref.entity_key !== externalEntityRef.entity_key ||
    nativeIdentityClaim.entity_ref.external_entity_reference_version !==
      externalEntityRef.external_entity_reference_version
  ) {
    throw new ContractValidationError('native identity claim must use the fixture external entity reference');
  }
  const qualifiedKnownZero = parseQualifiedValue<number, string, SemanticRevisionRef[]>(input.qualified_known_zero, {
    parseKnownValue: parseKnownNonNegativeInteger,
    ...RFC012A_STRING_QUALIFIED_DECODERS,
  });
  if (qualifiedKnownZero.value !== 0) {
    throw new ContractValidationError('qualified known zero must preserve exact zero');
  }
  const qualifiedUnknown = parseQualifiedValue<string, string, SemanticRevisionRef[]>(input.qualified_unknown, {
    parseKnownValue: (raw, label) => nonEmptyString(raw, label),
    ...RFC012A_STRING_QUALIFIED_DECODERS,
  });
  if (qualifiedUnknown.value !== null) {
    throw new ContractValidationError('qualified unknown must keep a null value');
  }
  const semanticRevisionRef = parseSemanticRevisionRef(input.semantic_revision_ref);
  const sameRef = (value: SemanticRevisionRef): boolean =>
    value.semantic_reference_contract_version === semanticRevisionRef.semantic_reference_contract_version &&
    value.fact_revision_id === semanticRevisionRef.fact_revision_id;
  if (
    nativeIdentityClaim.identity.provenance.length !== 1 ||
    nativeIdentityClaim.identity.provenance[0] === undefined ||
    !sameRef(nativeIdentityClaim.identity.provenance[0])
  ) {
    throw new ContractValidationError('native identity provenance must bind the fixture semantic revision reference');
  }
  if (
    qualifiedKnownZero.provenance.length !== 1 ||
    qualifiedKnownZero.provenance[0] === undefined ||
    !sameRef(qualifiedKnownZero.provenance[0])
  ) {
    throw new ContractValidationError(
      'qualified known zero provenance must bind the fixture semantic revision reference',
    );
  }
  if (qualifiedUnknown.provenance.length !== 0) {
    throw new ContractValidationError('qualified unknown provenance must remain empty');
  }

  return {
    fixture_contract_version: RFC012A_FIXTURE_CONTRACT_VERSION,
    canonical_source_instance_key: canonicalSourceInstanceKey,
    external_entity_ref: externalEntityRef,
    native_identity_claim: nativeIdentityClaim,
    semantic_revision_ref: semanticRevisionRef,
    qualified_known_zero: qualifiedKnownZero,
    qualified_unknown: qualifiedUnknown,
    coverage: { baseline, dominant, reset, expected },
  };
}

export type TypedAccessRequestOperation = 'catalog_discovery' | 'durable_history_runtime' | 'scoped_typed_observation';

export interface DeclaredKnownObjectGrant {
  relation_id: string;
  scope_root: boolean;
  access_root: string;
  identity_input_names: string[];
}

export interface NativeProbeGrantRequest {
  access_request_contract_version: typeof ACCESS_REQUEST_CONTRACT_VERSION;
  adapter_id: string;
  support_release_id: string;
  support_release_digest: number[];
  source_declaration_digest: number[];
  scope_program_digest: number[];
  declaration_id: string;
  program_id: string;
  capability_topology: SupportCapabilityTopology;
  operation: TypedAccessRequestOperation;
  selection: ContractVersionSelection;
  access_policy_digest: number[];
  probe: NativeArtifactProbe;
  grants: DeclaredKnownObjectGrant[];
  digest: number[];
}

export interface AccessReportRetrievalRequest {
  access_report_retrieval_contract_version: typeof ACCESS_REPORT_RETRIEVAL_CONTRACT_VERSION;
  adapter_id: string;
  support_release_id: string;
  support_release_digest: number[];
  source_declaration_digest: number[];
  scope_program_digest: number[];
  declaration_id: string;
  program_id: string;
  capability_topology: SupportCapabilityTopology;
  operation: TypedAccessRequestOperation;
  selection: ContractVersionSelection;
  access_policy_digest: number[];
  expected_report_digest: number[];
  digest: number[];
}

const ACCESS_REQUEST_IDENTIFIER_BYTES = 128;
const MAX_ACCESS_REQUEST_GRANTS = 256;
const MAX_ACCESS_REQUEST_IDENTITY_INPUTS = 32;
const MAX_ACCESS_REQUEST_MARKERS = 64;
const MAX_ACCESS_REQUEST_FACT_FAMILIES = 64;
const MAX_ACCESS_REQUEST_ENCODED_BYTES = 64 * 1024;
const REQUEST_MACHINE_ID = /^[a-z0-9][a-z0-9._-]{0,127}$/;
const ACCESS_REQUEST_OPERATION_TOPOLOGY: Record<TypedAccessRequestOperation, SupportCapabilityTopology> = {
  catalog_discovery: 'catalog',
  durable_history_runtime: 'durable',
  scoped_typed_observation: 'scoped',
};
const ACCESS_REQUEST_TOPOLOGY_CODES: Record<SupportCapabilityTopology, number> = {
  catalog: 1,
  durable: 2,
  scoped: 3,
};
const ACCESS_REQUEST_OPERATION_CODES: Record<TypedAccessRequestOperation, number> = {
  catalog_discovery: 1,
  durable_history_runtime: 2,
  scoped_typed_observation: 3,
};

function rightRotate(value: number, bits: number): number {
  return ((value >>> bits) | (value << (32 - bits))) >>> 0;
}

function sha256(message: Uint8Array): Uint8Array {
  const k = new Uint32Array([
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
    0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
    0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
    0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
    0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
    0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
    0xc67178f2,
  ]);
  const bitLength = message.byteLength * 8;
  const zeroPad = (64 - ((message.byteLength + 9) % 64)) % 64;
  const padded = new Uint8Array(message.byteLength + 1 + zeroPad + 8);
  padded.set(message);
  padded[message.byteLength] = 0x80;
  const view = new DataView(padded.buffer);
  view.setUint32(padded.byteLength - 8, Math.floor(bitLength / 0x1_0000_0000), false);
  view.setUint32(padded.byteLength - 4, bitLength >>> 0, false);
  let h0 = 0x6a09e667;
  let h1 = 0xbb67ae85;
  let h2 = 0x3c6ef372;
  let h3 = 0xa54ff53a;
  let h4 = 0x510e527f;
  let h5 = 0x9b05688c;
  let h6 = 0x1f83d9ab;
  let h7 = 0x5be0cd19;
  const words = new Uint32Array(64);
  for (let offset = 0; offset < padded.byteLength; offset += 64) {
    for (let index = 0; index < 16; index += 1) {
      words[index] = view.getUint32(offset + index * 4, false);
    }
    for (let index = 16; index < 64; index += 1) {
      const source15 = words[index - 15]!;
      const source2 = words[index - 2]!;
      const sigma0 = rightRotate(source15, 7) ^ rightRotate(source15, 18) ^ (source15 >>> 3);
      const sigma1 = rightRotate(source2, 17) ^ rightRotate(source2, 19) ^ (source2 >>> 10);
      words[index] = (words[index - 16]! + sigma0 + words[index - 7]! + sigma1) >>> 0;
    }
    let a = h0;
    let b = h1;
    let c = h2;
    let d = h3;
    let e = h4;
    let f = h5;
    let g = h6;
    let h = h7;
    for (let index = 0; index < 64; index += 1) {
      const sum1 = rightRotate(e, 6) ^ rightRotate(e, 11) ^ rightRotate(e, 25);
      const choose = (e & f) ^ (~e & g);
      const temp1 = (h + sum1 + choose + k[index]! + words[index]!) >>> 0;
      const sum0 = rightRotate(a, 2) ^ rightRotate(a, 13) ^ rightRotate(a, 22);
      const majority = (a & b) ^ (a & c) ^ (b & c);
      const temp2 = (sum0 + majority) >>> 0;
      h = g;
      g = f;
      f = e;
      e = (d + temp1) >>> 0;
      d = c;
      c = b;
      b = a;
      a = (temp1 + temp2) >>> 0;
    }
    h0 = (h0 + a) >>> 0;
    h1 = (h1 + b) >>> 0;
    h2 = (h2 + c) >>> 0;
    h3 = (h3 + d) >>> 0;
    h4 = (h4 + e) >>> 0;
    h5 = (h5 + f) >>> 0;
    h6 = (h6 + g) >>> 0;
    h7 = (h7 + h) >>> 0;
  }
  const digest = new Uint8Array(32);
  const digestView = new DataView(digest.buffer);
  digestView.setUint32(0, h0, false);
  digestView.setUint32(4, h1, false);
  digestView.setUint32(8, h2, false);
  digestView.setUint32(12, h3, false);
  digestView.setUint32(16, h4, false);
  digestView.setUint32(20, h5, false);
  digestView.setUint32(24, h6, false);
  digestView.setUint32(28, h7, false);
  return digest;
}

function sha256Hex(message: Uint8Array): string {
  return [...sha256(message)].map((byte) => byte.toString(16).padStart(2, '0')).join('');
}

function digestFieldHex(bytes: number[]): string {
  return bytes.map((byte) => byte.toString(16).padStart(2, '0')).join('');
}

class ContractByteSink {
  private readonly parts: Uint8Array[] = [];

  u8(value: number): void {
    this.parts.push(Uint8Array.of(value));
  }

  u32(value: number): void {
    const encoded = new Uint8Array(4);
    new DataView(encoded.buffer).setUint32(0, value, false);
    this.parts.push(encoded);
  }

  u64(value: number): void {
    if (!Number.isSafeInteger(value) || value < 0) {
      throw new ContractValidationError('request digest length is not a safe non-negative integer');
    }
    const encoded = new Uint8Array(8);
    const view = new DataView(encoded.buffer);
    view.setUint32(0, Math.floor(value / 0x1_0000_0000), false);
    view.setUint32(4, value >>> 0, false);
    this.parts.push(encoded);
  }

  bytes(value: Uint8Array): void {
    this.u64(value.byteLength);
    this.parts.push(value);
  }

  text(value: string): void {
    this.bytes(UTF8_ENCODER.encode(value));
  }

  digest(value: number[]): void {
    this.bytes(Uint8Array.from(value));
  }

  finish(): Uint8Array {
    const total = this.parts.reduce((sum, part) => sum + part.byteLength, 0);
    const encoded = new Uint8Array(total);
    let offset = 0;
    for (const part of this.parts) {
      encoded.set(part, offset);
      offset += part.byteLength;
    }
    return encoded;
  }
}

function encodeAccessRequestCoordinates(
  sink: ContractByteSink,
  contractVersion: number,
  request: {
    adapter_id: string;
    support_release_id: string;
    support_release_digest: number[];
    source_declaration_digest: number[];
    scope_program_digest: number[];
    declaration_id: string;
    program_id: string;
    capability_topology: SupportCapabilityTopology;
    operation: TypedAccessRequestOperation;
    selection: ContractVersionSelection;
    access_policy_digest: number[];
  },
): void {
  sink.u32(contractVersion);
  sink.text(request.adapter_id);
  sink.text(request.support_release_id);
  sink.digest(request.support_release_digest);
  sink.digest(request.source_declaration_digest);
  sink.digest(request.scope_program_digest);
  sink.text(request.declaration_id);
  sink.text(request.program_id);
  sink.u8(ACCESS_REQUEST_TOPOLOGY_CODES[request.capability_topology]);
  sink.u8(ACCESS_REQUEST_OPERATION_CODES[request.operation]);
  sink.u32(request.selection.selection_contract_version);
  sink.u32(request.selection.model_major);
  sink.u32(request.selection.external_entity_reference_version);
  sink.u32(request.selection.semantic_revision_reference_version);
  sink.u32(request.selection.coverage_contract_version);
  const families = Object.keys(request.selection.fact_family_versions).sort();
  sink.u64(families.length);
  for (const family of families) {
    sink.text(family);
    sink.u32(request.selection.fact_family_versions[family]!);
  }
  for (const version of [request.selection.query_pack_version, request.selection.observation_contract_version]) {
    if (version === null) {
      sink.u8(0);
    } else {
      sink.u8(1);
      sink.u32(version);
    }
  }
  sink.digest(request.access_policy_digest);
}

function nativeProbeGrantRequestDigest(request: NativeProbeGrantRequest): string {
  return `sha256:${sha256Hex(encodeNativeProbeGrantRequest(request))}`;
}

function encodeNativeProbeGrantRequest(request: NativeProbeGrantRequest): Uint8Array {
  const sink = new ContractByteSink();
  const domain = UTF8_ENCODER.encode('spaghetti/rfc012a/native-probe-grant-request/v1\0');
  const body = new ContractByteSink();
  encodeAccessRequestCoordinates(body, request.access_request_contract_version, request);
  body.text(request.probe.family);
  body.text(request.probe.platform);
  if (request.probe.version === null) {
    body.u8(0);
  } else {
    body.u8(1);
    body.text(request.probe.version);
  }
  const markers = [...new Set(request.probe.markers)].sort();
  body.u64(markers.length);
  for (const marker of markers) body.text(marker);
  body.u8(request.probe.contradictory_markers ? 1 : 0);
  body.u64(request.grants.length);
  for (const grant of request.grants) {
    body.text(grant.relation_id);
    body.u8(grant.scope_root ? 1 : 0);
    body.text(grant.access_root);
    body.u64(grant.identity_input_names.length);
    for (const name of grant.identity_input_names) body.text(name);
  }
  const bodyBytes = body.finish();
  const encoded = new Uint8Array(domain.byteLength + bodyBytes.byteLength);
  encoded.set(domain);
  encoded.set(bodyBytes, domain.byteLength);
  return encoded;
}

function accessReportRetrievalDigest(request: AccessReportRetrievalRequest): string {
  const domain = UTF8_ENCODER.encode('spaghetti/rfc012a/access-report-retrieval/v1\0');
  const body = new ContractByteSink();
  encodeAccessRequestCoordinates(body, request.access_report_retrieval_contract_version, request);
  body.digest(request.expected_report_digest);
  const bodyBytes = body.finish();
  const encoded = new Uint8Array(domain.byteLength + bodyBytes.byteLength);
  encoded.set(domain);
  encoded.set(bodyBytes, domain.byteLength);
  return `sha256:${sha256Hex(encoded)}`;
}

function requireMatchingDigest(actual: number[], expectedHex: string, label: string): void {
  if (`sha256:${digestFieldHex(actual)}` !== expectedHex) {
    throw new ContractValidationError(`${label} digest does not match its canonical encoding`);
  }
}

function requestMachineId(value: unknown, label: string): string {
  if (typeof value !== 'string') {
    throw new ContractValidationError(`${label} must be a machine identifier`);
  }
  if (value.length === 0 || value.length > ACCESS_REQUEST_IDENTIFIER_BYTES || !REQUEST_MACHINE_ID.test(value)) {
    throw new ContractValidationError(`${label} must be a machine identifier`);
  }
  if (UTF8_ENCODER.encode(value).length > ACCESS_REQUEST_IDENTIFIER_BYTES) {
    throw new ContractValidationError(`${label} must be a machine identifier`);
  }
  return value;
}

function chargeEncodedBytes(budget: { used: number }, value: string): void {
  budget.used += value.length;
  if (budget.used > MAX_ACCESS_REQUEST_ENCODED_BYTES) {
    throw new ContractValidationError('access request exceeds the encoded-byte limit');
  }
}

function preflightGrantEncodedBytes(grants: unknown[]): void {
  let used = 0;
  for (const grant of grants) {
    const input = record(grant, 'declared known-object grant');
    for (const [field, label] of [
      ['relation_id', 'grant relation id'],
      ['access_root', 'grant access root'],
    ] as const) {
      const value = input[field];
      if (typeof value === 'string') {
        if (value.length > ACCESS_REQUEST_IDENTIFIER_BYTES) {
          throw new ContractValidationError(`${label} must be a machine identifier`);
        }
        used += value.length;
        if (used > MAX_ACCESS_REQUEST_ENCODED_BYTES) {
          throw new ContractValidationError('access request exceeds the encoded-byte limit');
        }
      }
    }
    const names = input.identity_input_names;
    if (!Array.isArray(names)) continue;
    if (names.length > MAX_ACCESS_REQUEST_IDENTITY_INPUTS) {
      throw new ContractValidationError('grant identity inputs exceed the collection limit');
    }
    for (const name of names) {
      if (typeof name === 'string') {
        if (name.length > ACCESS_REQUEST_IDENTIFIER_BYTES) {
          throw new ContractValidationError('grant identity input must be a machine identifier');
        }
        used += name.length;
        if (used > MAX_ACCESS_REQUEST_ENCODED_BYTES) {
          throw new ContractValidationError('access request exceeds the encoded-byte limit');
        }
      }
    }
  }
}

function requiredNullableString(input: UnknownRecord, field: string, label: string): string | null {
  if (!hasOwn(input, field) || input[field] === undefined) {
    throw new ContractValidationError(`${label} is missing a required field`);
  }
  if (input[field] === null) return null;
  return requestMachineId(input[field], label);
}

function requiredNullableVersion(input: UnknownRecord, field: string, label: string): number | null {
  if (!hasOwn(input, field) || input[field] === undefined) {
    throw new ContractValidationError(`${label} is missing a required field`);
  }
  if (input[field] === null) return null;
  return contractVersion(input[field], label);
}

function digest32(value: unknown, label: string): number[] {
  if (!Array.isArray(value) || value.length !== 32) {
    throw new ContractValidationError(`${label} must contain 32 bytes`);
  }
  const bytes: number[] = [];
  for (let index = 0; index < 32; index += 1) {
    if (!Object.prototype.hasOwnProperty.call(value, index)) {
      throw new ContractValidationError(`${label} must contain 32 bytes`);
    }
    const item = value[index];
    if (!Number.isInteger(item) || Number(item) < 0 || Number(item) > 255) {
      throw new ContractValidationError(`${label} must contain 32 bytes`);
    }
    bytes.push(Number(item));
  }
  return bytes;
}

function nonzeroDigest32(value: unknown, label: string): number[] {
  const bytes = digest32(value, label);
  if (bytes.every((item) => item === 0)) {
    throw new ContractValidationError(`${label} must be a nonzero 32-byte digest`);
  }
  return bytes;
}

function parseRequestOperation(value: unknown): TypedAccessRequestOperation {
  if (value !== 'catalog_discovery' && value !== 'durable_history_runtime' && value !== 'scoped_typed_observation') {
    throw new ContractValidationError('unsupported request operation');
  }
  return value;
}

function parseRequestTopology(value: unknown): SupportCapabilityTopology {
  if (value !== 'catalog' && value !== 'durable' && value !== 'scoped') {
    throw new ContractValidationError('unsupported request capability topology');
  }
  return value;
}

function parseDeclaredGrant(value: unknown, encoded: { used: number }): DeclaredKnownObjectGrant {
  const input = record(value, 'declared known-object grant');
  assertKnownFields(
    input,
    ['relation_id', 'scope_root', 'access_root', 'identity_input_names'],
    'declared known-object grant',
  );
  assertRequiredFields(
    input,
    ['relation_id', 'scope_root', 'access_root', 'identity_input_names'],
    'declared known-object grant',
  );
  if (typeof input.scope_root !== 'boolean') {
    throw new ContractValidationError('grant scope_root must be boolean');
  }
  if (!Array.isArray(input.identity_input_names)) {
    throw new ContractValidationError('grant identity input list must not be empty');
  }
  if (input.identity_input_names.length === 0) {
    throw new ContractValidationError('grant identity input list must not be empty');
  }
  if (input.identity_input_names.length > MAX_ACCESS_REQUEST_IDENTITY_INPUTS) {
    throw new ContractValidationError('grant identity inputs exceed the collection limit');
  }
  const relationId = requestMachineId(input.relation_id, 'grant relation id');
  const accessRoot = requestMachineId(input.access_root, 'grant access root');
  chargeEncodedBytes(encoded, relationId);
  chargeEncodedBytes(encoded, accessRoot);
  const identityInputNames: string[] = [];
  for (const name of input.identity_input_names) {
    const parsed = requestMachineId(name, 'grant identity input');
    chargeEncodedBytes(encoded, parsed);
    identityInputNames.push(parsed);
  }
  if (new Set(identityInputNames).size !== identityInputNames.length) {
    throw new ContractValidationError('grant identity input contains duplicate value');
  }
  return {
    relation_id: relationId,
    scope_root: input.scope_root,
    access_root: accessRoot,
    identity_input_names: identityInputNames,
  };
}

function parseRequestProbe(value: unknown): NativeArtifactProbe {
  const input = record(value, 'native artifact probe');
  assertKnownFields(
    input,
    ['family', 'platform', 'version', 'markers', 'contradictory_markers'],
    'native artifact probe',
  );
  assertRequiredFields(
    input,
    ['family', 'platform', 'version', 'markers', 'contradictory_markers'],
    'native artifact probe',
  );
  if (!Array.isArray(input.markers)) {
    throw new ContractValidationError('probed native markers must be an array');
  }
  if (input.markers.length > MAX_ACCESS_REQUEST_MARKERS) {
    throw new ContractValidationError('native probe exceeds the marker collection limit');
  }
  if (typeof input.contradictory_markers !== 'boolean') {
    throw new ContractValidationError('contradictory_markers must be boolean');
  }
  const encoded = { used: 0 };
  const family = requestMachineId(input.family, 'probed artifact family');
  const platform = requestMachineId(input.platform, 'probed artifact platform');
  chargeEncodedBytes(encoded, family);
  chargeEncodedBytes(encoded, platform);
  const version = requiredNullableString(input, 'version', 'probed artifact version');
  if (version !== null) {
    chargeEncodedBytes(encoded, version);
  }
  const markers: string[] = [];
  for (const marker of input.markers) {
    const parsed = requestMachineId(marker, 'probed native marker');
    chargeEncodedBytes(encoded, parsed);
    markers.push(parsed);
  }
  markers.sort();
  const unique = [...new Set(markers)];
  return {
    family,
    platform,
    version,
    markers: unique,
    contradictory_markers: input.contradictory_markers,
  };
}

function parseRequestSelection(value: unknown, operation: TypedAccessRequestOperation): ContractVersionSelection {
  const input = record(value, 'contract version selection');
  assertKnownFields(
    input,
    [
      'selection_contract_version',
      'model_major',
      'external_entity_reference_version',
      'semantic_revision_reference_version',
      'coverage_contract_version',
      'fact_family_versions',
      'query_pack_version',
      'observation_contract_version',
    ],
    'contract version selection',
  );
  assertRequiredFields(
    input,
    [
      'selection_contract_version',
      'model_major',
      'external_entity_reference_version',
      'semantic_revision_reference_version',
      'coverage_contract_version',
      'fact_family_versions',
      'query_pack_version',
      'observation_contract_version',
    ],
    'contract version selection',
  );
  const familiesInput = record(input.fact_family_versions, 'selected fact families');
  let familyCount = 0;
  for (const family in familiesInput) {
    if (!hasOwn(familiesInput, family)) continue;
    familyCount += 1;
    if (familyCount > MAX_ACCESS_REQUEST_FACT_FAMILIES) {
      throw new ContractValidationError('selected fact families exceed the collection limit');
    }
  }
  const encoded = { used: 0 };
  const factFamilyVersions: Record<string, number> = {};
  for (const family in familiesInput) {
    if (!hasOwn(familiesInput, family)) continue;
    const parsedFamily = requestMachineId(family, 'selected fact family');
    chargeEncodedBytes(encoded, parsedFamily);
    factFamilyVersions[parsedFamily] = contractVersion(familiesInput[family], 'selected fact-family version');
  }
  const selection: ContractVersionSelection = {
    selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
    model_major: contractVersion(input.model_major, 'selected model major'),
    external_entity_reference_version: contractVersion(
      input.external_entity_reference_version,
      'selected external entity reference version',
    ),
    semantic_revision_reference_version: contractVersion(
      input.semantic_revision_reference_version,
      'selected semantic revision reference version',
    ),
    coverage_contract_version: contractVersion(input.coverage_contract_version, 'selected coverage contract version'),
    fact_family_versions: factFamilyVersions,
    query_pack_version: requiredNullableVersion(input, 'query_pack_version', 'selected query pack version'),
    observation_contract_version: requiredNullableVersion(
      input,
      'observation_contract_version',
      'selected observation contract version',
    ),
  };
  if (input.selection_contract_version !== CONTRACT_VERSION_SELECTION_VERSION) {
    throw new ContractValidationError('unsupported contract-version selection version');
  }
  if (operation === 'catalog_discovery' && selection.query_pack_version === null) {
    throw new ContractValidationError('catalog discovery requires a negotiated query-pack contract');
  }
  if (operation === 'scoped_typed_observation' && selection.observation_contract_version === null) {
    throw new ContractValidationError('scoped probe/grant request requires a negotiated observation contract');
  }
  return selection;
}

function parseRequestCoordinates(
  input: UnknownRecord,
  requireProgram: boolean,
): {
  adapter_id: string;
  support_release_id: string;
  support_release_digest: number[];
  source_declaration_digest: number[];
  scope_program_digest: number[];
  declaration_id: string;
  program_id: string;
  capability_topology: SupportCapabilityTopology;
  operation: TypedAccessRequestOperation;
  selection: ContractVersionSelection;
  access_policy_digest: number[];
} {
  const operation = parseRequestOperation(input.operation);
  const capabilityTopology = parseRequestTopology(input.capability_topology);
  if (ACCESS_REQUEST_OPERATION_TOPOLOGY[operation] !== capabilityTopology) {
    throw new ContractValidationError('probe/grant request topology does not match its operation');
  }
  const programId = input.program_id;
  if (typeof programId !== 'string') {
    throw new ContractValidationError('request program id must be a string');
  }
  if (requireProgram || operation === 'scoped_typed_observation') {
    requestMachineId(programId, 'request program id');
  } else if (programId !== '') {
    throw new ContractValidationError('catalog and durable probe/grant requests cannot carry grants or a program id');
  }
  return {
    adapter_id: requestMachineId(input.adapter_id, 'request adapter id'),
    support_release_id: requestMachineId(input.support_release_id, 'request support release id'),
    support_release_digest: nonzeroDigest32(input.support_release_digest, 'support release digest'),
    source_declaration_digest: nonzeroDigest32(input.source_declaration_digest, 'source declaration digest'),
    scope_program_digest: nonzeroDigest32(input.scope_program_digest, 'scope program digest'),
    declaration_id: requestMachineId(input.declaration_id, 'request declaration id'),
    program_id: programId,
    capability_topology: capabilityTopology,
    operation,
    selection: parseRequestSelection(input.selection, operation),
    access_policy_digest: nonzeroDigest32(input.access_policy_digest, 'access policy digest'),
  };
}

export function parseNativeProbeGrantRequest(value: unknown): NativeProbeGrantRequest {
  const input = record(value, 'native-probe/grant request');
  assertKnownFields(
    input,
    [
      'access_request_contract_version',
      'adapter_id',
      'support_release_id',
      'support_release_digest',
      'source_declaration_digest',
      'scope_program_digest',
      'declaration_id',
      'program_id',
      'capability_topology',
      'operation',
      'selection',
      'access_policy_digest',
      'probe',
      'grants',
      'digest',
    ],
    'native-probe/grant request',
  );
  assertRequiredFields(
    input,
    [
      'access_request_contract_version',
      'adapter_id',
      'support_release_id',
      'support_release_digest',
      'source_declaration_digest',
      'scope_program_digest',
      'declaration_id',
      'program_id',
      'capability_topology',
      'operation',
      'selection',
      'access_policy_digest',
      'probe',
      'grants',
      'digest',
    ],
    'native-probe/grant request',
  );
  if (input.access_request_contract_version !== ACCESS_REQUEST_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported native-probe/grant request contract version');
  }
  const coordinates = parseRequestCoordinates(input, false);
  if (!Array.isArray(input.grants)) {
    throw new ContractValidationError('probe/grant request grants must be an array');
  }
  if (input.grants.length > MAX_ACCESS_REQUEST_GRANTS) {
    throw new ContractValidationError('probe/grant request exceeds the grant collection limit');
  }
  preflightGrantEncodedBytes(input.grants);
  if (coordinates.operation === 'scoped_typed_observation') {
    if (input.grants.length === 0) {
      throw new ContractValidationError('scoped probe/grant request requires a bounded nonempty grant set');
    }
    if (coordinates.selection.observation_contract_version === null) {
      throw new ContractValidationError('scoped probe/grant request requires a negotiated observation contract');
    }
  } else if (input.grants.length !== 0) {
    throw new ContractValidationError('catalog and durable probe/grant requests cannot carry grants or a program id');
  }
  const encoded = { used: 0 };
  const grants = input.grants.map((grant) => parseDeclaredGrant(grant, encoded));
  let previous: string | undefined;
  let rootCount = 0;
  for (const grant of grants) {
    if (previous !== undefined && previous >= grant.relation_id) {
      throw new ContractValidationError('probe/grant relation ids must be strictly increasing');
    }
    previous = grant.relation_id;
    if (grant.scope_root) rootCount += 1;
  }
  if (coordinates.operation === 'scoped_typed_observation' && rootCount !== 1) {
    throw new ContractValidationError('scoped probe/grant request requires exactly one scope-root grant');
  }
  const parsed: NativeProbeGrantRequest = {
    access_request_contract_version: ACCESS_REQUEST_CONTRACT_VERSION,
    ...coordinates,
    probe: parseRequestProbe(input.probe),
    grants,
    digest: digest32(input.digest, 'request digest'),
  };
  requireMatchingDigest(parsed.digest, nativeProbeGrantRequestDigest(parsed), 'native-probe/grant request');
  return parsed;
}

export function parseAccessReportRetrieval(value: unknown): AccessReportRetrievalRequest {
  const input = record(value, 'access-report retrieval request');
  assertKnownFields(
    input,
    [
      'access_report_retrieval_contract_version',
      'adapter_id',
      'support_release_id',
      'support_release_digest',
      'source_declaration_digest',
      'scope_program_digest',
      'declaration_id',
      'program_id',
      'capability_topology',
      'operation',
      'selection',
      'access_policy_digest',
      'expected_report_digest',
      'digest',
    ],
    'access-report retrieval request',
  );
  assertRequiredFields(
    input,
    [
      'access_report_retrieval_contract_version',
      'adapter_id',
      'support_release_id',
      'support_release_digest',
      'source_declaration_digest',
      'scope_program_digest',
      'declaration_id',
      'program_id',
      'capability_topology',
      'operation',
      'selection',
      'access_policy_digest',
      'expected_report_digest',
      'digest',
    ],
    'access-report retrieval request',
  );
  if (input.access_report_retrieval_contract_version !== ACCESS_REPORT_RETRIEVAL_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported access-report retrieval contract version');
  }
  const coordinates = parseRequestCoordinates(input, true);
  if (coordinates.capability_topology !== 'scoped' || coordinates.operation !== 'scoped_typed_observation') {
    throw new ContractValidationError('access-report retrieval is scoped observation only');
  }
  if (coordinates.selection.observation_contract_version === null) {
    throw new ContractValidationError('access-report retrieval requires a negotiated observation contract');
  }
  const parsed: AccessReportRetrievalRequest = {
    access_report_retrieval_contract_version: ACCESS_REPORT_RETRIEVAL_CONTRACT_VERSION,
    ...coordinates,
    expected_report_digest: nonzeroDigest32(input.expected_report_digest, 'expected report digest'),
    digest: digest32(input.digest, 'request digest'),
  };
  requireMatchingDigest(parsed.digest, accessReportRetrievalDigest(parsed), 'access-report retrieval');
  return parsed;
}
