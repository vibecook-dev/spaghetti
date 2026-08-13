/**
 * workflow-ingestion.test.ts — the Workflow feature (2026-07 re-audit).
 *
 * Boots the real service (TS engine) against a temp rootDir containing
 * a workflow run + a top-level subagent + a nested workflow subagent, and
 * asserts the nested transcript is ingested (it was invisible before),
 * grouped under its run, and that the API surfaces workflows and their
 * subagents separately from top-level subagents.
 */

import { test, describe, beforeEach, after } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import * as path from 'node:path';
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from 'node:fs';

import { createSpaghettiService } from '../../create.js';
import type { LegacySpaghettiAPI as SpaghettiAPI } from '../../legacy-api.js';

const SLUG = '-Users-demo-wf';
const SESSION_ID = 'bbbb0000-1111-2222-3333-444455556666';

function userLine(uuid: string, text: string): string {
  return JSON.stringify({
    type: 'user',
    uuid,
    parentUuid: null,
    sessionId: SESSION_ID,
    timestamp: '2026-01-01T00:00:00.000Z',
    message: { role: 'user', content: text },
  });
}

describe('Workflow ingestion', () => {
  let tempDir: string;
  let rootDir: string;
  let dbPath: string;

  beforeEach((t) => {
    const safe = t.name.replace(/[^a-zA-Z0-9]/g, '_');
    tempDir = mkdtempSync(path.join(os.tmpdir(), `spaghetti-wf-${safe}-`));
    rootDir = path.join(tempDir, '.claude');
    dbPath = path.join(tempDir, 'test.db');
    const projectDir = path.join(rootDir, 'projects', SLUG);
    const sessionDir = path.join(projectDir, SESSION_ID);
    mkdirSync(path.join(sessionDir, 'workflows'), { recursive: true });
    mkdirSync(path.join(sessionDir, 'subagents', 'workflows', 'wf_run01'), { recursive: true });

    // Main session file + index.
    writeFileSync(path.join(projectDir, `${SESSION_ID}.jsonl`), userLine('u0', 'main prompt') + '\n');
    writeFileSync(
      path.join(projectDir, 'sessions-index.json'),
      JSON.stringify({ version: 1, entries: [{ sessionId: SESSION_ID, fullPath: '' }] }),
    );

    // A top-level subagent, a workflow run record, a nested workflow subagent + journal.
    writeFileSync(path.join(sessionDir, 'subagents', 'agent-atop.jsonl'), userLine('t0', 'top-level agent') + '\n');
    writeFileSync(
      path.join(sessionDir, 'workflows', 'wf_run01.json'),
      JSON.stringify({
        runId: 'wf_run01',
        workflowName: 'demo-run',
        status: 'completed',
        agentCount: 3,
        totalTokens: 4242,
        totalToolCalls: 11,
        durationMs: 5000,
      }),
    );
    writeFileSync(
      path.join(sessionDir, 'subagents', 'workflows', 'wf_run01', 'agent-anested.jsonl'),
      userLine('n0', 'nested workflow agent') + '\n',
    );
    writeFileSync(
      path.join(sessionDir, 'subagents', 'workflows', 'wf_run01', 'journal.jsonl'),
      JSON.stringify({ type: 'started', agentId: 'anested' }) + '\n',
    );
  });

  after(() => {
    rmSync(tempDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  });

  async function boot(): Promise<SpaghettiAPI> {
    const svc = createSpaghettiService({ engine: 'ts', rootDir, dbPath });
    await svc.initialize();
    return svc;
  }

  test('nested workflow transcript is ingested and grouped under its run', async () => {
    const svc = await boot();

    // getSessionSubagents returns ONLY the top-level one.
    const top = svc.getSessionSubagents(SLUG, SESSION_ID);
    assert.deepEqual(
      top.map((s) => s.agentId),
      ['atop'],
    );

    // The workflow run is surfaced with its analytics.
    const workflows = svc.getSessionWorkflows(SLUG, SESSION_ID);
    assert.equal(workflows.length, 1);
    assert.equal(workflows[0].workflowId, 'wf_run01');
    assert.equal(workflows[0].name, 'demo-run');
    assert.equal(workflows[0].status, 'completed');
    assert.equal(workflows[0].agentCount, 3);
    assert.equal(workflows[0].totalTokens, 4242);
    assert.equal(workflows[0].subagentCount, 1);

    // The nested transcript is grouped under the run (invisible pre-fix).
    const nested = svc.getWorkflowSubagents(SLUG, SESSION_ID, 'wf_run01');
    assert.deepEqual(
      nested.map((s) => s.agentId),
      ['anested'],
    );

    // And its messages are actually queryable.
    const msgs = svc.getSubagentMessages(SLUG, SESSION_ID, 'anested', 100, 0);
    assert.equal(msgs.total, 1);

    await svc.dispose();
  });

  test('a session with no workflows returns empty', async () => {
    // Remove the workflow dir + nested subagents for this case.
    rmSync(path.join(rootDir, 'projects', SLUG, SESSION_ID, 'workflows'), { recursive: true, force: true });
    rmSync(path.join(rootDir, 'projects', SLUG, SESSION_ID, 'subagents', 'workflows'), {
      recursive: true,
      force: true,
    });

    const svc = await boot();
    assert.deepEqual(svc.getSessionWorkflows(SLUG, SESSION_ID), []);
    // Top-level subagent still present.
    assert.equal(svc.getSessionSubagents(SLUG, SESSION_ID).length, 1);
    await svc.dispose();
  });
});
