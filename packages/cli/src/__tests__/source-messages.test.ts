import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { adaptMessageForDisplay, adaptMessagesForDisplay } from '../lib/source-messages.js';

describe('canonical compatibility message boundary', () => {
  test('uses one source-neutral DTO for every adapter', () => {
    const message = {
      type: 'assistant',
      uuid: 'm1',
      message: { role: 'assistant', content: [{ type: 'text', text: 'hello' }] },
    };
    for (const sourceId of ['claude-code', 'codex', 'grok']) {
      assert.equal(adaptMessageForDisplay(message, sourceId), message);
    }
  });

  test('rejects malformed values and filters them from batches', () => {
    const message = { type: 'user', uuid: 'm1', message: { role: 'user', content: 'hello' } };
    assert.equal(adaptMessageForDisplay(null, 'codex'), null);
    assert.equal(adaptMessageForDisplay({}, 'grok'), null);
    assert.deepEqual(adaptMessagesForDisplay([null, message, {}], 'claude-code'), [message]);
  });
});
