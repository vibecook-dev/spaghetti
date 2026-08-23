/**
 * RFC 011 Phase 1 — real N-API lifecycle contract.
 *
 * Rust unit tests pin the worker internals. This suite pins what SDK callers
 * actually receive: an async opener, a persistent class handle, typed reads,
 * exclusive-owner diagnostics, and deterministic disposal.
 */

import { afterEach, describe, test } from 'node:test';
import assert from 'node:assert/strict';
import { appendFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { loadNativeAddon, openSpaghettiEngine, type SpaghettiEngine } from '../index.js';

const native = loadNativeAddon();
const engines: SpaghettiEngine[] = [];
const tempDirs: string[] = [];
const SESSION_ID = '11111111-2222-3333-4444-555555555555';
const GROK_FIXTURE = fileURLToPath(
  new URL('../../../../crates/spaghetti-napi/fixtures/small-grok/.grok', import.meta.url),
);
const TEAM_AFFILIATION_FIXTURE = fileURLToPath(
  new URL('../../../../agent-support/claude-code/candidate-2026-08-15/fixtures/team-affiliation/', import.meta.url),
);

function markClaudeVersionFixture(root: string, version = '2.1.223'): void {
  writeFileSync(path.join(root, 'settings.json'), '{}');
  mkdirSync(path.join(root, 'sessions'), { recursive: true });
  writeFileSync(path.join(root, 'sessions', '2147483647.json'), JSON.stringify({ version }));
}

function temporaryDatabase(): string {
  const dir = mkdtempSync(path.join(tmpdir(), 'spaghetti-engine-'));
  tempDirs.push(dir);
  return path.join(dir, 'spaghetti.db');
}

async function openTracked(dbPath: string, ownerLabel: string): Promise<SpaghettiEngine> {
  const engine = await openSpaghettiEngine({ dbPath, ownerLabel, queryWorkers: 2 });
  engines.push(engine);
  return engine;
}

afterEach(async () => {
  for (const engine of engines.splice(0)) {
    await engine.dispose();
  }
  for (const dir of tempDirs.splice(0)) {
    rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 });
  }
});

