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

export type SupportReleaseStatus = 'candidate' | 'promoted' | 'retired';

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
  return value as UnknownRecord;
}

function nonEmptyString(value: unknown, label: string): string {
  if (typeof value !== 'string' || value.length === 0 || value.trim() !== value) {
    throw new ContractValidationError(`${label} must be a non-empty canonical string`);
  }
  return value;
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
  const result = value.map((entry) => positiveInteger(entry, label));
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
  const exactVersions = canonicalStringList(compatibility.exact_versions, 'exact artifact versions', false);
  if (exactVersions.some((version) => version.length > 128)) {
    throw new ContractValidationError('exact artifact version exceeds 128 bytes');
  }
  return {
    support_release_id: nonEmptyString(input.support_release_id, 'support release id'),
    status: input.status as SupportReleaseStatus,
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

const supportedPermissions: OperationPermissions = {
  version_probe: true,
  catalog: true,
  durable: true,
  scoped_observation: true,
  bounded_drift: true,
};

const recognizedPermissions: OperationPermissions = {
  version_probe: true,
  catalog: false,
  durable: false,
  scoped_observation: false,
  bounded_drift: true,
};

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
      supportedPermissions,
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
  const forwardCatalog = markerCompatible.filter((release) => release.artifact_compatibility.forward_catalog_only);
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

function parseContractVersionRequest(value: unknown): ContractVersionRequest {
  const input = record(value, 'contract version request');
  if (input.selection_contract_version !== CONTRACT_VERSION_SELECTION_VERSION) {
    throw new ContractValidationError('unsupported contract-version selection request version');
  }
  const result: ContractVersionRequest = {
    selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
    model_major: positiveInteger(input.model_major, 'requested model major'),
    external_entity_reference_version: positiveInteger(
      input.external_entity_reference_version,
      'requested external entity reference version',
    ),
    semantic_revision_reference_version: positiveInteger(
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
  if (input.query_pack_versions !== undefined) {
    result.query_pack_versions = versionList(input.query_pack_versions, 'requested query pack versions', true);
  }
  if (input.observation_contract_versions !== undefined) {
    result.observation_contract_versions = versionList(
      input.observation_contract_versions,
      'requested observation contract versions',
      true,
    );
  }
  return result;
}

function parseContractVersionOffer(value: unknown): ContractVersionOffer {
  const input = record(value, 'contract version offer');
  if (input.selection_contract_version !== CONTRACT_VERSION_SELECTION_VERSION) {
    throw new ContractValidationError('unsupported contract-version offer version');
  }
  return {
    selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
    model_major: positiveInteger(input.model_major, 'offered model major'),
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
      return { kind: 'decode' };
    case 'fact_family':
      return {
        kind: 'fact_family',
        family: nonEmptyString(input.family, 'fact family'),
        version: positiveInteger(input.version, 'fact-family version'),
      };
    case 'projection_pack':
      return {
        kind: 'projection_pack',
        pack: nonEmptyString(input.pack, 'projection pack'),
        version: positiveInteger(input.version, 'projection-pack version'),
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
      return { kind: input.kind };
    case 'unavailable':
      return { kind: 'unavailable', reason: nonEmptyString(input.reason, 'unavailable reason') };
    default:
      throw new ContractValidationError('unsupported coverage status');
  }
}

function parseCoverageProvenance(value: unknown): CoverageProvenance {
  const input = record(value, 'coverage provenance');
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
    adapter_id: nonEmptyString(input.adapter_id, 'coverage adapter id'),
    source_instance_key: parseOpaqueContractReference(input.source_instance_key, 'source instance key'),
    stream_key: parseOpaqueContractReference(input.stream_key, 'stream key'),
    object_key: parseOpaqueContractReference(input.object_key, 'object key'),
    generation: nonNegativeInteger(input.generation, 'coverage generation'),
    ...(position === undefined ? {} : { position }),
    status,
    provenance: parseCoverageProvenance(input.provenance),
  };
}

function parseCoverageScope(value: unknown): CoverageScope {
  const input = record(value, 'coverage scope');
  const result: CoverageScope = {
    adapter_id: nonEmptyString(input.adapter_id, 'coverage scope adapter id'),
    source_instance_key: parseOpaqueContractReference(input.source_instance_key, 'scope source instance key'),
    support_release_id: nonEmptyString(input.support_release_id, 'support release id'),
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
  if (input.kind !== 'absent' && input.kind !== 'deleted') {
    throw new ContractValidationError('coverage absence has an unsupported kind');
  }
  return {
    stream_key: parseOpaqueContractReference(input.stream_key, 'absence stream key'),
    object_key: parseOpaqueContractReference(input.object_key, 'absence object key'),
    generation: nonNegativeInteger(input.generation, 'absence generation'),
    kind: input.kind,
  };
}

function parseCoverageError(value: unknown): CoverageError {
  const input = record(value, 'coverage error');
  const result: CoverageError = { code: nonEmptyString(input.code, 'coverage error code') };
  if (input.stream_key !== undefined) {
    result.stream_key = parseOpaqueContractReference(input.stream_key, 'error stream key');
  }
  if (input.object_key !== undefined) {
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
