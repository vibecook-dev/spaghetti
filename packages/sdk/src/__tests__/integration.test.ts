/**
 * Integration test for @vibecook/spaghetti-sdk
 *
 * Exercises the full pipeline against real ~/.claude data using the
 * built-in node:test runner. No extra dependencies required.
 *
 * Run with: npx tsx packages/sdk/src/__tests__/integration.test.ts
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { existsSync, mkdtempSync, rmSync } from 'node:fs';
import { createSpaghettiService } from '../create.js';
import type { ProjectListItem, SessionListItem, MessagePage } from '../api.js';
import type { LegacySpaghettiAPI as SpaghettiAPI } from '../legacy-api.js';
import type { SearchResultSet, StoreStats, InitProgress } from '../data/segment-types.js';

// ═══════════════════════════════════════════════════════════════════════════
// SHARED STATE
// ═══════════════════════════════════════════════════════════════════════════

let spaghetti: SpaghettiAPI;
let projects: ProjectListItem[];
let firstSlug: string;
let sessions: SessionListItem[];
let firstSessionId: string;
let tempDir: string;

const claudeProjectsDir = path.join(os.homedir(), '.claude', 'projects');
const hasClaudeProjects = existsSync(claudeProjectsDir);
const privateIntegrationEnabled = process.env.SPAGHETTI_RUN_PRIVATE_INTEGRATION === '1';

// ═══════════════════════════════════════════════════════════════════════════
// TEST SUITE
// ═══════════════════════════════════════════════════════════════════════════

describe(
  '@vibecook/spaghetti-sdk private-corpus integration',
  { skip: !hasClaudeProjects || !privateIntegrationEnabled },
  () => {
    // ── 1. Initialize ──────────────────────────────────────────────────────

    test('1. Initialize the service', async () => {
      tempDir = mkdtempSync(path.join(os.tmpdir(), 'spaghetti-core-test-'));
      spaghetti = createSpaghettiService({
        dbPath: path.join(tempDir, 'spaghetti.db'),
      });

      const progressMessages: string[] = [];
      let readyDurationMs = 0;

      const offProgress = spaghetti.onProgress((progress: InitProgress) => {
        progressMessages.push(`[${progress.phase}] ${progress.message}`);
      });

      const offReady = spaghetti.onReady((info: { durationMs: number }) => {
        readyDurationMs = info.durationMs;
      });

      const t0 = performance.now();
      await spaghetti.initialize();
      const elapsed = performance.now() - t0;

      assert.ok(spaghetti.isReady(), 'Service should be ready after initialize()');
      assert.ok(readyDurationMs > 0, 'Ready event should report a positive durationMs');

      console.log(`\n  -- Initialization --`);
      console.log(`  Ready in ${readyDurationMs}ms (wall: ${elapsed.toFixed(0)}ms)`);
      console.log(`  Progress events: ${progressMessages.length}`);
      for (const msg of progressMessages) {
        console.log(`    ${msg}`);
      }

      offProgress();
      offReady();
    });

    // ── 2. getProjectList() ────────────────────────────────────────────────

    test('2. getProjectList()', () => {
      projects = spaghetti.getProjectList();

      assert.ok(Array.isArray(projects), 'Should return an array');
      assert.ok(projects.length > 0, 'Should have at least one project');

      for (const p of projects) {
        assert.ok(typeof p.slug === 'string' && p.slug.length > 0, `slug should be a non-empty string, got: ${p.slug}`);
        assert.ok(typeof p.folderName === 'string', `folderName should be a string`);
        assert.ok(typeof p.absolutePath === 'string', `absolutePath should be a string`);
        assert.ok(typeof p.sessionCount === 'number', `sessionCount should be a number`);
        assert.ok(typeof p.messageCount === 'number', `messageCount should be a number`);
        assert.ok(typeof p.lastActiveAt === 'string', `lastActiveAt should be a string`);
      }

      const totalSessions = projects.reduce((sum, p) => sum + p.sessionCount, 0);
      const totalMessages = projects.reduce((sum, p) => sum + p.messageCount, 0);

      console.log(`\n  -- Projects --`);
      console.log(`  Total projects: ${projects.length}`);
      console.log(`  Total sessions: ${totalSessions}`);
      console.log(`  Total messages: ${totalMessages}`);
      console.log(`  Top 5 by sessions:`);
      const sorted = [...projects].sort((a, b) => b.sessionCount - a.sessionCount);
      for (const p of sorted.slice(0, 5)) {
        console.log(`    ${p.slug} — ${p.sessionCount} sessions, ${p.messageCount} msgs, last: ${p.lastActiveAt}`);
      }

      firstSlug = projects[0].slug;
    });

    // ── 3. getSessionList() ────────────────────────────────────────────────

    test('3. getSessionList()', () => {
      sessions = spaghetti.getSessionList(firstSlug);

      assert.ok(Array.isArray(sessions), 'Should return an array');
      assert.ok(sessions.length > 0, `Should have at least one session for project "${firstSlug}"`);

      for (const s of sessions) {
        assert.ok(typeof s.sessionId === 'string' && s.sessionId.length > 0, `sessionId should be non-empty`);
        assert.ok(typeof s.startTime === 'string', `startTime should be a string`);
        assert.ok(typeof s.lastUpdate === 'string', `lastUpdate should be a string`);
        assert.ok(typeof s.messageCount === 'number', `messageCount should be a number`);
      }

      console.log(`\n  -- Sessions for "${firstSlug}" --`);
      console.log(`  Total sessions: ${sessions.length}`);
      console.log(`  First 3:`);
      for (const s of sessions.slice(0, 3)) {
        console.log(`    ${s.sessionId.substring(0, 12)}… — ${s.messageCount} msgs, ${s.startTime} → ${s.lastUpdate}`);
      }

      firstSessionId = sessions[0].sessionId;
    });

    // ── 4. getSessionMessages() ────────────────────────────────────────────

    test('4. getSessionMessages()', () => {
      const page: MessagePage = spaghetti.getSessionMessages(firstSlug, firstSessionId, 10, 0);

      assert.ok(Array.isArray(page.messages), 'messages should be an array');
      assert.ok(typeof page.total === 'number', 'total should be a number');
      assert.ok(typeof page.offset === 'number', 'offset should be a number');
      assert.ok(typeof page.hasMore === 'boolean', 'hasMore should be a boolean');
      assert.ok(page.messages.length <= 10, 'Should return at most 10 messages');
      assert.strictEqual(page.offset, 0, 'Offset should be 0');

      console.log(`\n  -- Messages for session ${firstSessionId.substring(0, 12)}… --`);
      console.log(
        `  Page: ${page.messages.length} messages (total: ${page.total}, offset: ${page.offset}, hasMore: ${page.hasMore})`,
      );

      if (page.messages.length > 0) {
        const first = page.messages[0] as unknown as Record<string, unknown>;
        console.log(`  First message type: ${first.type ?? 'unknown'}`);
      }
    });

    // ── 5. search() ────────────────────────────────────────────────────────

    test('5. search()', () => {
      const t0 = performance.now();
      const result: SearchResultSet = spaghetti.search({ text: 'function' });
      const elapsed = performance.now() - t0;

      assert.ok(typeof result.total === 'number', 'total should be a number');
      assert.ok(Array.isArray(result.results), 'results should be an array');
      assert.ok(typeof result.hasMore === 'boolean', 'hasMore should be a boolean');

      // 'function' is a very common term — we expect hits
      assert.ok(result.total > 0, `Search for "function" should return results, got total=${result.total}`);

      console.log(`\n  -- Search for "function" --`);
      console.log(`  Total results: ${result.total} (returned: ${result.results.length}, hasMore: ${result.hasMore})`);
      console.log(`  Search time: ${elapsed.toFixed(1)}ms`);
      if (result.results.length > 0) {
        const first = result.results[0];
        console.log(`  Top result: key=${first.key}, rank=${first.rank}`);
        console.log(`  Snippet: ${first.snippet.substring(0, 120)}…`);
      }
    });

    // ── 6. getStats() ─────────────────────────────────────────────────────

    test('6. getStats()', () => {
      const stats: StoreStats = spaghetti.getStats();

      assert.ok(typeof stats.totalSegments === 'number', 'totalSegments should be a number');
      assert.ok(typeof stats.segmentsByType === 'object', 'segmentsByType should be an object');
      assert.ok(typeof stats.totalFingerprints === 'number', 'totalFingerprints should be a number');
      assert.ok(typeof stats.dbSizeBytes === 'number', 'dbSizeBytes should be a number');
      assert.ok(typeof stats.searchIndexed === 'number', 'searchIndexed should be a number');

      console.log(`\n  -- Stats --`);
      console.log(`  Total segments:    ${stats.totalSegments}`);
      console.log(`  Search indexed:    ${stats.searchIndexed}`);
      console.log(`  Source files:      ${stats.totalFingerprints}`);
      console.log(`  DB size:           ${(stats.dbSizeBytes / 1024 / 1024).toFixed(2)} MB`);
      console.log(`  By type:`);
      for (const [type, count] of Object.entries(stats.segmentsByType)) {
        console.log(`    ${type}: ${count}`);
      }
    });

    // ── 7. getProjectMemory() ──────────────────────────────────────────────

    test('7. getProjectMemory()', () => {
      let withMemory = 0;
      let withoutMemory = 0;

      for (const p of projects) {
        const memory = spaghetti.getProjectMemory(p.slug);
        if (memory !== null) {
          withMemory++;
        } else {
          withoutMemory++;
        }
      }

      console.log(`\n  -- Project Memory --`);
      console.log(`  Projects with MEMORY.md:    ${withMemory}`);
      console.log(`  Projects without MEMORY.md: ${withoutMemory}`);

      // We don't assert > 0 since some setups may have no memory files
      assert.ok(typeof withMemory === 'number', 'withMemory should be a number');
    });

    // ── 8. Shutdown ────────────────────────────────────────────────────────

    test('8. shutdown()', () => {
      spaghetti.shutdown();
      rmSync(tempDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
      assert.strictEqual(spaghetti.isReady(), false, 'Service should not be ready after shutdown');

      console.log(`\n  -- Shutdown --`);
      console.log(`  Service shut down successfully.`);
    });
  },
);
