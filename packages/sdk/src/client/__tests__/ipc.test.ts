import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { afterEach, describe, test } from 'node:test';
import { MessageChannel } from 'node:worker_threads';

import { loadNativeAddon, openSpaghettiEngine } from '../../native.js';
import {
  IpcTransport,
  MessagePortIpcChannel,
  NapiTransport,
  SPAGHETTI_CLIENT_METHODS,
  SPAGHETTI_CLIENT_PROTOCOL_VERSION,
  SPAGHETTI_QUERY_CONTRACT_VERSION,
  SpaghettiClientError,
  decodeSpaghettiIpcFrame,
  encodeSpaghettiIpcFrame,
  openSpaghettiClient,
  serveSpaghettiIpc,
  type AnySpaghettiProtocolRequest,
  type SpaghettiClientMethod,
  type SpaghettiClientTransport,
  type SpaghettiIpcFrame,
  type SpaghettiProtocolRequest,
  type SpaghettiProtocolResponse,
  type SpaghettiTransportConnectRequest,
  type SpaghettiTransportConnectResponse,
  type SpaghettiTransportRequestOptions,
} from '../index.js';

type RequestHandler = (
  request: AnySpaghettiProtocolRequest,
  options?: SpaghettiTransportRequestOptions,
) => SpaghettiProtocolResponse | Promise<SpaghettiProtocolResponse>;

class FakeBackend implements SpaghettiClientTransport {
  readonly kind = 'fake-backend';
  readonly requests: AnySpaghettiProtocolRequest[] = [];
  readonly signals: Array<AbortSignal | undefined> = [];
  connectCalls = 0;
  disposeCalls = 0;

  constructor(
    private readonly handler: RequestHandler = (request) => success(request, { contractVersion: 1 }),
    private readonly negotiation: Partial<SpaghettiTransportConnectResponse> = {},
  ) {}

  async connect(request: SpaghettiTransportConnectRequest): Promise<SpaghettiTransportConnectResponse> {
    this.connectCalls += 1;
    return {
      transportKind: this.kind,
      protocolVersion: request.protocolVersions[0]!,
      queryContractVersion: request.queryContractVersions[0]!,
      engineVersion: 'fake-ipc-engine',
      methods: SPAGHETTI_CLIENT_METHODS,
      ...this.negotiation,
    };
  }

  async request<M extends SpaghettiClientMethod>(
    request: SpaghettiProtocolRequest<M>,
    options?: SpaghettiTransportRequestOptions,
  ): Promise<SpaghettiProtocolResponse<M>> {
    this.requests.push(request as AnySpaghettiProtocolRequest);
    this.signals.push(options?.signal);
    return (await this.handler(request as AnySpaghettiProtocolRequest, options)) as SpaghettiProtocolResponse<M>;
  }

  async dispose(): Promise<void> {
    this.disposeCalls += 1;
  }
}

function success(request: AnySpaghettiProtocolRequest, result: unknown): SpaghettiProtocolResponse {
  return {
    protocolVersion: request.protocolVersion,
    queryContractVersion: request.queryContractVersion,
    requestId: request.requestId,
    ok: true,
    result,
  } as SpaghettiProtocolResponse;
}

function expectClientError(error: unknown, code: SpaghettiClientError['code']): boolean {
  assert.ok(error instanceof SpaghettiClientError);
  assert.equal(error.code, code);
  return true;
}

function createIpcFixture(backend: SpaghettiClientTransport, ownsTransport = true) {
  const ports = new MessageChannel();
  const clientChannel = new MessagePortIpcChannel(ports.port1);
  const hostChannel = new MessagePortIpcChannel(ports.port2);
  const host = serveSpaghettiIpc({ channel: hostChannel, transport: backend, ownsTransport });
  const transport = new IpcTransport({ channel: clientChannel, connectTimeoutMs: 1_000 });
  return { clientChannel, hostChannel, host, transport };
}

async function waitFor(predicate: () => boolean, message: string): Promise<void> {
  for (let attempt = 0; attempt < 200; attempt += 1) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 1));
  }
  assert.fail(message);
}

