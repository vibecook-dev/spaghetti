import { after, before, describe, test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { createSpaghettiService } from '../legacy-oracle.js';
import { createCodexSource } from '../sources/index.js';
import type { SpaghettiAPI } from '../index.js';

const COLLISION_SLUG = '-tmp-spaghetti-path-with-dash';
const CLAUDE_PATH = '/tmp/spaghetti/path-with-dash';
const CODEX_PATH = '/tmp/spaghetti-path/with-dash';
const CLAUDE_SESSION = '11111111-1111-4111-8111-111111111111';
const CODEX_SESSION = '22222222-2222-4222-8222-222222222222';

function writeClaudeFixture(rootDir: string): void {
  const projectDir = path.join(rootDir, 'projects', COLLISION_SLUG);
  mkdirSync(projectDir, { recursive: true });
  const fullPath = path.join(projectDir, `${CLAUDE_SESSION}.jsonl`);
  const records = [
    {
      type: 'user',
      uuid: 'claude-user',
      parentUuid: null,
      sessionId: CLAUDE_SESSION,
      timestamp: '2026-07-18T10:00:00.000Z',
      message: { role: 'user', content: 'claude collision marker' },
    },
    {
      type: 'assistant',
      uuid: 'claude-assistant',
      parentUuid: 'claude-user',
      sessionId: CLAUDE_SESSION,
      timestamp: '2026-07-18T10:00:01.000Z',
      message: { role: 'assistant', content: [{ type: 'text', text: 'claude reply' }] },
    },
  ];
  writeFileSync(fullPath, records.map((record) => JSON.stringify(record)).join('\n') + '\n');
  writeFileSync(
    path.join(projectDir, 'sessions-index.json'),
    JSON.stringify({
      version: 1,
      originalPath: CLAUDE_PATH,
      entries: [
        {
          sessionId: CLAUDE_SESSION,
          fullPath,
          fileMtime: Date.parse('2026-07-18T10:00:01.000Z'),
          firstPrompt: 'claude collision marker',
          summary: '',
          messageCount: 2,
          created: '2026-07-18T10:00:00.000Z',
          modified: '2026-07-18T10:00:01.000Z',
          gitBranch: 'claude-branch',
          projectPath: CLAUDE_PATH,
          isSidechain: false,
        },
      ],
    }),
  );
  mkdirSync(path.join(projectDir, 'memory'), { recursive: true });
  writeFileSync(path.join(projectDir, 'memory', 'MEMORY.md'), '# Claude-only collision memory\n');

  // A stale Claude directory containing only an orphaned subagent folder
  // must not become a user-visible project.
  mkdirSync(path.join(rootDir, 'projects', '-tmp-empty-ghost', 'orphan-session', 'subagents'), {
    recursive: true,
  });
}

function writeCodexFixture(rootDir: string): void {
  const dayDir = path.join(rootDir, 'sessions', '2026', '07', '18');
  mkdirSync(dayDir, { recursive: true });
  const records = [
    {
      timestamp: '2026-07-18T11:00:00.000Z',
      type: 'session_meta',
      payload: { id: CODEX_SESSION, cwd: CODEX_PATH, cli_version: '0.91.0', originator: 'codex_cli_rs' },
    },
    {
      timestamp: '2026-07-18T11:00:01.000Z',
      type: 'response_item',
      payload: {
        type: 'message',
        role: 'user',
        content: [{ type: 'input_text', text: 'codex collision marker' }],
      },
    },
    {
      timestamp: '2026-07-18T11:00:02.000Z',
      type: 'response_item',
      payload: {
        type: 'message',
        role: 'assistant',
        content: [{ type: 'output_text', text: 'codex reply' }],
      },
    },
  ];
  writeFileSync(
    path.join(dayDir, `rollout-2026-07-18T11-00-00-${CODEX_SESSION}.jsonl`),
    records.map((record) => JSON.stringify(record)).join('\n') + '\n',
  );
}

describe('aggregated project identity survives lossy slug collisions', () => {
  let api: SpaghettiAPI;
  let tempDir: string;

  before(async () => {
    tempDir = mkdtempSync(path.join(os.tmpdir(), 'spaghetti-project-identity-'));
    const claudeRoot = path.join(tempDir, '.claude');
    const codexRoot = path.join(tempDir, '.codex');
    writeClaudeFixture(claudeRoot);
    writeCodexFixture(codexRoot);
    api = createSpaghettiService({
      engine: 'ts',
      rootDir: claudeRoot,
      additionalSources: [createCodexSource({ rootDir: codexRoot })],
      dbPath: path.join(tempDir, 'spaghetti.db'),
    });
    await api.initialize();
  });

  after(async () => {
    await api.dispose();
    rmSync(tempDir, { recursive: true, force: true });
  });

  test('distinct paths get distinct project IDs even when their slugs collide', () => {
    const projects = api.getProjectList();
    assert.equal(projects.length, 2, 'empty Claude ghost is omitted');
    assert.deepEqual(new Set(projects.map((project) => project.slug)), new Set([COLLISION_SLUG]));
    assert.equal(new Set(projects.map((project) => project.projectId)).size, 2);
    assert.deepEqual(new Set(projects.map((project) => project.absolutePath)), new Set([CLAUDE_PATH, CODEX_PATH]));
  });

  test('locator reads never mix sessions from the other colliding project', () => {
    const projects = api.getProjectList();
    const claude = projects.find((project) => project.absolutePath === CLAUDE_PATH);
    const codex = projects.find((project) => project.absolutePath === CODEX_PATH);
    assert.ok(claude);
    assert.ok(codex);

    const claudeSessions = api.getSessionList(claude);
    const codexSessions = api.getSessionList(codex);
    assert.deepEqual(
      claudeSessions.map((session) => [session.sourceId, session.projectSlug, session.sessionId]),
      [['claude-code', COLLISION_SLUG, CLAUDE_SESSION]],
    );
    assert.deepEqual(
      codexSessions.map((session) => [session.sourceId, session.projectSlug, session.sessionId]),
      [['codex', COLLISION_SLUG, CODEX_SESSION]],
    );
    assert.equal(api.getProjectMemory(claude), '# Claude-only collision memory\n');
    assert.equal(api.getProjectMemory(codex), null, 'colliding Codex locator must not leak Claude memory');

    // Legacy slug reads remain a deliberate union for API compatibility.
    assert.equal(api.getSessionList(COLLISION_SLUG).length, 2);
  });

  test('project-scoped search uses exact source/slug membership', () => {
    const projects = api.getProjectList();
    const claude = projects.find((project) => project.absolutePath === CLAUDE_PATH);
    const codex = projects.find((project) => project.absolutePath === CODEX_PATH);
    assert.ok(claude);
    assert.ok(codex);

    assert.equal(api.search({ text: 'collision', projectMembers: claude.members }).total, 1);
    assert.equal(api.search({ text: 'collision', projectMembers: codex.members }).total, 1);
  });
});
