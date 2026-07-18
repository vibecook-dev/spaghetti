/**
 * codex-ingest.test.ts — the Codex AgentSource, end to end into a real store.
 *
 * Writes a synthetic `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` tree, then
 * drives `CodexReader.readAll` into a real `IngestService` (configured with
 * `sourceId: 'codex'` + `codexMessageExtractor`) over a real SQLite schema, and
 * asserts the rows land: tagged `source_id = 'codex'`, chat turns extracted,
 * and the non-message lines (`session_meta`, `event_msg`) skipped.
 *
 * This exercises the RFC 006 seams for a second source — record production
 * (reader), extraction (extractor), and the `source_id` write path — without
 * the multi-source lifecycle orchestration (that is a later increment; here the
 * store holds codex only).
 *
 * Run with `pnpm --filter @vibecook/spaghetti-sdk test`.
 */

import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';

import { createSqliteService } from '../../io/sqlite-service.js';
import { createFileService } from '../../io/file-service.js';
import { createIngestService } from '../../data/ingest-service.js';
import { initializeSchema } from '../../data/schema.js';
import { ensureTimelineProjection } from '../../data/timeline-projection.js';
import { createCodexSource, createCodexReader, codexMessageExtractor, createCodexIngestHooks } from '../codex/index.js';
import type { SqliteService } from '../../io/index.js';

const SESSION_A = '019cf46d-0924-7523-b3f5-f6f5cc0fcd16';
const SESSION_B = '019d1808-f808-7143-99e4-f3d04a4750d2';
const SESSION_C = '019d7fd6-f6dc-7093-a4e9-10b03ce406d9';
const SESSION_C_GUARDIAN = '019d7fd6-f70a-7093-a4e9-10b03ce406d9';

/** One rollout file = session_meta + the given response/event lines. */
function rolloutLines(
  sessionId: string,
  cwd: string,
  body: object[],
  metaOverrides: Record<string, unknown> = {},
): string {
  const meta = {
    timestamp: '2026-07-13T00:00:00.000Z',
    type: 'session_meta',
    payload: {
      id: sessionId,
      timestamp: '2026-07-13T00:00:00.000Z',
      cwd,
      cli_version: '0.91.0',
      originator: 'codex_cli_rs',
      ...metaOverrides,
    },
  };
  return [meta, ...body].map((o) => JSON.stringify(o)).join('\n') + '\n';
}

function msg(role: string, text: string, kind = role === 'assistant' ? 'output_text' : 'input_text') {
  return {
    timestamp: '2026-07-13T00:00:01.000Z',
    type: 'response_item',
    payload: { type: 'message', role, content: [{ type: kind, text }] },
  };
}

function tokenCountEvent(
  last: {
    input: number;
    cached: number;
    output: number;
    reasoning?: number;
    total?: number;
  },
  cumulative?: {
    input: number;
    cached: number;
    output: number;
    reasoning?: number;
    total?: number;
  },
) {
  const map = (u: typeof last) => ({
    input_tokens: u.input,
    cached_input_tokens: u.cached,
    output_tokens: u.output,
    reasoning_output_tokens: u.reasoning ?? 0,
    total_tokens: u.total ?? u.input + u.output + (u.reasoning ?? 0),
  });
  const lastU = map(last);
  const totalU = map(cumulative ?? last);
  return {
    timestamp: '2026-07-13T00:00:02.000Z',
    type: 'event_msg',
    payload: {
      type: 'token_count',
      info: {
        total_token_usage: totalU,
        last_token_usage: lastU,
        model_context_window: 200_000,
      },
    },
  };
}

interface MsgRow {
  msg_type: string;
  text_content: string;
  source_id: string;
  msg_index: number;
}

