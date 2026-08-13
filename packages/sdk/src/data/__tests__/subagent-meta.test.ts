/**
 * subagent-meta.test.ts — the agent-{id}.meta.json sidecar reader.
 *
 * The stored/queried agent_type should come from the sidecar's real
 * (free-form) agentType when present, falling back to the filename-
 * inferred kind (task/prompt_suggestion/compact) otherwise.
 */

import { test, describe, beforeEach, after } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import * as path from 'node:path';
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from 'node:fs';

import { createSpaghettiService } from '../../create.js';
import type { LegacySpaghettiAPI as SpaghettiAPI } from '../../legacy-api.js';

const SLUG = '-Users-demo-meta';
const SESSION_ID = 'cccc0000-1111-2222-3333-444455556666';

function userLine(uuid: string): string {
  return JSON.stringify({
    type: 'user',
    uuid,
    parentUuid: null,
    sessionId: SESSION_ID,
    timestamp: '2026-01-01T00:00:00.000Z',
    message: { role: 'user', content: 'hi' },
  });
}

describe('Subagent .meta.json sidecar', () => {
  let tempDir: string;
  let rootDir: string;
  let dbPath: string;
  let subagentsDir: string;

  beforeEach((t) => {
    tempDir = mkdtempSync(path.join(os.tmpdir(), `spaghetti-meta-${t.name.replace(/[^a-z0-9]/gi, '_')}-`));
    rootDir = path.join(tempDir, '.claude');
    dbPath = path.join(tempDir, 'test.db');
    const projectDir = path.join(rootDir, 'projects', SLUG);
    subagentsDir = path.join(projectDir, SESSION_ID, 'subagents');
    mkdirSync(subagentsDir, { recursive: true });
    writeFileSync(path.join(projectDir, `${SESSION_ID}.jsonl`), userLine('u0') + '\n');
    writeFileSync(
      path.join(projectDir, 'sessions-index.json'),
      JSON.stringify({ version: 1, entries: [{ sessionId: SESSION_ID, fullPath: '' }] }),
    );
    // Two subagents: one with a meta sidecar, one without.
    writeFileSync(path.join(subagentsDir, 'agent-awithmeta.jsonl'), userLine('a0') + '\n');
    writeFileSync(
      path.join(subagentsDir, 'agent-awithmeta.meta.json'),
      JSON.stringify({ agentType: 'general-purpose', description: 'does things', name: 'gp' }),
    );
    writeFileSync(path.join(subagentsDir, 'agent-anometa.jsonl'), userLine('b0') + '\n');
  });

  after(() => rmSync(tempDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 }));

  async function boot(): Promise<SpaghettiAPI> {
    const svc = createSpaghettiService({ engine: 'ts', rootDir, dbPath });
    await svc.initialize();
    return svc;
  }

  test('agent_type comes from meta when present, filename inference otherwise', async () => {
    const svc = await boot();
    const byId = Object.fromEntries(svc.getSessionSubagents(SLUG, SESSION_ID).map((s) => [s.agentId, s.agentType]));
    // meta.agentType wins over the filename-inferred 'task'.
    assert.equal(byId['awithmeta'], 'general-purpose');
    // no sidecar → filename inference.
    assert.equal(byId['anometa'], 'task');
    await svc.dispose();
  });
});
