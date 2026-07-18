#!/usr/bin/env node
/**
 * generate-grok-fixture.mjs
 *
 * Deterministically generate a fake ~/.grok directory tree for the Grok
 * native cold-ingest correctness gate (RS ↔ TS ingest-diff).
 *
 * Output layout (rooted at --out):
 *   <out>/.grok/sessions/<url-encoded-cwd>/<session-uuid>/
 *     chat_history.jsonl
 *     summary.json
 *
 * Sibling noise files that native must ignore can be added later; the
 * reader only discovers basenames `chat_history.jsonl`.
 *
 * Usage:
 *   node scripts/generate-grok-fixture.mjs --out crates/spaghetti-napi/fixtures/small-grok
 */
import { mkdirSync, writeFileSync, utimesSync } from 'node:fs';
import * as path from 'node:path';
import { parseArgs } from 'node:util';

const { values } = parseArgs({
  options: {
    out: { type: 'string' },
  },
});

if (!values.out) {
  console.error('Usage: generate-grok-fixture.mjs --out <path>');
  process.exit(2);
}

const OUT = path.resolve(values.out);
const FIXED_MTIME = new Date('2026-04-01T00:00:00Z');

function writePinned(filePath, content) {
  mkdirSync(path.dirname(filePath), { recursive: true });
  writeFileSync(filePath, content);
  utimesSync(filePath, FIXED_MTIME, FIXED_MTIME);
}

/**
 * @param {string} cwd
 * @param {string} sessionId
 * @param {object} opts
 * @param {string} opts.title
 * @param {string} opts.branch
 * @param {string} opts.created
 * @param {string} opts.updated
 * @param {object[]} opts.lines
 * @param {object[]} [opts.events]
 * @param {object} [opts.signals]
 */
function writeSession(cwd, sessionId, opts) {
  const sessionDir = path.join(OUT, '.grok', 'sessions', encodeURIComponent(cwd), sessionId);
  const chat = opts.lines.map((l) => JSON.stringify(l)).join('\n') + '\n';
  writePinned(path.join(sessionDir, 'chat_history.jsonl'), chat);
  writePinned(
    path.join(sessionDir, 'summary.json'),
    JSON.stringify(
      {
        info: { id: sessionId, cwd },
        created_at: opts.created,
        updated_at: opts.updated,
        generated_title: opts.title,
        session_summary: opts.title,
        head_branch: opts.branch,
        git_root_dir: cwd + '/',
      },
      null,
      2,
    ) + '\n',
  );
  // Session aggregates (cold path attributes contextTokensUsed to last assistant).
  writePinned(
    path.join(sessionDir, 'signals.json'),
    JSON.stringify(
      opts.signals ?? {
        contextTokensUsed: 1200,
        contextWindowTokens: 500000,
        turnCount: 1,
      },
    ) + '\n',
  );
  // Timeline sidecar for per-message timestamps.
  if (opts.events?.length) {
    writePinned(
      path.join(sessionDir, 'events.jsonl'),
      opts.events.map((e) => JSON.stringify(e)).join('\n') + '\n',
    );
  }
  // Noise the reader must ignore (never becomes a message row).
  writePinned(
    path.join(sessionDir, 'updates.jsonl'),
    JSON.stringify({ type: 'ui_noise', payload: 'ignore me' }) + '\n',
  );
}

// ─── Project A: full conversational mix + tool skip ─────────────────────────
const PROJ_A = '/tmp/grok-proj-a';
const SESS_A1 = '019f5d61-da35-7b60-a1b5-02055fd8fcdd';
const SESS_A2 = '019f5d62-bb11-7c70-b2c6-13166fe9fdee';

writeSession(PROJ_A, SESS_A1, {
  title: 'Codebase Onboarding',
  branch: 'main',
  created: '2026-04-01T09:00:00.000Z',
  updated: '2026-04-01T10:30:00.000Z',
  signals: { contextTokensUsed: 4200, contextWindowTokens: 500000, turnCount: 1 },
  // 0 system (pre-turn), 1 user (turn0 @ count=1), 2 reasoning, 3 assistant, 4 tool, 5 assistant
  events: [
    {
      ts: '2026-04-01T10:00:10.000Z',
      type: 'turn_started',
      conversation_message_count: 1,
      turn_number: 0,
    },
    { ts: '2026-04-01T10:00:11.000Z', type: 'loop_started', loop_index: 0 },
    { ts: '2026-04-01T10:00:12.000Z', type: 'first_token' },
    { ts: '2026-04-01T10:00:20.000Z', type: 'loop_started', loop_index: 1 },
    { ts: '2026-04-01T10:00:21.000Z', type: 'first_token' },
    { ts: '2026-04-01T10:00:30.000Z', type: 'turn_ended', outcome: 'completed' },
  ],
  lines: [
    { type: 'system', content: 'You are Grok, a coding assistant.' },
    { type: 'user', content: [{ type: 'text', text: 'how is text rendered?' }] },
    {
      type: 'reasoning',
      id: 'rs_onboard_1',
      summary: [{ type: 'summary_text', text: 'The user wants onboarding help.' }],
      encrypted_content: 'opaque-blob',
      status: 'completed',
    },
    {
      type: 'assistant',
      content: "I'll explore the repo.",
      tool_calls: [{ id: 'call-1', name: 'list_dir', arguments: '{}' }],
    },
    { type: 'tool_result', tool_call_id: 'call-1', content: 'a/\nb/\nc.ts' },
    { type: 'assistant', content: 'Text is rendered via the terminal layer.' },
  ],
});