describe('persistent SpaghettiEngine', { skip: !native }, () => {
  test('opens asynchronously, reports health, queries, and disposes', async () => {
    const engine = await openTracked(temporaryDatabase(), 'sdk-lifecycle-test');

    assert.equal(engine.status.state, 'running');
    assert.equal(engine.status.writerAlive, true);
    assert.equal(engine.status.aliveQueryWorkers, 2);
    assert.equal(engine.status.observation.state, 'idle');
    assert.equal(engine.status.observation.reconcileInFlight, false);
    assert.equal(engine.status.observation.recoveryRequired, false);
    assert.equal(engine.status.observation.reconcilesTotal, 0);
    assert.equal(engine.status.owner?.ownerLabel, 'sdk-lifecycle-test');

    const health = await engine.health();
    assert.equal(health.healthy, true, health.detail);

    const overview = await engine.overview();
    assert.equal(overview.schemaVersion > 0, true);
    assert.equal(overview.commitSeq, 0);
    assert.deepEqual([overview.projects, overview.sessions, overview.messages], [0, 0, 0]);
    assert.deepEqual([overview.canonicalSessions, overview.canonicalMessages], [0, 0]);
    assert.equal(overview.journalMode, 'wal');
    assert.equal(overview.queryOnly, true);
    assert.equal(overview.readOnly, true);

    const firstCancellationEpoch = engine.cancelPendingQueries();
    assert.equal(engine.cancelPendingQueries(), firstCancellationEpoch + 1);
    assert.equal((await engine.overview()).schemaVersion, overview.schemaVersion);

    const stopped = await engine.dispose();
    assert.equal(stopped.state, 'stopped');
    assert.equal(stopped.writerAlive, false);
    assert.equal(stopped.aliveQueryWorkers, 0);
    assert.equal(stopped.observation.state, 'stopped');
    assert.equal((await engine.health()).healthy, false);
    await assert.rejects(engine.overview(), /shutting down|stopped/i);
  });

  test('rejects a second owner with the current owner metadata', async () => {
    const dbPath = temporaryDatabase();
    await openTracked(dbPath, 'first-owner');

    await assert.rejects(openSpaghettiEngine({ dbPath, ownerLabel: 'second-owner' }), (error: Error) => {
      assert.match(error.message, /already owned/i);
      assert.match(error.message, /first-owner/);
      return true;
    });
  });

  test('reopens after deterministic disposal without leaked locks', async () => {
    const dbPath = temporaryDatabase();
    const first = await openTracked(dbPath, 'first');
    await first.dispose();

    const second = await openTracked(dbPath, 'second');
    assert.equal(second.status.owner?.ownerLabel, 'second');
    assert.equal((await second.health()).healthy, true);
  });

  test('reconciles declared Claude objects through the persistent handle', async () => {
    const dbPath = temporaryDatabase();
    const root = path.join(path.dirname(dbPath), 'claude-source');
    mkdirSync(root, { recursive: true });
    writeFileSync(path.join(root, 'settings.json'), '{"model":"claude-sonnet"}');
    const engine = await openTracked(dbPath, 'sdk-reconcile-test');

    const first = await engine.reconcileClaude({ roots: [root], reason: 'sdk_fixture' });
    assert.equal(first.instancesDiscovered, 1);
    assert.equal(first.streamsReconciled > 0, true);
    assert.equal(first.objectsDiscovered, 1);
    assert.equal(first.objectsRegistered, 1);
    assert.equal(first.recordsDecoded, 1);
    assert.equal(first.commits, 2, 'one source commit plus one usage-v2 readiness barrier');
    assert.equal((first.lastCommitSeq ?? 0) > 0, true);
    assert.equal(engine.status.observation.state, 'live');
    assert.equal(engine.status.observation.reconcilesTotal, 1);
    assert.equal(engine.status.observation.lastCommitSeq, first.lastCommitSeq);
    assert.equal(engine.status.observation.lastError, undefined);

    const overview = await engine.overview();
    assert.equal(overview.canonicalSessions, 0);
    assert.equal(overview.canonicalMessages, 0);

    const unchanged = await engine.reconcileClaude({ roots: [root] });
    assert.equal(unchanged.objectsRegistered, 0);
    assert.equal(unchanged.recordsDecoded, 0);
    assert.equal(unchanged.objectsUnchanged, 1);
    assert.equal(unchanged.commits, 0);
    assert.equal(engine.status.observation.state, 'live');
    assert.equal(engine.status.observation.reconcilesTotal, 2);
    assert.equal(engine.status.observation.dirtyInstances, 0);
    assert.equal(engine.status.observation.fullReconcileRequired, false);
    assert.equal(engine.status.observation.recoveryRequired, false);
  });

  test('reports common dependency-access accounting through N-API', async () => {
    const engine = await openTracked(temporaryDatabase(), 'sdk-access-accounting-test');
    const result = await engine.reconcileAdapter({
      adapterId: 'grok',
      roots: [GROK_FIXTURE],
      reason: 'sdk_access_fixture',
    });

    assert.ok(result.dependencyAccessAttempts > 0);
    assert.ok(result.dependencyObjectsAccessed > 0);
    assert.ok(result.dependencyBytesRead > 0);
    assert.equal(result.dependencyAccessDenials, 0);
    assert.equal(result.dependencyAccessAbandoned, 0);
    assert.equal(result.dependencyMaxDepth, 1);
    assert.equal(result.dependencyTraceEntriesDropped, 0);
  });

  test('reports canonical observation history separately from compatibility tables', async () => {
    const dbPath = temporaryDatabase();
    const root = path.join(path.dirname(dbPath), 'claude-history');
    const project = path.join(root, 'projects', '-tmp-shadow-project');
    mkdirSync(project, { recursive: true });
    writeFileSync(
      path.join(project, `${SESSION_ID}.jsonl`),
      `${JSON.stringify({
        type: 'user',
        uuid: 'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee',
        parentUuid: null,
        timestamp: '2026-08-12T00:00:00.000Z',
        sessionId: SESSION_ID,
        cwd: '/tmp/shadow-project',
        gitBranch: 'main',
        message: { role: 'user', content: 'observe me' },
      })}\n`,
    );
    const engine = await openTracked(dbPath, 'sdk-overview-test');

    await engine.reconcileClaude({ roots: [root], reason: 'sdk_overview_fixture' });
    const overview = await engine.overview();

    assert.deepEqual([overview.projects, overview.sessions, overview.messages], [0, 0, 0]);
    assert.deepEqual([overview.canonicalSessions, overview.canonicalMessages], [1, 1]);

    const replay = await engine.replayChanges({ limit: 1 });
    assert.equal(replay.contractVersion, 1);
    assert.equal(replay.atCommitSeq, overview.commitSeq);
    assert.equal(replay.changes.length, 1);
    assert.equal(replay.hasMore, true);
    assert.deepEqual(replay.nextCursor, replay.changes[0]?.cursor);
    assert.match(replay.changes[0]?.entityKeyBase64Url ?? '', /^[A-Za-z0-9_-]*$/);
    assert.match(
      replay.changes[0]?.payloadBase64 ?? '',
      /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/,
    );
    assert.equal(replay.payloadBytes > 0, true);
    assert.equal(replay.payloadBytes <= replay.payloadByteLimit, true);

    const afterSnapshot = await engine.replayChanges({
      after: { commitSeq: replay.atCommitSeq, ordinal: 0xffff_ffff },
    });
    assert.deepEqual(afterSnapshot.changes, []);
    assert.equal(afterSnapshot.hasMore, false);
  });

  test('reports response-level usage with per-bucket quality through the SDK', async () => {
    const dbPath = temporaryDatabase();
    const root = path.join(path.dirname(dbPath), 'claude-usage');
    const project = path.join(root, 'projects', '-tmp-usage-project');
    mkdirSync(project, { recursive: true });
    const assistant = (uuid: string, id: string, usage: Record<string, number>) =>
      `${JSON.stringify({
        type: 'assistant',
        uuid,
        parentUuid: null,
        timestamp: '2026-08-12T00:00:00.000Z',
        sessionId: SESSION_ID,
        cwd: '/tmp/usage-project',
        version: '1',
        gitBranch: 'main',
        isSidechain: false,
        userType: 'external',
        requestId: 'request-1',
        message: {
          model: 'claude-sonnet',
          id,
          type: 'message',
          role: 'assistant',
          content: [{ type: 'text', text: 'usage response' }],
          usage,
        },
      })}\n`;
    writeFileSync(
      path.join(project, `${SESSION_ID}.jsonl`),
      // Two rows for one response id: an evolving counter, not two responses.
      assistant('aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee', 'response-1', {
        input_tokens: 12,
        output_tokens: 3,
      }) +
        assistant('aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeef', 'response-1', {
          input_tokens: 12,
          output_tokens: 9,
        }),
    );
    markClaudeVersionFixture(root);
    const engine = await openTracked(dbPath, 'sdk-usage-test');
    await engine.reconcileClaude({ roots: [root], reason: 'sdk_usage_fixture' });

    const projects = await engine.listHistoryProjects({ limit: 10 });
    const projectId = projects.items[0]?.projectId;
    assert.ok(projectId);
    const usage = await engine.getUsage({ projectId });

    assert.equal(usage.aggregate.contributionCount, 1);
    assert.equal(usage.aggregate.exact.inputTokens, 12);
    assert.equal(usage.aggregate.exact.outputTokens, 9);
    assert.equal(usage.aggregate.sessionCount, 1);
    // The cache buckets were never asserted, so they stay unknown rather than
    // being summed as zero.
    assert.equal(usage.aggregate.combined.cacheCreationTokens, 0);
    const unknownCache = usage.coverage.filter(
      (entry) => entry.valueQuality === 'unknown' && entry.bucket.startsWith('cache'),
    );
    assert.equal(unknownCache.length, 2);
    for (const entry of unknownCache) {
      assert.equal(entry.unknownReason, 'missing');
      assert.notEqual(entry.completeness, 'complete');
    }
    const input = usage.coverage.find((entry) => entry.bucket === 'input');
    assert.equal(input?.nativeField, 'message.usage.input_tokens');
    assert.equal(input?.authority, 'native_response');
    assert.equal(usage.window, undefined);

    const windowed = await engine.getUsage({ projectId, from: '2026-08-12', to: '2026-08-12' });
    assert.equal(windowed.window?.days.length, 1);
    assert.equal(windowed.window?.days[0]?.aggregate.exact.outputTokens, 9);
    assert.equal(windowed.window?.untimed.aggregate.contributionCount, 0);
  });

  test('keeps legacy history but withholds typed coverage for an exact candidate Claude version', async () => {
    const dbPath = temporaryDatabase();
    const root = path.join(path.dirname(dbPath), 'claude-unsupported-version');
    const project = path.join(root, 'projects', '-tmp-unsupported-project');
    mkdirSync(project, { recursive: true });
    writeFileSync(
      path.join(project, `${SESSION_ID}.jsonl`),
      `${JSON.stringify({
        type: 'assistant',
        uuid: 'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee',
        parentUuid: null,
        timestamp: '2026-08-12T00:00:00.000Z',
        sessionId: SESSION_ID,
        cwd: '/tmp/unsupported-project',
        version: '1',
        gitBranch: 'main',
        isSidechain: false,
        userType: 'external',
        requestId: 'unsupported-request',
        message: {
          model: 'claude-sonnet',
          id: 'unsupported-response',
          type: 'message',
          role: 'assistant',
          content: [{ type: 'text', text: 'legacy history remains readable' }],
          usage: { input_tokens: 2, output_tokens: 1 },
        },
      })}\n`,
    );
    markClaudeVersionFixture(root);

    const engine = await openTracked(dbPath, 'sdk-unsupported-version-test');
    await engine.reconcileClaude({ roots: [root], reason: 'sdk_unsupported_version_fixture' });

    const projectId = (await engine.listHistoryProjects({ limit: 10 })).items[0]?.projectId;
    assert.ok(projectId);
    const sessionId = (await engine.listHistorySessions({ projectId, limit: 10 })).items[0]?.sessionId;
    assert.ok(sessionId);
    // History and usage still decode; it is the typed durable coverage that an
    // unauthorized support release cannot mint.
    const usage = await engine.getUsage({ projectId, sessionId });
    assert.equal(usage.aggregate.contributionCount, 1);

    const coverage = await engine.getFactFamilyCoverage({
      projectId,
      sessionId,
      ownerId: 'runtime.usage-v2',
      family: 'runtime.usage-v2',
      familyVersion: 1,
      limit: 10,
    });
    assert.equal(coverage.status, 'not_materialized');
    assert.equal(coverage.coverage, undefined);
    assert.deepEqual(coverage.items, []);
  });

  test('correlates native team leads and child metadata without copying actor usage', async () => {
    const dbPath = temporaryDatabase();
    const teamSessionId = '01234567-89ab-cdef-0123-456789abcdef';
    const teamConfig = JSON.parse(readFileSync(path.join(TEAM_AFFILIATION_FIXTURE, 'team-config.json'), 'utf8'))
      .data as {
      name: string;
      leadAgentId: string;
      leadSessionId: string;
      members: Array<{ agentId: string; name: string }>;
    };
    const childMetadata = JSON.parse(readFileSync(path.join(TEAM_AFFILIATION_FIXTURE, 'subagent-meta.json'), 'utf8'))
      .data as { agentType: string; name: string; teamName: string };
    teamConfig.leadSessionId = teamSessionId;
    const root = path.join(path.dirname(dbPath), 'claude-team-affiliation');
    const project = path.join(root, 'projects', '-fixture-team-project');
    const childDir = path.join(project, teamSessionId, 'subagents');
    const teamDir = path.join(root, 'teams', teamConfig.name);
    mkdirSync(childDir, { recursive: true });
    mkdirSync(teamDir, { recursive: true });
    writeFileSync(
      path.join(project, `${teamSessionId}.jsonl`),
      `${JSON.stringify({
        type: 'assistant',
        uuid: 'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee',
        parentUuid: null,
        timestamp: '2026-08-12T00:00:00.000Z',
        sessionId: teamSessionId,
        cwd: '/fixture/project',
        version: '1',
        gitBranch: 'main',
        isSidechain: false,
        userType: 'external',
        requestId: 'root-request',
        message: {
          model: 'fixture-model',
          id: 'root-response',
          type: 'message',
          role: 'assistant',
          content: [{ type: 'text', text: 'root response' }],
          usage: {
            input_tokens: 10,
            output_tokens: 1,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
          },
        },
      })}\n`,
    );
    writeFileSync(
      path.join(childDir, 'agent-child.jsonl'),
      `${JSON.stringify({
        type: 'assistant',
        uuid: 'bbbbbbbb-bbbb-cccc-dddd-eeeeeeeeeeee',
        parentUuid: null,
        timestamp: '2026-08-12T00:00:01.000Z',
        sessionId: teamSessionId,
        cwd: '/fixture/project',
        version: '1',
        gitBranch: 'main',
        isSidechain: true,
        userType: 'external',
        requestId: 'child-request',
        message: {
          model: 'fixture-model',
          id: 'child-response',
          type: 'message',
          role: 'assistant',
          content: [{ type: 'text', text: 'child response' }],
          usage: {
            input_tokens: 20,
            output_tokens: 2,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
          },
        },
      })}\n`,
    );
    writeFileSync(path.join(childDir, 'agent-child.meta.json'), JSON.stringify(childMetadata));
    const configPath = path.join(teamDir, 'config.json');
    writeFileSync(configPath, JSON.stringify(teamConfig));
    markClaudeVersionFixture(root);

    const engine = await openTracked(dbPath, 'sdk-team-affiliation-test');
    await engine.reconcileClaude({ roots: [root], reason: 'sdk_team_affiliation_fixture' });
    const projects = await engine.listHistoryProjects({ limit: 10 });
    const projectId = projects.items[0]?.projectId;
    assert.ok(projectId);
    const sessions = await engine.listHistorySessions({ projectId, limit: 10 });
    const sessionId = sessions.items[0]?.sessionId;
    assert.ok(sessionId);

    // Affiliation is a grouping over the same responses. Editing it must never
    // change the session's canonical contribution total (RFC 012C 7.5).
    const usage = await engine.getUsage({ projectId, sessionId });
    assert.equal(usage.aggregate.contributionCount, 2);
    assert.equal(usage.aggregate.combined.inputTokens, 30);

    writeFileSync(
      path.join(childDir, 'agent-child.meta.json'),
      JSON.stringify({ agentType: childMetadata.agentType, name: childMetadata.name }),
    );
    await engine.reconcileClaude({ roots: [root], reason: 'sdk_team_child_removed' });
    const afterChildRemoved = await engine.getUsage({ projectId, sessionId });
    assert.equal(afterChildRemoved.aggregate.contributionCount, 2);
    assert.equal(afterChildRemoved.aggregate.combined.inputTokens, 30);

    rmSync(configPath);
    await engine.reconcileClaude({ roots: [root], reason: 'sdk_team_root_removed' });
    const afterTeamRemoved = await engine.getUsage({ projectId, sessionId });
    assert.equal(afterTeamRemoved.aggregate.contributionCount, 2);
    assert.equal(afterTeamRemoved.aggregate.combined.inputTokens, 30);
  });

  test('starts, refreshes, and stops native Claude observation', async () => {
    const dbPath = temporaryDatabase();
    const root = path.join(path.dirname(dbPath), 'claude-observed');
    mkdirSync(root, { recursive: true });
    writeFileSync(path.join(root, 'settings.json'), '{"model":"claude-sonnet"}');
    const engine = await openTracked(dbPath, 'sdk-observation-test');

    const started = await engine.startClaudeObservation({ roots: [root] });
    assert.equal(started.observation.state, 'live');
    assert.equal(started.observation.supervisorsRunning, 1);
    assert.equal(started.observation.watchedInstances, 1);
    assert.equal(started.observation.watchRoots, 1);

    const beforeRefresh = started.observation.reconcilesTotal;
    const refreshed = await engine.refreshClaudeObservation();
    assert.equal(refreshed.observation.reconcilesTotal > beforeRefresh, true);
    assert.equal(refreshed.observation.state, 'live');

    const stopped = await engine.stopClaudeObservation();
    assert.equal(stopped.observation.supervisorsRunning, 0);
    await assert.rejects(engine.refreshClaudeObservation(), /not running/i);
  });
});
