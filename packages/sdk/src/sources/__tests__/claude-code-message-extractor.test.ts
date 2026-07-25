import { test } from 'node:test';
import assert from 'node:assert/strict';
import { claudeCodeMessageExtractor } from '../claude-code/message-extractor.js';
import { createClaudeCodeSource } from '../claude-code/index.js';
import { normalizeClaudeFirstPrompt } from '../claude-code/session-metadata.js';

// The extractor is a behavior-identical relocation of the inline extraction
// that used to live in `data/ingest-service.ts` (RFC 006). These tests pin the
// projection shape so the seam can't silently drift; the dual-engine parity
// harness (`test:ingest-diff`) covers byte-level agreement with the Rust path.

test('extracts a user message with string content', () => {
  const out = claudeCodeMessageExtractor.extract({
    type: 'user',
    uuid: 'u-1',
    timestamp: '2026-07-13T00:00:00.000Z',
    message: { role: 'user', content: 'hello world' },
  });
  assert.ok(out);
  assert.equal(out.msgType, 'user');
  assert.equal(out.text, 'hello world');
  assert.equal(out.uuid, 'u-1');
  assert.equal(out.timestamp, '2026-07-13T00:00:00.000Z');
  assert.deepEqual(out.sessionMetadata, { humanPrompt: 'hello world' });
  assert.deepEqual(out.tokens, {
    inputTokens: 0,
    outputTokens: 0,
    cacheCreationTokens: 0,
    cacheReadTokens: 0,
  });
});

test('flattens assistant text + tool_use blocks and maps usage tokens', () => {
  const out = claudeCodeMessageExtractor.extract({
    type: 'assistant',
    message: {
      role: 'assistant',
      content: [
        { type: 'text', text: 'let me check' },
        { type: 'tool_use', name: 'Bash', input: { command: 'ls' } },
      ],
      usage: {
        input_tokens: 12,
        output_tokens: 3,
        cache_creation_input_tokens: 5,
        cache_read_input_tokens: 7,
      },
    },
  });
  assert.ok(out);
  assert.equal(out.msgType, 'assistant');
  assert.equal(out.text, 'let me check\n[tool:Bash]');
  assert.deepEqual(out.tokens, {
    inputTokens: 12,
    outputTokens: 3,
    cacheCreationTokens: 5,
    cacheReadTokens: 7,
  });
});

test('pulls text from user tool_result blocks (string and array content)', () => {
  const out = claudeCodeMessageExtractor.extract({
    type: 'user',
    message: {
      role: 'user',
      content: [
        { type: 'tool_result', content: 'plain result' },
        { type: 'tool_result', content: [{ type: 'text', text: 'nested result' }] },
      ],
    },
  });
  assert.ok(out);
  assert.equal(out.text, 'plain result\nnested result');
});

test('indexes summary and ai-title prose', () => {
  assert.equal(claudeCodeMessageExtractor.extract({ type: 'summary', summary: 'a recap' })?.text, 'a recap');
  const title = claudeCodeMessageExtractor.extract({ type: 'ai-title', aiTitle: 'My Session' });
  assert.equal(title?.text, 'My Session');
  assert.deepEqual(title?.sessionMetadata, { aiTitle: 'My Session' });
});

test('does not project Claude local-command wrappers as human prompts', () => {
  const caveat = claudeCodeMessageExtractor.extract({
    type: 'user',
    isMeta: true,
    message: { role: 'user', content: '<local-command-caveat>ignore</local-command-caveat>' },
  });
  const command = claudeCodeMessageExtractor.extract({
    type: 'user',
    message: { role: 'user', content: '<command-name>/login</command-name>' },
  });
  assert.equal(caveat?.sessionMetadata, undefined);
  assert.equal(command?.sessionMetadata, undefined);
  assert.equal(normalizeClaudeFirstPrompt('<task-notification>generated</task-notification>'), 'No prompt');
  assert.equal(normalizeClaudeFirstPrompt('No prompt'), 'No prompt');
  assert.equal(normalizeClaudeFirstPrompt('A real prompt'), 'A real prompt');
  // sessions-index.json entries routinely omit firstPrompt despite the type.
  assert.equal(normalizeClaudeFirstPrompt(undefined), 'No prompt');
  assert.equal(normalizeClaudeFirstPrompt(null), 'No prompt');
});

test('unknown/other types get "unknown" msgType and empty text, never null', () => {
  const out = claudeCodeMessageExtractor.extract({ foo: 'bar' });
  assert.ok(out, 'claude-code stores a row per line — extract never returns null');
  assert.equal(out.msgType, 'unknown');
  assert.equal(out.text, '');
});

test('createClaudeCodeSource wires the extractor onto source.messages', () => {
  const source = createClaudeCodeSource({ rootDir: '/tmp/x', stateDir: '/tmp/y' });
  assert.equal(source.messages, claudeCodeMessageExtractor);
});
