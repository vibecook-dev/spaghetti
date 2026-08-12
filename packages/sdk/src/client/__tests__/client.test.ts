import assert from 'node:assert/strict';
import { afterEach, describe, test } from 'node:test';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

import { loadNativeAddon, type SpaghettiEngine } from '../../native.js';
import {
  NapiTransport,
  SPAGHETTI_CLIENT_METHODS,
  SPAGHETTI_CLIENT_PROTOCOL_VERSION,
  SPAGHETTI_QUERY_CONTRACT_VERSION,
  SpaghettiClientError,
  normalizeTransportError,
  openEmbeddedSpaghettiClient,
  openSpaghettiClient,
  type AnySpaghettiProtocolRequest,
  type SpaghettiClientMethod,
  type SpaghettiClientTransport,
  type SpaghettiProtocolRequest,
  type SpaghettiProtocolResponse,
  type SpaghettiTransportConnectRequest,
  type SpaghettiTransportConnectResponse,
  type SpaghettiTransportRequestOptions,
} from '../index.js';

type ResponseResolver = (response: unknown) => void;
type Handler = (
  request: AnySpaghettiProtocolRequest,
  options?: SpaghettiTransportRequestOptions,
) => unknown | Promise<unknown>;

class FakeTransport implements SpaghettiClientTransport {
  readonly kind = 'fake';
  readonly requests: AnySpaghettiProtocolRequest[] = [];
  readonly requestSignals: Array<AbortSignal | undefined> = [];
  disposeCalls = 0;

  constructor(
    private readonly handler: Handler = (request) => success(request, { requestId: request.requestId }),
    private readonly negotiation: Partial<SpaghettiTransportConnectResponse> = {},
  ) {}

  async connect(request: SpaghettiTransportConnectRequest): Promise<SpaghettiTransportConnectResponse> {
    return {
      transportKind: this.kind,
      protocolVersion: request.protocolVersions[0]!,
      queryContractVersion: request.queryContractVersions[0]!,
      engineVersion: 'fake-1.0.0',
      methods: SPAGHETTI_CLIENT_METHODS,
      ...this.negotiation,
    };
  }

  async request<M extends SpaghettiClientMethod>(
    request: SpaghettiProtocolRequest<M>,
    options?: SpaghettiTransportRequestOptions,
  ): Promise<SpaghettiProtocolResponse<M>> {
    this.requests.push(request as AnySpaghettiProtocolRequest);
    this.requestSignals.push(options?.signal);
    return (await this.handler(request as AnySpaghettiProtocolRequest, options)) as SpaghettiProtocolResponse<M>;
  }

  async dispose(): Promise<void> {
    this.disposeCalls += 1;
  }
}

function success(
  request: AnySpaghettiProtocolRequest,
  result: unknown,
): {
  protocolVersion: number;
  queryContractVersion: number;
  requestId: number;
  ok: true;
  result: unknown;
} {
  return {
    protocolVersion: request.protocolVersion,
    queryContractVersion: request.queryContractVersion,
    requestId: request.requestId,
    ok: true,
    result,
  };
}

function errorCode(error: unknown, code: SpaghettiClientError['code']): boolean {
  assert.ok(error instanceof SpaghettiClientError);
  assert.equal(error.code, code);
  return true;
}

