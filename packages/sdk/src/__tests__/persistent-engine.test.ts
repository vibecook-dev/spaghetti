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

  test('returns usage-v2 rows and writer-owned readiness in one native snapshot', async () => {
    const dbPath = temporaryDatabase();
    const root = path.join(path.dirname(dbPath), 'claude-usage-v2');
    const project = path.join(root, 'projects', '-tmp-usage-v2-project');
    mkdirSync(project, { recursive: true });
    writeFileSync(
      path.join(project, `${SESSION_ID}.jsonl`),
      `${JSON.stringify({
        type: 'assistant',
        uuid: 'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee',
        parentUuid: null,
        timestamp: '2026-08-12T00:00:00.000Z',
        sessionId: SESSION_ID,
        cwd: '/tmp/usage-v2-project',
        version: '1',
        gitBranch: 'main',
        isSidechain: false,
        userType: 'external',
        requestId: 'request-1',
        message: {
          model: 'claude-sonnet',
          id: 'response-1',
          type: 'message',
          role: 'assistant',
          content: [{ type: 'text', text: 'usage response' }],
          usage: { input_tokens: 12, output_tokens: 3 },
        },
      })}\n`,
    );
    const engine = await openTracked(dbPath, 'sdk-usage-v2-test');
    await engine.reconcileClaude({ roots: [root], reason: 'sdk_usage_v2_fixture' });

    const projects = await engine.listHistoryProjects({ limit: 10 });
    const projectId = projects.items[0]?.projectId;
    assert.ok(projectId);
    const sessions = await engine.listHistorySessions({ projectId, limit: 10 });
    const sessionId = sessions.items[0]?.sessionId;
    assert.ok(sessionId);
    const usage = await engine.getRuntimeUsageV2({ projectId, sessionId, limit: 10 });

    assert.equal(usage.projectionStatus, 'shadow');
    assert.equal(usage.projectionReadiness.projectionId, 'runtime.usage-v2');
    assert.equal(usage.projectionReadiness.state, 'ready');
    assert.equal(usage.projectionReadiness.completedVersion, 1);
    assert.equal(usage.projectionReadiness.lastCommitSeq, usage.atCommitSeq);
    assert.equal(usage.querySelection.materialized, false);
    assert.equal(usage.querySelection.selected.queryId, 'legacy.usage');
    assert.equal(usage.querySelection.rollback.queryId, 'legacy.usage');
    assert.equal(usage.querySelection.selectionEpoch, 0);
    assert.equal(usage.aggregate.responseCount, 1);
    assert.equal(usage.aggregate.inputTokens.knownTokens, 12);
    assert.equal(usage.items[0]?.nativeMessageId, 'response-1');

    const selectedLegacyTotals = await engine.getRuntimeUsageTotals({ scopes: [{ projectId, sessionId }] });
    assert.equal(selectedLegacyTotals.status, 'resolved');
    assert.equal(selectedLegacyTotals.resolvedQuery?.queryId, 'legacy.usage');
    assert.equal(selectedLegacyTotals.selectionVector.length, 1);
    assert.match(selectedLegacyTotals.selectionVector[0]?.selectionScopeRef ?? '', /^v1:/);
    assert.equal(selectedLegacyTotals.selectionVector[0]?.v2Eligible, true);
    assert.ok(selectedLegacyTotals.legacy);
    assert.equal(selectedLegacyTotals.usageV2, undefined);

    const explicitV2Totals = await engine.getRuntimeUsageTotals({
      scopes: [{ projectId, sessionId }],
      requestedQueryId: 'runtime.usage-v2',
    });
    assert.equal(explicitV2Totals.status, 'resolved');
    assert.equal(explicitV2Totals.resolvedQuery?.queryId, 'runtime.usage-v2');
    assert.equal(explicitV2Totals.legacy, undefined);
    assert.equal(explicitV2Totals.usageV2?.responseCount, 1);
    assert.equal(explicitV2Totals.usageV2?.inputTokens.knownTokens, 12);
    assert.equal(explicitV2Totals.atCommitSeq, selectedLegacyTotals.atCommitSeq);

    const compatibility = await engine.getRuntimeUsageCompatibility({ scopes: [{ projectId, sessionId }] });
    assert.equal(compatibility.status, 'ready');
    assert.equal(compatibility.comparisonStatus, 'incomparable');
    assert.equal(compatibility.inputTokens?.relation, 'equal');
    assert.equal(compatibility.cacheCreationInputTokens?.relation, 'incomparable');
    assert.equal(compatibility.cacheCreationInputTokens?.absoluteDeltaTokens, undefined);

    await assert.rejects(
      engine.getRuntimeUsageTotals({ scopes: [{ projectId }, { projectId, sessionId }] }),
      /must not overlap/i,
    );
    await assert.rejects(engine.getRuntimeUsageTotals({ scopes: [] }), /between 1 and 128 scopes/i);
    await assert.rejects(
      engine.getRuntimeUsageTotals({
        scopes: [
          { projectId, sessionId },
          { projectId, sessionId },
        ],
      }),
      /duplicate scope/i,
    );

    const coverage = await engine.getFactFamilyCoverage({
      projectId,
      sessionId,
      ownerId: 'runtime.usage-v2',
      family: 'runtime.usage-v2',
      familyVersion: 1,
      limit: 10,
    });
    assert.equal(coverage.status, 'materialized');
    assert.equal(coverage.coverage?.completeness, 'complete');
    assert.equal(coverage.coverage?.lastCommitSeq, coverage.atCommitSeq);
    assert.match(coverage.coverage?.sourceInstanceRef ?? '', /^v1:/);
    assert.match(coverage.coverage?.contentDigestRef ?? '', /^v1:/);
    assert.equal(coverage.items.length, 1);
    assert.equal(coverage.items[0]?.kind, 'point');
    assert.match(coverage.items[0]?.streamRef ?? '', /^v1:/);
    assert.match(coverage.items[0]?.objectRef ?? '', /^v1:/);
    assert.equal(JSON.stringify(coverage).includes(root), false);

    const replayOptions = {
      adapterId: 'claude-code',
      roots: [root],
      projectId,
      sessionId,
      ownerId: 'runtime.usage-v2',
      family: 'runtime.usage-v2',
      familyVersion: 1,
      expectedSourceInstanceRef: coverage.coverage!.sourceInstanceRef,
      expectedContentDigestRef: coverage.coverage!.contentDigestRef,
      expectedCoverageLastCommitSeq: coverage.coverage!.lastCommitSeq,
      reason: 'sdk explicit replay contract test',
    };
    const replay = await engine.replayFactFamily(replayOptions);
    assert.equal(replay.contractVersion, 1);
    assert.equal(replay.authorizedSourceInstanceRef, replayOptions.expectedSourceInstanceRef);
    assert.equal(replay.authorizedContentDigestRef, replayOptions.expectedContentDigestRef);
    assert.equal(replay.outcome.recordsDecoded, 1);
    await assert.rejects(engine.replayFactFamily(replayOptions), /authorization is stale/i);

    const beforePromotion = await engine.getRuntimeUsageV2({ projectId, sessionId, limit: 10 });
    assert.equal(beforePromotion.querySelection.sourceInstanceRef, coverage.coverage!.sourceInstanceRef);
    const promoted = await engine.selectRuntimeUsageQuery({
      projectId,
      sessionId,
      targetQueryId: 'runtime.usage-v2',
      expectedMaterialized: beforePromotion.querySelection.materialized,
      expectedSelectedQueryId: beforePromotion.querySelection.selected.queryId,
      expectedSelectedContractVersion: beforePromotion.querySelection.selected.contractVersion,
      expectedSelectionEpoch: beforePromotion.querySelection.selectionEpoch,
      reason: 'sdk usage-v2 promotion contract test',
    });
    assert.equal(promoted.contractVersion, 1);
    assert.equal(promoted.selection.materialized, true);
    assert.equal(promoted.selection.selected.queryId, 'runtime.usage-v2');
    assert.equal(promoted.selection.rollback.queryId, 'legacy.usage');
    assert.equal(promoted.selection.selectionEpoch, 1);

    await assert.rejects(
      engine.selectRuntimeUsageQuery({
        projectId,
        sessionId,
        targetQueryId: 'legacy.usage',
        expectedMaterialized: beforePromotion.querySelection.materialized,
        expectedSelectedQueryId: beforePromotion.querySelection.selected.queryId,
        expectedSelectedContractVersion: beforePromotion.querySelection.selected.contractVersion,
        expectedSelectionEpoch: beforePromotion.querySelection.selectionEpoch,
        reason: 'stale rollback authorization must fail',
      }),
      /expectation is stale/i,
    );
    const selectedUsage = await engine.getRuntimeUsageV2({ projectId, sessionId, limit: 10 });
    assert.equal(selectedUsage.projectionStatus, 'selected');
    assert.deepEqual(selectedUsage.querySelection, promoted.selection);
    const selectedV2Totals = await engine.getRuntimeUsageTotals({ scopes: [{ projectId, sessionId }] });
    assert.equal(selectedV2Totals.status, 'resolved');
    assert.equal(selectedV2Totals.resolvedQuery?.queryId, 'runtime.usage-v2');
    assert.equal(selectedV2Totals.usageV2?.responseCount, 1);

    const rollbackRequest = {
      projectId,
      sessionId,
      targetQueryId: 'legacy.usage' as const,
      expectedMaterialized: promoted.selection.materialized,
      expectedSelectedQueryId: promoted.selection.selected.queryId,
      expectedSelectedContractVersion: promoted.selection.selected.contractVersion,
      expectedSelectionEpoch: promoted.selection.selectionEpoch,
      reason: 'sdk usage-v2 explicit rollback contract test',
    };
    const rolledBack = await engine.selectRuntimeUsageQuery(rollbackRequest);
    assert.equal(rolledBack.selection.selected.queryId, 'legacy.usage');
    assert.equal(rolledBack.selection.rollback.queryId, 'legacy.usage');
    assert.equal(rolledBack.selection.selectionEpoch, 2);
    assert.equal((await engine.getRuntimeUsageV2({ projectId, sessionId, limit: 10 })).projectionStatus, 'shadow');
    assert.equal(
      (await engine.getRuntimeUsageTotals({ scopes: [{ projectId, sessionId }] })).resolvedQuery?.queryId,
      'legacy.usage',
    );

    const retriedRollback = await engine.selectRuntimeUsageQuery(rollbackRequest);
    assert.equal(retriedRollback.atCommitSeq, rolledBack.atCommitSeq);
    assert.deepEqual(retriedRollback.selection, rolledBack.selection);
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

    const engine = await openTracked(dbPath, 'sdk-team-affiliation-test');
    await engine.reconcileClaude({ roots: [root], reason: 'sdk_team_affiliation_fixture' });
    const projects = await engine.listHistoryProjects({ limit: 10 });
    const projectId = projects.items[0]?.projectId;
    assert.ok(projectId);
    const sessions = await engine.listHistorySessions({ projectId, limit: 10 });
    const sessionId = sessions.items[0]?.sessionId;
    assert.ok(sessionId);

    const usage = await engine.getRuntimeUsageV2({ projectId, sessionId, limit: 10 });
    assert.equal(usage.aggregate.responseCount, 2);
    assert.equal(usage.aggregate.inputTokens.knownTokens, 30);
    assert.equal(usage.actors.length, 2);
    const rootActor = usage.actors.find((actor) => actor.role === 'root');
    const childActor = usage.actors.find((actor) => actor.role === 'child');
    assert.ok(rootActor);
    assert.ok(childActor);
    const rootTeam = rootActor.affiliations.find(
      (affiliation) => affiliation.dimension === 'team' && affiliation.state === 'present',
    );
    const childTeam = childActor.affiliations.find(
      (affiliation) => affiliation.dimension === 'team' && affiliation.state === 'present',
    );
    assert.ok(rootTeam);
    assert.ok(childTeam);
    assert.equal(rootTeam.targetRef.entityKey, childTeam.targetRef.entityKey);
    assert.notEqual(rootTeam.memberRef?.entityKey, childTeam.memberRef?.entityKey);
    assert.equal(
      rootTeam.nativeMemberId,
      teamConfig.members.find((member) => member.agentId === teamConfig.leadAgentId)?.name,
    );
    assert.equal(childTeam.nativeMemberId, childMetadata.name);

    const teamUsage = await engine.getRuntimeUsageV2({
      projectId,
      sessionId,
      affiliationDimension: 'team',
      affiliationTargetRef: rootTeam.targetRef.entityKey,
      limit: 10,
    });
    assert.equal(teamUsage.aggregate.responseCount, 2);
    assert.equal(teamUsage.aggregate.inputTokens.knownTokens, 30);

    writeFileSync(
      path.join(childDir, 'agent-child.meta.json'),
      JSON.stringify({ agentType: childMetadata.agentType, name: childMetadata.name }),
    );
    await engine.reconcileClaude({ roots: [root], reason: 'sdk_team_child_removed' });
    const rootOnly = await engine.getRuntimeUsageV2({
      projectId,
      sessionId,
      affiliationDimension: 'team',
      affiliationTargetRef: rootTeam.targetRef.entityKey,
      limit: 10,
    });
    assert.equal(rootOnly.aggregate.responseCount, 1);
    assert.equal(rootOnly.aggregate.inputTokens.knownTokens, 10);
    assert.equal((await engine.getRuntimeUsageV2({ projectId, sessionId, limit: 10 })).aggregate.responseCount, 2);

    rmSync(configPath);
    await engine.reconcileClaude({ roots: [root], reason: 'sdk_team_root_removed' });
    const noTeamUsage = await engine.getRuntimeUsageV2({
      projectId,
      sessionId,
      affiliationDimension: 'team',
      affiliationTargetRef: rootTeam.targetRef.entityKey,
      limit: 10,
    });
    assert.equal(noTeamUsage.aggregate.responseCount, 0);
    assert.equal((await engine.getRuntimeUsageV2({ projectId, sessionId, limit: 10 })).aggregate.responseCount, 2);
  });

  test('negotiates a composite usage source vector without mixing contracts', async () => {
    const dbPath = temporaryDatabase();
    const base = path.dirname(dbPath);
    const roots = [path.join(base, 'claude-usage-a'), path.join(base, 'claude-usage-b')];
    const sessionIds = ['aaaaaaaa-1111-2222-3333-444444444444', 'bbbbbbbb-1111-2222-3333-444444444444'];
    const transcriptPaths: string[] = [];
    for (const [index, root] of roots.entries()) {
      const project = path.join(root, 'projects', `-tmp-usage-${index}`);
      mkdirSync(project, { recursive: true });
      const transcriptPath = path.join(project, `${sessionIds[index]}.jsonl`);
      transcriptPaths.push(transcriptPath);
      writeFileSync(
        transcriptPath,
        `${JSON.stringify({
          type: 'assistant',
          uuid: `${index}aaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee`,
          parentUuid: null,
          timestamp: `2026-08-1${index + 1}T00:00:00.000Z`,
          sessionId: sessionIds[index],
          cwd: `/tmp/usage-${index}`,
          version: '1',
          gitBranch: 'main',
          isSidechain: false,
          userType: 'external',
          requestId: `request-${index}`,
          message: {
            model: 'claude-sonnet',
            id: `response-${index}`,
            type: 'message',
            role: 'assistant',
            content: [{ type: 'text', text: `usage response ${index}` }],
            usage: {
              input_tokens: (index + 1) * 10,
              output_tokens: index + 1,
              cache_creation_input_tokens: 0,
              cache_read_input_tokens: 0,
            },
          },
        })}\n`,
      );
      if (index === 0) {
        appendFileSync(
          transcriptPath,
          `${JSON.stringify({
            type: 'assistant',
            uuid: '0bbbbbbb-bbbb-cccc-dddd-eeeeeeeeeeee',
            parentUuid: null,
            timestamp: '2026-08-11T00:00:00.001Z',
            sessionId: sessionIds[index],
            cwd: '/tmp/usage-0',
            version: '1',
            gitBranch: 'main',
            isSidechain: false,
            userType: 'external',
            requestId: 'request-0',
            message: {
              model: 'claude-sonnet',
              id: 'response-0',
              type: 'message',
              role: 'assistant',
              content: [{ type: 'text', text: 'usage response 0 revised' }],
              usage: {
                input_tokens: 12,
                output_tokens: 2,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
              },
            },
          })}\n`,
        );
      }
    }

    const engine = await openTracked(dbPath, 'sdk-usage-vector-test');
    await engine.reconcileClaude({ roots, reason: 'sdk_usage_vector_fixture' });
    const projects = (await engine.listHistoryProjects({ limit: 10 })).items;
    assert.equal(projects.length, 2);
    const members = await Promise.all(
      projects.map(async (project) => {
        const session = (await engine.listHistorySessions({ projectId: project.projectId, limit: 10 })).items[0];
        assert.ok(session);
        return { projectId: project.projectId, sessionId: session.sessionId };
      }),
    );

    const legacy = await engine.getRuntimeUsageTotals({ scopes: members });
    assert.equal(legacy.status, 'resolved');
    assert.equal(legacy.resolvedQuery?.queryId, 'legacy.usage');
    assert.equal(legacy.selectionVector.length, 2);
    assert.equal(new Set(legacy.selectionVector.map((item) => item.selectionScopeRef)).size, 2);
    assert.ok(legacy.selectionVector.every((item) => item.v2Eligible));

    const shadowV2 = await engine.getRuntimeUsageTotals({
      scopes: members,
      requestedQueryId: 'runtime.usage-v2',
    });
    assert.equal(shadowV2.status, 'resolved');
    assert.equal(shadowV2.usageV2?.responseCount, 2);
    assert.equal(shadowV2.usageV2?.inputTokens.knownTokens, 32);
    assert.equal(shadowV2.usageV2?.outputTokens.knownTokens, 4);
    const reversedShadowV2 = await engine.getRuntimeUsageTotals({
      scopes: [...members].reverse(),
      requestedQueryId: 'runtime.usage-v2',
    });
    assert.deepEqual(
      reversedShadowV2.selectionVector.map((item) => item.selectionScopeRef),
      shadowV2.selectionVector.map((item) => item.selectionScopeRef),
    );
    assert.deepEqual(reversedShadowV2.usageV2, shadowV2.usageV2);

    const telemetryBefore = (await engine.getStats()).performance?.queries.runtimeUsageCompatibility;
    assert.equal(telemetryBefore?.samples, 0);
    const comparison = await engine.getRuntimeUsageCompatibility({ scopes: members });
    assert.equal(comparison.status, 'ready');
    assert.equal(comparison.comparisonStatus, 'different');
    assert.match(comparison.comparisonRef, /^v1:/);
    assert.equal(comparison.legacy.contributionCount, 3);
    assert.equal(comparison.usageV2?.responseCount, 2);
    assert.equal(comparison.inputTokens?.relation, 'legacy_higher');
    assert.equal(comparison.inputTokens?.legacyCombinedTokens, 42);
    assert.equal(comparison.inputTokens?.v2KnownTokens, 32);
    assert.equal(comparison.inputTokens?.absoluteDeltaTokens, 10);
    assert.equal(comparison.outputTokens?.relation, 'legacy_higher');
    assert.equal(comparison.outputTokens?.absoluteDeltaTokens, 1);
    assert.equal(comparison.cacheCreationInputTokens?.relation, 'equal');
    assert.equal(comparison.cacheReadInputTokens?.relation, 'equal');
    assert.equal(JSON.stringify(comparison).includes(base), false);
    const reversedComparison = await engine.getRuntimeUsageCompatibility({ scopes: [...members].reverse() });
    assert.equal(reversedComparison.comparisonRef, comparison.comparisonRef);
    const compatibilityTelemetry = (await engine.getStats()).performance?.queries.runtimeUsageCompatibility;
    assert.equal(compatibilityTelemetry?.samples, 2);
    assert.equal(compatibilityTelemetry?.readySamples, 2);
    assert.equal(compatibilityTelemetry?.differentSamples, 2);
    assert.equal(compatibilityTelemetry?.legacyHigherBuckets, 4);
    assert.equal(compatibilityTelemetry?.equalBuckets, 4);
    assert.equal(compatibilityTelemetry?.sampledAbsoluteDeltaTokens, 22);
    assert.equal(compatibilityTelemetry?.maxAbsoluteDeltaTokens, 10);

    const promote = async (member: (typeof members)[number]) => {
      const current = await engine.getRuntimeUsageV2({ ...member, limit: 1 });
      return await engine.selectRuntimeUsageQuery({
        ...member,
        targetQueryId: 'runtime.usage-v2',
        expectedMaterialized: current.querySelection.materialized,
        expectedSelectedQueryId: current.querySelection.selected.queryId,
        expectedSelectedContractVersion: current.querySelection.selected.contractVersion,
        expectedSelectionEpoch: current.querySelection.selectionEpoch,
        reason: 'sdk composite vector promotion',
      });
    };

    await promote(members[0]!);
    const mixed = await engine.getRuntimeUsageTotals({ scopes: members });
    assert.equal(mixed.status, 'mixed_selection');
    assert.equal(mixed.resolvedQuery, undefined);
    assert.equal(mixed.legacy, undefined);
    assert.equal(mixed.usageV2, undefined);

    const explicitLegacy = await engine.getRuntimeUsageTotals({
      scopes: members,
      requestedQueryId: 'legacy.usage',
    });
    assert.equal(explicitLegacy.status, 'resolved');
    assert.equal(explicitLegacy.resolvedQuery?.queryId, 'legacy.usage');
    assert.ok(explicitLegacy.legacy);

    await promote(members[1]!);
    const selectedV2 = await engine.getRuntimeUsageTotals({ scopes: members });
    assert.equal(selectedV2.status, 'resolved');
    assert.equal(selectedV2.resolvedQuery?.queryId, 'runtime.usage-v2');
    assert.equal(selectedV2.usageV2?.responseCount, 2);
    assert.equal(selectedV2.usageV2?.inputTokens.knownTokens, 32);
    assert.equal(selectedV2.legacy, undefined);

    appendFileSync(transcriptPaths[0]!, '{"type":"assistant"');
    await engine.reconcileClaude({ roots, reason: 'sdk_usage_vector_incomplete_tail' });
    const selectedButUnready = await engine.getRuntimeUsageTotals({ scopes: members });
    assert.equal(selectedButUnready.status, 'not_ready');
    assert.equal(selectedButUnready.resolvedQuery?.queryId, 'runtime.usage-v2');
    assert.equal(selectedButUnready.legacy, undefined);
    assert.equal(selectedButUnready.usageV2, undefined);
    assert.ok(selectedButUnready.selectionVector.some((item) => !item.v2Eligible));

    const unavailableComparison = await engine.getRuntimeUsageCompatibility({ scopes: members });
    assert.equal(unavailableComparison.status, 'not_ready');
    assert.equal(unavailableComparison.comparisonStatus, 'not_ready');
    assert.equal(unavailableComparison.usageV2, undefined);
    assert.equal(unavailableComparison.inputTokens, undefined);
    const telemetryAfterUnavailable = (await engine.getStats()).performance?.queries.runtimeUsageCompatibility;
    assert.equal(telemetryAfterUnavailable?.samples, 3);
    assert.equal(telemetryAfterUnavailable?.notReadySamples, 1);

    const rollback = async (member: (typeof members)[number]) => {
      const current = await engine.getRuntimeUsageV2({ ...member, limit: 1 });
      return await engine.selectRuntimeUsageQuery({
        ...member,
        targetQueryId: 'legacy.usage',
        expectedMaterialized: current.querySelection.materialized,
        expectedSelectedQueryId: current.querySelection.selected.queryId,
        expectedSelectedContractVersion: current.querySelection.selected.contractVersion,
        expectedSelectionEpoch: current.querySelection.selectionEpoch,
        reason: 'sdk composite unhealthy rollback drill',
      });
    };
    await rollback(members[0]!);
    assert.equal((await engine.getRuntimeUsageTotals({ scopes: members })).status, 'mixed_selection');
    await rollback(members[1]!);
    const restoredLegacy = await engine.getRuntimeUsageTotals({ scopes: members });
    assert.equal(restoredLegacy.status, 'resolved');
    assert.equal(restoredLegacy.resolvedQuery?.queryId, 'legacy.usage');
    assert.ok(restoredLegacy.legacy);
    const retainedV2 = await engine.getRuntimeUsageV2({ ...members[1]!, limit: 10 });
    assert.equal(retainedV2.projectionStatus, 'shadow');
    assert.equal(retainedV2.aggregate.responseCount, 1);
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
