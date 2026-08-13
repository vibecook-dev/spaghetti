/**
 * multi-source.test.ts — createSpaghettiService ingests two sources into one store.
 *
 * Wires the real Claude `small` fixture plus a synthetic Codex rollout tree via
 * `additionalSources`, initializes once, and asserts the unified index spans
 * both agents: `getSourceIds()` reports both, `getProjectList()` includes each
 * unique projects, and source filtering works. Also checks a warm re-init on
 * the same DB doesn't duplicate.
 *
 * This is the RFC 006 multi-source lifecycle end to end: one shared store, one
 * `LifecycleOwner` per source, reads served by the agent-agnostic coordinator.
 *
 * Run with `pnpm --filter @vibecook/spaghetti-sdk test`.
 */

import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';

import { createSpaghettiService } from '../legacy-oracle.js';
import { createCodexSource } from '../sources/index.js';
import type { LegacySpaghettiAPI as SpaghettiAPI } from '../legacy-api.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE_ROOT_DIR = path.resolve(here, '../../../../crates/spaghetti-napi/fixtures/small/.claude');
const CODEX_SESSION = '019cf46d-0924-7523-b3f5-f6f5cc0fcd16';
const CODEX_CWD = '/Users/test/project1';
const CODEX_SLUG = '-Users-test-project1';

function writeCodexFixture(codexRoot: string): void {
  const dayDir = path.join(codexRoot, 'sessions', '2026', '07', '13');
  mkdirSync(dayDir, { recursive: true });
  const lines = [
    {
      timestamp: '2026-07-13T00:00:00.000Z',
      type: 'session_meta',
      payload: { id: CODEX_SESSION, cwd: CODEX_CWD, cli_version: '0.91.0', originator: 'codex_cli_rs' },
    },
    {
      timestamp: '2026-07-13T00:00:01.000Z',
      type: 'response_item',
      payload: { type: 'message', role: 'user', content: [{ type: 'input_text', text: 'codex hello' }] },
    },
    {
      timestamp: '2026-07-13T00:00:02.000Z',
      type: 'response_item',
      payload: { type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'codex reply' }] },
    },
  ];
  writeFileSync(
    path.join(dayDir, `rollout-2026-07-13T00-00-00-${CODEX_SESSION}.jsonl`),
    lines.map((l) => JSON.stringify(l)).join('\n') + '\n',
  );
}