describe('SpaghettiClient protocol', () => {
  test('negotiates versions and correlates monotonic request IDs', async () => {
    const transport = new FakeTransport();
    const client = await openSpaghettiClient({ transport, clientName: 'protocol-test' });

    assert.equal(client.info.transportKind, 'fake');
    assert.equal(client.info.protocolVersion, SPAGHETTI_CLIENT_PROTOCOL_VERSION);
    assert.equal(client.info.queryContractVersion, SPAGHETTI_QUERY_CONTRACT_VERSION);
    assert.deepEqual(client.info.methods, SPAGHETTI_CLIENT_METHODS);

    await client.getOverview();
    await client.listProjects({ limit: 10 });

    assert.deepEqual(
      transport.requests.map(({ requestId, method, payload }) => ({ requestId, method, payload })),
      [
        { requestId: 1, method: 'getOverview', payload: undefined },
        { requestId: 2, method: 'listProjects', payload: { limit: 10 } },
      ],
    );
    await client.dispose();
    await client.dispose();
    assert.equal(transport.disposeCalls, 1);
  });

  test('reports methods omitted by a partial transport as unsupported capabilities', async () => {
    const partial = new FakeTransport(undefined, { methods: ['getOverview'] });
    const client = await openSpaghettiClient({ transport: partial });
    await client.getOverview();
    await assert.rejects(client.listProjects(), (error) => errorCode(error, 'unsupported_capability'));
    assert.deepEqual(
      partial.requests.map((request) => request.method),
      ['getOverview'],
    );
    await client.dispose();
  });

  test('refuses an incompatible transport and disposes it', async () => {
    const incompatible = new FakeTransport(undefined, { protocolVersion: 99 });
    await assert.rejects(openSpaghettiClient({ transport: incompatible }), (error) =>
      errorCode(error, 'protocol_mismatch'),
    );
    assert.equal(incompatible.disposeCalls, 1);

    const unknownMethod = new FakeTransport(undefined, {
      methods: ['getOverview', 'dropDatabase' as SpaghettiClientMethod],
    });
    await assert.rejects(openSpaghettiClient({ transport: unknownMethod }), (error) =>
      errorCode(error, 'protocol_mismatch'),
    );
    assert.equal(unknownMethod.disposeCalls, 1);
  });

  test('preserves structured transport errors and response correlation', async () => {
    const busy = new FakeTransport((request) => ({
      protocolVersion: request.protocolVersion,
      queryContractVersion: request.queryContractVersion,
      requestId: request.requestId,
      ok: false,
      error: { code: 'database_busy', message: 'The Spaghetti database is busy.' },
    }));
    const client = await openSpaghettiClient({ transport: busy });
    await assert.rejects(client.getStats(), (error) => {
      assert.equal(errorCode(error, 'database_busy'), true);
      assert.equal((error as SpaghettiClientError).requestId, 1);
      return true;
    });
    await client.dispose();

    const mismatched = new FakeTransport((request) => ({
      ...success(request, {}),
      requestId: request.requestId + 1,
    }));
    const mismatchedClient = await openSpaghettiClient({ transport: mismatched });
    await assert.rejects(mismatchedClient.getOverview(), (error) => errorCode(error, 'protocol_mismatch'));
    await mismatchedClient.dispose();

    const wrongContract = new FakeTransport((request) =>
      success(request, { contractVersion: SPAGHETTI_QUERY_CONTRACT_VERSION + 1 }),
    );
    const wrongContractClient = await openSpaghettiClient({ transport: wrongContract });
    await assert.rejects(wrongContractClient.listProjects(), (error) => errorCode(error, 'protocol_mismatch'));
    await wrongContractClient.dispose();
  });

  test('cancels and suppresses a superseded result even when the transport ignores abort', async () => {
    const pending = new Map<number, ResponseResolver>();
    const transport = new FakeTransport(
      (request) =>
        new Promise((resolve) => {
          pending.set(request.requestId, resolve);
        }),
    );
    const client = await openSpaghettiClient({ transport });

    const first = client.search({ text: 'first' }, { supersessionKey: 'search-box' });
    const second = client.search({ text: 'second' }, { supersessionKey: 'search-box' });

    assert.equal(transport.requestSignals[0]?.aborted, true);
    assert.equal(transport.requestSignals[1]?.aborted, false);
    await assert.rejects(first, (error) => errorCode(error, 'cancelled'));
    pending.get(1)?.(success(transport.requests[0]!, { contractVersion: 1, items: [] }));
    pending.get(2)?.(success(transport.requests[1]!, { contractVersion: 1, items: [] }));

    assert.deepEqual(await second, { contractVersion: 1, items: [] });
    await client.dispose();
  });

  test('sanitizes unclassified native failures behind a diagnostic ID', () => {
    const normalized = normalizeTransportError(
      new Error('SQL failed near secret transcript /Users/example/.claude/projects/private.jsonl'),
      'napi-42',
    );
    assert.deepEqual(normalized, {
      code: 'internal',
      message: 'The query failed internally. Diagnostic: napi-42.',
      diagnosticId: 'napi-42',
    });
  });

  test('rejects pre-aborted and post-disposal requests without dispatch', async () => {
    const transport = new FakeTransport();
    const client = await openSpaghettiClient({ transport });
    const controller = new AbortController();
    controller.abort();

    await assert.rejects(client.listProjects(undefined, { signal: controller.signal }), (error) =>
      errorCode(error, 'cancelled'),
    );
    assert.equal(transport.requests.length, 0);

    await client.dispose();
    await assert.rejects(client.getOverview(), (error) => errorCode(error, 'transport_closed'));
    assert.equal(transport.requests.length, 0);
  });

  test('disposal rejects in-flight work even when a transport never settles it', async () => {
    const transport = new FakeTransport(() => new Promise(() => undefined));
    const client = await openSpaghettiClient({ transport });
    const pending = client.getOverview();

    await client.dispose();

    await assert.rejects(pending, (error) => errorCode(error, 'cancelled'));
    assert.equal(transport.requestSignals[0]?.aborted, true);
  });

  test('subscribes through ordered replay pages and advances empty polling to the snapshot watermark', async () => {
    const firstPage = {
      contractVersion: 1,
      atCommitSeq: 2,
      changes: [
        {
          cursor: { commitSeq: 1, ordinal: 0 },
          topic: 'history.session.changed',
          schemaVersion: 1,
          entityKeyBase64Url: 'c2Vzc2lvbg',
          operation: 'upsert',
          payloadBase64: 'e30=',
        },
      ],
      nextCursor: { commitSeq: 1, ordinal: 0 },
      hasMore: true,
      payloadBytes: 39,
      payloadByteLimit: 12 * 1024 * 1024,
    };
    const finalPage = {
      ...firstPage,
      changes: [{ ...firstPage.changes[0]!, cursor: { commitSeq: 2, ordinal: 0 } }],
      nextCursor: { commitSeq: 2, ordinal: 0 },
      hasMore: false,
    };
    let replayCalls = 0;
    let markThirdStarted: (() => void) | undefined;
    const thirdStarted = new Promise<void>((resolve) => {
      markThirdStarted = resolve;
    });
    const transport = new FakeTransport((request) => {
      assert.equal(request.method, 'replayChanges');
      replayCalls += 1;
      if (replayCalls === 1) return success(request, firstPage);
      if (replayCalls === 2) return success(request, finalPage);
      markThirdStarted?.();
      return new Promise(() => undefined);
    });
    const client = await openSpaghettiClient({ transport });
    const cancellation = new AbortController();
    const subscription = client.subscribe(
      {
        from: { commitSeq: 0, ordinal: 0 },
        topics: ['history.session.changed'],
        batchSize: 1,
      },
      { signal: cancellation.signal, pollIntervalMs: 1 },
    );
    const iterator = subscription[Symbol.asyncIterator]();

    assert.deepEqual(await iterator.next(), { value: firstPage, done: false });
    assert.deepEqual(await iterator.next(), { value: finalPage, done: false });
    const waiting = iterator.next();
    await thirdStarted;

    assert.deepEqual(
      transport.requests.map((request) => request.payload),
      [
        {
          after: { commitSeq: 0, ordinal: 0 },
          topics: ['history.session.changed'],
          limit: 1,
        },
        {
          after: { commitSeq: 1, ordinal: 0 },
          topics: ['history.session.changed'],
          limit: 1,
        },
        {
          after: { commitSeq: 2, ordinal: 0xffff_ffff },
          topics: ['history.session.changed'],
          limit: 1,
        },
      ],
    );
    cancellation.abort();
    assert.deepEqual(await waiting, { value: undefined, done: true });
    assert.equal(transport.requestSignals[2]?.aborted, true);
    await client.dispose();
  });

  test('rejects replay pages that cannot make durable cursor progress', async () => {
    const transport = new FakeTransport((request) =>
      success(request, {
        contractVersion: 1,
        atCommitSeq: 1,
        changes: [],
        hasMore: true,
        payloadBytes: 0,
        payloadByteLimit: 12 * 1024 * 1024,
      }),
    );
    const client = await openSpaghettiClient({ transport });
    const iterator = client.subscribe(undefined, { pollIntervalMs: 1 })[Symbol.asyncIterator]();

    await assert.rejects(iterator.next(), (error) => errorCode(error, 'protocol_mismatch'));
    await client.dispose();
  });

  test('validates subscription bounds and ends a pending subscription on disposal', async () => {
    let markStarted: (() => void) | undefined;
    const started = new Promise<void>((resolve) => {
      markStarted = resolve;
    });
    const transport = new FakeTransport(() => {
      markStarted?.();
      return new Promise(() => undefined);
    });
    const client = await openSpaghettiClient({ transport });

    assert.throws(
      () => client.subscribe({ batchSize: 0 }),
      (error) => errorCode(error, 'invalid_request'),
    );
    assert.throws(
      () => client.subscribe(undefined, { pollIntervalMs: 0 }),
      (error) => errorCode(error, 'invalid_request'),
    );

    const iterator = client.subscribe()[Symbol.asyncIterator]();
    const pending = iterator.next();
    await started;
    await client.dispose();
    assert.deepEqual(await pending, { value: undefined, done: true });
    assert.equal(transport.requestSignals[0]?.aborted, true);
  });
});