describe('Spaghetti IPC framing', () => {
  test('round-trips every frame kind through the versioned binary envelope', () => {
    const frames: SpaghettiIpcFrame[] = [
      {
        type: 'connect',
        request: { clientName: 'test', protocolVersions: [1], queryContractVersions: [1] },
      },
      {
        type: 'connect-result',
        ok: true,
        transportKind: 'ipc',
        protocolVersion: 1,
        queryContractVersion: 1,
        engineVersion: 'test-engine',
        methods: ['getOverview'],
      },
      {
        type: 'connect-result',
        ok: false,
        error: { code: 'database_busy', message: 'The Spaghetti database is busy.' },
      },
      {
        type: 'request',
        request: {
          protocolVersion: 1,
          queryContractVersion: 1,
          requestId: 7,
          method: 'getOverview',
          payload: undefined,
        },
      },
      {
        type: 'response',
        response: {
          protocolVersion: 1,
          queryContractVersion: 1,
          requestId: 7,
          ok: true,
          result: { contractVersion: 1, commitSeq: 0 },
        } as unknown as SpaghettiProtocolResponse,
      },
      {
        type: 'response',
        response: {
          protocolVersion: 1,
          queryContractVersion: 1,
          requestId: 8,
          ok: false,
          error: {
            code: 'reset_required',
            message: 'Read a new snapshot.',
            reason: 'retention_gap',
            currentCommitSeq: 42,
            oldestAvailable: { commitSeq: 17, ordinal: 0 },
          },
        },
      },
      { type: 'cancel', requestId: 7 },
      { type: 'close' },
    ];

    for (const frame of frames) {
      assert.deepEqual(decodeSpaghettiIpcFrame(encodeSpaghettiIpcFrame(frame)), frame);
    }
  });

  test('rejects corrupt headers, lengths, JSON, and protocol vocabulary', () => {
    const valid = encodeSpaghettiIpcFrame({ type: 'close' });

    const badMagic = valid.slice();
    badMagic[0] = 0;
    assert.throws(() => decodeSpaghettiIpcFrame(badMagic), /magic/);

    const badLength = valid.slice();
    new DataView(badLength.buffer).setUint32(6, 99, false);
    assert.throws(() => decodeSpaghettiIpcFrame(badLength), /length/);

    const badJson = valid.slice();
    badJson[badJson.length - 1] = 0xff;
    assert.throws(() => decodeSpaghettiIpcFrame(badJson), /UTF-8 JSON/);

    const unknownMethod = encodeSpaghettiIpcFrame({
      type: 'request',
      request: {
        protocolVersion: 1,
        queryContractVersion: 1,
        requestId: 1,
        method: 'rawSql',
        payload: undefined,
      },
    } as unknown as SpaghettiIpcFrame);
    assert.throws(() => decodeSpaghettiIpcFrame(unknownMethod), /Unknown request method/);
  });
});

