import {
  ContractValidationError,
  parseExternalEntityRef,
  type ExternalEntityRef,
  type OpaqueContractReference,
} from './rfc012a.js';
import {
  createCatalogQueryContractRequest,
  parseCatalogContinuationRequest,
  parseCatalogQueryContractRequest,
  parseCatalogQueryContractSelectionForRequest,
  type CatalogContinuationRequest,
  type CatalogQueryContractRequest,
  type CatalogSnapshotId,
} from './rfc012b.js';
import {
  parseCatalogEntityResolutionResponse,
  parseCatalogPageRequestBinding,
  parseCatalogPortableCoveragePlan,
  parseCatalogProjectPage,
  parseCatalogReadinessQueryResult,
  parseCatalogReadinessResponse,
  parseCatalogSessionPage,
  parseCatalogSnapshotExpired,
  type CatalogEntityResolutionResponse,
  type CatalogPageRequestBinding,
  type CatalogPortableCoveragePlan,
  type CatalogProjectPage,
  type CatalogQueryKind,
  type CatalogReadinessResponse,
  type CatalogSessionPage,
  type CatalogSnapshotExpired,
} from './rfc012b-pages.js';

const MAX_CATALOG_PAGE_SIZE = 1_000;
const CATALOG_ENTITY_KEY_SORT_SPEC_VERSION = 1;

type UnknownRecord = Record<string, unknown>;

/** Caller-held authority returned by getCatalogReadiness. */
export interface CatalogQueryContext {
  contractRequest: CatalogQueryContractRequest;
  coveragePlan: CatalogPortableCoveragePlan;
  readiness: CatalogReadinessResponse;
}

export interface CatalogLibraryPageRequest {
  context: CatalogQueryContext;
  pageSize: number;
  continuation?: CatalogContinuationRequest;
}

export interface CatalogEntityResolutionRequest {
  context: CatalogQueryContext;
  externalRef: ExternalEntityRef;
}

export type CatalogProjectPageResult = CatalogProjectPage | CatalogSnapshotExpired;
export type CatalogSessionPageResult = CatalogSessionPage | CatalogSnapshotExpired;

/** Transport DTOs intentionally omit the full plan and source evidence. */
export interface CatalogReadinessTransportRequest {
  contractRequest: CatalogQueryContractRequest;
}

export interface CatalogPageTransportRequest {
  contractRequest: CatalogQueryContractRequest;
  coveragePlanId: OpaqueContractReference;
  snapshotId: CatalogSnapshotId;
  pageSize: number;
  continuation?: CatalogContinuationRequest;
}

export interface CatalogResolutionTransportRequest {
  contractRequest: CatalogQueryContractRequest;
  coveragePlanId: OpaqueContractReference;
  snapshotId: CatalogSnapshotId;
  externalRef: ExternalEntityRef;
}

export interface PreparedCatalogPageRequest {
  context: CatalogQueryContext;
  transportRequest: CatalogPageTransportRequest;
  expectedRequest: CatalogPageRequestBinding | undefined;
  continuation: CatalogContinuationRequest | undefined;
}

export interface PreparedCatalogResolutionRequest {
  context: CatalogQueryContext;
  transportRequest: CatalogResolutionTransportRequest;
  expectedRequest: {
    contract_selection: CatalogReadinessResponse['contract_selection'];
    snapshot_id: CatalogSnapshotId;
    external_ref: ExternalEntityRef;
  };
}

export function defaultCatalogReadinessTransportRequest(): CatalogReadinessTransportRequest {
  return { contractRequest: createCatalogQueryContractRequest() };
}

export function parseCatalogReadinessTransportRequest(value: unknown): CatalogReadinessTransportRequest {
  const input = record(value, 'catalog readiness request');
  assertKnownFields(input, ['contractRequest'], 'catalog readiness request');
  return { contractRequest: parseCatalogQueryContractRequest(input.contractRequest) };
}

export function catalogQueryContextFromResponse(requestInput: unknown, response: unknown): CatalogQueryContext {
  const request = parseCatalogQueryContractRequest(requestInput);
  const parsed = parseCatalogReadinessQueryResult(response, request);
  return {
    contractRequest: request,
    coveragePlan: parsed.coverage_plan,
    readiness: parsed.readiness,
  };
}

export function parseCatalogQueryContext(value: unknown): CatalogQueryContext {
  const input = record(value, 'catalog query context');
  assertKnownFields(input, ['contractRequest', 'coveragePlan', 'readiness'], 'catalog query context');
  const contractRequest = parseCatalogQueryContractRequest(input.contractRequest);
  const coveragePlan = parseCatalogPortableCoveragePlan(input.coveragePlan);
  const readinessInput = record(input.readiness, 'catalog readiness response');
  const selection = parseCatalogQueryContractSelectionForRequest(readinessInput.contract_selection, contractRequest);
  return {
    contractRequest,
    coveragePlan,
    readiness: parseCatalogReadinessResponse(readinessInput, selection, coveragePlan),
  };
}