describe('NapiTransport dispatch', () => {
  test('maps every canonical client method to exactly one engine method', async () => {
    const calls: Array<{ method: string; args: unknown[] }> = [];
    const status = { owner: { engineVersion: 'fake-native' } };
    const target = {
      status,
      cancelPendingQueries: () => 1,
      dispose: async () => status,
    };
    const engine = new Proxy(target, {
      get(object, property, receiver) {
        if (property in object) return Reflect.get(object, property, receiver);
        return (...args: unknown[]) => {
          calls.push({ method: String(property), args });
          return Promise.resolve({ contractVersion: 1, method: property });
        };
      },
    }) as unknown as SpaghettiEngine;
    const client = await openSpaghettiClient({
      transport: new NapiTransport({ engine, ownsEngine: false }),
    });

    await client.getHealth();
    await client.getOverview();
    await client.replayChanges();
    await client.listProjects({ limit: 1 });
    await client.listSessions({ projectId: 'project' });
    await client.getSession({ sessionId: 'session' });
    await client.getMessages({ projectId: 'project', sessionId: 'session' });
    await client.search({ text: 'needle' });
    await client.getTimeline({ projectId: 'project', sessionId: 'session' });
    await client.listDelegations({ projectId: 'project', sessionId: 'session' });
    await client.listWorkflows({ projectId: 'project', sessionId: 'session' });
    await client.getWorkflow({ workflowId: 'workflow' });
    await client.listWorkflowMembers({ workflowId: 'workflow' });
    await client.listMemoryDocuments({ projectId: 'project' });
    await client.listTaskCollections();
    await client.listTasks({ collectionId: 'collection' });
    await client.listPlans();
    await client.listToolResults({ projectId: 'project', sessionId: 'session' });
    await client.listArtifacts({ sessionId: 'session' });
    await client.listSources();
    await client.getStats();
    await client.getUsage({ projectId: 'project' });
    await client.getUsageActivity({ projectId: 'project', from: '2026-08-01', to: '2026-08-12' });
    await client.getRuntimeSnapshot();
    await client.getRunState({ runId: 'run' });
    await client.listTeams();
    await client.getTeam({ teamId: 'team' });
    await client.listTeamInboxes({ teamId: 'team' });
    await client.listTeamInboxMessages({ inboxId: 'inbox' });

    assert.deepEqual(
      calls.map((call) => call.method),
      [
        'health',
        'overview',
        'replayChanges',
        'listHistoryProjects',
        'listHistorySessions',
        'getSession',
        'getMessages',
        'search',
        'getTimeline',
        'listDelegations',
        'listWorkflows',
        'getWorkflow',
        'listWorkflowMembers',
        'listMemoryDocuments',
        'listTaskCollections',
        'listTasks',
        'listPlans',
        'listToolResults',
        'listArtifacts',
        'listSources',
        'getStats',
        'getUsage',
        'getUsageActivity',
        'getRuntimeSnapshot',
        'getRunState',
        'listTeams',
        'getTeam',
        'listTeamInboxes',
        'listTeamInboxMessages',
      ],
    );
    assert.deepEqual(
      calls.filter((call) => !(call.args.at(-1) instanceof AbortSignal)).map((call) => call.method),
      [],
      'every engine query receives the transport AbortSignal as its final argument',
    );
    await client.dispose();
  });
});

