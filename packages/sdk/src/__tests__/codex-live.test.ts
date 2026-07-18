/**
 * codex-live.test.ts — Codex Plane 2 (live disk ingest).
 *
 * Starts a live multi-source service, appends a new turn to a Codex rollout on
 * disk, and asserts the watcher streams it into the shared store (so `api.live`
 * reflects Codex activity). Uses the real filesystem watcher, so it polls with a
 * timeout rather than asserting synchronously.
 *
 * Run with `pnpm --filter @vibecook/spaghetti-sdk test`.
 */

import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { mkdtempSync, mkdirSync, writeFileSync, appendFileSync, rmSync } from 'node:fs';

import { createSpaghettiService } from '../index.js';
import { createCodexSource } from '../sources/index.js';
import type { SpaghettiAPI } from '../index.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE_ROOT_DIR = path.resolve(here, '../../../../crates/spaghetti-napi/fixtures/small/.claude');
const CODEX_SESSION = '019cf46d-0924-7523-b3f5-f6f5cc0fcd16';
const CODEX_CWD = '/tmp/codex-live-proj';
const CODEX_SLUG = '-tmp-codex-live-proj';

function line(obj: object): string {
  return JSON.stringify(obj) + '\n';
}

async function waitFor(fn: () => boolean, timeoutMs = 5000): Promise<boolean> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (fn()) return true;
    await new Promise((r) => setTimeout(r, 40));
  }
  return false;
}

describe('Codex live disk ingest (Plane 2)', () => {
  let spaghetti: SpaghettiAPI;
  let tempDir: string;
  let rolloutPath: string;

  before(async () => {
    tempDir = mkdtempSync(path.join(os.tmpdir(), 'spaghetti-codex-live-'));
    const codexRoot = path.join(tempDir, '.codex');
    const dayDir = path.join(codexRoot, 'sessions', '2026', '07', '13');
    mkdirSync(dayDir, { recursive: true });
    rolloutPath = path.join(dayDir, `rollout-2026-07-13T00-00-00-${CODEX_SESSION}.jsonl`);
    // session_meta + one user turn.
    writeFileSync(
      rolloutPath,
      line({
        timestamp: '2026-07-13T00:00:00.000Z',
        type: 'session_meta',
        payload: { id: CODEX_SESSION, cwd: CODEX_CWD, cli_version: '0.91.0' },
      }) +
        line({
          timestamp: '2026-07-13T00:00:01.000Z',
          type: 'response_item',
          payload: { type: 'message', role: 'user', content: [{ type: 'input_text', text: 'first turn' }] },
        }),
    );

    spaghetti = createSpaghettiService({
      rootDir: FIXTURE_ROOT_DIR,
      additionalSources: [createCodexSource({ rootDir: codexRoot })],
      dbPath: path.join(tempDir, 'spaghetti.db'),
      live: true,
    });
    await spaghetti.initialize();
  });

  after(async () => {
    // Await the full teardown before deleting: Windows cannot remove a
    // directory while the SQLite handle is still open (EPERM), and the
    // retry knobs absorb transient locks from AV scanners.
    await spaghetti.dispose();
    rmSync(tempDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  });

  function codexMessageCount(): number {
    return spaghetti.getSessionMessages(CODEX_SLUG, CODEX_SESSION, 100, 0).total;
  }

  test('baseline: the initial turn is ingested', () => {
    assert.equal(codexMessageCount(), 1);
  });

  test('appended prose and tool records are streamed into the store live', async () => {
    // Append an assistant turn (plus a token_count event the extractor skips).
    appendFileSync(
      rolloutPath,
      line({
        timestamp: '2026-07-13T00:00:02.000Z',
        type: 'event_msg',
        payload: { type: 'token_count', info: {} },
      }) +
        line({
          timestamp: '2026-07-13T00:00:03.000Z',
          type: 'response_item',
          payload: {
            type: 'message',
            role: 'assistant',
            content: [{ type: 'output_text', text: 'live streamed reply' }],
          },
        }),
    );

    const arrived = await waitFor(() => codexMessageCount() === 2);
    assert.ok(arrived, 'the appended turn should be ingested live within the timeout');

    const { messages } = spaghetti.getSessionMessages(CODEX_SLUG, CODEX_SESSION, 100, 0);
    const texts = messages.map((m) => JSON.stringify(m));
    assert.ok(
      texts.some((t) => t.includes('live streamed reply')),
      'the appended assistant turn is present',
    );
    // The skipped event_msg did not become a message row.
    assert.equal(codexMessageCount(), 2);

    appendFileSync(
      rolloutPath,
      line({
        timestamp: '2026-07-13T00:00:04.000Z',
        type: 'response_item',
        payload: {
          type: 'function_call',
          id: 'fc-live',
          call_id: 'call-live',
          name: 'wait',
          arguments: '{"cell_id":"live"}',
        },
      }) +
        line({
          timestamp: '2026-07-13T00:00:05.000Z',
          type: 'response_item',
          payload: { type: 'function_call_output', call_id: 'call-live', output: 'live tool result' },
        }),
    );

    const toolArrived = await waitFor(() => codexMessageCount() === 4);
    assert.ok(toolArrived, 'the tool call and result should be ingested live within the timeout');
    const timeline = spaghetti.getSessionTimeline(CODEX_SLUG, CODEX_SESSION, { limit: 20 });
    const tool = timeline.messages.find((message) => message.type === 'tool_use');
    assert.equal(tool?.toolUse?.toolName, 'wait');
    assert.equal(tool?.toolUse?.result?.content, 'live tool result');
  });
});
