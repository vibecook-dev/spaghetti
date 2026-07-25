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

  test('skips Codex developer and injected user context', () => {
    const developer = {
      type: 'response_item',
      payload: {
        type: 'message',
        role: 'developer',
        content: [{ type: 'input_text', text: 'internal instructions' }],
      },
    };
    const environment = {
      type: 'response_item',
      payload: {
        type: 'message',
        role: 'user',
        content: [{ type: 'input_text', text: '<environment_context><cwd>/tmp/x</cwd></environment_context>' }],
      },
    };
    const guardian = {
      type: 'response_item',
      payload: {
        type: 'message',
        role: 'user',
        content: [
          {
            type: 'input_text',
            text: 'The following is the Codex agent history added since your last approval assessment. Continue.',
          },
        ],
      },
    };
    assert.equal(adaptMessageForDisplay(developer, 'codex'), null);
    assert.equal(adaptMessageForDisplay(environment, 'codex'), null);
    assert.equal(adaptMessageForDisplay(guardian, 'codex'), null);
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

  test('maps Grok assistant prose and embedded calls to assistant content blocks', () => {
    const out = adaptMessageForDisplay(
      {
        type: 'assistant',
        content: 'grok reply',
        tool_calls: [{ id: 'call-1', name: 'read_file', arguments: '{"target_file":"README.md"}' }],
      },
      'grok',
    );
    assert.ok(out);
    assert.equal(out!.type, 'assistant');
    const content = (out as unknown as { message: { content: Record<string, unknown>[] } }).message.content;
    assert.deepEqual(content, [
      { type: 'text', text: 'grok reply' },
      { type: 'tool_use', id: 'call-1', name: 'read_file', input: { target_file: 'README.md' } },
    ]);
  });

  test('maps readable Grok reasoning to thinking and skips opaque reasoning', () => {
    const out = adaptMessageForDisplay(
      { type: 'reasoning', id: 'rs_1', summary: [{ type: 'summary_text', text: 'thinking' }] },
      'grok',
    );
    assert.ok(out);
    assert.equal(out!.type, 'assistant');
    assert.deepEqual((out as { message: { content: unknown[] } }).message.content, [
      { type: 'thinking', thinking: 'thinking' },
    ]);
    assert.equal(out!.uuid, 'rs_1');
    assert.equal(adaptMessageForDisplay({ type: 'reasoning', id: 'opaque', summary: [] }, 'grok'), null);
  });

  test('maps Grok local results and backend calls into pairable tool blocks', () => {
    const result = adaptMessageForDisplay({ type: 'tool_result', tool_call_id: 'c', content: 'x' }, 'grok');
    assert.equal(result?.type, 'user');
    assert.deepEqual((result as { message: { content: unknown[] } }).message.content, [
      { type: 'tool_result', tool_use_id: 'c', content: 'x' },
    ]);

    const backend = adaptMessageForDisplay(
      {
        type: 'backend_tool_call',
        kind: { id: 'ws-1', tool_type: 'web_search', action: { type: 'search', query: 'tokens' } },
      },
      'grok',
    );
    assert.equal(backend?.type, 'assistant');
    assert.deepEqual((backend as { message: { content: unknown[] } }).message.content, [
      { type: 'tool_use', id: 'ws-1', name: 'web_search', input: { type: 'search', query: 'tokens' } },
    ]);
  });

  test('Grok transcript excludes injected users and unwraps genuine queries', () => {
    const synthetic = {
      type: 'user',
      synthetic_reason: 'system_reminder',
      content: [{ type: 'text', text: '<system-reminder>internal</system-reminder>' }],
    };
    const bootstrap = {
      type: 'user',
      content: [{ type: 'text', text: '<user_info>OS: macOS</user_info>' }],
    };
    const query = {
      type: 'user',
      content: [
        { type: 'text', text: '<user_query>real prompt</user_query>\n<system-reminder>later</system-reminder>' },
      ],
    };
    assert.equal(adaptMessageForDisplay(synthetic, 'grok'), null);
    assert.equal(adaptMessageForDisplay(bootstrap, 'grok'), null);
    const out = adaptMessageForDisplay(query, 'grok') as { message: { content: string } };
    assert.equal(out.message.content, 'real prompt');
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
  test('Claude local-command wrappers do not become user timeline rows', () => {
    const raw = [
      {
        type: 'user',
        isMeta: true,
        message: { role: 'user', content: '<local-command-caveat>ignore</local-command-caveat>' },
      },
      {
        type: 'user',
        message: { role: 'user', content: '<command-name>/login</command-name>' },
      },
      {
        type: 'user',
        message: { role: 'user', content: 'actual human prompt' },
      },
    ];
    const timeline = transformRawMessagesToTimeline(raw, { sourceId: 'claude-code' });
    assert.equal(timeline.length, 1);
    assert.equal(timeline[0]?.content, 'actual human prompt');
  });

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

  test('Codex timeline only receives genuine human turns', () => {
    const raw = [
      {
        type: 'response_item',
        payload: { type: 'message', role: 'developer', content: [{ type: 'input_text', text: 'system rules' }] },
      },
      {
        type: 'response_item',
        payload: {
          type: 'message',
          role: 'user',
          content: [{ type: 'input_text', text: '<permissions instructions>private context' }],
        },
      },
      {
        type: 'response_item',
        payload: { type: 'message', role: 'user', content: [{ type: 'input_text', text: 'human prompt' }] },
      },
      {
        type: 'response_item',
        payload: { type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'answer' }] },
      },
    ];

    const timeline = transformRawMessagesToTimeline(raw, { sourceId: 'codex' });
    assert.deepEqual(
      timeline.map((message) => [message.type, message.content]),
      [
        ['user', 'human prompt'],
        ['assistant', 'answer'],
      ],
    );
  });

  test('Codex tool calls/results are paired and readable reasoning becomes thinking', () => {
    const raw = [
      {
        timestamp: '2026-07-13T00:00:01.000Z',
        type: 'response_item',
        payload: {
          type: 'function_call',
          id: 'fc-1',
          call_id: 'call-1',
          name: 'wait',
          arguments: '{"cell_id":"42","yield_time_ms":1000}',
        },
      },
      {
        timestamp: '2026-07-13T00:00:02.000Z',
        type: 'response_item',
        payload: {
          type: 'function_call_output',
          call_id: 'call-1',
          output: [
            { type: 'input_text', text: 'Script completed\n' },
            { type: 'input_text', text: 'done' },
          ],
        },
      },
      {
        timestamp: '2026-07-13T00:00:03.000Z',
        type: 'response_item',
        payload: {
          type: 'custom_tool_call',
          id: 'ctc-1',
          call_id: 'call-2',
          name: 'exec',
          input: 'await tools.exec_command({ cmd: "pwd" })',
        },
      },
      {
        timestamp: '2026-07-13T00:00:04.000Z',
        type: 'response_item',
        payload: { type: 'custom_tool_call_output', call_id: 'call-2', output: 'workspace' },
      },
      {
        timestamp: '2026-07-13T00:00:05.000Z',
        type: 'response_item',
        payload: {
          type: 'tool_search_call',
          id: 'tsc-1',
          call_id: 'call-3',
          arguments: { query: 'browser tool', limit: 4 },
        },
      },
      {
        timestamp: '2026-07-13T00:00:06.000Z',
        type: 'response_item',
        payload: {
          type: 'tool_search_output',
          call_id: 'call-3',
          tools: [{ type: 'function', name: 'browser_open' }],
        },
      },
      {
        timestamp: '2026-07-13T00:00:07.000Z',
        type: 'response_item',
        payload: {
          type: 'reasoning',
          id: 'rs-1',
          summary: [{ type: 'summary_text', text: 'I checked the process state.' }],
          encrypted_content: 'opaque',
        },
      },
      {
        timestamp: '2026-07-13T00:00:08.000Z',
        type: 'response_item',
        payload: { type: 'reasoning', id: 'rs-2', summary: [], encrypted_content: 'opaque' },
      },
    ];

    const timeline = transformRawMessagesToTimeline(raw, { sourceId: 'codex' });
    assert.deepEqual(
      timeline.map((message) => message.type),
      ['tool_use', 'tool_use', 'tool_use', 'thinking'],
    );
    assert.deepEqual(timeline[0]?.toolUse?.input, { cell_id: '42', yield_time_ms: 1000 });
    assert.equal(timeline[0]?.toolUse?.result?.content, 'Script completed\ndone');
    assert.deepEqual(timeline[1]?.toolUse?.input, { input: 'await tools.exec_command({ cmd: "pwd" })' });
    assert.equal(timeline[1]?.toolUse?.result?.content, 'workspace');
    assert.deepEqual(timeline[2]?.toolUse?.input, { query: 'browser tool', limit: 4 });
    assert.match(timeline[2]?.toolUse?.result?.content ?? '', /browser_open/);
    assert.equal(timeline[3]?.content, 'I checked the process state.');
    assert.equal(timeline[0]?.toolUse?.result?.rawJson, undefined);
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

  test('Grok tools pair across raw rows and reasoning becomes thinking', () => {
    const raw = [
      {
        type: 'assistant',
        content: '',
        tool_calls: [
          { id: 'call-1', name: 'read_file', arguments: '{"target_file":"README.md"}' },
          { id: 'call-2', name: 'grep', arguments: 'not-json' },
        ],
      },
      { type: 'tool_result', tool_call_id: 'call-1', content: '# Project' },
      { type: 'tool_result', tool_call_id: 'call-2', content: '3 matches' },
      { type: 'reasoning', id: 'rs-1', summary: [{ type: 'summary_text', text: 'I found the files.' }] },
      { type: 'reasoning', id: 'rs-2', summary: [] },
      { type: 'system', content: 'internal model prompt' },
    ];

    const timeline = transformRawMessagesToTimeline(raw, { sourceId: 'grok' });
    assert.deepEqual(
      timeline.map((message) => message.type),
      ['tool_use', 'tool_use', 'thinking'],
    );
    assert.deepEqual(timeline[0]?.toolUse?.input, { target_file: 'README.md' });
    assert.equal(timeline[0]?.toolUse?.result?.content, '# Project');
    assert.deepEqual(timeline[1]?.toolUse?.input, { input: 'not-json' });
    assert.equal(timeline[1]?.toolUse?.result?.content, '3 matches');
    assert.equal(timeline[2]?.content, 'I found the files.');
  });
});
