import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { parseExternalEntityRef } from '../../contracts/rfc012a.js';
import { parseCatalogQueryContractRequest } from '../../contracts/rfc012b.js';
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

class CatalogFixtureTransport implements SpaghettiClientTransport {
  readonly kind = 'catalog-fixture';
  readonly requests: AnySpaghettiProtocolRequest[] = [];
  private closed = false;

  constructor(private readonly mutateProject?: (page: Record<string, unknown>) => void) {}

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

  assert.deepEqual(
    calls.map(({ method }) => method),
    ['getCatalogReadinessJson', 'listLibraryProjectsJson', 'resolveCatalogEntityJson'],
  );
  const pageRequest = calls[1]!.request;
  assert.deepEqual(pageRequest.contract_request, context.contractRequest);
  assert.equal(pageRequest.coverage_plan_id, context.coveragePlan.coverage_plan_id);
  assert.deepEqual(pageRequest.snapshot_id, context.readiness.readiness.last_complete_snapshot);
  assert.equal(pageRequest.page_size, pageFixture.project_page.request.page_size);
  assert.equal('contractRequest' in pageRequest, false);
  assert.equal('coveragePlanId' in pageRequest, false);
  assert.equal('snapshotId' in pageRequest, false);
  await client.dispose();
});

test('catalog methods remain part of the complete transport negotiation set', () => {
  assert.equal(SPAGHETTI_CLIENT_PROTOCOL_VERSION, 1);
  assert.equal(SPAGHETTI_QUERY_CONTRACT_VERSION, 1);
  assert.ok(SPAGHETTI_CLIENT_METHODS.includes('getCatalogReadiness'));
  assert.ok(SPAGHETTI_CLIENT_METHODS.includes('listLibraryProjects'));
  assert.ok(SPAGHETTI_CLIENT_METHODS.includes('listLibrarySessions'));
  assert.ok(SPAGHETTI_CLIENT_METHODS.includes('resolveCatalogEntity'));
  assert.deepEqual(normalizeTransportError(new Error('IncompatibleCatalogContract: query_pack_version'), 'test'), {
    code: 'protocol_mismatch',
    message: 'The client and transport do not share a supported protocol contract.',
    reason: 'catalog_query_pack_version',
  });
});
