import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { parseExternalEntityRef, parseOpaqueContractReference } from '../../contracts/rfc012a.js';
import { parseCatalogQueryContractRequest } from '../../contracts/rfc012b.js';
import { parseCatalogHydrationEntityRef } from '../../contracts/rfc012b-hydration.js';
import { catalogQueryContextFromResponse } from '../../contracts/rfc012b-client.js';
import type { SpaghettiEngine } from '../../native.js';
import {
  NapiTransport,
  SPAGHETTI_CLIENT_METHODS,
  SPAGHETTI_CLIENT_PROTOCOL_VERSION,
  SPAGHETTI_QUERY_CONTRACT_VERSION,
  SpaghettiClientError,
  normalizeTransportError,
  openSpaghettiClient,
  type AnySpaghettiProtocolRequest,
  type SpaghettiClientMethod,
  type SpaghettiClientTransport,
  type SpaghettiProtocolRequest,
  type SpaghettiProtocolResponse,
  type SpaghettiTransportConnectRequest,
  type SpaghettiTransportConnectResponse,
} from '../index.js';

interface CatalogPageFixture {
  contract_selection: unknown;
  published_plan: Record<string, unknown>;
  project_page: Record<string, unknown> & { request: Record<string, unknown>; published_readiness: unknown };
  session_page: Record<string, unknown> & { request: Record<string, unknown> };
  resolutions: Record<string, Record<string, unknown> & { request: Record<string, unknown> }>;
  snapshot_expired: unknown;
}

const pageFixture = JSON.parse(
  readFileSync(
    new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012b-catalog-pages-v1.json', import.meta.url),
    'utf8',
  ),
) as CatalogPageFixture;
const queryFixture = JSON.parse(
  readFileSync(
    new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012b-catalog-query-v1.json', import.meta.url),
    'utf8',
  ),
) as { contract_request: unknown };
const hydrationFixture = JSON.parse(
  readFileSync(
    new URL(
      '../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012b-catalog-hydration-v1.json',
      import.meta.url,
    ),
    'utf8',
  ),
) as { command: Record<string, unknown>; accepted_receipt: Record<string, unknown> };
const hydrationFamilies = { 'catalog.project': 1, 'catalog.session': 1 } as const;

class CatalogFixtureTransport implements SpaghettiClientTransport {
  readonly kind = 'catalog-fixture';
  readonly requests: AnySpaghettiProtocolRequest[] = [];
  private closed = false;

  constructor(
    private readonly mutateProject?: (page: Record<string, unknown>) => void,
    private readonly beforeHydration?: () => Promise<void>,
  ) {}

  async connect(request: SpaghettiTransportConnectRequest): Promise<SpaghettiTransportConnectResponse> {
    return {
      transportKind: this.kind,
      protocolVersion: request.protocolVersions[0]!,
      queryContractVersion: request.queryContractVersions[0]!,
      engineVersion: 'catalog-fixture-v1',
      methods: SPAGHETTI_CLIENT_METHODS,
    };
  }

  async request<M extends SpaghettiClientMethod>(
    request: SpaghettiProtocolRequest<M>,
  ): Promise<SpaghettiProtocolResponse<M>> {
    this.requests.push(request as AnySpaghettiProtocolRequest);
    let result: unknown;
    switch (request.method) {
      case 'getCatalogReadiness':
        result = readinessEnvelope();
        break;
      case 'listLibraryProjects': {
        const payload = request.payload as { continuation?: unknown };
        if (payload.continuation !== undefined) {
          result = structuredClone(pageFixture.snapshot_expired);
        } else {
          const page = structuredClone(pageFixture.project_page);
          this.mutateProject?.(page);
          result = page;
        }
        break;
      }
      case 'listLibrarySessions':
        result = structuredClone(pageFixture.session_page);
        break;
      case 'resolveCatalogEntity':
        result = structuredClone(pageFixture.resolutions.live);
        break;
      case 'requestCatalogHydration': {
        await this.beforeHydration?.();
        const payload = request.payload as { selectedBaseSessionRef: unknown; locatorClaimKey: unknown };
        result = hydrationSchedulingEnvelope(payload.selectedBaseSessionRef, payload.locatorClaimKey);
        break;
      }
      default:
        result = { contractVersion: 1 };
    }
    return {
      protocolVersion: request.protocolVersion,
      queryContractVersion: request.queryContractVersion,
      requestId: request.requestId,
      ok: true,
      result,
    } as SpaghettiProtocolResponse<M>;
  }

