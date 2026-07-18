import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { adaptMessageForDisplay, adaptMessagesForDisplay } from '../lib/source-messages.js';

describe('adaptMessageForDisplay', () => {
  test('passes Claude-shaped messages through', () => {
    const raw = {
      type: 'user',
      uuid: 'u1',
      message: { role: 'user', content: 'hello' },
    };
    const out = adaptMessageForDisplay(raw, 'claude-code');
    assert.equal(out, raw);
  });

  test('maps Codex user response_item to type user', () => {
    const raw = {
      timestamp: '2026-07-13T00:00:01.000Z',
      type: 'response_item',
      payload: {
        type: 'message',
        role: 'user',
        id: 'msg-1',
        content: [{ type: 'input_text', text: 'codex hello' }],
      },
    };
    const out = adaptMessageForDisplay(raw, 'codex');
    assert.ok(out);
    assert.equal(out!.type, 'user');
    assert.equal((out as any).message.content, 'codex hello');
  });

  test('maps Codex assistant response_item to type assistant with text blocks', () => {
    const raw = {
      timestamp: '2026-07-13T00:00:02.000Z',
      type: 'response_item',
      payload: {
        type: 'message',
        role: 'assistant',
        content: [{ type: 'output_text', text: 'codex reply' }],
      },
    };
    const out = adaptMessageForDisplay(raw, 'codex');
    assert.ok(out);
    assert.equal(out!.type, 'assistant');
    const blocks = (out as any).message.content;
    assert.ok(Array.isArray(blocks));
    assert.equal(blocks[0].text, 'codex reply');
  });

  test('skips non-message Codex lines', () => {
    const raw = { type: 'session_meta', payload: { id: 'x' } };
    assert.equal(adaptMessageForDisplay(raw, 'codex'), null);
  });

  test('maps Grok user record (content block array) to type user', () => {
    const out = adaptMessageForDisplay(
      {
        type: 'user',
        content: [{ type: 'text', text: 'grok hi' }],
        timestamp: '2026-04-01T10:00:10.000Z',
      },
      'grok',
    );
    assert.ok(out);
    assert.equal(out!.type, 'user');
    assert.equal((out as any).message.content, 'grok hi');
    assert.equal((out as any).timestamp, '2026-04-01T10:00:10.000Z');
  });

  test('maps Grok assistant record (string content) to assistant text blocks', () => {
    const out = adaptMessageForDisplay({ type: 'assistant', content: 'grok reply' }, 'grok');
    assert.ok(out);
    assert.equal(out!.type, 'assistant');
    assert.equal((out as any).message.content[0].text, 'grok reply');
  });

  test('maps Grok reasoning summary to assistant thinking', () => {
    const out = adaptMessageForDisplay(
      { type: 'reasoning', id: 'rs_1', summary: [{ type: 'summary_text', text: 'thinking' }] },
      'grok',
    );
    assert.ok(out);
    assert.equal(out!.type, 'assistant');
    assert.deepEqual((out as any).message.content, [{ type: 'thinking', thinking: 'thinking' }]);
    assert.equal(out!.uuid, 'rs_1');
  });

  test('maps Grok tool I/O into pairable display blocks', () => {
    const result = adaptMessageForDisplay({ type: 'tool_result', tool_call_id: 'c', content: 'x' }, 'grok');
    assert.deepEqual((result as any).message.content, [{ type: 'tool_result', tool_use_id: 'c', content: 'x' }]);
    const backend = adaptMessageForDisplay(
      {
        type: 'backend_tool_call',
        kind: { id: 'ws-1', tool_type: 'web_search', action: { type: 'search', query: 'tokens' } },
      },
      'grok',
    );
    assert.deepEqual((backend as any).message.content, [
      { type: 'tool_use', id: 'ws-1', name: 'web_search', input: { type: 'search', query: 'tokens' } },
    ]);
  });

  test('adaptMessagesForDisplay filters nulls', () => {
    const msgs = adaptMessagesForDisplay(
      [
        {
          type: 'response_item',
          payload: { type: 'message', role: 'user', content: [{ type: 'input_text', text: 'a' }] },
        },
        { type: 'event_msg', payload: {} },
        {
          type: 'response_item',
          payload: { type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'b' }] },
        },
      ],
      'codex',
    );
    assert.equal(msgs.length, 2);
    assert.equal(msgs[0]!.type, 'user');
    assert.equal(msgs[1]!.type, 'assistant');
  });
});