writeSession(PROJ_A, SESS_A2, {
  title: 'Follow-up on rendering',
  branch: 'main',
  created: '2026-04-01T11:00:00.000Z',
  updated: '2026-04-01T11:15:00.000Z',
  signals: { contextTokensUsed: 800, contextWindowTokens: 500000, turnCount: 1 },
  events: [
    {
      ts: '2026-04-01T11:00:05.000Z',
      type: 'turn_started',
      conversation_message_count: 0,
      turn_number: 0,
    },
    { ts: '2026-04-01T11:00:05.100Z', type: 'loop_started', loop_index: 0 },
    { ts: '2026-04-01T11:00:06.000Z', type: 'first_token' },
    { ts: '2026-04-01T11:00:10.000Z', type: 'turn_ended', outcome: 'completed' },
  ],
  lines: [
    { type: 'user', content: [{ type: 'text', text: 'and what about markdown?' }] },
    { type: 'assistant', content: 'Markdown is converted to ANSI sequences.' },
  ],
});

// ─── Project B: backend_tool_call + multi-block user ────────────────────────
const PROJ_B = '/tmp/grok-proj-b';
const SESS_B1 = '019f54c0-0dd3-7482-a3ee-e73ca610e8a3';

writeSession(PROJ_B, SESS_B1, {
  title: 'Token Research',
  branch: 'feature/tokens',
  created: '2026-04-01T12:00:00.000Z',
  updated: '2026-04-01T12:45:00.000Z',
  signals: { contextTokensUsed: 9900, contextWindowTokens: 500000, turnCount: 1 },
  // 0 user, 1 backend_tool_call, 2 reasoning, 3 reasoning (same loop), 4 assistant
  events: [
    {
      ts: '2026-04-01T12:00:10.000Z',
      type: 'turn_started',
      conversation_message_count: 0,
      turn_number: 0,
    },
    { ts: '2026-04-01T12:00:11.000Z', type: 'loop_started', loop_index: 0 },
    { ts: '2026-04-01T12:00:12.000Z', type: 'first_token' },
    { ts: '2026-04-01T12:00:30.000Z', type: 'turn_ended', outcome: 'completed' },
  ],
  lines: [
    {
      type: 'user',
      content: [
        { type: 'text', text: 'research token attribution' },
        { type: 'text', text: 'across agents' },
      ],
    },
    {
      type: 'backend_tool_call',
      kind: { tool_type: 'web_search', action: { type: 'search', query: 'tokens' } },
    },
    {
      type: 'reasoning',
      id: 'rs_token_1',
      summary: [{ type: 'summary_text', text: 'Keep the answer concise.' }],
      encrypted_content: 'yyy',
    },
    {
      type: 'reasoning',
      id: 'rs_token_2',
      summary: [{ type: 'summary_text', text: 'Still the same model loop.' }],
      encrypted_content: 'zzz',
    },
    { type: 'assistant', content: 'Here is a summary of token models.' },
  ],
});

// ─── Project C: empty-ish edge — system only + long text truncation path ───
const PROJ_C = '/Users/test/grok-long';
const SESS_C1 = '019f6000-aaaa-7bbb-8ccc-ddddeeee0001';

writeSession(PROJ_C, SESS_C1, {
  title: 'Long reply session',
  branch: 'dev',
  created: '2026-04-01T14:00:00.000Z',
  updated: '2026-04-01T14:05:00.000Z',
  signals: { contextTokensUsed: 2500, contextWindowTokens: 500000, turnCount: 1 },
  events: [
    {
      ts: '2026-04-01T14:00:01.000Z',
      type: 'turn_started',
      conversation_message_count: 1,
      turn_number: 0,
    },
    { ts: '2026-04-01T14:00:01.100Z', type: 'loop_started', loop_index: 0 },
    { ts: '2026-04-01T14:00:02.000Z', type: 'first_token' },
    { ts: '2026-04-01T14:00:05.000Z', type: 'turn_ended', outcome: 'completed' },
  ],
  lines: [
    { type: 'system', content: 'You are Grok.' },
    { type: 'user', content: [{ type: 'text', text: 'write a long answer' }] },
    // 2500 chars — both engines truncate FTS at 2000.
    { type: 'assistant', content: 'L'.repeat(2500) },
  ],
});

console.log(`wrote Grok fixture under ${path.join(OUT, '.grok')}`);
console.log('  projects: 3 (proj-a, proj-b, /Users/test/grok-long)');
console.log('  sessions: 4');
console.log('  mtime pinned to 2026-04-01T00:00:00Z');