describe('IpcTransport and SpaghettiIpcHost', () => {
  test('reports exact encoded frame directions and byte lengths without affecting queries', async () => {
    const backend = new FakeBackend();
    const ports = new MessageChannel();
    const host = serveSpaghettiIpc({
      channel: new MessagePortIpcChannel(ports.port2),
      transport: backend,
    });
    const observations: Array<{ direction: 'sent' | 'received'; byteLength: number }> = [];
    const client = await openSpaghettiClient({
      transport: new IpcTransport({
        channel: new MessagePortIpcChannel(ports.port1),
        onFrame: (observation) => {
          observations.push(observation);
          if (observations.length % 2 === 0) throw new Error('diagnostic observer failure');
        },
      }),
    });

    await client.getOverview();
    assert.deepEqual(
      observations.map((observation) => observation.direction),
      ['sent', 'received', 'sent', 'received'],
    );
    assert.equal(
      observations.every(({ byteLength }) => byteLength >= 10),
      true,
    );

    await client.dispose();
    await host.dispose();
  });

  test('negotiates once and correlates concurrent out-of-order responses', async () => {
    const resolvers = new Map<number, (response: SpaghettiProtocolResponse) => void>();
    const backend = new FakeBackend(
      (request) =>
        new Promise((resolve) => {
          resolvers.set(request.requestId, resolve);
        }),
    );
    const fixture = createIpcFixture(backend);
    const client = await openSpaghettiClient({ transport: fixture.transport, clientName: 'ipc-test' });

    assert.equal(client.info.transportKind, 'ipc');
    assert.equal(client.info.engineVersion, 'fake-ipc-engine');
    assert.equal(backend.connectCalls, 1);

    const overview = client.getOverview();
    const projects = client.listProjects({ limit: 2 });
    await waitFor(() => backend.requests.length === 2, 'IPC requests did not reach the host');
    assert.deepEqual(
      backend.requests.map(({ requestId, method }) => ({ requestId, method })),
      [
        { requestId: 1, method: 'getOverview' },
        { requestId: 2, method: 'listProjects' },
      ],
    );

    resolvers.get(2)?.(success(backend.requests[1]!, { contractVersion: 1, items: ['second'] }));
    resolvers.get(1)?.(success(backend.requests[0]!, { contractVersion: 1, value: 'first' }));
    assert.deepEqual(await projects, { contractVersion: 1, items: ['second'] });
    assert.deepEqual(await overview, { contractVersion: 1, value: 'first' });

    await client.dispose();
    await fixture.host.dispose();
    assert.equal(backend.disposeCalls, 1);
  });

  test('propagates supersession cancellation and suppresses the late response', async () => {
    const resolvers = new Map<number, (response: SpaghettiProtocolResponse) => void>();
    const backend = new FakeBackend(
      (request) =>
        new Promise((resolve) => {
          resolvers.set(request.requestId, resolve);
        }),
    );
    const fixture = createIpcFixture(backend);
    const client = await openSpaghettiClient({ transport: fixture.transport });

    const first = client.search({ text: 'old' }, { supersessionKey: 'search' });
    const firstRejected = assert.rejects(first, (error) => expectClientError(error, 'cancelled'));
    await waitFor(() => backend.requests.length === 1, 'first search did not reach the IPC host');
    const second = client.search({ text: 'new' }, { supersessionKey: 'search' });
    await waitFor(() => backend.requests.length === 2, 'second search did not reach the IPC host');
    await waitFor(() => backend.signals[0]?.aborted === true, 'cancel frame did not abort host work');

    resolvers.get(1)?.(success(backend.requests[0]!, { contractVersion: 1, items: ['stale'] }));
    resolvers.get(2)?.(success(backend.requests[1]!, { contractVersion: 1, items: ['fresh'] }));
    await firstRejected;
    assert.deepEqual(await second, { contractVersion: 1, items: ['fresh'] });

    await client.dispose();
    await fixture.host.dispose();
  });

  test('enforces request bounds before IPC dispatch', async () => {
    const backend = new FakeBackend();
    const fixture = createIpcFixture(backend);
    const client = await openSpaghettiClient({ transport: fixture.transport });

    await assert.rejects(client.search({ text: 'x'.repeat(70_000) }), (error) =>
      expectClientError(error, 'invalid_request'),
    );
    assert.equal(backend.requests.length, 0);

    const received: SpaghettiIpcFrame[] = [];
    const unsubscribe = fixture.clientChannel.onMessage((encoded) => {
      received.push(decodeSpaghettiIpcFrame(encoded));
    });
    await fixture.clientChannel.send(
      encodeSpaghettiIpcFrame({
        type: 'request',
        request: {
          protocolVersion: SPAGHETTI_CLIENT_PROTOCOL_VERSION,
          queryContractVersion: SPAGHETTI_QUERY_CONTRACT_VERSION,
          requestId: 99,
          method: 'search',
          payload: { text: 'x'.repeat(70_000) },
        },
      }),
    );
    await waitFor(
      () => received.some((frame) => frame.type === 'response' && frame.response.requestId === 99),
      'host did not reject an oversized raw IPC request',
    );
    const bounded = received.find((frame) => frame.type === 'response' && frame.response.requestId === 99);
    assert.equal(bounded?.type, 'response');
    if (bounded?.type === 'response') {
      assert.equal(bounded.response.ok, false);
      if (!bounded.response.ok) assert.equal(bounded.response.error.reason, 'payload_too_large');
    }
    assert.equal(backend.requests.length, 0);
    unsubscribe();

    await client.dispose();
    await fixture.host.dispose();
  });

  test('times out a silent host and closes both MessagePort adapters idempotently', async () => {
    const ports = new MessageChannel();
    const clientChannel = new MessagePortIpcChannel(ports.port1);
    const peerChannel = new MessagePortIpcChannel(ports.port2);
    const transport = new IpcTransport({ channel: clientChannel, connectTimeoutMs: 10 });

    await assert.rejects(
      transport.connect({
        clientName: 'timeout-test',
        protocolVersions: [SPAGHETTI_CLIENT_PROTOCOL_VERSION],
        queryContractVersions: [SPAGHETTI_QUERY_CONTRACT_VERSION],
      }),
      (error) => expectClientError(error, 'transport_unavailable'),
    );
    await transport.dispose();
    await transport.dispose();
    await peerChannel.close();
  });

  test('settles in-flight client work when the host shuts down', async () => {
    const backend = new FakeBackend((request, options) => {
      return new Promise((resolve) => {
        options?.signal?.addEventListener(
          'abort',
          () => {
            resolve({
              protocolVersion: request.protocolVersion,
              queryContractVersion: request.queryContractVersion,
              requestId: request.requestId,
              ok: false,
              error: { code: 'cancelled', message: 'The query was cancelled.' },
            });
          },
          { once: true },
        );
      });
    });
    const fixture = createIpcFixture(backend);
    const client = await openSpaghettiClient({ transport: fixture.transport });
    const pending = client.getOverview();
    const rejected = assert.rejects(pending, (error) => expectClientError(error, 'transport_closed'));
    await waitFor(() => backend.requests.length === 1, 'in-flight query did not reach the IPC host');

    await fixture.host.dispose();
    await rejected;
    assert.equal(backend.signals[0]?.aborted, true);
    await client.dispose();
  });

  test('closes a malformed channel and disposes the owned backing transport', async () => {
    const backend = new FakeBackend();
    const fixture = createIpcFixture(backend);

    await fixture.clientChannel.send(new Uint8Array([1, 2, 3]));
    await waitFor(() => backend.disposeCalls === 1, 'malformed frame did not close the IPC host');
    await fixture.host.dispose();
    await fixture.transport.dispose();
    assert.equal(backend.disposeCalls, 1);
  });
});

