import assert from 'node:assert/strict';
import { describe, test } from 'node:test';
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

import { loadNativeAddon } from '@vibecook/spaghetti-sdk';
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
});

describe('observation shadow host status', () => {
  test('is explicitly disabled unless configured', async () => {
    const runtime = new SdkRuntime({ dbPath: '/tmp/not-opened.db', engine: 'ts' }, sink());

    assert.deepEqual(await runtime.getObservationShadowStatus(), { enabled: false, state: 'disabled' });
    await runtime.dispose();
  });

  test('owns an isolated Rust shadow after legacy readiness', { skip: !native }, async () => {
    const directory = mkdtempSync(path.join(tmpdir(), 'spaghetti-host-shadow-'));
    const root = path.join(directory, 'claude');
    const project = path.join(root, 'projects', '-tmp-shadow-project');
    const productionDb = path.join(directory, 'production.db');
    const shadowDb = path.join(directory, 'shadow.db');
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
        cwd: '/tmp/shadow-project',
        gitBranch: 'main',
        message: { role: 'user', content: 'observe the utility host' },
      })}\n`,
    );
    const errors: string[] = [];
    const runtime = new SdkRuntime(
      {
        dbPath: productionDb,
        engine: 'ts',
        rootDir: root,
        additionalSources: [],
        observationShadow: { dbPath: shadowDb },
      },
      { ...sink(), initError: (message) => errors.push(message) },
    );

    try {
      runtime.start();
      const report = await waitForShadow(runtime);
      assert.deepEqual(errors, []);
      assert.equal(runtime.isReady(), true);
      assert.equal(report.state, 'running', report.error);
      assert.equal(report.snapshot?.status.observation.supervisorsRunning, 1);
      assert.deepEqual(
        [report.snapshot?.overview.canonicalSessions, report.snapshot?.overview.canonicalMessages],
        [1, 1],
      );
      assert.equal(report.historyParity?.exact, true);
      assert.equal(existsSync(productionDb), true);
      assert.equal(existsSync(shadowDb), true);
      assert.notEqual(report.databasePath, productionDb);
    } finally {
      await runtime.dispose();
      rmSync(directory, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 });
    }
  });
});

async function waitForShadow(runtime: SdkRuntime) {
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    const report = await runtime.getObservationShadowStatus();
    if (report.state !== 'starting') return report;
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  throw new Error('Timed out waiting for utility observation shadow startup');
}
