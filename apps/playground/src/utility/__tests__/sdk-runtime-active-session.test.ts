import assert from 'node:assert/strict';
import { describe, test } from 'node:test';
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { MessageChannel } from 'node:worker_threads';

import {
  IpcTransport,
  loadNativeAddon,
  MessagePortIpcChannel,
  openSpaghettiClient,
  type InitProgress,
  type SpaghettiClient,
} from '@vibecook/spaghetti-sdk';
import type { ObservationService } from '@vibecook/spaghetti-sdk/observation';
import { activeSessionChangeForBatch, SdkRuntime, type ActiveStreamIdentity } from '../sdk-runtime.js';

const native = loadNativeAddon();
const SESSION_ID = '11111111-2222-3333-4444-555555555555';

function sink() {
  return {
    progress: () => {},
    ready: () => {},
    change: () => {},
    activeSessionChange: () => {},
    initError: () => {},
  };
}

const stream: ActiveStreamIdentity = {
  streamId: 'stream-1',
  sourceId: 'codex',
  projectSlug: '-tmp-project',
  sessionId: 'session-1',
};

describe('active transcript stream routing', () => {
  test('routes only source-aware changes for the one open session', () => {
    assert.deepEqual(
      activeSessionChangeForBatch(stream, {
        changes: [
          {
            key: 'live:message:7',
            type: 'message',
            action: 'upsert',
            sourceId: 'codex',
            projectSlug: '-tmp-project',
            sessionId: 'session-1',
            revision: 7,
          },
          {
            key: 'live:message:8',
            type: 'message',
            action: 'upsert',
            sourceId: 'grok',
            projectSlug: '-tmp-project',
            sessionId: 'session-1',
            revision: 8,
          },
        ],
        timestamp: 100,
      }),
      { ...stream, revision: 7, reason: 'append' },
    );
  });

  test('does not wake the active transcript for another session', () => {
    assert.equal(
      activeSessionChangeForBatch(stream, {
        changes: [
          {
            key: 'live:message:9',
            type: 'message',
            action: 'upsert',
            sourceId: 'codex',
            projectSlug: '-tmp-project',
            sessionId: 'session-2',
            revision: 9,
          },
        ],
        timestamp: 101,
      }),
      null,
    );
    assert.equal(
      activeSessionChangeForBatch(stream, {
        changes: [
          {
            key: 'legacy:message:10',
            type: 'message',
            action: 'upsert',
            projectSlug: '-tmp-project',
            sessionId: 'session-1',
            revision: 10,
          },
        ],
        timestamp: 102,
      }),
      null,
    );
  });

  test('classifies tool updates, subagents, and resets', () => {
    for (const [type, reason] of [
      ['tool_result', 'upsert'],
      ['subagent', 'subagent'],
      ['session', 'reset'],
    ] as const) {
      assert.equal(
        activeSessionChangeForBatch(stream, {
          changes: [
            {
              key: `live:${type}:10`,
              type,
              action: 'upsert',
              sourceId: 'codex',
              projectSlug: '-tmp-project',
              sessionId: 'session-1',
              revision: 10,
            },
          ],
          timestamp: 102,
        })?.reason,
        reason,
      );
    }
    assert.equal(activeSessionChangeForBatch(stream, { changes: [], timestamp: 103 })?.reason, 'reset');
  });

  test('deduplicates concurrent initial snapshots without sharing stream ownership', async () => {
    let timelineCalls = 0;
    let subagentCalls = 0;
    let resolveTimeline!: (value: Awaited<ReturnType<ObservationService['getSessionTimeline']>>) => void;
    const timeline = new Promise<Awaited<ReturnType<ObservationService['getSessionTimeline']>>>((resolve) => {
      resolveTimeline = resolve;
    });
    const service = {
      getSessionTimeline: () => {
        timelineCalls += 1;
        return timeline;
      },
      getSessionSubagents: async () => {
        subagentCalls += 1;
        return [];
      },
      getSessionTimelineFacets: async () => {
        throw new Error('same-snapshot facets should be reused');
      },
      dispose: async () => undefined,
    } as unknown as ObservationService;
    const runtime = new SdkRuntime({ dbPath: '/tmp/not-opened.db' }, sink());
    (runtime as unknown as { service: ObservationService }).service = service;

    const request = { sourceId: 'codex', limit: 30 };
    const first = runtime.openSessionStream('-tmp-project', 'session-1', request);
    const second = runtime.openSessionStream('-tmp-project', 'session-1', request);
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(timelineCalls, 1);
    assert.equal(subagentCalls, 1);

    resolveTimeline({
      messages: [],
      total: 0,
      facets: { total: 0, messageCounts: {}, toolCounts: {} },
      hasMore: false,
    });
    const [firstSnapshot, secondSnapshot] = await Promise.all([first, second]);
    assert.notEqual(firstSnapshot.streamId, secondSnapshot.streamId);
    assert.deepEqual(firstSnapshot.page, secondSnapshot.page);
    runtime.closeSessionStream(firstSnapshot.streamId);
    assert.equal(
      (runtime as unknown as { activeStream: ActiveStreamIdentity | null }).activeStream?.streamId,
      secondSnapshot.streamId,
    );
    await runtime.dispose();
  });
});