  async dispose(): Promise<void> {
    this.closed = true;
  }

  get isClosed(): boolean {
    return this.closed;
  }
}

function readinessEnvelope(): unknown {
  return {
    coverage_plan: structuredClone(pageFixture.published_plan),
    readiness: {
      catalog_readiness_response_contract_version: 1,
      contract_selection: structuredClone(pageFixture.contract_selection),
      readiness: structuredClone(pageFixture.project_page.published_readiness),
    },
  };
}

function hydrationSchedulingEnvelope(selectedInput: unknown, locatorClaimKey: unknown): unknown {
  const selected = parseCatalogHydrationEntityRef(selectedInput);
  const command = structuredClone(hydrationFixture.command);
  const snapshot = structuredClone(pageFixture.project_page.request.snapshot_id);
  const sourceWithBinding = (pageFixture.published_plan.required_sources as Array<Record<string, unknown>>)[0]!;
  const source = {
    adapter_id: sourceWithBinding.adapter_id,
    source_instance_key: sourceWithBinding.source_instance_key,
    support_release_id: sourceWithBinding.support_release_id,
    catalog_declaration_digest: sourceWithBinding.catalog_declaration_digest,
    access_policy_digest: sourceWithBinding.access_policy_digest,
  };
  const selection = structuredClone(pageFixture.contract_selection) as {
    contract_versions: { fact_family_versions: Record<string, number> };
  };
  selection.contract_versions.fact_family_versions = structuredClone(hydrationFamilies);
  command.contract_selection = selection;
  command.snapshot_id = snapshot;
  command.source = source;
  const authorization = command.authorization as Record<string, unknown>;
  Object.assign(authorization, source);
  authorization.handoff = {
    presentation_ref: selected,
    member_refs: [selected],
    relation_keys: [],
    selected_base_session_ref: selected,
    locator_claim_key: locatorClaimKey,
  };
  command.requested_scope = {
    hydration_scope_contract_version: 1,
    fact_family_versions: structuredClone(selection.contract_versions.fact_family_versions),
    max_source_objects_per_pass: 1,
    max_records_per_pass: 4_096,
    max_bytes_per_pass: 256 * 1024 * 1024,
  };
  const receipt = structuredClone(hydrationFixture.accepted_receipt);
  receipt.request_key = command.request_key;
  receipt.command_id = command.command_id;
  receipt.coalescing_key = command.coalescing_key;
  receipt.selected_base_session_ref = selected;
  receipt.snapshot_id = snapshot;
  receipt.emitted_at_commit = (snapshot as { complete_commit: number }).complete_commit + 1;
  return { command, receipt, active_schedule: null };
}

function hydrationContext(): ReturnType<typeof catalogQueryContextFromResponse> {
  const request = structuredClone(queryFixture.contract_request) as {
    contract_versions: { fact_family_versions: Record<string, number[]> };
  };
  request.contract_versions.fact_family_versions = {
    'catalog.project': [1],
    'catalog.session': [1],
  };
  const response = readinessEnvelope() as {
    readiness: { contract_selection: { contract_versions: { fact_family_versions: Record<string, number> } } };
  };
  response.readiness.contract_selection.contract_versions.fact_family_versions = structuredClone(hydrationFamilies);
  const parsedRequest = parseCatalogQueryContractRequest(request);
  return catalogQueryContextFromResponse(parsedRequest, response);
}

function clientErrorCode(error: unknown, code: SpaghettiClientError['code']): boolean {
  assert.ok(error instanceof SpaghettiClientError);
  assert.equal(error.code, code);
  return true;
}