const native = loadNativeAddon();
const tempDirs: string[] = [];

afterEach(() => {
  for (const directory of tempDirs.splice(0)) {
    rmSync(directory, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 });
  }
});

describe('N-API and IPC semantic parity', { skip: !native }, () => {
  test('returns identical normalized results from one persistent Rust engine', async () => {
    const directory = mkdtempSync(path.join(tmpdir(), 'spaghetti-ipc-parity-'));
    tempDirs.push(directory);
    const engine = await openSpaghettiEngine({
      dbPath: path.join(directory, 'spaghetti.db'),
      ownerLabel: 'ipc-parity-test',
      queryWorkers: 2,
    });
    const direct = await openSpaghettiClient({
      transport: new NapiTransport({ engine, ownsEngine: false }),
      clientName: 'direct-parity',
    });
    const fixture = createIpcFixture(new NapiTransport({ engine, ownsEngine: false }));
    const ipc = await openSpaghettiClient({ transport: fixture.transport, clientName: 'ipc-parity' });

    try {
      assert.deepEqual(await ipc.getOverview(), await direct.getOverview());
      assert.deepEqual(await ipc.replayChanges(), await direct.replayChanges());
      const [ipcWait, directWait] = await Promise.all([
        ipc.waitForCommit({ afterCommitSeq: 0, timeoutMs: 1 }),
        direct.waitForCommit({ afterCommitSeq: 0, timeoutMs: 1 }),
      ]);
      assert.deepEqual(
        { observedCommitSeq: ipcWait.observedCommitSeq, reason: ipcWait.reason },
        { observedCommitSeq: directWait.observedCommitSeq, reason: directWait.reason },
      );
      assert.ok(ipcWait.waitedMs >= 1);
      assert.ok(directWait.waitedMs >= 1);
      assert.deepEqual(await ipc.listProjects(), await direct.listProjects());
      const ipcStats = await ipc.getStats();
      const directStats = await direct.getStats();
      const { performance: ipcPerformance, ...ipcDurableStats } = ipcStats;
      const { performance: directPerformance, ...directDurableStats } = directStats;
      assert.deepEqual(ipcDurableStats, directDurableStats);
      for (const performance of [ipcPerformance, directPerformance]) {
        assert.ok(performance, 'native transport must expose owner performance telemetry');
        assert.equal(performance.writer.committed, 0);
        assert.equal(performance.writer.failed, 0);
        assert.equal(
          performance.writer.checkpoint.completed +
            performance.writer.checkpoint.blocked +
            performance.writer.checkpoint.failures,
          performance.writer.checkpoint.attempts,
        );
        assert.equal(
          performance.source.totals.recordsDecoded +
            performance.source.totals.decodeRetries +
            performance.source.totals.decodeFailures,
          performance.source.totals.decodeAttempts,
        );
        assert.ok(performance.source.dimensionCapacity > 0);
        assert.ok(performance.source.dimensions.length <= performance.source.dimensionCapacity);
        assert.ok(performance.queries.requestsCompleted > 0);
        assert.ok(performance.queries.queueHighWatermark > 0);
        assert.ok(performance.storage.databaseFileBytes > 0);
      }
      await assert.rejects(ipc.listProjects({ cursor: 'not-a-cursor' }), (error) =>
        expectClientError(error, 'cursor_invalid'),
      );
      await assert.rejects(direct.listProjects({ cursor: 'not-a-cursor' }), (error) =>
        expectClientError(error, 'cursor_invalid'),
      );

      const ipcCancellation = new AbortController();
      const pendingWait = ipc.waitForCommit(
        { afterCommitSeq: 0, timeoutMs: 30_000 },
        { signal: ipcCancellation.signal },
      );
      ipcCancellation.abort();
      await assert.rejects(pendingWait, (error) => expectClientError(error, 'cancelled'));
    } finally {
      await ipc.dispose();
      await fixture.host.dispose();
      await direct.dispose();
      await engine.dispose();
    }
  });
});