export function prepareCatalogPageRequest(value: unknown, queryKind: CatalogQueryKind): PreparedCatalogPageRequest {
  const input = record(value, `${queryKind} catalog page request`);
  assertKnownFields(input, ['context', 'pageSize', 'continuation'], `${queryKind} catalog page request`);
  const context = parseCatalogQueryContext(input.context);
  requireLibraryScope(context.coveragePlan);
  const pageSize = positivePageSize(input.pageSize);
  const selection = context.readiness.contract_selection;
  const continuation =
    input.continuation === undefined ? undefined : parseCatalogContinuationRequest(input.continuation, selection);
  if (continuation !== undefined && continuation.page_size !== pageSize) {
    throw new ContractValidationError('catalog page size differs from the caller-held continuation');
  }
  const snapshotId = continuation?.snapshot_id ?? requireQueryableSnapshot(context.readiness);
  const expectedRequest =
    continuation === undefined
      ? undefined
      : parseCatalogPageRequestBinding({
          contract_selection: selection,
          snapshot_id: continuation.snapshot_id,
          query_kind: queryKind,
          query_fingerprint: continuation.query_fingerprint,
          sort_spec_version: continuation.sort_spec_version,
          page_size: continuation.page_size,
          after_cursor: continuation.cursor,
        });
  return {
    context,
    transportRequest: {
      contractRequest: context.contractRequest,
      coveragePlanId: context.coveragePlan.coverage_plan_id,
      snapshotId,
      pageSize,
      ...(continuation === undefined ? {} : { continuation }),
    },
    expectedRequest,
    continuation,
  };
}

export function parseCatalogProjectPageResult(
  value: unknown,
  prepared: PreparedCatalogPageRequest,
): CatalogProjectPageResult {
  return parseCatalogPageResult(value, prepared, 'projects', parseCatalogProjectPage);
}

export function parseCatalogSessionPageResult(
  value: unknown,
  prepared: PreparedCatalogPageRequest,
): CatalogSessionPageResult {
  return parseCatalogPageResult(value, prepared, 'sessions', parseCatalogSessionPage);
}

export function prepareCatalogResolutionRequest(value: unknown): PreparedCatalogResolutionRequest {
  const input = record(value, 'catalog entity resolution request');
  assertKnownFields(input, ['context', 'externalRef'], 'catalog entity resolution request');
  const context = parseCatalogQueryContext(input.context);
  requireLibraryScope(context.coveragePlan);
  const snapshotId = requireQueryableSnapshot(context.readiness);
  const externalRef = parseExternalEntityRef(input.externalRef);
  return {
    context,
    transportRequest: {
      contractRequest: context.contractRequest,
      coveragePlanId: context.coveragePlan.coverage_plan_id,
      snapshotId,
      externalRef,
    },
    expectedRequest: {
      contract_selection: context.readiness.contract_selection,
      snapshot_id: snapshotId,
      external_ref: externalRef,
    },
  };
}

export function parseCatalogResolutionResult(
  value: unknown,
  prepared: PreparedCatalogResolutionRequest,
): CatalogEntityResolutionResponse {
  return parseCatalogEntityResolutionResponse(value, prepared.expectedRequest);
}

function parseCatalogPageResult<R extends CatalogProjectPage | CatalogSessionPage>(
  value: unknown,
  prepared: PreparedCatalogPageRequest,
  queryKind: CatalogQueryKind,
  parsePage: (value: unknown, expectedRequest: unknown, expectedPlan: unknown) => R,
): R | CatalogSnapshotExpired {
  if (isSnapshotExpired(value)) {
    if (prepared.continuation === undefined) {
      throw new ContractValidationError('an initial catalog page cannot return snapshot expiration');
    }
    return parseCatalogSnapshotExpired(
      value,
      prepared.continuation,
      prepared.context.readiness.contract_selection,
      prepared.context.coveragePlan.scope,
    );
  }
  const expectedRequest = prepared.expectedRequest ?? initialPageRequest(value, prepared, queryKind);
  return parsePage(value, expectedRequest, prepared.context.coveragePlan);
}

function initialPageRequest(
  value: unknown,
  prepared: PreparedCatalogPageRequest,
  queryKind: CatalogQueryKind,
): CatalogPageRequestBinding {
  const response = record(value, `${queryKind} catalog page`);
  const request = parseCatalogPageRequestBinding(response.request, prepared.context.readiness.contract_selection);
  if (
    !snapshotsEqual(request.snapshot_id, prepared.transportRequest.snapshotId) ||
    request.query_kind !== queryKind ||
    request.sort_spec_version !== CATALOG_ENTITY_KEY_SORT_SPEC_VERSION ||
    request.page_size !== prepared.transportRequest.pageSize ||
    request.after_cursor !== undefined
  ) {
    throw new ContractValidationError('initial catalog page does not match the caller-held request');
  }
  return request;
}

function requireQueryableSnapshot(readiness: CatalogReadinessResponse): CatalogSnapshotId {
  const snapshot = readiness.readiness.last_complete_snapshot;
  if (snapshot === undefined) {
    throw new ContractValidationError('catalog readiness does not retain a queryable snapshot');
  }
  return snapshot;
}

function requireLibraryScope(plan: CatalogPortableCoveragePlan): void {
  if (plan.scope.kind !== 'library') {
    throw new ContractValidationError('library catalog queries require a library coverage plan');
  }
}

function positivePageSize(value: unknown): number {
  if (!Number.isSafeInteger(value) || (value as number) < 1 || (value as number) > MAX_CATALOG_PAGE_SIZE) {
    throw new ContractValidationError(`catalog page size must be an integer between 1 and ${MAX_CATALOG_PAGE_SIZE}`);
  }
  return value as number;
}

function isSnapshotExpired(value: unknown): boolean {
  return (
    value !== null &&
    typeof value === 'object' &&
    !Array.isArray(value) &&
    (value as UnknownRecord).catalog_snapshot_expiration_contract_version !== undefined
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
    if (!known.has(key)) throw new ContractValidationError(`${label} contains an unknown field`);
  }
}