test('typed catalog client retains readiness authority across pages, expiration, and resolution', async () => {
  const transport = new CatalogFixtureTransport();
  const client = await openSpaghettiClient({ transport });
  const contractRequest = parseCatalogQueryContractRequest(queryFixture.contract_request);
  const context = await client.getCatalogReadiness(contractRequest);
  const snapshot = context.readiness.readiness.last_complete_snapshot;
  assert.ok(snapshot);

  const projectPageSize = pageFixture.project_page.request.page_size as number;
  const projects = await client.listLibraryProjects({ context, pageSize: projectPageSize });
  assert.ok('rows' in projects);
  if (!('rows' in projects)) throw new Error('expected project page');
  assert.ok(projects.next_continuation);

  const expired = await client.listLibraryProjects({
    context,
    pageSize: projectPageSize,
    continuation: projects.next_continuation,
  });
  assert.ok('latest_snapshot' in expired);

  const sessionPageSize = pageFixture.session_page.request.page_size as number;
  const sessions = await client.listLibrarySessions({ context, pageSize: sessionPageSize });
  assert.ok('rows' in sessions);

  const externalRef = parseExternalEntityRef(pageFixture.resolutions.live.request.external_ref);
  const resolution = await client.resolveCatalogEntity({ context, externalRef });
  assert.equal(resolution.resolution.state, 'live');

  const session = (pageFixture.session_page.rows as Array<{ row: { session_ref: unknown } }>)[0]!.row.session_ref;
  const selectedBaseSessionRef = parseCatalogHydrationEntityRef(session);
  const locatorClaimKey = parseOpaqueContractReference(
    (hydrationFixture.command.authorization as { handoff: { locator_claim_key: string } }).handoff.locator_claim_key,
  );
  const hydration = await client.requestCatalogHydration({
    context: hydrationContext(),
    selectedBaseSessionRef,
    locatorClaimKey,
    stableRequestToken: 'catalog-client-hydration-1',
  });
  assert.equal(hydration.receipt.outcome.state, 'accepted');
  assert.equal(
    (
      await client.requestCatalogHydration({
        context: hydrationContext(),
        selectedBaseSessionRef,
        locatorClaimKey,
        stableRequestToken: 'catalog-client-hydration-1',
      })
    ).receipt.receipt_id,
    hydration.receipt.receipt_id,
  );

  const firstPagePayload = transport.requests.find((request) => request.method === 'listLibraryProjects')?.payload as
    | Record<string, unknown>
    | undefined;
  assert.ok(firstPagePayload);
  assert.equal(firstPagePayload.coveragePlanId, context.coveragePlan.coverage_plan_id);
  assert.deepEqual(firstPagePayload.snapshotId, snapshot);
  assert.equal('coveragePlan' in firstPagePayload, false);
  assert.equal('readiness' in firstPagePayload, false);

  await client.dispose();
  assert.equal(transport.isClosed, true);
});

test('typed catalog client rejects caller and transport drift with path-safe errors', async () => {
  const transport = new CatalogFixtureTransport((page) => {
    const request = page.request as Record<string, unknown>;
    const snapshot = request.snapshot_id as Record<string, unknown>;
    snapshot.complete_commit = (snapshot.complete_commit as number) + 1;
  });
  const client = await openSpaghettiClient({ transport });
  const context = await client.getCatalogReadiness(parseCatalogQueryContractRequest(queryFixture.contract_request));

  const poisoned = structuredClone(context) as unknown as Record<string, unknown>;
  poisoned['/Users/alice/private/session.jsonl'] = 'secret';
  await assert.rejects(
    client.listLibraryProjects({
      context: poisoned as unknown as typeof context,
      pageSize: pageFixture.project_page.request.page_size as number,
    }),
    (error) => {
      assert.equal(clientErrorCode(error, 'invalid_request'), true);
      assert.doesNotMatch((error as Error).message, /Users|alice|private|session\.jsonl|secret/);
      return true;
    },
  );

  await assert.rejects(
    client.listLibraryProjects({
      context,
      pageSize: pageFixture.project_page.request.page_size as number,
    }),
    (error) => clientErrorCode(error, 'protocol_mismatch'),
  );
  await client.dispose();
});

