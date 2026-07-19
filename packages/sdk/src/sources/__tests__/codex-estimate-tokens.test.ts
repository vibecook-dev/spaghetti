/**
 * Tiktoken estimate helper + end-to-end ingest when token_count is absent.
 */

import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';

import { createSqliteService } from '../../io/sqlite-service.js';
import { createFileService } from '../../io/file-service.js';
import { createIngestService } from '../../data/ingest-service.js';
import { createQueryService } from '../../data/query-service.js';
import { initializeSchema } from '../../data/schema.js';
import { createCodexSource, createCodexReader, codexMessageExtractor, createCodexIngestHooks } from '../codex/index.js';
import { countTextTokens, estimateTokensFromMessageRows } from '../codex/estimate-tokens.js';
import type { SqliteService } from '../../io/index.js';

const SESSION_NO_USAGE = '019cf46d-aaaa-7523-b3f5-f6f5cc0fcd99';
const SESSION_WITH_USAGE = '019cf46d-bbbb-7523-b3f5-f6f5cc0fcd88';

describe('countTextTokens / estimateTokensFromMessageRows', () => {
  test('empty text is 0', () => {
    assert.equal(countTextTokens(''), 0);
    assert.equal(countTextTokens(null), 0);
  });

  test('counts non-zero tokens for prose', () => {
    const n = countTextTokens('hello world, how are you today?');
    assert.ok(n > 0);
    assert.ok(n < 50);
  });

  test('maps user→input and assistant→output', () => {
    const est = estimateTokensFromMessageRows([
      { msg_index: 1, msg_type: 'user', text_content: 'hello there friend' },
      { msg_index: 2, msg_type: 'assistant', text_content: 'hi back to you' },
      { msg_index: 3, msg_type: 'system', text_content: 'ignored' },
    ]);
    assert.equal(est.length, 2);
    assert.ok(est[0]!.inputTokens > 0);
    assert.equal(est[0]!.outputTokens, 0);
    assert.equal(est[1]!.inputTokens, 0);
    assert.ok(est[1]!.outputTokens > 0);
  });
});

describe('Codex tiktoken fallback when token_count is missing', () => {
  let tempDir: string;
  let sqlite: SqliteService;

  before(() => {
    tempDir = mkdtempSync(path.join(os.tmpdir(), 'spaghetti-codex-est-'));
    const codexRoot = path.join(tempDir, '.codex');
    const dayDir = path.join(codexRoot, 'sessions', '2026', '07', '13');
    mkdirSync(dayDir, { recursive: true });

    const meta = (id: string, cwd: string) => ({
      timestamp: '2026-07-13T00:00:00.000Z',
      type: 'session_meta',
      payload: { id, cwd, cli_version: '0.91.0', originator: 'codex_cli_rs' },
    });
    const msg = (role: string, text: string) => ({
      timestamp: '2026-07-13T00:00:01.000Z',
      type: 'response_item',
      payload: {
        type: 'message',
        role,
        content: [{ type: role === 'assistant' ? 'output_text' : 'input_text', text }],
      },
    });
    const tokenCount = {
      timestamp: '2026-07-13T00:00:02.000Z',
      type: 'event_msg',
      payload: {
        type: 'token_count',
        info: {
          total_token_usage: {
            input_tokens: 999,
            cached_input_tokens: 0,
            output_tokens: 50,
            reasoning_output_tokens: 0,
            total_tokens: 1049,
          },
          last_token_usage: {
            input_tokens: 999,
            cached_input_tokens: 0,
            output_tokens: 50,
            reasoning_output_tokens: 0,
            total_tokens: 1049,
          },
          model_context_window: 200_000,
        },
      },
    };

    // No token_count → estimate
    writeFileSync(
      path.join(dayDir, `rollout-2026-07-13T00-00-00-${SESSION_NO_USAGE}.jsonl`),
      [
        meta(SESSION_NO_USAGE, '/tmp/est-proj'),
        msg('user', 'please estimate my tokens from this user message text'),
        msg('assistant', 'sure, here is a longer assistant reply with several words'),
      ]
        .map((o) => JSON.stringify(o))
        .join('\n') + '\n',
    );

    // With token_count → official, not estimated
    writeFileSync(
      path.join(dayDir, `rollout-2026-07-13T00-05-00-${SESSION_WITH_USAGE}.jsonl`),
      [
        meta(SESSION_WITH_USAGE, '/tmp/official-proj'),
        msg('user', 'official path'),
        msg('assistant', 'official reply'),
        tokenCount,
      ]
        .map((o) => JSON.stringify(o))
        .join('\n') + '\n',
    );

    const dbPath = path.join(tempDir, 'est.db');
    sqlite = createSqliteService();
    sqlite.open({ path: dbPath });
    initializeSchema(sqlite);

    const ingest = createIngestService(() => sqlite, {
      sourceId: 'codex',
      messages: codexMessageExtractor,
      hooks: createCodexIngestHooks(),
    });
    ingest.open(dbPath);
    const reader = createCodexReader(createCodexSource({ rootDir: codexRoot }), createFileService());
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

  test('session without token_count is estimated and has non-zero text tokens', () => {
    const row = sqlite.get<{ tokens_estimated: number; input: number; output: number }>(
      `SELECT s.tokens_estimated as tokens_estimated,
              COALESCE((SELECT SUM(input_tokens) FROM messages WHERE session_id = s.id), 0) as input,
              COALESCE((SELECT SUM(output_tokens) FROM messages WHERE session_id = s.id), 0) as output
       FROM sessions s WHERE s.id = ?`,
      SESSION_NO_USAGE,
    );
    assert.ok(row);
    assert.equal(row!.tokens_estimated, 1);
    assert.ok(row!.input > 0, 'user text estimated as input');
    assert.ok(row!.output > 0, 'assistant text estimated as output');
    // Estimates must not magically match the official fixture numbers
    assert.notEqual(row!.input, 999);
  });

  test('session with token_count is official (tokens_estimated=0)', () => {
    const row = sqlite.get<{ tokens_estimated: number; input: number; output: number }>(
      `SELECT s.tokens_estimated as tokens_estimated,
              COALESCE((SELECT SUM(input_tokens) FROM messages WHERE session_id = s.id), 0) as input,
              COALESCE((SELECT SUM(output_tokens) FROM messages WHERE session_id = s.id), 0) as output
       FROM sessions s WHERE s.id = ?`,
      SESSION_WITH_USAGE,
    );
    assert.ok(row);
    assert.equal(row!.tokens_estimated, 0);
    assert.equal(row!.input, 999);
    assert.equal(row!.output, 50);
  });

  test('query summaries surface tokensEstimated', () => {
    const query = createQueryService(() => sqlite);
    query.prepareTokenActivity();
    const est = query.getSessionSummaries('-tmp-est-proj', { sourceId: 'codex' });
    assert.equal(est.length, 1);
    assert.equal(est[0]!.tokensEstimated, true);
    assert.ok(totalTokens(est[0]!.tokenUsage) > 0);

    const off = query.getSessionSummaries('-tmp-official-proj', { sourceId: 'codex' });
    assert.equal(off.length, 1);
    assert.equal(off[0]!.tokensEstimated, false);
    assert.equal(off[0]!.tokenUsage.inputTokens, 999);

    const projEst = query.getProjectSummaries({ sourceId: 'codex' }).find((p) => p.slug === '-tmp-est-proj');
    assert.ok(projEst);
    assert.equal(projEst!.tokensEstimated, true);
  });
});

function totalTokens(u: {
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
}): number {
  return u.inputTokens + u.outputTokens + u.cacheCreationTokens + u.cacheReadTokens;
}
