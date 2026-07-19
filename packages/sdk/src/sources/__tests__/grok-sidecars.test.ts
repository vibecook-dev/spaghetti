/**
 * Grok sidecar join — turn-scoped timestamps + session token aggregates.
 */

import { describe, test } from 'node:test';
import assert from 'node:assert/strict';

import {
  buildTimestampMap,
  parseGrokEvents,
  parseGrokSignals,
  collectChatLineTypes,
  distributeGrokSessionTokens,
} from '../grok/sidecars.js';

describe('Grok sidecars (turn-scoped join)', () => {
  test('conversation_message_count is the exact user line index', () => {
    // Pre-turn: 0 system, 1 bootstrap user; turn0 user at 2.
    const types = [
      'system',
      'user',
      'user', // turn0
      'reasoning',
      'reasoning', // same loop
      'assistant',
      'tool_result',
      'reasoning',
      'assistant',
      'user', // turn1 @ 9
      'reasoning',
      'assistant',
    ];
    const events = parseGrokEvents(`
{"ts":"2026-04-01T10:00:00.000Z","type":"turn_started","turn_number":0,"conversation_message_count":2}
{"ts":"2026-04-01T10:00:01.000Z","type":"loop_started","loop_index":0}
{"ts":"2026-04-01T10:00:02.000Z","type":"first_token"}
{"ts":"2026-04-01T10:00:10.000Z","type":"loop_started","loop_index":1}
{"ts":"2026-04-01T10:00:11.000Z","type":"first_token"}
{"ts":"2026-04-01T10:00:20.000Z","type":"turn_ended"}
{"ts":"2026-04-01T11:00:00.000Z","type":"turn_started","turn_number":1,"conversation_message_count":9}
{"ts":"2026-04-01T11:00:01.000Z","type":"loop_started","loop_index":0}
{"ts":"2026-04-01T11:00:02.000Z","type":"first_token"}
{"ts":"2026-04-01T11:00:10.000Z","type":"turn_ended"}
`);
    const map = buildTimestampMap(types, events, '2026-04-01T09:00:00.000Z');

    assert.equal(map.get(0), '2026-04-01T09:00:00.000Z'); // pre-turn system
    assert.equal(map.get(1), '2026-04-01T09:00:00.000Z'); // pre-turn user
    assert.equal(map.get(2), '2026-04-01T10:00:00.000Z'); // turn0 user
    // Multiple reasonings share loop_started before assistant advances
    assert.equal(map.get(3), '2026-04-01T10:00:01.000Z');
    assert.equal(map.get(4), '2026-04-01T10:00:01.000Z');
    assert.equal(map.get(5), '2026-04-01T10:00:02.000Z'); // first_token 0
    assert.equal(map.get(6), '2026-04-01T10:00:02.000Z'); // result shares its call timestamp
    assert.equal(map.get(7), '2026-04-01T10:00:10.000Z');
    assert.equal(map.get(8), '2026-04-01T10:00:11.000Z');
    assert.equal(map.get(9), '2026-04-01T11:00:00.000Z'); // turn1 user
    assert.equal(map.get(10), '2026-04-01T11:00:01.000Z');
    assert.equal(map.get(11), '2026-04-01T11:00:02.000Z');
  });

  test('extra users in the same turn share turn_started', () => {
    const types = ['user', 'user', 'reasoning', 'assistant'];
    const events = parseGrokEvents(`
{"ts":"2026-04-01T10:00:00.000Z","type":"turn_started","turn_number":0,"conversation_message_count":0}
{"ts":"2026-04-01T10:00:01.000Z","type":"loop_started"}
{"ts":"2026-04-01T10:00:02.000Z","type":"first_token"}
{"ts":"2026-04-01T10:00:10.000Z","type":"turn_ended"}
`);
    const map = buildTimestampMap(types, events, null);
    assert.equal(map.get(0), '2026-04-01T10:00:00.000Z');
    assert.equal(map.get(1), '2026-04-01T10:00:00.000Z');
    assert.equal(map.get(2), '2026-04-01T10:00:01.000Z');
    assert.equal(map.get(3), '2026-04-01T10:00:02.000Z');
  });

  test('compaction selects the latest valid counter epoch', () => {
    const types = ['system', 'user', 'reasoning', 'assistant', 'user', 'assistant'];
    const events = parseGrokEvents(`
{"ts":"2026-04-01T08:00:00.000Z","type":"turn_started","turn_number":20,"conversation_message_count":10}
{"ts":"2026-04-01T08:10:00.000Z","type":"turn_started","turn_number":21,"conversation_message_count":20}
{"ts":"2026-04-01T10:00:00.000Z","type":"turn_started","turn_number":0,"conversation_message_count":1}
{"ts":"2026-04-01T10:00:01.000Z","type":"loop_started"}
{"ts":"2026-04-01T10:00:02.000Z","type":"first_token"}
{"ts":"2026-04-01T11:00:00.000Z","type":"turn_started","turn_number":1,"conversation_message_count":4}
{"ts":"2026-04-01T11:00:01.000Z","type":"loop_started"}
{"ts":"2026-04-01T11:00:02.000Z","type":"first_token"}
`);
    const map = buildTimestampMap(types, events, '2026-04-01T09:00:00.000Z');
    assert.equal(map.get(0), '2026-04-01T09:00:00.000Z');
    assert.equal(map.get(1), '2026-04-01T10:00:00.000Z');
    assert.equal(map.get(2), '2026-04-01T10:00:01.000Z');
    assert.equal(map.get(3), '2026-04-01T10:00:02.000Z');
    assert.equal(map.get(4), '2026-04-01T11:00:00.000Z');
    assert.equal(map.get(5), '2026-04-01T11:00:02.000Z');
  });

  test('no events → only fallback on system/user', () => {
    const types = ['system', 'user', 'assistant'];
    const map = buildTimestampMap(types, [], '2026-04-01T09:00:00.000Z');
    assert.equal(map.get(0), '2026-04-01T09:00:00.000Z');
    assert.equal(map.get(1), '2026-04-01T09:00:00.000Z');
    assert.equal(map.has(2), false);
  });

  test('parseGrokSignals reads contextTokensUsed', () => {
    const s = parseGrokSignals(JSON.stringify({ contextTokensUsed: 4200, contextWindowTokens: 500000 }));
    assert.ok(s);
    assert.equal(s!.contextTokensUsed, 4200);
  });

  test('collectChatLineTypes skips blank lines', () => {
    const types = collectChatLineTypes(
      ['{"type":"user","content":"a"}', '', '{"type":"assistant","content":"b"}', '\n'].join('\n'),
    );
    assert.deepEqual(types, ['user', 'assistant']);
  });

  test('session token estimates are distributed without changing their total', () => {
    const allocations = distributeGrokSessionTokens(
      [
        '{"type":"user","content":"short"}',
        '{"type":"assistant","content":"a much longer response"}',
        '{"type":"reasoning","summary":"think","encrypted_content":"ignored-ciphertext"}',
      ].join('\n'),
      101,
      new Set([0, 1, 2]),
    );
    assert.equal(
      [...allocations.values()].reduce((sum, value) => sum + value, 0),
      101,
    );
    assert.ok((allocations.get(1) ?? 0) > (allocations.get(0) ?? 0));
    assert.ok(allocations.has(2));
  });
});
