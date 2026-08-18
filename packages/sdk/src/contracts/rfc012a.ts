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
  identity: QualifiedValue<NativeIdentity, unknown, unknown>;
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

export class ContractValidationError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'ContractValidationError';
  }
}

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

function assertKnownFields(input: UnknownRecord, fields: readonly string[], label: string): void {
  const known = new Set(fields);
  for (const key of Object.keys(input)) {
    if (!known.has(key)) throw new ContractValidationError(`${label} contains unknown field ${key}`);
  }
}

function nonEmptyString(value: unknown, label: string): string {
  if (typeof value !== 'string' || value.length === 0 || value.trim() !== value) {
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
  if (!Number.isSafeInteger(value)) {
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
  if (typeof version === 'string' && (version.length === 0 || version.length > 128 || version.trim() !== version)) {
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

export function parseQualifiedValue<T = unknown, A = unknown, P = unknown>(value: unknown): QualifiedValue<T, A, P> {
  const input = record(value, 'qualified value');
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
  const isUnknown = quality === 'unknown';
  if (isUnknown !== (input.value === null)) {
    throw new ContractValidationError('quality is unknown if and only if value is null');
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
  if (input.effective_at !== undefined) safeInteger(input.effective_at, 'effective_at');
  return input as unknown as QualifiedValue<T, A, P>;
}

export function parseNativeIdentityClaim(value: unknown): NativeIdentityClaim {
  const input = record(value, 'native identity claim');
  const identity = parseQualifiedValue<NativeIdentity>(input.identity);
  const parsedIdentity =
    identity.value === null
      ? null
      : (() => {
          const nativeIdentity = record(identity.value, 'native identity');
          return {
            native_namespace: nonEmptyString(nativeIdentity.native_namespace, 'native identity namespace'),
            native_id: nonEmptyString(nativeIdentity.native_id, 'native identity'),
          };
        })();
  return {
    entity_ref: parseExternalEntityRef(input.entity_ref),
    identity: { ...identity, value: parsedIdentity },
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
