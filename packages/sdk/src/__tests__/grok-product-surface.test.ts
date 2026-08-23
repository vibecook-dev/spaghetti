/**
 * Grok's product surface, through the engine that ships.
 *
 * This replaces `grok-native-smoke.test.ts`, which drove the retired RFC 003
 * bulk writer (`createSpaghettiService({ engine: 'rs' })` → `native.ingest`).
 * That function only exists under the `legacy-oracle` Cargo feature, so its
 * gate — `loadLegacyNativeAddon()` — was never satisfied by the default addon
 * and the whole suite had been skipping silently for the entire 012 period.
 * The assertions were worth keeping; the path under them was not.
 *
 * What is checked is Grok-specific and not covered by the multi-adapter host
 * test: that reasoning, tool calls and their results, and long assistant
 * content survive Grok's decoder into the shared product API.
 *
 * Skips only when the native addon cannot load at all (unsupported platform or
 * missing prebuild). It does not skip on a feature the default build lacks.
 */

import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { cpSync, mkdtempSync, rmSync } from 'node:fs';

import { createObservationService, loadNativeAddon, type ObservationService } from '../index.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const GROK_FIXTURE = path.resolve(here, '../../../../crates/spaghetti-napi/fixtures/small-grok/.grok');

const SESS_A1 = '019f5d61-da35-7b60-a1b5-02055fd8fcdd';
const SLUG_A = '-tmp-grok-proj-a';
const SLUG_B = '-tmp-grok-proj-b';
const SLUG_C = '-Users-test-grok-long';

const native = loadNativeAddon();

describe('Grok product surface', { skip: !native }, () => {
  let service: ObservationService;
  let tempDir: string;

  before(async () => {
    tempDir = mkdtempSync(path.join(os.tmpdir(), 'spaghetti-grok-'));
    const grokRoot = path.join(tempDir, '.grok');
    cpSync(GROK_FIXTURE, grokRoot, { recursive: true, preserveTimestamps: true });
    service = createObservationService({
      dbPath: path.join(tempDir, 'spaghetti.db'),
      sources: [{ adapterId: 'grok', roots: [grokRoot] }],
      ownerLabel: 'grok-product-surface-test',
      live: false,
    });
    await service.initialize();
  });

  after(async () => {
    await service.dispose();
    rmSync(tempDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  });

  test('the configured adapter is the only source', async () => {
    assert.deepEqual(await service.getSourceIds(), ['grok']);
  });

  test('all three fixture projects are indexed under grok', async () => {
    const projects = await service.getProjectList({ sourceId: 'grok' });
    assert.deepEqual(projects.map((project) => project.slug).sort(), [SLUG_A, SLUG_B, SLUG_C].sort());
    for (const project of projects) assert.deepEqual(project.sourceIds, ['grok']);
  });

  test('a session keeps its title and its rich transcript records', async () => {
    const projects = await service.getProjectList({ sourceId: 'grok' });
    const projectA = projects.find((project) => project.slug === SLUG_A)!;
    const sessions = await service.getSessionList(projectA, { sourceId: 'grok' });
    const a1 = sessions.find((session) => session.sessionId === SESS_A1);
    assert.ok(a1, 'session A1 is present');
    assert.equal(a1.sourceId, 'grok');
    assert.equal(a1.firstPrompt, 'Codebase Onboarding');

    const { messages } = await service.getSessionMessages(SLUG_A, SESS_A1, 50, 0, { sourceId: 'grok' });
    const blob = messages.map((message) => JSON.stringify(message)).join('\n');
    assert.ok(blob.includes('how is text rendered?'), 'the user turn survives decoding');
    assert.ok(blob.includes("I'll explore the repo."), 'the assistant turn survives decoding');
    assert.ok(blob.includes('The user wants onboarding help.'), 'the reasoning summary survives decoding');

    // The tool result rides on the record Grok wrote it into, as the JSON it
    // was, rather than as a re-encoded approximation of it.
    const toolResults = messages.flatMap((message) => {
      const content = (message as { content?: unknown }).content;
      if (typeof content !== 'string' || !content.trimStart().startsWith('[')) return [];
      const decoded = JSON.parse(content) as Array<{ content?: string; kind?: string }>;
      return decoded.filter((entry) => entry.kind === 'tool_result');
    });
    assert.equal(toolResults.length, 1, 'exactly one tool result reached the product API');
    assert.equal(toolResults[0]!.content, 'a/\nb/\nc.ts');
  });

  test('the timeline resolves Grok tool calls to their results', async () => {
    const timeline = await service.getSessionTimeline(SLUG_A, SESS_A1, { sourceId: 'grok', limit: 50 });
    const tool = timeline.messages.find((message) => message.type === 'tool_use');
    assert.equal(tool?.toolUse?.toolName, 'list_dir');
    assert.equal(tool?.toolUse?.result?.content, 'a/\nb/\nc.ts');
    assert.ok(
      timeline.messages.some((message) => message.type === 'thinking'),
      'Grok reasoning reaches the timeline as thinking',
    );
    // Grok's session-opening system record is a timeline row on this path.
    // The retired bulk writer dropped it; the engine keeps it, and the
    // playground renders it, so this pins the behaviour that ships.
    assert.ok(
      timeline.messages.some((message) => message.type === 'system'),
      'the session-opening system record is a timeline row',
    );
    assert.deepEqual(
      timeline.messages.map((message) => message.type),
      ['system', 'user', 'thinking', 'assistant', 'tool_use', 'assistant'],
      'the timeline is in native order, with the tool call between its two assistant turns',
    );
  });

  test('a long assistant answer is stored whole and its prompt stays searchable', async () => {
    const projects = await service.getProjectList({ sourceId: 'grok' });
    const projectC = projects.find((project) => project.slug === SLUG_C)!;
    const sessions = await service.getSessionList(projectC, { sourceId: 'grok' });
    assert.equal(sessions.length, 1);
    const { messages } = await service.getSessionMessages(SLUG_C, sessions[0]!.sessionId, 20, 0, { sourceId: 'grok' });

    // Full-text search truncates its index; the stored record does not.
    const longest = messages
      .map((message) => JSON.stringify(message))
      .sort((left, right) => right.length - left.length)[0]!;
    assert.ok(longest.length > 2500, 'the 2,500-character assistant answer is retained in full');

    const results = await service.search({ text: 'write a long answer' });
    assert.ok(results.total > 0 || results.results.length > 0, "the long session's prompt is searchable");
  });

  test('stats count the fixture corpus', async () => {
    const stats = await service.getStats();
    const messageCount = stats.segmentsByType.messages ?? stats.searchIndexed;
    assert.ok(messageCount >= 16, `expected at least the fixture's 16 canonical lines, got ${messageCount}`);
  });
});