describe('multi-source ingest (claude + codex)', () => {
  let spaghetti: SpaghettiAPI;
  let tempDir: string;
  let dbPath: string;
  let codexRoot: string;

  before(async () => {
    tempDir = mkdtempSync(path.join(os.tmpdir(), 'spaghetti-multi-'));
    dbPath = path.join(tempDir, 'spaghetti.db');
    codexRoot = path.join(tempDir, '.codex');
    writeCodexFixture(codexRoot);

    spaghetti = createSpaghettiService({
      rootDir: FIXTURE_ROOT_DIR,
      additionalSources: [createCodexSource({ rootDir: codexRoot })],
      dbPath,
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

  test('getSourceIds() reports both sources', () => {
    assert.deepEqual(spaghetti.getSourceIds(), ['claude-code', 'codex']);
  });

  test('getProjectList aggregates agents that share a workspace', () => {
    const projects = spaghetti.getProjectList();
    assert.equal(projects.length, 3, 'the shared cwd appears once, not once per agent');
    const sharedProject = projects.find((p) => p.slug === CODEX_SLUG);
    assert.ok(sharedProject, 'shared project present');
    assert.deepEqual(sharedProject.sourceIds, ['claude-code', 'codex']);
    assert.equal(sharedProject.sessionCount, 6, 'five Claude sessions plus one Codex session');
    assert.deepEqual(sharedProject.members, [
      { sourceId: 'claude-code', slug: CODEX_SLUG },
      { sourceId: 'codex', slug: CODEX_SLUG },
    ]);
    const sharedSessions = spaghetti.getSessionList(sharedProject);
    assert.equal(sharedSessions.length, 6);
    assert.deepEqual(new Set(sharedSessions.map((session) => session.sourceId)), new Set(['claude-code', 'codex']));
  });

  test('getProjectTokenActivity aggregates project members and honors the source filter', () => {
    const sharedProject = spaghetti.getProjectList().find((project) => project.slug === CODEX_SLUG);
    assert.ok(sharedProject);
    const all = spaghetti.getProjectTokenActivity(sharedProject, { from: '2020-01-01', to: '2030-01-01' });
    assert.ok(all.days.length > 0);
    assert.deepEqual(new Set(all.days.flatMap((day) => day.sourceIds)), new Set(['claude-code', 'codex']));
    const codex = spaghetti.getProjectTokenActivity(sharedProject, {
      sourceId: 'codex',
      from: '2020-01-01',
      to: '2030-01-01',
    });
    assert.ok(codex.days.length > 0);
    assert.ok(codex.days.every((day) => day.sourceIds.length === 1 && day.sourceIds[0] === 'codex'));
  });

  test('the codex session and its messages are queryable', () => {
    // Scoped list — no client-side sourceId filter required once the API
    // threads the agent dimension through.
    const codexSessions = spaghetti.getSessionList(CODEX_SLUG, { sourceId: 'codex' });
    assert.equal(codexSessions.length, 1);
    assert.equal(codexSessions[0].sessionId, CODEX_SESSION);
    assert.equal(codexSessions[0].sourceId, 'codex');

    const { messages } = spaghetti.getSessionMessages(CODEX_SLUG, CODEX_SESSION, 50, 0, {
      sourceId: 'codex',
    });
    const texts = messages.map((m) => JSON.stringify(m));
    assert.ok(
      texts.some((t) => t.includes('codex hello')),
      'user turn present',
    );
    assert.ok(
      texts.some((t) => t.includes('codex reply')),
      'assistant turn present',
    );
  });

  test('getSessionList scopes by sourceId when a slug is shared', () => {
    const scoped = spaghetti.getSessionList(CODEX_SLUG, { sourceId: 'codex' });
    assert.equal(scoped.length, 1);
    assert.ok(scoped.every((s) => s.sourceId === 'codex'));
    const claude = spaghetti.getSessionList(CODEX_SLUG, { sourceId: 'claude-code' });
    assert.equal(claude.length, 5);
    assert.ok(claude.every((s) => s.sourceId === 'claude-code'));
  });

  test('getProjectMemory is null for non-claude sources', () => {
    assert.equal(spaghetti.getProjectMemory(CODEX_SLUG, { sourceId: 'codex' }), null);
  });

  test('getProjectList filters to the codex source only', () => {
    const codexOnly = spaghetti.getProjectList({ sourceId: 'codex' });
    assert.equal(codexOnly.length, 1);
    assert.equal(codexOnly[0].slug, CODEX_SLUG);
    assert.deepEqual(codexOnly[0].sourceIds, ['codex']);
  });

  test('rebuildIndex() preserves BOTH sources (file-delete does not orphan codex)', async () => {
    await spaghetti.rebuildIndex();
    // The whole-DB rebuild fans across owners; every source must come back.
    assert.deepEqual(spaghetti.getSourceIds(), ['claude-code', 'codex']);
    const codexOnly = spaghetti.getProjectList({ sourceId: 'codex' });
    assert.equal(codexOnly.length, 1);
    assert.equal(codexOnly[0].slug, CODEX_SLUG);
    // Claude side still present too.
    assert.ok(spaghetti.getProjectList({ sourceId: 'claude-code' }).length > 0, 'claude projects survive the rebuild');
  });

  test('warm re-init on the same DB does not duplicate', async () => {
    const again = createSpaghettiService({
      rootDir: FIXTURE_ROOT_DIR,
      additionalSources: [createCodexSource({ rootDir: codexRoot })],
      dbPath,
    });
    await again.initialize();
    try {
      assert.equal(again.getProjectList({ sourceId: 'codex' }).length, 1);
    } finally {
      again.shutdown();
    }
  });
});
