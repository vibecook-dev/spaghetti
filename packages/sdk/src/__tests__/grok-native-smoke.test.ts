/**
 * grok-native-smoke.test.ts — end-to-end Grok cold ingest through the native
 * engine (engine=rs → GrokLifecycleOwner → native.ingest({ sourceId: 'grok' })).
 *
 * Uses the committed small-grok fixture. Skips when the native addon is not
 * loadable (unsupported platform / missing prebuild) so CI hosts without a
 * rebuilt binary still pass; when native is present, asserts the full product
 * surface (source ids, projects, rich messages, timeline tools, FTS truncate).
 *
 * Pair with `pnpm test:ingest-diff:grok` for RS↔TS row parity.
 *
 * Run: pnpm --filter @vibecook/spaghetti-sdk test
 */

import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { mkdtempSync, rmSync } from 'node:fs';

import { createSpaghettiService, createGrokSource, loadNativeAddon } from '../index.js';
import type { SpaghettiAPI } from '../index.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const GROK_FIXTURE = path.resolve(here, '../../../../crates/spaghetti-napi/fixtures/small-grok/.grok');

const SESS_A1 = '019f5d61-da35-7b60-a1b5-02055fd8fcdd';
const SLUG_A = '-tmp-grok-proj-a';
const SLUG_B = '-tmp-grok-proj-b';
const SLUG_C = '-Users-test-grok-long';

const native = loadNativeAddon();

describe('Grok native cold ingest smoke', { skip: !native }, () => {
  let spaghetti: SpaghettiAPI;
  let tempDir: string;
  let progressMessages: string[];

  before(async () => {
    assert.ok(native, 'native addon required (test is skipped when missing)');
    tempDir = mkdtempSync(path.join(os.tmpdir(), 'spaghetti-grok-native-'));
    progressMessages = [];

    spaghetti = createSpaghettiService({
      source: createGrokSource({ rootDir: GROK_FIXTURE }),
      dbPath: path.join(tempDir, 'spaghetti.db'),
      engine: 'rs',
    });

    spaghetti.onProgress((p) => {
      if (p.message) progressMessages.push(p.message);
    });

    await spaghetti.initialize();
  });

  after(async () => {
    // Await the full teardown before deleting: Windows cannot remove a
    // directory while the SQLite handle is still open (EPERM), and the
    // retry knobs absorb transient locks from AV scanners.
    await spaghetti.dispose();
    try {
      rmSync(tempDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
    } catch (err) {
      // On Windows the rs-engine path can keep the DB handle alive
      // marginally longer than dispose() (native connection teardown is
      // not synchronously observable from JS). A leaked tmpdir is
      // preferable to a red suite whose assertions all passed — but keep
      // it loud so a real regression stays visible.
      console.warn(`[grok-native-smoke] temp dir not removed: ${String(err)}`);
    }
  });

  test('progress mentions native Grok ingest', () => {
    const joined = progressMessages.join('\n');
    assert.match(
      joined,
      /native Grok ingest/i,
      `expected native progress line, got:\n${joined || '(no progress events)'}`,
    );
  });

  test('getSourceIds reports only grok', () => {
    assert.deepEqual(spaghetti.getSourceIds(), ['grok']);
  });

  test('three fixture projects are indexed', () => {
    const projects = spaghetti.getProjectList({ sourceId: 'grok' });
    const slugs = projects.map((p) => p.slug).sort();
    assert.deepEqual(slugs, [SLUG_A, SLUG_B, SLUG_C].sort());
    for (const p of projects) {
      assert.equal(p.sourceId, 'grok');
    }
  });

  test('session A1 has title and retains rich transcript records', () => {
    const sessions = spaghetti.getSessionList(SLUG_A, { sourceId: 'grok' });
    const a1 = sessions.find((s) => s.sessionId === SESS_A1);
    assert.ok(a1, 'session A1 present');
    assert.equal(a1.sourceId, 'grok');
    assert.equal(a1.firstPrompt, 'Codebase Onboarding');

    const { messages } = spaghetti.getSessionMessages(SLUG_A, SESS_A1, 50, 0, { sourceId: 'grok' });
    const blob = messages.map((m) => JSON.stringify(m)).join('\n');
    assert.ok(blob.includes('how is text rendered?'), 'user turn present');
    assert.ok(blob.includes("I'll explore the repo."), 'assistant turn present');
    assert.ok(blob.includes('The user wants onboarding help.'), 'reasoning summary present');
    const rawToolResult = messages.find((message) => (message as { type?: string }).type === 'tool_result') as
      | { content?: string }
      | undefined;
    assert.equal(rawToolResult?.content, 'a/\nb/\nc.ts', 'tool_result content is retained');

    const types = messages.map((m) => {
      const rec = m as { type?: string };
      return rec.type;
    });
    // SessionMessage shape from store is the raw Grok line (type field).
    assert.ok(types.includes('system'));
    assert.ok(types.includes('user'));
    assert.ok(types.includes('assistant'));
    assert.ok(types.includes('reasoning'));
    assert.ok(types.includes('tool_result'));

    const timeline = spaghetti.getSessionTimeline(SLUG_A, SESS_A1, { limit: 50 });
    const tool = timeline.messages.find((message) => message.type === 'tool_use');
    assert.equal(tool?.toolUse?.toolName, 'list_dir');
    assert.equal(tool?.toolUse?.result?.content, 'a/\nb/\nc.ts');
    assert.ok(timeline.messages.some((message) => message.type === 'thinking'));
    assert.ok(!timeline.messages.some((message) => message.type === 'system'));
  });

  test('long assistant keeps full raw content; user turn is searchable', () => {
    const sessions = spaghetti.getSessionList(SLUG_C, { sourceId: 'grok' });
    assert.equal(sessions.length, 1);
    const sid = sessions[0].sessionId;
    const { messages } = spaghetti.getSessionMessages(SLUG_C, sid, 20, 0, { sourceId: 'grok' });
    const assistant = messages.find((m) => (m as { type?: string }).type === 'assistant') as
      | { type?: string; content?: string }
      | undefined;
    assert.ok(assistant);
    // messages.data stores the full JSONL record (2500-char content) — FTS truncates separately.
    assert.equal(typeof assistant.content, 'string');
    assert.equal(assistant.content!.length, 2500);

    const results = spaghetti.search({ text: 'write a long answer' });
    assert.ok(results.total > 0 || results.results.length > 0, 'user prompt from long-session is searchable');
  });

  test('stats report grok messages', () => {
    const stats = spaghetti.getStats();
    // 16 canonical lines across the fixture (see ingest-diff:grok)
    const msgCount = stats.segmentsByType.messages ?? stats.searchIndexed;
    assert.ok(msgCount >= 16, `expected >=16 messages, got ${msgCount}`);
  });
});