const native = loadNativeAddon();
const tempDirs: string[] = [];

afterEach(() => {
  for (const dir of tempDirs.splice(0)) rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 });
});

describe('embedded SpaghettiClient', { skip: !native }, () => {
  test('negotiates and queries the persistent Rust engine through N-API', async () => {
    const directory = mkdtempSync(path.join(tmpdir(), 'spaghetti-client-'));
    tempDirs.push(directory);
    const client = await openEmbeddedSpaghettiClient({
      dbPath: path.join(directory, 'spaghetti.db'),
      ownerLabel: 'client-integration-test',
      queryWorkers: 2,
    });

    assert.equal(client.info.transportKind, 'napi');
    assert.equal(client.info.engineVersion, native?.nativeVersion());
    const overview = await client.getOverview();
    assert.equal(overview.commitSeq, 0);
    assert.equal(overview.queryOnly, true);
    const replay = await client.replayChanges();
    assert.equal(replay.contractVersion, 1);
    assert.equal(replay.atCommitSeq, 0);
    assert.deepEqual(replay.changes, []);
    assert.equal(replay.hasMore, false);
    assert.equal(replay.payloadBytes, 0);
    assert.equal(replay.payloadByteLimit, 12 * 1024 * 1024);
    const projects = await client.listProjects();
    assert.equal(projects.contractVersion, 1);
    assert.equal(projects.atCommitSeq, 0);
    assert.deepEqual(projects.items, []);

    await assert.rejects(client.listProjects({ limit: 0 }), (error) => errorCode(error, 'invalid_request'));
    await assert.rejects(client.replayChanges({ limit: 0 }), (error) => errorCode(error, 'invalid_request'));
    await assert.rejects(client.listProjects({ cursor: 'not-a-cursor' }), (error) =>
      errorCode(error, 'cursor_invalid'),
    );
    await assert.rejects(client.search({ text: 'x'.repeat(70_000) }), (error) => errorCode(error, 'invalid_request'));

    await client.dispose();
    await assert.rejects(client.getOverview(), (error) => errorCode(error, 'transport_closed'));
  });
});