test('typed hydration client rejects token retargeting and allocation-heavy tokens', async () => {
  const transport = new CatalogFixtureTransport();
  const client = await openSpaghettiClient({ transport });
  const context = hydrationContext();
  const selectedBaseSessionRef = parseCatalogHydrationEntityRef(
    (pageFixture.session_page.rows as Array<{ row: { session_ref: unknown } }>)[0]!.row.session_ref,
  );
  const locatorClaimKey = parseOpaqueContractReference(
    (hydrationFixture.command.authorization as { handoff: { locator_claim_key: string } }).handoff.locator_claim_key,
  );
  await client.requestCatalogHydration({
    context,
    selectedBaseSessionRef,
    locatorClaimKey,
    stableRequestToken: 'retarget-proof',
  });
  const differentLocator = parseOpaqueContractReference(
    (hydrationFixture.command.authorization as { authorization_id: string }).authorization_id,
  );
  await assert.rejects(
    client.requestCatalogHydration({
      context,
      selectedBaseSessionRef,
      locatorClaimKey: differentLocator,
      stableRequestToken: 'retarget-proof',
    }),
    (error) => clientErrorCode(error, 'protocol_mismatch'),
  );
  const before = transport.requests.length;
  for (const stableRequestToken of ['x'.repeat(1_025), '\ud800']) {
    await assert.rejects(
      client.requestCatalogHydration({ context, selectedBaseSessionRef, locatorClaimKey, stableRequestToken }),
      (error) => clientErrorCode(error, 'invalid_request'),
    );
  }
  assert.equal(transport.requests.length, before);
  await client.dispose();
});

test('typed hydration client serializes caller-held receipt lineage per stable token', async () => {
  let releaseFirst!: () => void;
  const firstBlocked = new Promise<void>((resolve) => {
    releaseFirst = resolve;
  });
  const transport = new CatalogFixtureTransport(undefined, () => firstBlocked);
  const client = await openSpaghettiClient({ transport });
  const context = hydrationContext();
  const selectedBaseSessionRef = parseCatalogHydrationEntityRef(
    (pageFixture.session_page.rows as Array<{ row: { session_ref: unknown } }>)[0]!.row.session_ref,
  );
  const locatorClaimKey = parseOpaqueContractReference(
    (hydrationFixture.command.authorization as { handoff: { locator_claim_key: string } }).handoff.locator_claim_key,
  );
  const request = {
    context,
    selectedBaseSessionRef,
    locatorClaimKey,
    stableRequestToken: 'serialized-hydration-lineage',
  };
  const first = client.requestCatalogHydration(request);
  const second = client.requestCatalogHydration(request);
  await new Promise<void>((resolve) => setImmediate(resolve));
  assert.equal(transport.requests.filter(({ method }) => method === 'requestCatalogHydration').length, 1);
  releaseFirst();
  const [firstResult, secondResult] = await Promise.all([first, second]);
  assert.equal(firstResult.receipt.receipt_id, secondResult.receipt.receipt_id);
  assert.equal(transport.requests.filter(({ method }) => method === 'requestCatalogHydration').length, 2);
  await client.dispose();
});