describe('production observation host status', () => {
  test('is the unconditional production owner', async () => {
    const runtime = new SdkRuntime({ dbPath: '/tmp/not-opened.db' }, sink());

    assert.deepEqual(runtime.getObservationOwnerStatus(), { enabled: true, state: 'starting' });
    assert.equal(runtime.engine, 'rs');
    await runtime.dispose();
    assert.deepEqual(runtime.getObservationOwnerStatus(), { enabled: true, state: 'stopped' });
  });

  test('reports the latest structured startup snapshot to late callers', async () => {
    const progress: InitProgress[] = [];
    let finishInitialize = (): void => undefined;
    const initializing = new Promise<void>((resolve) => {
      finishInitialize = resolve;
    });
    const runtime = new SdkRuntime(
      { dbPath: '/tmp/not-opened.db' },
      { ...sink(), progress: (item) => progress.push(item) },
    );
    const service = {
      isReady: () => false,
      onProgress(listener: (item: InitProgress) => void) {
        listener({
          phase: 'parsing',
          message: 'claude-code scan in progress…',
          sourceId: 'claude-code',
          sourceStage: 'active',
          sourceIndex: 1,
          sourceCount: 3,
        });
        return () => undefined;
      },
      onReady: () => () => undefined,
      onChange: () => () => undefined,
      initialize: () => initializing,
      dispose: async () => finishInitialize(),
    } as unknown as ObservationService;
    (runtime as unknown as { createService: () => ObservationService }).createService = () => service;

    runtime.start();
    await new Promise((resolve) => setImmediate(resolve));

    assert.equal(progress.length, 1);
    assert.deepEqual(runtime.getObservationOwnerStatus().progress, progress[0]);
    await runtime.dispose();
    assert.equal(runtime.getObservationOwnerStatus().state, 'stopped');
  });

  test('owns one Rust production database and serves canonical IPC', { skip: !native }, async () => {
    const directory = mkdtempSync(path.join(tmpdir(), 'spaghetti-production-host-'));
    const root = path.join(directory, 'claude');
    const project = path.join(root, 'projects', '-tmp-production-project');
    const productionDb = path.join(directory, 'production.db');
    mkdirSync(project, { recursive: true });
    for (const secondary of ['todos', 'tasks', 'plans']) mkdirSync(path.join(root, secondary));
    writeFileSync(
      path.join(project, `${SESSION_ID}.jsonl`),
      `${JSON.stringify({
        type: 'user',
        uuid: 'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee',
        parentUuid: null,
        timestamp: '2026-08-12T00:00:00.000Z',
        sessionId: SESSION_ID,
        cwd: '/tmp/production-project',
        gitBranch: 'main',
        message: { role: 'user', content: 'observe the utility host' },
      })}\n`,
    );
    const errors: string[] = [];
    const runtime = new SdkRuntime(
      {
        dbPath: productionDb,
        rootDir: root,
        additionalSources: [],
      },
      { ...sink(), initError: (message) => errors.push(message) },
    );
    let client: SpaghettiClient | undefined;

    try {
      const ports = new MessageChannel();
      const attached = runtime.attachObservationClient(ports.port2);
      const opened = openSpaghettiClient({
        transport: new IpcTransport({ channel: new MessagePortIpcChannel(ports.port1) }),
        clientName: 'playground-utility-test',
      });
      [, client] = await Promise.all([attached, opened]);

      const report = await waitForHost(runtime);
      assert.deepEqual(errors, []);
      assert.equal(runtime.isReady(), true);
      assert.equal(report.state, 'running', report.error);
      const owner = runtime.getObservationOwnerStatus();
      assert.equal(owner.enabled, true);
      assert.equal(owner.state, 'running');
      assert.equal(owner.progress?.message, 'Rust observation service is ready.');
      assert.equal(owner.progress?.sourceCount, 1);
      assert.ok((owner.progress?.elapsedMs ?? -1) >= 0);
      assert.equal(report.snapshot?.status.observation.supervisorsRunning, 1);
      assert.equal(report.snapshot?.health.healthy, true);
      assert.equal(existsSync(productionDb), true);
      assert.equal(report.databasePath, productionDb);
      assert.equal(client.info.transportKind, 'playground-utility');
      const [overview, projects] = await Promise.all([client.getOverview(), client.listProjects({ limit: 10 })]);
      assert.deepEqual([overview.canonicalSessions, overview.canonicalMessages], [1, 1]);
      assert.equal(projects.items[0]?.nativeProjectKey, '-tmp-production-project');
    } finally {
      await client?.dispose();
      await runtime.dispose();
      rmSync(directory, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 });
    }
  });
});

async function waitForHost(runtime: SdkRuntime) {
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    const report = await runtime.getObservationHostStatus();
    if (report.state !== 'starting') return report;
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  throw new Error('Timed out waiting for utility observation host startup');
}
