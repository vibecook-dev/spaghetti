import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { SegmentChangeBatch } from '../../data/segment-types.js';
import { changeBatchMatchesTopic } from '../live/change-filter.js';
import { AsyncSnapshotCoordinator } from '../live/use-async-snapshot.js';

function deferred<T>(): { promise: Promise<T>; resolve(value: T): void } {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

test('async React snapshots suppress superseded and disposed results', async () => {
  const first = deferred<string>();
  const second = deferred<string>();
  const currentDelivered = deferred<void>();
  const delivered: string[] = [];
  const coordinator = new AsyncSnapshotCoordinator<string>();
  const reject = (error: unknown): void => assert.fail(String(error));

  coordinator.load(
    () => first.promise,
    (value) => delivered.push(value),
    reject,
  );
  coordinator.load(
    () => second.promise,
    (value) => {
      delivered.push(value);
      currentDelivered.resolve();
    },
    reject,
  );
  first.resolve('stale');
  second.resolve('current');
  await currentDelivered.promise;
  assert.deepEqual(delivered, ['current']);

  const afterDispose = deferred<string>();
  coordinator.load(
    () => afterDispose.promise,
    (value) => delivered.push(value),
    reject,
  );
  coordinator.dispose();
  afterDispose.resolve('disposed');
  await afterDispose.promise;
  await Promise.resolve();
  assert.deepEqual(delivered, ['current']);

  let invokedAfterDispose = false;
  coordinator.load(
    async () => {
      invokedAfterDispose = true;
      return 'too-late';
    },
    (value) => delivered.push(value),
    reject,
  );
  await Promise.resolve();
  assert.equal(invokedAfterDispose, false);
  assert.deepEqual(delivered, ['current']);
});

test('durable invalidation filters preserve scoped refresh and global reset', () => {
  const scoped: SegmentChangeBatch = {
    timestamp: 1,
    changes: [
      {
        key: 'message:one',
        type: 'message',
        action: 'upsert',
        projectSlug: 'project-a',
        sessionId: 'session-a',
      },
    ],
  };
  assert.equal(changeBatchMatchesTopic(scoped, { kind: 'session', slug: 'project-a', sessionId: 'session-a' }), true);
  assert.equal(changeBatchMatchesTopic(scoped, { kind: 'session', slug: 'project-b' }), false);
  assert.equal(changeBatchMatchesTopic({ timestamp: 2, changes: [] }, { kind: 'settings' }), true);
});
