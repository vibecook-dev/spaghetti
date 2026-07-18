/**
 * grok-multi-source.test.ts — createSpaghettiService ingests Grok alongside Claude.
 *
 * Wires the real Claude `small` fixture plus a synthetic Grok session tree via
 * `additionalSources`, initializes once, and asserts the unified index includes
 * Grok: `getSourceIds()` reports it, `getProjectList()` includes the Grok
 * project, and its session + turns are queryable. Proves the `create.ts` Grok
 * owner branch and the RFC 006 multi-source lifecycle end to end.
 *
 * Run with `pnpm --filter @vibecook/spaghetti-sdk test`.
 */

import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';

import { createSpaghettiService } from '../index.js';
import { createGrokSource } from '../sources/index.js';
import type { SpaghettiAPI } from '../index.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE_ROOT_DIR = path.resolve(here, '../../../../crates/spaghetti-napi/fixtures/small/.claude');
const GROK_SESSION = '019f5d61-da35-7b60-a1b5-02055fd8fcdd';
const GROK_CWD = '/tmp/grok-proj';
const GROK_SLUG = '-tmp-grok-proj';

function writeGrokFixture(grokRoot: string): void {
  const sessionDir = path.join(grokRoot, 'sessions', encodeURIComponent(GROK_CWD), GROK_SESSION);
  mkdirSync(sessionDir, { recursive: true });
  const lines = [
    { type: 'system', content: 'You are Grok.' },
    { type: 'user', content: [{ type: 'text', text: 'grok hello' }] },
    { type: 'reasoning', id: 'rs_1', summary: [{ type: 'summary_text', text: 'thinking about it' }] },
    { type: 'assistant', content: 'grok reply', tool_calls: [{ id: 'c1', name: 'read_file', arguments: '{}' }] },
    { type: 'tool_result', tool_call_id: 'c1', content: 'tool output' },
  ];
  writeFileSync(path.join(sessionDir, 'chat_history.jsonl'), lines.map((l) => JSON.stringify(l)).join('\n') + '\n');
  writeFileSync(
    path.join(sessionDir, 'summary.json'),
    JSON.stringify({
      info: { id: GROK_SESSION, cwd: GROK_CWD },
      created_at: '2026-07-13T21:28:41.941Z',
      updated_at: '2026-07-13T23:07:59.611Z',
      generated_title: 'Grok Onboarding Session',
      head_branch: 'main',
    }),
  );
}

describe('multi-source ingest (claude + grok)', () => {
  let spaghetti: SpaghettiAPI;
  let tempDir: string;
  let grokRoot: string;

  before(async () => {
    tempDir = mkdtempSync(path.join(os.tmpdir(), 'spaghetti-grok-ms-'));
    grokRoot = path.join(tempDir, '.grok');
    writeGrokFixture(grokRoot);

    spaghetti = createSpaghettiService({
      rootDir: FIXTURE_ROOT_DIR,
      additionalSources: [createGrokSource({ rootDir: grokRoot })],
      dbPath: path.join(tempDir, 'spaghetti.db'),
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

  test('getSourceIds() reports grok alongside claude-code', () => {
    assert.deepEqual(spaghetti.getSourceIds(), ['claude-code', 'grok']);
  });

  test('getProjectList includes the grok project', () => {
    const grokProject = spaghetti.getProjectList().find((p) => p.sourceId === 'grok');
    assert.ok(grokProject, 'grok project present');
    assert.equal(grokProject.slug, GROK_SLUG);
  });

  test('the grok session is queryable with its title and rich tool I/O', () => {
    const sessions = spaghetti.getSessionList(GROK_SLUG, { sourceId: 'grok' });
    assert.equal(sessions.length, 1);
    assert.equal(sessions[0].sessionId, GROK_SESSION);
    assert.equal(sessions[0].sourceId, 'grok');

    const { messages } = spaghetti.getSessionMessages(GROK_SLUG, GROK_SESSION, 50, 0, { sourceId: 'grok' });
    const blob = messages.map((m) => JSON.stringify(m)).join('\n');
    assert.ok(blob.includes('grok hello'), 'user turn present');
    assert.ok(blob.includes('grok reply'), 'assistant turn present');
    assert.ok(blob.includes('thinking about it'), 'reasoning summary present');
    assert.ok(blob.includes('tool output'), 'tool_result was retained');
    const timeline = spaghetti.getSessionTimeline(GROK_SLUG, GROK_SESSION, { limit: 20 });
    const tool = timeline.messages.find((message) => message.type === 'tool_use');
    assert.equal(tool?.toolUse?.toolName, 'read_file');
    assert.equal(tool?.toolUse?.result?.content, 'tool output');
  });

  test('getProjectList filters to the grok source only', () => {
    const grokOnly = spaghetti.getProjectList({ sourceId: 'grok' });
    assert.equal(grokOnly.length, 1);
    assert.equal(grokOnly[0].slug, GROK_SLUG);
  });
});
