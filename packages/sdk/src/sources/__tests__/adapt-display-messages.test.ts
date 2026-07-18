import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { adaptMessageForDisplay, adaptMessagesForDisplay } from '../adapt-display-messages.js';
import { transformRawMessagesToTimeline } from '../../react/chat/transform-messages.js';

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
    assert.equal((out as { message: { content: string } }).message.content, 'codex hello');
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
    const blocks = (out as { message: { content: { text: string }[] } }).message.content;
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
    assert.equal((out as { message: { content: string } }).message.content, 'grok hi');
    assert.equal((out as { timestamp: string }).timestamp, '2026-04-01T10:00:10.000Z');
  });

  test('maps Grok assistant record (string content) to assistant text blocks', () => {
    const out = adaptMessageForDisplay({ type: 'assistant', content: 'grok reply' }, 'grok');
    assert.ok(out);
    assert.equal(out!.type, 'assistant');
    assert.equal((out as { message: { content: { text: string }[] } }).message.content[0].text, 'grok reply');
  });

  test('maps Grok reasoning summary to a thin system line', () => {
    const out = adaptMessageForDisplay(
      { type: 'reasoning', id: 'rs_1', summary: [{ type: 'summary_text', text: 'thinking' }] },
      'grok',
    );
    assert.ok(out);
    assert.equal(out!.type, 'system');
    assert.equal((out as { content: string }).content, 'thinking');
    assert.equal(out!.uuid, 'rs_1');
  });

  test('skips Grok tool I/O records (no displayable row)', () => {
    assert.equal(adaptMessageForDisplay({ type: 'tool_result', tool_call_id: 'c', content: 'x' }, 'grok'), null);
    assert.equal(adaptMessageForDisplay({ type: 'backend_tool_call', kind: {} }, 'grok'), null);
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

describe('transformRawMessagesToTimeline + sourceId', () => {
  test('without sourceId, Codex response_item rows produce an empty timeline', () => {
    const raw = [
      {
        timestamp: '2026-07-13T00:00:01.000Z',
        type: 'response_item',
        payload: {
          type: 'message',
          role: 'user',
          content: [{ type: 'input_text', text: 'hi' }],
        },
      },
    ];
    assert.equal(transformRawMessagesToTimeline(raw).length, 0);
  });

  test('with sourceId=codex, Codex chat turns appear in the timeline', () => {
    const raw = [
      {
        timestamp: '2026-07-13T00:00:01.000Z',
        type: 'response_item',
        payload: {
          type: 'message',
          role: 'user',
          id: 'u1',
          content: [{ type: 'input_text', text: 'codex hello' }],
        },
      },
      {
        timestamp: '2026-07-13T00:00:02.000Z',
        type: 'response_item',
        payload: {
          type: 'message',
          role: 'assistant',
          id: 'a1',
          content: [{ type: 'output_text', text: 'codex reply' }],
        },
      },
    ];
    const timeline = transformRawMessagesToTimeline(raw, { sourceId: 'codex' });
    assert.equal(timeline.length, 2);
    assert.equal(timeline[0].type, 'user');
    assert.equal(timeline[0].content, 'codex hello');
    assert.equal(timeline[1].type, 'assistant');
    assert.equal(timeline[1].content, 'codex reply');
  });

  test('with sourceId=grok, Grok user/assistant rows appear in the timeline', () => {
    const raw = [
      { type: 'user', content: [{ type: 'text', text: 'grok hi' }], timestamp: '2026-04-01T10:00:00.000Z' },
      { type: 'assistant', content: 'grok reply', timestamp: '2026-04-01T10:00:01.000Z' },
    ];
    const timeline = transformRawMessagesToTimeline(raw, { sourceId: 'grok' });
    assert.equal(timeline.length, 2);
    assert.equal(timeline[0].type, 'user');
    assert.equal(timeline[0].content, 'grok hi');
    assert.equal(timeline[1].type, 'assistant');
    assert.equal(timeline[1].content, 'grok reply');
  });
});
