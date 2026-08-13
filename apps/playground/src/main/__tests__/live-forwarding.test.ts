import assert from 'node:assert/strict';
import { describe, test } from 'node:test';
import type { Change, InitProgress, SegmentChangeBatch } from '@vibecook/spaghetti-sdk';
import type { ObservationService } from '@vibecook/spaghetti-sdk/observation';
import { attachPlaygroundEventForwarding, liveChangeToBatch } from '../../utility/live-forwarding.js';

describe('playground live event forwarding', () => {
  test('forwards the Rust-owned lifecycle and durable invalidations once', () => {
    let progressListener: ((progress: InitProgress) => void) | undefined;
    let readyListener: ((info: { durationMs: number }) => void) | undefined;
    let legacyChangeListener: ((batch: SegmentChangeBatch) => void) | undefined;
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
    } as unknown as ObservationService;

    const progressEvents: InitProgress[] = [];
    const readyEvents: Array<{ durationMs: number }> = [];
    const changeEvents: SegmentChangeBatch[] = [];
    const dispose = attachPlaygroundEventForwarding(sdk, {
      progress: (event) => progressEvents.push(event),
      ready: (event) => readyEvents.push(event),
      change: (event) => changeEvents.push(event),
    });

    const progress = { phase: 'indexing', message: 'updated' } as InitProgress;
    progressListener?.(progress);
    readyListener?.({ durationMs: 42 });
    legacyChangeListener?.({ changes: [], timestamp: 10 });

    assert.deepEqual(progressEvents, [progress]);
    assert.deepEqual(readyEvents, [{ durationMs: 42 }]);
    assert.deepEqual(changeEvents, [{ changes: [], timestamp: 10 }]);

    dispose();
    dispose();
    assert.equal(disposed.length, 3, 'every owned subscription is disposed exactly once');
    assert.deepEqual(new Set(disposed), new Set(['progress', 'ready', 'legacy-change']));
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
