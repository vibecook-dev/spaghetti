import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import type { ChatSessionMessage } from '@vibecook/spaghetti-sdk/react';
import { attachCrossPageToolResults, prependTimelinePage, reconcileTimelineTail } from './timeline-pages.js';

function message(id: string, type: ChatSessionMessage['type']): ChatSessionMessage {
  return {
    timelineId: id,
    uuid: id,
    parentUuid: null,
    timestamp: id,
    sessionId: 'session',
    type,
  } as ChatSessionMessage;
}

describe('timeline page reconciliation', () => {
  test('keeps the oldest cursor page while adding and refreshing tail rows', () => {
    const oldest = message('1', 'user');
    const overlap = message('2', 'assistant');
    const replacement = { ...overlap, content: 'updated' };
    const latest = message('3', 'assistant');

    assert.deepEqual(reconcileTimelineTail([oldest, overlap], [replacement, latest]), [oldest, replacement, latest]);
    assert.deepEqual(prependTimelinePage([overlap, latest], [oldest, overlap]), [oldest, overlap, latest]);
  });

  test('attaches a result whose call arrived in the older page', () => {
    const call = {
      ...message('call', 'tool_use'),
      toolUse: { toolId: 'tool-1', toolName: 'Read', input: {} },
    } as ChatSessionMessage;
    const result = {
      ...message('result', 'tool_result'),
      toolResult: { toolId: 'tool-1', isError: false, content: 'done' },
    } as ChatSessionMessage;

    const attached = attachCrossPageToolResults([call, result]);
    assert.equal(attached.length, 1);
    assert.deepEqual(attached[0]?.toolUse?.result, result.toolResult);
  });

  test('keeps genuinely orphaned results visible', () => {
    const result = {
      ...message('result', 'tool_result'),
      toolResult: { toolId: 'missing', isError: true, content: 'failed' },
    } as ChatSessionMessage;
    assert.deepEqual(attachCrossPageToolResults([result]), [result]);
  });
});