test('NapiTransport maps typed catalog DTOs to the strict snake-case JSON boundary', async () => {
  const calls: Array<{ method: string; request: Record<string, unknown> }> = [];
  const engine = {
    status: { owner: { engineVersion: 'catalog-native-fixture-v1' } },
    cancelPendingQueries: () => 0,
    dispose: async () => ({ disposed: true }),
    getCatalogReadinessJson: async (requestJson: string) => {
      calls.push({ method: 'getCatalogReadinessJson', request: JSON.parse(requestJson) as Record<string, unknown> });
      return JSON.stringify(readinessEnvelope());
    },
    listLibraryProjectsJson: async (requestJson: string) => {
      calls.push({ method: 'listLibraryProjectsJson', request: JSON.parse(requestJson) as Record<string, unknown> });
      return JSON.stringify(pageFixture.project_page);
    },
    resolveCatalogEntityJson: async (requestJson: string) => {
      calls.push({ method: 'resolveCatalogEntityJson', request: JSON.parse(requestJson) as Record<string, unknown> });
      return JSON.stringify(pageFixture.resolutions.live);
    },
    requestCatalogHydrationJson: async (requestJson: string) => {
      const request = JSON.parse(requestJson) as Record<string, unknown>;
      calls.push({ method: 'requestCatalogHydrationJson', request });
      return JSON.stringify(hydrationSchedulingEnvelope(request.selected_base_session_ref, request.locator_claim_key));
    },
  } as unknown as SpaghettiEngine;
  const client = await openSpaghettiClient({
    transport: new NapiTransport({ engine, ownsEngine: false }),
  });
  const context = await client.getCatalogReadiness(parseCatalogQueryContractRequest(queryFixture.contract_request));
  await client.listLibraryProjects({
    context,
    pageSize: pageFixture.project_page.request.page_size as number,
  });
  await client.resolveCatalogEntity({
    context,
    externalRef: parseExternalEntityRef(pageFixture.resolutions.live.request.external_ref),
  });
  const selectedBaseSessionRef = parseCatalogHydrationEntityRef(
    (pageFixture.session_page.rows as Array<{ row: { session_ref: unknown } }>)[0]!.row.session_ref,
  );
  const locatorClaimKey = parseOpaqueContractReference(
    (hydrationFixture.command.authorization as { handoff: { locator_claim_key: string } }).handoff.locator_claim_key,
  );
  await client.requestCatalogHydration({
    context: hydrationContext(),
    selectedBaseSessionRef,
    locatorClaimKey,
    stableRequestToken: 'napi-hydration-1',
  });

  assert.deepEqual(
    calls.map(({ method }) => method),
    ['getCatalogReadinessJson', 'listLibraryProjectsJson', 'resolveCatalogEntityJson', 'requestCatalogHydrationJson'],
  );
  const pageRequest = calls[1]!.request;
  assert.deepEqual(pageRequest.contract_request, context.contractRequest);
  assert.equal(pageRequest.coverage_plan_id, context.coveragePlan.coverage_plan_id);
  assert.deepEqual(pageRequest.snapshot_id, context.readiness.readiness.last_complete_snapshot);
  assert.equal(pageRequest.page_size, pageFixture.project_page.request.page_size);
  assert.equal('contractRequest' in pageRequest, false);
  assert.equal('coveragePlanId' in pageRequest, false);
  assert.equal('snapshotId' in pageRequest, false);
  const hydrationRequest = calls[3]!.request;
  assert.deepEqual(hydrationRequest.selected_base_session_ref, selectedBaseSessionRef);
  assert.equal(hydrationRequest.locator_claim_key, locatorClaimKey);
  assert.equal(hydrationRequest.stable_request_token, 'napi-hydration-1');
  assert.equal('selectedBaseSessionRef' in hydrationRequest, false);
  await client.dispose();
});

test('catalog methods remain part of the complete transport negotiation set', () => {
  assert.equal(SPAGHETTI_CLIENT_PROTOCOL_VERSION, 1);
  assert.equal(SPAGHETTI_QUERY_CONTRACT_VERSION, 1);
  assert.ok(SPAGHETTI_CLIENT_METHODS.includes('getCatalogReadiness'));
  assert.ok(SPAGHETTI_CLIENT_METHODS.includes('listLibraryProjects'));
  assert.ok(SPAGHETTI_CLIENT_METHODS.includes('listLibrarySessions'));
  assert.ok(SPAGHETTI_CLIENT_METHODS.includes('resolveCatalogEntity'));
  assert.ok(SPAGHETTI_CLIENT_METHODS.includes('requestCatalogHydration'));
  assert.deepEqual(normalizeTransportError(new Error('IncompatibleCatalogContract: query_pack_version'), 'test'), {
    code: 'protocol_mismatch',
    message: 'The client and transport do not share a supported protocol contract.',
    reason: 'catalog_query_pack_version',
  });
});
