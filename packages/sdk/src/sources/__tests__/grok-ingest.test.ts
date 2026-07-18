/**
 * grok-ingest.test.ts — the Grok AgentSource, end to end into a real store.
 *
 * Writes a synthetic `~/.grok/sessions/<url-encoded-cwd>/<uuid>/` tree (each
 * session dir carrying `chat_history.jsonl` + `summary.json`), then drives
 * `GrokReader.readAll` into a real `IngestService` (configured with
 * `sourceId: 'grok'` + `grokMessageExtractor`) over a real SQLite schema, and
 * asserts the rows land: tagged `source_id = 'grok'`, all canonical record
 * types retained, synthetic users classified, and rich timeline rows produced.
 *
 * Exercises the RFC 006 seams for the third source — record production (reader),
 * extraction (extractor), and the `source_id` write path — without the
 * multi-source lifecycle orchestration.
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
import { createGrokSource, createGrokReader, grokMessageExtractor } from '../grok/index.js';
import type { SqliteService } from '../../io/index.js';

const SESSION_A = '019f5d61-da35-7b60-a1b5-02055fd8fcdd';
const SESSION_B = '019f54c0-0dd3-7482-a3ee-e73ca610e8a3';

/** Write one Grok session dir: sessions/<enc(cwd)>/<uuid>/{chat_history,summary}. */
function writeSession(grokRoot: string, cwd: string, sessionId: string, title: string, chatLines: object[]): void {
  const sessionDir = path.join(grokRoot, 'sessions', encodeURIComponent(cwd), sessionId);
  mkdirSync(sessionDir, { recursive: true });
  writeFileSync(path.join(sessionDir, 'chat_history.jsonl'), chatLines.map((o) => JSON.stringify(o)).join('\n') + '\n');
  writeFileSync(
    path.join(sessionDir, 'summary.json'),
    JSON.stringify({
      info: { id: sessionId, cwd },
      created_at: '2026-07-13T21:28:41.941460Z',
      updated_at: '2026-07-13T23:07:59.611347Z',
      generated_title: title,
      session_summary: title,
      head_branch: 'main',
      git_root_dir: cwd + '/',
    }),
  );
}

interface MsgRow {
  msg_type: string;
  text_content: string;
  source_id: string;
  msg_index: number;
}

