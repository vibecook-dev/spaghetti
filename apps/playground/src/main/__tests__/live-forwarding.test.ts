import assert from 'node:assert/strict';
import { describe, test } from 'node:test';
import type { Change, InitProgress, SegmentChangeBatch, SpaghettiAPI } from '@vibecook/spaghetti-sdk';
import {
  attachPlaygroundEventForwarding,
  LIVE_FORWARD_THROTTLE_MS,
  liveChangeToBatch,
  PLAYGROUND_LIVE_TOPICS,
} from '../../utility/live-forwarding.js';

describe('playground live event forwarding', () => {
  test('prewarms every visible scope and forwards lifecycle plus live changes', () => {
    let progressListener: ((progress: InitProgress) => void) | undefined;
    let readyListener: ((info: { durationMs: number }) => void) | undefined;
    let legacyChangeListener: ((batch: SegmentChangeBatch) => void) | undefined;
    let liveChangeListener: ((changes: Change[]) => void) | undefined;
    let liveChangeOptions: { throttleMs: number; latest: false } | undefined;
    const prewarmed: string[] = [];
    const disposed: string[] = [];

    const sdk = {
      onProgress(listener: typeof progressListener) {
        progressListener = listener;
        return () => disposed.push('progress');
      },
      onReady(listener: typeof readyListener) {
        readyListener = listener;
        return () => disposed.push('ready');
      },
      onChange(listener: typeof legacyChangeListener) {
        legacyChangeListener = listener;
        return () => disposed.push('legacy-change');
      },
      live: {
        prewarm(topic: (typeof PLAYGROUND_LIVE_TOPICS)[number]) {
          prewarmed.push(topic.kind);
          return () => disposed.push(`prewarm:${topic.kind}`);
        },
        onChange(listener: typeof liveChangeListener, options: typeof liveChangeOptions) {
          liveChangeListener = listener;
          liveChangeOptions = options;
          return () => disposed.push('live-change');
        },
      },
    } as unknown as SpaghettiAPI;

    const progressEvents: InitProgress[] = [];
    const readyEvents: Array<{ durationMs: number }> = [];
    const changeEvents: SegmentChangeBatch[] = [];
    const dispose = attachPlaygroundEventForwarding(sdk, {
      progress: (event) => progressEvents.push(event),
      ready: (event) => readyEvents.push(event),
      change: (event) => changeEvents.push(event),
    });

    assert.deepEqual(
      prewarmed,
      PLAYGROUND_LIVE_TOPICS.map((topic) => topic.kind),
    );
    assert.deepEqual(liveChangeOptions, { throttleMs: LIVE_FORWARD_THROTTLE_MS, latest: false });

    const progress = { phase: 'indexing', message: 'updated' } as InitProgress;
    progressListener?.(progress);
    readyListener?.({ durationMs: 42 });
    legacyChangeListener?.({ changes: [], timestamp: 10 });
    liveChangeListener?.([
      {
        type: 'session.rewritten',
        sourceId: 'codex',
        seq: 7,
        ts: 20,
        slug: 'project-a',
        sessionId: 'session-a',
      },
      {
        type: 'tool-result.added',
        sourceId: 'codex',
        seq: 8,
        ts: 21,
        slug: 'project-a',
        sessionId: 'session-a',
        toolUseId: 'tool-a',
      },
    ]);

    assert.deepEqual(progressEvents, [progress]);
    assert.deepEqual(readyEvents, [{ durationMs: 42 }]);
    assert.deepEqual(changeEvents, [
      { changes: [], timestamp: 10 },
      {
        changes: [
          {
            key: 'live:session.rewritten:7',
            type: 'session',
            action: 'upsert',
            sourceId: 'codex',
            projectSlug: 'project-a',
            sessionId: 'session-a',
            revision: 7,
          },
          {
            key: 'live:tool-result.added:8',
            type: 'tool_result',
            action: 'upsert',
            sourceId: 'codex',
            projectSlug: 'project-a',
            sessionId: 'session-a',
            revision: 8,
          },
        ],
        timestamp: 21,
      },
    ]);

    dispose();
    dispose();
    assert.equal(
      disposed.length,
      4 + PLAYGROUND_LIVE_TOPICS.length,
      'every owned subscription is disposed exactly once',
    );
    assert.deepEqual(
      new Set(disposed),
      new Set([
        'progress',
        'ready',
        'legacy-change',
        'live-change',
        ...PLAYGROUND_LIVE_TOPICS.map((topic) => `prewarm:${topic.kind}`),
      ]),
    );
  });

  test('adapts every live change kind to the existing renderer batch contract', () => {
    const cases: Array<{ change: Change; type: SegmentChangeBatch['changes'][number]['type'] }> = [
      { change: { type: 'session.message.added', seq: 1, ts: 1 } as Change, type: 'message' },
      { change: { type: 'session.created', seq: 2, ts: 2 } as Change, type: 'session' },
      { change: { type: 'session.rewritten', seq: 3, ts: 3 } as Change, type: 'session' },
      { change: { type: 'subagent.updated', seq: 4, ts: 4 } as Change, type: 'subagent' },
      { change: { type: 'tool-result.added', seq: 5, ts: 5 } as Change, type: 'tool_result' },
      { change: { type: 'file-history.added', seq: 6, ts: 6 } as Change, type: 'file_history' },
      { change: { type: 'todo.updated', seq: 7, ts: 7 } as Change, type: 'todo' },
      { change: { type: 'task.updated', seq: 8, ts: 8 } as Change, type: 'task' },
      { change: { type: 'plan.upserted', seq: 9, ts: 9 } as Change, type: 'plan' },
      { change: { type: 'settings.changed', seq: 10, ts: 10 } as Change, type: 'config_settings' },
    ];

    for (const { change, type } of cases) {
      const batch = liveChangeToBatch(change);
      assert.equal(batch.changes.length, 1);
      assert.equal(batch.changes[0]?.type, type);
      assert.equal(batch.changes[0]?.key, `live:${change.type}:${change.seq}`);
      assert.equal(batch.changes[0]?.revision, change.seq);
      assert.equal(batch.timestamp, change.ts);
    }
  });
});