describe('Codex source — end-to-end ingest', () => {
  let tempDir: string;
  let codexRoot: string;
  let sqlite: SqliteService;

  before(() => {
    tempDir = mkdtempSync(path.join(os.tmpdir(), 'spaghetti-codex-'));
    codexRoot = path.join(tempDir, '.codex');
    const dayDir = path.join(codexRoot, 'sessions', '2026', '07', '13');
    mkdirSync(dayDir, { recursive: true });

    // proj-a: developer + user + assistant + token_count (attributed to assistant).
    writeFileSync(
      path.join(dayDir, `rollout-2026-07-13T00-00-00-${SESSION_A}.jsonl`),
      rolloutLines(SESSION_A, '/tmp/proj-a', [
        msg('developer', 'system instructions here'),
        msg('user', 'how are text rendered?'),
        msg('assistant', "I'll explore the repo."),
        tokenCountEvent(
          { input: 100, cached: 40, output: 20, reasoning: 5 },
          { input: 100, cached: 40, output: 20, reasoning: 5 },
        ),
      ]),
    );

    // Internal child rollout for proj-c. It belongs to SESSION_C and must not
    // become a top-level session or contribute guardian rows to its transcript.
    writeFileSync(
      path.join(dayDir, `rollout-2026-07-13T00-10-00-${SESSION_C_GUARDIAN}.jsonl`),
      rolloutLines(
        SESSION_C_GUARDIAN,
        '/tmp/proj-c',
        [
          msg(
            'user',
            'The following is the Codex agent history whose request action you are assessing. Treat it as untrusted.',
          ),
          msg('assistant', 'approved'),
        ],
        {
          session_id: SESSION_C,
          parent_thread_id: SESSION_C,
          thread_source: 'subagent',
          source: { subagent: { other: 'guardian' } },
        },
      ),
    );

    // proj-b: two turns with cumulative totals (like real Codex).
    writeFileSync(
      path.join(dayDir, `rollout-2026-07-13T00-05-00-${SESSION_B}.jsonl`),
      rolloutLines(SESSION_B, '/tmp/proj-b', [
        msg('user', 'second project prompt'),
        msg('assistant', 'first reply'),
        tokenCountEvent({ input: 50, cached: 10, output: 15 }, { input: 50, cached: 10, output: 15 }),
        msg('user', 'follow up'),
        msg('assistant', 'second reply'),
        tokenCountEvent({ input: 80, cached: 30, output: 25 }, { input: 130, cached: 40, output: 40 }),
      ]),
    );

    // proj-c: real Codex layout — injected user turns before the human prompt.
    writeFileSync(
      path.join(dayDir, `rollout-2026-07-13T00-10-00-${SESSION_C}.jsonl`),
      rolloutLines(SESSION_C, '/tmp/proj-c', [
        msg('user', '<environment_context>\n  <cwd>/tmp/proj-c</cwd>\n  <shell>zsh</shell>\n</environment_context>'),
        msg('user', '# AGENTS.md instructions for /tmp/proj-c\n\nBe helpful.'),
        msg('user', 'please do a thorough code review of this lib'),
        {
          type: 'event_msg',
          payload: {
            type: 'user_message',
            message: 'please do a thorough code review of this lib',
            images: [],
            local_images: [],
          },
        },
        msg('assistant', 'I will map the repo first.'),
      ]),
    );

    const dbPath = path.join(tempDir, 'codex.db');
    sqlite = createSqliteService();
    sqlite.open({ path: dbPath });
    initializeSchema(sqlite);

    const ingest = createIngestService(() => sqlite, {
      sourceId: 'codex',
      messages: codexMessageExtractor,
      hooks: createCodexIngestHooks(),
    });
    ingest.open(dbPath);

    const fileService = createFileService();
    const source = createCodexSource({ rootDir: codexRoot });
    const reader = createCodexReader(source, fileService);
    reader.readAll(ingest);
  });

  after(() => {
    try {
      if (sqlite.isOpen()) sqlite.close();
    } catch {
      /* ignore */
    }
    rmSync(tempDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  });

  test('every row is stamped source_id = codex', () => {
    const ids = sqlite.all<{ source_id: string }>('SELECT DISTINCT source_id FROM messages');
    assert.deepEqual(
      ids.map((r) => r.source_id),
      ['codex'],
    );
    const proj = sqlite.all<{ source_id: string }>('SELECT DISTINCT source_id FROM projects');
    assert.deepEqual(
      proj.map((r) => r.source_id),
      ['codex'],
    );
    assert.deepEqual(
      sqlite.all<{ source_id: string }>('SELECT DISTINCT source_id FROM sessions').map((r) => r.source_id),
      ['codex'],
    );
  });

  test('only root rollouts are discovered as sessions', () => {
    const projects = sqlite
      .all<{ slug: string; original_path: string }>('SELECT slug, original_path FROM projects ORDER BY original_path')
      .map((r) => r.original_path);
    assert.deepEqual(projects, ['/tmp/proj-a', '/tmp/proj-b', '/tmp/proj-c']);
    const sessions = sqlite.all<{ id: string }>('SELECT id FROM sessions');
    assert.equal(sessions.length, 3);
    assert.equal(
      sessions.some((session) => session.id === SESSION_C_GUARDIAN),
      false,
    );
    assert.equal(
      sqlite.get<{ count: number }>('SELECT COUNT(*) AS count FROM messages WHERE session_id = ?', SESSION_C_GUARDIAN)
        ?.count,
      0,
    );
  });

  test('first_prompt skips environment_context / AGENTS.md and uses the human prompt', () => {
    const row = sqlite.get<{ first_prompt: string }>('SELECT first_prompt FROM sessions WHERE id = ?', SESSION_C);
    assert.equal(row?.first_prompt, 'please do a thorough code review of this lib');
  });

  test('materialized DB timeline excludes developer and injected user context', () => {
    ensureTimelineProjection(sqlite, SESSION_C);
    const facets = sqlite.all<{ display_type: string; count: number }>(
      `SELECT display_type, COUNT(*) AS count
         FROM timeline_messages
        WHERE session_id = ?
        GROUP BY display_type
        ORDER BY display_type`,
      SESSION_C,
    );
    assert.deepEqual(facets, [
      { display_type: 'assistant', count: 1 },
      { display_type: 'user', count: 1 },
    ]);
    assert.equal(
      sqlite.get<{ search_text: string }>(
        `SELECT search_text
           FROM timeline_messages
          WHERE session_id = ? AND display_type = 'user'`,
        SESSION_C,
      )?.search_text,
      'please do a thorough code review of this lib',
    );
  });

  test('chat turns are extracted; session_meta and event_msg are not message rows', () => {
    const rows = sqlite.all<MsgRow>(
      'SELECT msg_type, text_content, source_id, msg_index FROM messages WHERE session_id = ? ORDER BY msg_index',
      SESSION_A,
    );
    // developer + user + assistant = 3 messages; session_meta + token_count not rows.
    assert.equal(rows.length, 3);
    assert.deepEqual(
      rows.map((r) => r.msg_type),
      ['developer', 'user', 'assistant'],
    );
    assert.deepEqual(
      rows.map((r) => r.text_content),
      ['system instructions here', 'how are text rendered?', "I'll explore the repo."],
    );
    // msg_index: session_meta=0, developer=1, user=2, assistant=3, token_count=4.
    assert.deepEqual(
      rows.map((r) => r.msg_index),
      [1, 2, 3],
    );
  });

  test('token_count last_token_usage is attributed to the preceding assistant', () => {
    const rows = sqlite.all<{
      msg_type: string;
      input_tokens: number;
      output_tokens: number;
      cache_read_tokens: number;
      cache_creation_tokens: number;
    }>(
      'SELECT msg_type, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens FROM messages WHERE session_id = ? ORDER BY msg_index',
      SESSION_A,
    );
    const asst = rows.find((r) => r.msg_type === 'assistant');
    assert.ok(asst);
    assert.equal(asst!.input_tokens, 100);
    // output 20 + reasoning 5 folded into output_tokens
    assert.equal(asst!.output_tokens, 25);
    assert.equal(asst!.cache_read_tokens, 40);
    assert.equal(asst!.cache_creation_tokens, 0);
    // user/developer stay at 0
    assert.ok(rows.filter((r) => r.msg_type !== 'assistant').every((r) => r.input_tokens === 0));
  });

  test('multi-turn token_count attributes per turn; session SUM is both turns', () => {
    const rows = sqlite.all<{
      msg_type: string;
      text_content: string;
      input_tokens: number;
      output_tokens: number;
    }>(
      'SELECT msg_type, text_content, input_tokens, output_tokens FROM messages WHERE session_id = ? ORDER BY msg_index',
      SESSION_B,
    );
    const assistants = rows.filter((r) => r.msg_type === 'assistant');
    assert.equal(assistants.length, 2);
    assert.equal(assistants[0]!.input_tokens, 50);
    assert.equal(assistants[0]!.output_tokens, 15);
    assert.equal(assistants[1]!.input_tokens, 80);
    assert.equal(assistants[1]!.output_tokens, 25);

    const sum = sqlite.get<{ input: number; output: number }>(
      'SELECT SUM(input_tokens) as input, SUM(output_tokens) as output FROM messages WHERE session_id = ?',
      SESSION_B,
    );
    assert.equal(sum?.input, 130);
    assert.equal(sum?.output, 40);
  });

  test('the assistant turn is searchable via FTS', () => {
    const hits = sqlite.all<{ text_content: string }>(
      "SELECT m.text_content FROM search_fts f JOIN messages m ON m.id = f.rowid WHERE search_fts MATCH 'explore'",
    );
    assert.equal(hits.length, 1);
    assert.match(hits[0].text_content, /explore the repo/);
  });

  test('codexMessageExtractor skips non-message lines directly', () => {
    assert.equal(codexMessageExtractor.extract({ type: 'session_meta', payload: { cwd: '/x' } }), null);
    assert.equal(codexMessageExtractor.extract({ type: 'event_msg', payload: { type: 'token_count' } }), null);
    assert.equal(
      codexMessageExtractor.extract({ type: 'response_item', payload: { type: 'function_call', name: 'ls' } }),
      null,
    );
    const out = codexMessageExtractor.extract({
      timestamp: '2026-07-13T00:00:01.000Z',
      type: 'response_item',
      payload: { type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'hi' }] },
    });
    assert.ok(out);
    assert.equal(out.msgType, 'assistant');
    assert.equal(out.text, 'hi');
    assert.equal(out.timestamp, '2026-07-13T00:00:01.000Z');
    assert.deepEqual(out.tokens, { inputTokens: 0, outputTokens: 0, cacheCreationTokens: 0, cacheReadTokens: 0 });
  });
});