describe('Grok source — end-to-end ingest', () => {
  let tempDir: string;
  let grokRoot: string;
  let sqlite: SqliteService;

  before(() => {
    tempDir = mkdtempSync(path.join(os.tmpdir(), 'spaghetti-grok-'));
    grokRoot = path.join(tempDir, '.grok');

    // proj-a exercises internal context, a wrapped human query and local tools.
    writeSession(grokRoot, '/tmp/proj-a', SESSION_A, 'Codebase Onboarding', [
      { type: 'system', content: 'You are Grok, a coding assistant.' },
      {
        type: 'user',
        synthetic_reason: 'project_instructions',
        content: [{ type: 'text', text: '<system-reminder>internal rules</system-reminder>' }],
      },
      {
        type: 'user',
        content: [
          {
            type: 'text',
            text: '<user_query>how are text rendered?</user_query>\n<system-reminder>later context</system-reminder>',
          },
        ],
      },
      {
        type: 'assistant',
        content: "I'll explore the repo.",
        tool_calls: [{ id: 'call-1', name: 'list_dir', arguments: '{}' }],
      },
      {
        type: 'reasoning',
        id: 'rs_1',
        summary: [{ type: 'summary_text', text: 'The user wants onboarding.' }],
        encrypted_content: 'xxx',
        status: 'completed',
      },
      { type: 'tool_result', tool_call_id: 'call-1', content: 'a/\nb/\nc.ts' },
    ]);

    // proj-b exercises a standalone backend web-search call.
    writeSession(grokRoot, '/tmp/proj-b', SESSION_B, 'Token Research', [
      { type: 'user', content: [{ type: 'text', text: 'second project prompt' }] },
      {
        type: 'backend_tool_call',
        kind: { tool_type: 'web_search', id: 'ws-1', action: { type: 'search', query: 'tokens' } },
      },
      { type: 'assistant', content: 'here is the answer' },
    ]);

    const dbPath = path.join(tempDir, 'grok.db');
    sqlite = createSqliteService();
    sqlite.open({ path: dbPath });
    initializeSchema(sqlite);

    const ingest = createIngestService(() => sqlite, { sourceId: 'grok', messages: grokMessageExtractor });
    ingest.open(dbPath);

    const fileService = createFileService();
    const source = createGrokSource({ rootDir: grokRoot });
    const reader = createGrokReader(source, fileService);
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

  test('every row is stamped source_id = grok', () => {
    for (const table of ['messages', 'projects', 'sessions']) {
      const ids = sqlite.all<{ source_id: string }>(`SELECT DISTINCT source_id FROM ${table}`);
      assert.deepEqual(
        ids.map((r) => r.source_id),
        ['grok'],
        `${table} should be all grok`,
      );
    }
  });

  test('two projects and two sessions were discovered from the session tree', () => {
    const projects = sqlite
      .all<{ original_path: string }>('SELECT original_path FROM projects ORDER BY original_path')
      .map((r) => r.original_path);
    assert.deepEqual(projects, ['/tmp/proj-a', '/tmp/proj-b']);
    assert.equal(sqlite.all<{ id: string }>('SELECT id FROM sessions').length, 2);
  });

  test('all canonical records are retained with useful projections', () => {
    const rows = sqlite.all<MsgRow>(
      'SELECT msg_type, text_content, source_id, msg_index FROM messages WHERE session_id = ? ORDER BY msg_index',
      SESSION_A,
    );
    assert.deepEqual(
      rows.map((r) => r.msg_type),
      ['system', 'context', 'user', 'assistant', 'reasoning', 'tool_result'],
    );
    assert.deepEqual(
      rows.map((r) => r.text_content),
      [
        '',
        '',
        'how are text rendered?',
        "I'll explore the repo.\nlist_dir {}",
        'The user wants onboarding.',
        'a/\nb/\nc.ts',
      ],
    );
    assert.deepEqual(
      rows.map((r) => r.msg_index),
      [0, 1, 2, 3, 4, 5],
    );

    const bRows = sqlite.all<MsgRow>(
      'SELECT msg_type, msg_index FROM messages WHERE session_id = ? ORDER BY msg_index',
      SESSION_B,
    );
    assert.deepEqual(
      bRows.map((r) => r.msg_type),
      ['user', 'tool_use', 'assistant'],
    );
    assert.deepEqual(
      bRows.map((r) => r.msg_index),
      [0, 1, 2],
    );
  });

  test('the assistant turn is searchable via FTS', () => {
    const hits = sqlite.all<{ text_content: string }>(
      "SELECT m.text_content FROM search_fts f JOIN messages m ON m.id = f.rowid WHERE search_fts MATCH 'explore'",
    );
    assert.equal(hits.length, 1);
    assert.match(hits[0].text_content, /explore the repo/);
  });

  test('the session title comes from summary.json generated_title', () => {
    const row = sqlite.get<{ first_prompt: string }>('SELECT first_prompt FROM sessions WHERE id = ?', SESSION_A);
    assert.equal(row?.first_prompt, 'Codebase Onboarding');
  });

  test('grokMessageExtractor: context and tool I/O are retained without per-message tokens/time', () => {
    const result = grokMessageExtractor.extract({ type: 'tool_result', tool_call_id: 'c', content: 'x' });
    assert.equal(result?.msgType, 'tool_result');
    assert.equal(result?.uuid, 'c');
    assert.equal(result?.text, 'x');

    const backend = grokMessageExtractor.extract({
      type: 'backend_tool_call',
      kind: { tool_type: 'web_search', id: 'ws', action: { query: 'tokens' } },
    });
    assert.equal(backend?.msgType, 'tool_use');
    assert.equal(backend?.uuid, 'ws');

    const user = grokMessageExtractor.extract({ type: 'user', content: [{ type: 'text', text: 'hi' }] });
    assert.equal(user?.msgType, 'user');
    assert.equal(user?.text, 'hi');

    const context = grokMessageExtractor.extract({
      type: 'user',
      synthetic_reason: 'system_reminder',
      content: [{ type: 'text', text: 'internal' }],
    });
    assert.equal(context?.msgType, 'context');
    assert.equal(context?.text, '');

    const reasoning = grokMessageExtractor.extract({
      type: 'reasoning',
      id: 'rs_9',
      summary: [{ type: 'summary_text', text: 'thinking' }],
      encrypted_content: 'zzz',
    });
    assert.equal(reasoning?.msgType, 'reasoning');
    assert.equal(reasoning?.text, 'thinking');
    assert.equal(reasoning?.uuid, 'rs_9');
    assert.equal(reasoning?.timestamp, null);
    assert.deepEqual(reasoning?.tokens, {
      inputTokens: 0,
      outputTokens: 0,
      cacheCreationTokens: 0,
      cacheReadTokens: 0,
    });
  });

  test('materialized timeline exposes human, assistant, thinking and paired tool rows', () => {
    ensureTimelineProjection(sqlite, SESSION_A);
    const rows = sqlite.all<{ display_type: string; tool_name: string | null; data: string }>(
      `SELECT display_type, tool_name, data
         FROM timeline_messages
        WHERE session_id = ?
        ORDER BY timeline_index`,
      SESSION_A,
    );
    assert.deepEqual(
      rows.map((row) => row.display_type),
      ['user', 'assistant', 'tool_use', 'thinking'],
    );
    assert.equal(rows[2]?.tool_name, 'list_dir');
    const tool = JSON.parse(rows[2]!.data) as { toolUse?: { result?: { content?: string; rawJson?: unknown } } };
    assert.equal(tool.toolUse?.result?.content, 'a/\nb/\nc.ts');
    assert.equal(tool.toolUse?.result?.rawJson, undefined);

    ensureTimelineProjection(sqlite, SESSION_B);
    const backendRows = sqlite.all<{ display_type: string; tool_name: string | null }>(
      'SELECT display_type, tool_name FROM timeline_messages WHERE session_id = ? ORDER BY timeline_index',
      SESSION_B,
    );
    assert.deepEqual(backendRows, [
      { display_type: 'user', tool_name: null },
      { display_type: 'tool_use', tool_name: 'web_search' },
      { display_type: 'assistant', tool_name: null },
    ]);
  });
});
