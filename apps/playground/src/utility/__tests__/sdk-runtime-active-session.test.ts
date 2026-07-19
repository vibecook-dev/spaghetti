import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import { activeSessionChangeForBatch, type ActiveStreamIdentity } from '../sdk-runtime.js';

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
