import assert from 'node:assert/strict';
import { describe, test } from 'node:test';
import { deserializeError, isSdkHostMessage, serializeError } from '../sdk-protocol.js';

describe('SDK utility protocol', () => {
  test('preserves useful error metadata across the process boundary', () => {
    const original = Object.assign(new Error('database disk image is malformed'), {
      name: 'SqliteError',
      code: 'SQLITE_CORRUPT',
    });
    const serialized = serializeError(original);
    const restored = deserializeError(serialized) as NodeJS.ErrnoException;

    assert.equal(serialized.name, 'SqliteError');
    assert.equal(serialized.message, original.message);
    assert.equal(serialized.code, 'SQLITE_CORRUPT');
    assert.equal(restored.name, 'SqliteError');
    assert.equal(restored.message, original.message);
    assert.equal(restored.code, 'SQLITE_CORRUPT');
    assert.match(restored.stack ?? '', /database disk image is malformed/);
  });

  test('accepts only recognized host message envelopes', () => {
    assert.equal(isSdkHostMessage({ type: 'host-ready' }), true);
    assert.equal(isSdkHostMessage({ type: 'response', id: 1, ok: true, result: null }), true);
    assert.equal(isSdkHostMessage({ type: 'event', data: { event: 'ready', payload: { durationMs: 1 } } }), true);
    assert.equal(
      isSdkHostMessage({
        type: 'event',
        data: {
          event: 'active-session-change',
          payload: {
            streamId: 'stream-1',
            sourceId: 'codex',
            projectSlug: 'project',
            sessionId: 'session',
            revision: 1,
            reason: 'append',
          },
        },
      }),
      true,
    );
    assert.equal(isSdkHostMessage({ type: 'not-a-host-message' }), false);
    assert.equal(isSdkHostMessage({ type: 'response' }), false);
    assert.equal(isSdkHostMessage({ type: 'shutdown-complete', id: '1' }), false);
    assert.equal(isSdkHostMessage(null), false);
    assert.equal(isSdkHostMessage('host-ready'), false);
  });
});
