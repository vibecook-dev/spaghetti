import assert from 'node:assert/strict';
import { afterEach, describe, test } from 'node:test';
import { appendFileSync, existsSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  compareClaudeObservationHistory,
  compareClaudeObservationHistoryQueries,
  compareClaudeObservationUsage,
  createSpaghettiService,
  defaultClaudeObservationShadowDbPath,
  listActiveSessionsFromDir,
  loadNativeAddon,
  openClaudeObservationShadow,
  type ClaudeObservationShadow,
  type SpaghettiEngineHistoryProject,
  type SpaghettiEngineHistorySession,
  type SpaghettiEngineOverview,
  type TeamDirectory,
} from '../index.js';

const native = loadNativeAddon();
const here = path.dirname(fileURLToPath(import.meta.url));
const SMALL_CLAUDE_FIXTURE = path.resolve(here, '../../../../crates/spaghetti-napi/fixtures/small/.claude');
const SESSION_ID = '11111111-2222-3333-4444-555555555555';
const SECOND_SESSION_ID = '22222222-3333-4444-5555-666666666666';
const shadows: ClaudeObservationShadow[] = [];
const legacyServices: Array<{ dispose(): Promise<void> }> = [];
const tempDirs: string[] = [];

function fixture(): { directory: string; productionDb: string; root: string; transcript: string } {
  const directory = mkdtempSync(path.join(tmpdir(), 'spaghetti-shadow-'));
  tempDirs.push(directory);
  const root = path.join(directory, 'claude');
  const project = path.join(root, 'projects', '-tmp-shadow-project');
  mkdirSync(project, { recursive: true });
  return {
    directory,
    productionDb: path.join(directory, 'production.db'),
    root,
    transcript: path.join(project, `${SESSION_ID}.jsonl`),
  };
}

function message(uuid: string, role: 'user' | 'assistant', content: string): string {
  return sessionMessage(SESSION_ID, '2026-08-12T00:00:00.000Z', uuid, role, content);
}

function sessionMessage(
  sessionId: string,
  timestamp: string,
  uuid: string,
  role: 'user' | 'assistant',
  content: string,
): string {
  return `${JSON.stringify({
    type: role,
    uuid,
    parentUuid: null,
    timestamp,
    sessionId,
    cwd: '/tmp/shadow-project',
    gitBranch: 'main',
    message: { role, content },
  })}\n`;
}

function sessionIndex(sessionId: string, modified: string): string {
  return JSON.stringify({
    version: 1,
    originalPath: '/tmp/shadow-project',
    entries: [
      {
        sessionId,
        fullPath: `/tmp/shadow-project/${sessionId}.jsonl`,
        fileMtime: 1_786_507_200_000,
        firstPrompt: `indexed ${sessionId}`,
        summary: 'native index summary',
        messageCount: 9,
        created: '2026-08-10T00:00:00.000Z',
        modified,
        gitBranch: 'index-branch',
        projectPath: '/tmp/shadow-project',
        isSidechain: false,
      },
    ],
  });
}

function writeTeamFixture(root: string): TeamDirectory {
  const teamId = 'alpha-team';
  const teamRoot = path.join(root, 'teams', teamId);
  const inboxRoot = path.join(teamRoot, 'inboxes');
  mkdirSync(inboxRoot, { recursive: true });
  const config = {
    name: 'Alpha Team',
    description: 'shadow query fixture',
    createdAt: 1_786_507_200_000,
    leadAgentId: `lead@${teamId}`,
    leadSessionId: SESSION_ID,
    members: [
      {
        agentId: `lead@${teamId}`,
        name: 'lead',
        agentType: 'team-lead',
        model: 'claude-test',
        prompt: 'coordinate',
        color: 'blue',
        planModeRequired: true,
        joinedAt: 1_786_507_200_000,
        tmuxPaneId: 'leader',
        cwd: '/tmp/shadow-project',
        subscriptions: ['changes'],
        backendType: 'in-process',
      },
    ],
  };
  const messages = [
    {
      from: 'worker',
      text: 'first message',
      summary: 'first',
      timestamp: '2026-08-12T01:00:00.000Z',
      color: 'green',
      read: false,
      msg_id: 'message-1',
      msgV: 1,
      type: 'message',
    },
    {
      from: 'lead',
      text: 'second message',
      timestamp: '2026-08-12T02:00:00.000Z',
      read: true,
      msg_id: 'message-2',
      msgV: 1,
      type: 'message',
    },
  ];
  writeFileSync(path.join(teamRoot, 'config.json'), JSON.stringify(config));
  writeFileSync(path.join(inboxRoot, 'lead.json'), JSON.stringify(messages));
  return { teamId, config, inboxes: { lead: messages } };
}

async function allCanonicalProjects(shadow: ClaudeObservationShadow): Promise<SpaghettiEngineHistoryProject[]> {
  const items: SpaghettiEngineHistoryProject[] = [];
  let cursor: string | undefined;
  do {
    const page = await shadow.listHistoryProjects({ limit: 2, cursor });
    items.push(...page.items);
    cursor = page.nextCursor;
  } while (cursor);
  return items;
}

async function allCanonicalSessions(
  shadow: ClaudeObservationShadow,
  projects: readonly SpaghettiEngineHistoryProject[],
): Promise<SpaghettiEngineHistorySession[]> {
  const items: SpaghettiEngineHistorySession[] = [];
  for (const project of projects) {
    let cursor: string | undefined;
    do {
      const page = await shadow.listHistorySessions(project.projectId, { limit: 3, cursor });
      items.push(...page.items);
      cursor = page.nextCursor;
    } while (cursor);
  }
  return items;
}

afterEach(async () => {
  for (const shadow of shadows.splice(0)) await shadow.dispose();
  for (const service of legacyServices.splice(0)) await service.dispose();
  for (const directory of tempDirs.splice(0)) {
    rmSync(directory, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 });
  }
});

describe('Claude observation shadow', () => {
  test('derives a sibling database path without replacing the production extension', () => {
    assert.equal(
      defaultClaudeObservationShadowDbPath('/tmp/cache/spaghetti-rs.db'),
      '/tmp/cache/spaghetti-rs.observation-shadow.db',
    );
    assert.equal(
      defaultClaudeObservationShadowDbPath('/tmp/cache/spaghetti'),
      '/tmp/cache/spaghetti.observation-shadow.db',
    );
  });

  test('compares only explicitly scoped Claude compatibility counts', () => {
    const overview = {
      commitSeq: 8,
      canonicalSessions: 2,
      canonicalMessages: 7,
    } as SpaghettiEngineOverview;
    assert.deepEqual(compareClaudeObservationHistory(overview, { sessions: 2, messages: 5, subagentMessages: 2 }), {
      atCommitSeq: 8,
      exact: true,
      sessions: { legacy: 2, canonical: 2, delta: 0, exact: true },
      messages: {
        legacyParent: 5,
        legacySubagent: 2,
        legacyTotal: 7,
        canonical: 7,
        delta: 0,
        exact: true,
      },
    });
    assert.throws(
      () => compareClaudeObservationHistory(overview, { sessions: -1, messages: 5, subagentMessages: 2 }),
      /non-negative safe integer/,
    );
  });

  test('normalizes project/session query parity and classifies metadata-only projects', () => {
    const project = {
      nativeProjectKey: 'project',
      transcriptSessionCount: 1,
      messageCount: 3,
      memoryDocumentCount: 1,
      hasMemoryIndex: true,
    } as SpaghettiEngineHistoryProject;
    const metadataOnly = {
      nativeProjectKey: 'metadata-only',
      transcriptSessionCount: 0,
      messageCount: 0,
      memoryDocumentCount: 1,
      hasMemoryIndex: true,
    } as SpaghettiEngineHistoryProject;
    const session = {
      nativeProjectKey: 'project',
      nativeSessionId: SESSION_ID,
      messageCount: 3,
    } as SpaghettiEngineHistorySession;
    const parity = compareClaudeObservationHistoryQueries(
      [project, metadataOnly],
      [session],
      [
        {
          nativeProjectKey: 'project',
          sessionCount: 1,
          parentMessageCount: 2,
          subagentMessageCount: 1,
          hasMemory: true,
        },
      ],
      [
        {
          nativeProjectKey: 'project',
          nativeSessionId: SESSION_ID,
          parentMessageCount: 2,
          subagentMessageCount: 1,
        },
      ],
    );
    assert.equal(parity.exact, true);
    assert.deepEqual(parity.projects.acceptedCanonicalOnly, ['metadata-only']);
    assert.deepEqual(parity.projects.unexpectedCanonicalOnly, []);
    assert.deepEqual(parity.sessions.unexpectedCanonicalOnly, []);
    assert.deepEqual(parity.acceptedDifferences, ['canonical_message_count_includes_subagents']);

    assert.equal(
      compareClaudeObservationHistoryQueries(
        [{ ...project, messageCount: 2 }],
        [session],
        [
          {
            nativeProjectKey: 'project',
            sessionCount: 1,
            parentMessageCount: 2,
            subagentMessageCount: 1,
            hasMemory: true,
          },
        ],
        [
          {
            nativeProjectKey: 'project',
            nativeSessionId: SESSION_ID,
            parentMessageCount: 2,
            subagentMessageCount: 1,
          },
        ],
      ).exact,
      false,
    );
  });

  test('normalizes Claude usage components without equating legacy row counts or provider totals', () => {
    const zeroTokens = {
      inputTokens: 0,
      outputTokens: 0,
      cacheCreationTokens: 0,
      cacheReadTokens: 0,
      componentTotalTokens: 0,
    };
    const exact = {
      inputTokens: 10,
      outputTokens: 2,
      cacheCreationTokens: 3,
      cacheReadTokens: 4,
      componentTotalTokens: 19,
    };
    const aggregate = {
      exact,
      estimated: zeroTokens,
      combined: exact,
      quality: 'exact' as const,
      exactContributionCount: 1,
      estimatedContributionCount: 0,
      contributionCount: 1,
      sessionCount: 1,
    };
    const parity = compareClaudeObservationUsage(
      { aggregate } as never,
      {
        aggregate,
        days: [{ date: '2026-08-12', aggregate }],
        untimed: {
          aggregate: {
            ...aggregate,
            exact: zeroTokens,
            combined: zeroTokens,
            quality: 'unavailable',
            exactContributionCount: 0,
            contributionCount: 0,
            sessionCount: 0,
          },
        },
      } as never,
      {
        totals: exact,
        days: [{ date: '2026-08-12', tokenUsage: exact, sessionCount: 1 }],
      },
    );
    assert.equal(parity.exact, true);
    assert.deepEqual(parity.acceptedDifferences, [
      'canonical_component_total_is_not_provider_billing_total',
      'canonical_contribution_count_is_not_legacy_message_count',
      'canonical_session_count_is_not_legacy_transcript_session_count',
      'canonical_days_require_usage_evidence',
    ]);

    const mismatch = compareClaudeObservationUsage(
      { aggregate } as never,
      {
        aggregate,
        days: [{ date: '2026-08-12', aggregate }],
        untimed: { aggregate: { ...aggregate, contributionCount: 1 } },
      } as never,
      {
        totals: { ...exact, outputTokens: 3 },
        days: [
          { date: '2026-08-10', tokenUsage: zeroTokens, sessionCount: 7 },
          { date: '2026-08-11', tokenUsage: exact, sessionCount: 99 },
        ],
      },
    );
    assert.equal(mismatch.exact, false);
    assert.deepEqual(mismatch.totals.mismatchedFields, ['outputTokens']);
    assert.deepEqual(mismatch.activity.missingCanonical, ['2026-08-11']);
    assert.deepEqual(mismatch.activity.unexpectedCanonical, ['2026-08-12']);
    assert.deepEqual(mismatch.activity.acceptedLegacyZeroUsageDays, ['2026-08-10']);
    assert.equal(mismatch.activity.unexpectedUntimedContributionCount, 1);

    const scopeMismatch = compareClaudeObservationUsage(
      { projectId: 'project-a', aggregate } as never,
      {
        projectId: 'project-b',
        aggregate: { ...aggregate, estimated: { ...zeroTokens, inputTokens: 1 } },
        days: [{ date: '2026-08-12', aggregate }],
        untimed: { aggregate: { ...aggregate, contributionCount: 0 } },
      } as never,
      {
        totals: exact,
        days: [{ date: '2026-08-12', tokenUsage: exact, sessionCount: 42 }],
      },
    );
    assert.equal(scopeMismatch.exact, false);
    assert.deepEqual(scopeMismatch.scope.mismatchedFields, ['projectId']);
    assert.deepEqual(scopeMismatch.activity.unexpectedEstimatedFields, ['inputTokens']);
  });
});

describe('Claude observation shadow native lifecycle', { skip: !native }, () => {
  test('rejects the production database and its SQLite sidecars before opening an engine', async () => {
    const { productionDb, root } = fixture();
    for (const shadowDbPath of [productionDb, `${productionDb}-wal`, `${productionDb}.owner.json`]) {
      await assert.rejects(
        openClaudeObservationShadow({ productionDbPath: productionDb, shadowDbPath, roots: [root] }),
        /must be isolated/,
      );
    }
    assert.equal(existsSync(productionDb), false);
  });

  test(
    'rejects a symlink alias of the production database before owner acquisition',
    {
      skip: process.platform === 'win32',
    },
    async () => {
      const { directory, productionDb, root } = fixture();
      writeFileSync(productionDb, 'production sentinel');
      const alias = path.join(directory, 'shadow-alias.db');
      symlinkSync(productionDb, alias);

      await assert.rejects(
        openClaudeObservationShadow({ productionDbPath: productionDb, shadowDbPath: alias, roots: [root] }),
        /must be isolated/,
      );
      assert.equal(existsSync(`${productionDb}.owner.json`), false);
    },
  );

  test(
    'rejects a dangling symlink aimed at a not-yet-created production database',
    {
      skip: process.platform === 'win32',
    },
    async () => {
      const { directory, productionDb, root } = fixture();
      const alias = path.join(directory, 'dangling-shadow-alias.db');
      symlinkSync(productionDb, alias);

      await assert.rejects(
        openClaudeObservationShadow({ productionDbPath: productionDb, shadowDbPath: alias, roots: [root] }),
        /must be isolated/,
      );
      assert.equal(existsSync(productionDb), false);
    },
  );

  test('owns an isolated database, observes live history, and exposes typed parity evidence', async () => {
    const { productionDb, root, transcript } = fixture();
    writeFileSync(transcript, message('aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee', 'user', 'first'));
    const shadow = await openClaudeObservationShadow({
      productionDbPath: productionDb,
      roots: [root],
      ownerLabel: 'sdk-shadow-test',
    });
    shadows.push(shadow);

    assert.equal(existsSync(productionDb), false, 'shadow startup must not touch production storage');
    assert.equal(shadow.status.observation.supervisorsRunning, 1);
    assert.equal(path.basename(shadow.databasePath), 'production.observation-shadow.db');

    const initial = await shadow.snapshot();
    assert.equal(initial.mode, 'shadow');
    assert.equal(initial.health.healthy, true, initial.health.detail);
    assert.deepEqual([initial.overview.canonicalSessions, initial.overview.canonicalMessages], [1, 1]);
    assert.equal(initial.overview.messages, 0, 'compatibility tables remain distinct');

    appendFileSync(transcript, message('bbbbbbbb-cccc-dddd-eeee-ffffffffffff', 'assistant', 'second'));
    await shadow.refresh();
    const parity = await shadow.compareHistory({ sessions: 1, messages: 2, subagentMessages: 0 });
    assert.equal(parity.exact, true);
    assert.equal(parity.messages.canonical, 2);

    const [stopped, stoppedAgain] = await Promise.all([shadow.dispose(), shadow.dispose()]);
    assert.equal(stopped.state, 'stopped');
    assert.equal(stoppedAgain.state, 'stopped');
    assert.equal(stopped.observation.supervisorsRunning, 0);

    const reopened = await openClaudeObservationShadow({
      productionDbPath: productionDb,
      shadowDbPath: shadow.databasePath,
      roots: [root],
      ownerLabel: 'sdk-shadow-restart-test',
    });
    shadows.push(reopened);
    const resumed = await reopened.snapshot();
    assert.deepEqual([resumed.overview.canonicalSessions, resumed.overview.canonicalMessages], [1, 2]);
    assert.equal(resumed.status.owner?.ownerLabel, 'sdk-shadow-restart-test');
    assert.equal((await reopened.dispose()).state, 'stopped');
  });

  test('queries canonical projects and sessions with Rust-owned ordering, pagination, and enrichment', async () => {
    const { productionDb, root } = fixture();
    const projectsRoot = path.join(root, 'projects');
    const recentProject = path.join(projectsRoot, '-tmp-recent-project');
    const olderProject = path.join(projectsRoot, '-tmp-older-project');
    const indexOnlyProject = path.join(projectsRoot, '-tmp-index-only-project');
    const memoryOnlyProject = path.join(projectsRoot, '-tmp-memory-only-project');
    for (const project of [recentProject, olderProject, indexOnlyProject, memoryOnlyProject]) {
      mkdirSync(project, { recursive: true });
    }
    writeFileSync(
      path.join(recentProject, `${SESSION_ID}.jsonl`),
      sessionMessage(
        SESSION_ID,
        '2026-08-12T02:00:00.000Z',
        'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee',
        'user',
        'recent transcript',
      ),
    );
    writeFileSync(
      path.join(recentProject, `${SECOND_SESSION_ID}.jsonl`),
      sessionMessage(
        SECOND_SESSION_ID,
        '2026-08-12T01:00:00.000Z',
        'cccccccc-dddd-eeee-ffff-000000000000',
        'assistant',
        'second recent transcript',
      ),
    );
    writeFileSync(
      path.join(recentProject, 'sessions-index.json'),
      JSON.stringify({
        ...JSON.parse(sessionIndex(SESSION_ID, '2026-08-12T03:00:00.000Z')),
        entries: [
          JSON.parse(sessionIndex(SESSION_ID, '2026-08-12T03:00:00.000Z')).entries[0],
          JSON.parse(sessionIndex(SECOND_SESSION_ID, '2026-08-12T01:30:00.000Z')).entries[0],
        ],
      }),
    );
    writeFileSync(
      path.join(olderProject, '44444444-5555-6666-7777-888888888888.jsonl'),
      sessionMessage(
        '44444444-5555-6666-7777-888888888888',
        '2026-08-11T02:00:00.000Z',
        'bbbbbbbb-cccc-dddd-eeee-ffffffffffff',
        'assistant',
        'older transcript',
      ),
    );
    writeFileSync(
      path.join(indexOnlyProject, 'sessions-index.json'),
      sessionIndex('33333333-4444-5555-6666-777777777777', '2026-08-10T01:00:00.000Z'),
    );
    mkdirSync(path.join(memoryOnlyProject, 'memory'), { recursive: true });
    writeFileSync(path.join(memoryOnlyProject, 'memory', 'MEMORY.md'), '# Canonical memory\n');

    const shadow = await openClaudeObservationShadow({ productionDbPath: productionDb, roots: [root] });
    shadows.push(shadow);

    const first = await shadow.listHistoryProjects({ limit: 2 });
    assert.equal(first.contractVersion, 1);
    assert.equal(first.items.length, 2);
    assert.equal(first.items[0]?.nativeProjectKey, '-tmp-recent-project');
    assert.equal(first.items[0]?.latestActivitySource, 'session_index');
    assert.equal(first.items[0]?.transcriptSessionCount, 2);
    assert.equal(first.items[0]?.messageCount, 2);
    assert.equal(first.items[0]?.index?.entryCount, 2);
    assert.ok(first.nextCursor);

    const second = await shadow.listHistoryProjects({ limit: 2, cursor: first.nextCursor });
    assert.equal(second.items.length, 2);
    assert.equal(second.nextCursor, undefined);
    assert.deepEqual(
      [...first.items, ...second.items].map((item) => item.nativeProjectKey),
      ['-tmp-recent-project', '-tmp-older-project', '-tmp-index-only-project', '-tmp-memory-only-project'],
    );
    assert.equal(first.atCommitSeq, second.atCommitSeq);
    const indexOnly = second.items.find((item) => item.nativeProjectKey === '-tmp-index-only-project');
    assert.equal(indexOnly?.transcriptSessionCount, 0);
    assert.equal(indexOnly?.messageCount, 0);
    const memoryOnly = second.items.find((item) => item.nativeProjectKey === '-tmp-memory-only-project');
    assert.equal(memoryOnly?.memoryDocumentCount, 1);
    assert.equal(memoryOnly?.hasMemoryIndex, true);
    assert.equal(memoryOnly?.latestActivityAt, undefined);

    const recent = first.items[0];
    assert.ok(recent);
    const sessions = await shadow.listHistorySessions(recent.projectId, { limit: 1 });
    assert.equal(sessions.projectId, recent.projectId);
    assert.equal(sessions.atCommitSeq, first.atCommitSeq);
    assert.equal(sessions.items.length, 1);
    assert.equal(sessions.items[0]?.nativeSessionId, SESSION_ID);
    assert.equal(sessions.items[0]?.messageCount, 1);
    assert.equal(sessions.items[0]?.firstMessageAt, '2026-08-12T02:00:00.000Z');
    assert.equal(sessions.items[0]?.firstMessageTimeQuality, 'native_exact');
    assert.equal(sessions.items[0]?.index?.messageCount, 9, 'native index count stays separate');
    assert.equal(sessions.items[0]?.index?.transcriptStatus, 'present');
    assert.equal(sessions.items[0]?.index?.resolutionStatus, 'resolved');
    assert.ok(sessions.nextCursor);
    const remainingSessions = await shadow.listHistorySessions(recent.projectId, {
      limit: 1,
      cursor: sessions.nextCursor,
    });
    assert.equal(remainingSessions.atCommitSeq, sessions.atCommitSeq);
    assert.deepEqual(
      [...sessions.items, ...remainingSessions.items].map((item) => item.nativeSessionId),
      [SESSION_ID, SECOND_SESSION_ID],
    );
    assert.equal(remainingSessions.nextCursor, undefined);

    await assert.rejects(shadow.listHistoryProjects({ limit: 0 }), /limit must be between 1 and 200/i);
    await assert.rejects(shadow.listHistoryProjects({ cursor: 'not-a-cursor' }), /cursor/i);
    await assert.rejects(shadow.listHistorySessions(recent.projectId, { cursor: first.nextCursor }), /cursor/i);
    await assert.rejects(shadow.listHistorySessions('not-a-project'), /project id/i);

    appendFileSync(
      path.join(recentProject, `${SESSION_ID}.jsonl`),
      sessionMessage(
        SESSION_ID,
        '2026-08-12T04:00:00.000Z',
        'dddddddd-eeee-ffff-0000-111111111111',
        'assistant',
        'invalidate the old snapshot cursor',
      ),
    );
    await shadow.refresh();
    await assert.rejects(shadow.listHistoryProjects({ limit: 2, cursor: first.nextCursor }), /cursor expired/i);
  });

  test('queries durable runtime presence and bounded team/inbox snapshots', async () => {
    const { productionDb, root, transcript } = fixture();
    writeFileSync(transcript, message('aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee', 'user', 'runtime fixture'));
    mkdirSync(path.join(root, 'sessions'), { recursive: true });
    writeFileSync(
      path.join(root, 'sessions', '4242.json'),
      JSON.stringify({
        pid: 4242,
        sessionId: SESSION_ID,
        cwd: '/tmp/shadow-project',
        startedAt: 1_786_507_200_000,
        kind: 'local',
        entrypoint: 'cli',
        name: 'fixture',
        status: 'working',
        updatedAt: 1_786_507_201_000,
        statusUpdatedAt: 1_786_507_201_000,
        procStart: 'process-start',
        version: '1.0.0',
        peerProtocol: 7,
        nameSource: 'native',
        bridgeSessionId: 'bridge',
        messagingSocketPath: '/tmp/shadow.sock',
      }),
    );
    const writtenTeam = writeTeamFixture(root);

    const legacy = createSpaghettiService({ rootDir: root, dbPath: productionDb, engine: 'ts' });
    legacyServices.push(legacy);
    await legacy.initialize();
    const legacyTeam = legacy.getTeams()[0];
    const legacyPresence = listActiveSessionsFromDir(path.join(root, 'sessions'), { requireAlive: false })[0];
    assert.deepEqual(legacyTeam, writtenTeam);
    assert.ok(legacyPresence);

    const shadow = await openClaudeObservationShadow({ productionDbPath: productionDb, roots: [root] });
    shadows.push(shadow);

    const runtime = await shadow.getRuntimeSnapshot({ limit: 1 });
    assert.equal(runtime.contractVersion, 1);
    assert.equal(runtime.entries.length, 1);
    assert.equal(runtime.entries[0]?.kind, 'presence');
    assert.equal(runtime.entries[0]?.presence.nativePid, legacyPresence.pid);
    assert.equal(runtime.entries[0]?.presence.nativeStatus, legacyPresence.status);
    assert.equal(runtime.entries[0]?.presence.nativeSessionId, legacyPresence.sessionId);
    assert.equal(runtime.entries[0]?.presence.cwd, legacyPresence.cwd);
    assert.equal(runtime.entries[0]?.presence.sessionPresent, true);
    assert.equal(runtime.entries[0]?.presence.runPresent, true);
    assert.equal(runtime.entries[0]?.presence.presenceStatus, 'resolved');
    assert.ok(runtime.nextCursor);
    const runtimeRest = await shadow.getRuntimeSnapshot({ limit: 10, cursor: runtime.nextCursor });
    const activeRun = runtimeRest.entries.find((entry) => entry.kind === 'run' && entry.run.state === 'active');
    assert.ok(activeRun?.run);
    const exactRun = await shadow.getRunState(activeRun.run.runId);
    assert.equal(exactRun.atCommitSeq, runtime.atCommitSeq);
    assert.equal(exactRun.run?.nativeRunId, activeRun.run.nativeRunId);
    assert.equal(exactRun.run?.state, 'active');

    const projectPage = await shadow.listHistoryProjects({ limit: 1 });
    const project = projectPage.items[0];
    assert.ok(project);
    const sessionPage = await shadow.listHistorySessions(project.projectId, { limit: 1 });
    const session = sessionPage.items[0];
    assert.ok(session);
    const sessionDetails = await shadow.getSession(session.sessionId);
    assert.equal(sessionDetails.atCommitSeq, runtime.atCommitSeq);
    assert.equal(sessionDetails.session?.nativeSessionId, SESSION_ID);
    assert.equal(sessionDetails.session?.messageCount, 1);
    assert.equal(sessionDetails.session?.runCount, 1);
    assert.equal(sessionDetails.session?.presenceCount, 1);

    const canonicalMessages = await shadow.getMessages(project.projectId, session.sessionId, { limit: 1 });
    const legacyMessages = legacy.getSessionMessages('-tmp-shadow-project', SESSION_ID, 1, 0, {
      sourceId: 'claude-code',
    });
    assert.equal(canonicalMessages.contractVersion, 1);
    assert.equal(canonicalMessages.atCommitSeq, runtime.atCommitSeq);
    assert.equal(canonicalMessages.items.length, 1);
    assert.deepEqual(canonicalMessages.items[0]?.nativePayload, legacyMessages.messages[0]);
    assert.ok(canonicalMessages.payloadBytes > 0);
    assert.ok(canonicalMessages.payloadBytes <= canonicalMessages.payloadByteLimit);

    const sources = await shadow.listSources({ limit: 1 });
    assert.equal(sources.items.length, 1);
    assert.equal(sources.items[0]?.adapterId, 'claude-code');
    assert.deepEqual(
      sources.items.map((source) => source.adapterId),
      legacy.getSourceIds(),
    );
    assert.ok(sources.items[0]?.factCount);

    const stats = await shadow.getStats();
    assert.equal(stats.atCommitSeq, runtime.atCommitSeq);
    assert.equal(stats.sourceInstances, 1);
    assert.equal(stats.sourceObjects > 0, true);
    assert.equal(stats.searchableMessages, 1);
    assert.equal(stats.entities.find((count) => count.name === 'messages')?.count, 1);
    assert.equal(stats.allocatedDatabaseBytes, stats.databasePageCount * stats.databasePageSizeBytes);

    const teamPage = await shadow.listTeams({ limit: 1 });
    assert.equal(teamPage.contractVersion, 1);
    assert.equal(teamPage.items.length, 1);
    assert.equal(teamPage.items[0]?.nativeTeamId, legacyTeam?.teamId);
    assert.equal(teamPage.items[0]?.config?.name, legacyTeam?.config?.name);
    assert.equal(teamPage.items[0]?.inboxCount, 1);
    assert.equal(teamPage.items[0]?.messageCount, 2);
    assert.equal(teamPage.items[0]?.unreadMessageCount, 1);
    const teamId = teamPage.items[0]?.teamId;
    assert.ok(teamId);
    const details = await shadow.getTeam(teamId);
    assert.equal(details.members.length, 1);
    assert.equal(details.members[0]?.nativeName, 'lead');
    assert.deepEqual(details.members[0]?.subscriptions, ['changes']);
    assert.equal(details.team.config?.leadSessionPresent, true);

    const inboxes = await shadow.listTeamInboxes(teamId, { limit: 1 });
    assert.equal(inboxes.items.length, 1);
    assert.equal(inboxes.items[0]?.nativeRecipientName, 'lead');
    const inboxId = inboxes.items[0]?.inboxId;
    assert.ok(inboxId);
    const firstMessage = await shadow.listTeamInboxMessages(inboxId, { limit: 1 });
    assert.equal(firstMessage.items[0]?.text, legacyTeam?.inboxes.lead?.[0]?.text);
    assert.equal(firstMessage.items[0]?.nativeMessageId, 'message-1');
    assert.ok(firstMessage.nextCursor);
    const secondMessage = await shadow.listTeamInboxMessages(inboxId, {
      limit: 1,
      cursor: firstMessage.nextCursor,
    });
    assert.equal(secondMessage.items[0]?.text, legacyTeam?.inboxes.lead?.[1]?.text);
    assert.equal(secondMessage.nextCursor, undefined);

    await assert.rejects(shadow.getRuntimeSnapshot({ limit: 0 }), /runtime page limit/i);
    await assert.rejects(shadow.getSession('not-a-session'), /session detail id/i);
    await assert.rejects(shadow.getMessages(project.projectId, session.sessionId, { limit: 0 }), /message page limit/i);
    await assert.rejects(shadow.getRunState('not-a-run'), /run state id/i);
    await assert.rejects(shadow.listSources({ limit: 0 }), /source page limit/i);
    await assert.rejects(shadow.listTeams({ limit: 0 }), /team page limit/i);
    await assert.rejects(shadow.getTeam('not-a-team'), /team id/i);
    await assert.rejects(shadow.listTeamInboxes(teamId, { cursor: runtime.nextCursor }), /cursor/i);
  });

  test('queries memory, tasks, plans, tool results, and binary artifacts through Rust-owned pages', async () => {
    const directory = mkdtempSync(path.join(tmpdir(), 'spaghetti-shadow-capabilities-'));
    tempDirs.push(directory);
    const productionDb = path.join(directory, 'legacy.db');
    const legacy = createSpaghettiService({
      rootDir: SMALL_CLAUDE_FIXTURE,
      dbPath: productionDb,
      engine: 'ts',
    });
    legacyServices.push(legacy);
    await legacy.initialize();
    const shadow = await openClaudeObservationShadow({
      productionDbPath: productionDb,
      roots: [SMALL_CLAUDE_FIXTURE],
      ownerLabel: 'sdk-shadow-capability-query-test',
    });
    shadows.push(shadow);

    const projects = await allCanonicalProjects(shadow);
    const sessions = await allCanonicalSessions(shadow, projects);
    const memoryProject = projects.find((project) => project.hasMemoryIndex);
    assert.ok(memoryProject);
    const memory = await shadow.listMemoryDocuments(memoryProject.projectId, { limit: 1 });
    assert.equal(memory.contractVersion, 1);
    assert.equal(memory.items[0]?.isIndex, true);
    assert.equal(memory.items[0]?.content, legacy.getProjectMemory(memoryProject.nativeProjectKey));
    assert.ok(memory.payloadBytes <= memory.payloadByteLimit);

    const todoSession = sessions.find((session) => session.nativeSessionId === '03ddf851-127d-6cfe-a095-f121d263f759');
    assert.ok(todoSession);
    const taskCollections = await shadow.listTaskCollections({ sessionId: todoSession.sessionId, limit: 1 });
    assert.equal(taskCollections.items.length, 1);
    assert.equal(taskCollections.items[0]?.collectionKind, 'todo_list');
    assert.equal(taskCollections.items[0]?.itemCount, 3);
    const tasks = await shadow.listTasks(taskCollections.items[0]!.collectionId, { limit: 2 });
    assert.equal(tasks.items.length, 2);
    assert.ok(tasks.nextCursor);
    const remainingTasks = await shadow.listTasks(taskCollections.items[0]!.collectionId, {
      limit: 2,
      cursor: tasks.nextCursor,
    });
    assert.deepEqual(
      [...tasks.items, ...remainingTasks.items].map((task) => [task.subject, task.taskStatus]),
      [
        ['Write the parser', 'completed'],
        ['Port the writer', 'in_progress'],
        ['Add the diff harness', 'pending'],
      ],
    );

    const firstPlan = await shadow.listPlans({ limit: 1 });
    assert.equal(firstPlan.items.length, 1);
    assert.equal(firstPlan.items[0]?.nativePlanId, 'fixture-refactor');
    assert.equal(firstPlan.items[0]?.content, '# Fixture Refactor Plan\n\n1. Extract the parser.\n2. Ship it.\n');
    assert.ok(firstPlan.nextCursor);
    const secondPlan = await shadow.listPlans({ limit: 1, cursor: firstPlan.nextCursor });
    assert.equal(secondPlan.items[0]?.nativePlanId, 'untitled-notes');

    const toolSession = sessions.find((session) => session.nativeSessionId === '54770fec-7aff-bc2f-0c2e-10170aa2987b');
    assert.ok(toolSession);
    const toolResults = await shadow.listToolResults(toolSession.projectId, toolSession.sessionId, { limit: 1 });
    assert.equal(toolResults.items.length, 1);
    assert.equal(
      toolResults.items[0]?.content,
      legacy.getToolResult(
        toolResults.items[0]!.nativeProjectKey,
        toolResults.items[0]!.nativeSessionId,
        toolResults.items[0]!.nativeToolUseId,
      ),
    );

    const artifactSession = sessions.find(
      (session) => session.nativeSessionId === '40f26ec0-7084-ef15-b183-2d832ca4ecd6',
    );
    assert.ok(artifactSession);
    const artifacts = await shadow.listArtifacts(artifactSession.sessionId, { limit: 1 });
    assert.equal(artifacts.items.length, 1);
    assert.equal(Buffer.from(artifacts.items[0]!.contentBase64!, 'base64').toString(), 'first snapshot contents\n');
    assert.ok(artifacts.payloadBytes <= artifacts.payloadByteLimit);

    assert.equal(memory.atCommitSeq, taskCollections.atCommitSeq);
    assert.equal(taskCollections.atCommitSeq, firstPlan.atCommitSeq);
    assert.equal(firstPlan.atCommitSeq, toolResults.atCommitSeq);
    assert.equal(toolResults.atCommitSeq, artifacts.atCommitSeq);
    await assert.rejects(shadow.listMemoryDocuments('not-a-project'), /memory document project id/i);
    await assert.rejects(
      shadow.listTaskCollections({ sessionId: todoSession.sessionId, runId: 'run_v1_cnVu' }),
      /at most one/i,
    );
    await assert.rejects(shadow.listTasks('not-a-collection'), /task collection id/i);
    await assert.rejects(shadow.listPlans({ limit: 0 }), /plan page limit/i);
    await assert.rejects(
      shadow.listToolResults(
        projects.find((project) => project.projectId !== todoSession.projectId)!.projectId,
        todoSession.sessionId,
      ),
      /does not identify a current session/i,
    );
    await assert.rejects(shadow.listArtifacts('not-a-session'), /artifact session id/i);
  });

  test('searches parent and delegated messages in one canonical FTS score domain', async () => {
    const directory = mkdtempSync(path.join(tmpdir(), 'spaghetti-shadow-search-'));
    tempDirs.push(directory);
    const productionDb = path.join(directory, 'legacy.db');
    const legacy = createSpaghettiService({
      rootDir: SMALL_CLAUDE_FIXTURE,
      dbPath: productionDb,
      engine: 'ts',
    });
    legacyServices.push(legacy);
    await legacy.initialize();
    const shadow = await openClaudeObservationShadow({
      productionDbPath: productionDb,
      roots: [SMALL_CLAUDE_FIXTURE],
      ownerLabel: 'sdk-shadow-search-query-test',
    });
    shadows.push(shadow);

    const marker = await shadow.search({ text: 'searchable-wf-marker', branchKind: 'delegated', limit: 1 });
    assert.equal(marker.contractVersion, 1);
    assert.equal(marker.querySyntax, 'literal_phrase_v1');
    assert.equal(marker.scoreDirection, 'lower_is_better');
    assert.equal(marker.totalIsExact, true);
    assert.equal(marker.total, legacy.search({ text: 'searchable-wf-marker' }).total);
    assert.equal(marker.items.length, 1);
    assert.equal(marker.items[0]?.branchKind, 'delegated');
    assert.equal(marker.items[0]?.nativeChildId, 'afixture01');
    assert.match(marker.items[0]!.snippet, /searchable-wf-marker/);
    assert.ok(marker.payloadBytes <= marker.payloadByteLimit);

    const markerHit = marker.items[0]!;
    assert.ok(markerHit.projectId);
    const timeline = await shadow.getTimeline({
      projectId: markerHit.projectId,
      sessionId: markerHit.sessionId,
      branchKind: 'delegated',
      search: 'searchable-wf-marker',
      limit: 1,
    });
    assert.equal(timeline.contractVersion, 1);
    assert.equal(timeline.order, 'newest_first');
    assert.equal(timeline.searchSyntax, 'literal_phrase_v1');
    assert.equal(timeline.totalIsExact, true);
    assert.equal(timeline.total, 1);
    assert.ok(timeline.facets.totalMessages > timeline.total, 'facets describe the unfiltered session');
    assert.equal(timeline.items[0]?.messageId, markerHit.messageId);
    assert.equal(timeline.items[0]?.branchKind, 'delegated');
    assert.equal(timeline.items[0]?.nativeChildId, 'afixture01');
    assert.deepEqual(timeline.items[0]?.contentKinds, ['text']);
    assert.ok(timeline.payloadBytes <= timeline.payloadByteLimit);

    const timelineFirst = await shadow.getTimeline({
      projectId: markerHit.projectId,
      sessionId: markerHit.sessionId,
      limit: 1,
    });
    assert.ok(timelineFirst.nextCursor);
    const timelineNext = await shadow.getTimeline({
      projectId: markerHit.projectId,
      sessionId: markerHit.sessionId,
      limit: 1,
      cursor: timelineFirst.nextCursor,
    });
    assert.equal(timelineNext.atCommitSeq, timelineFirst.atCommitSeq);
    assert.equal(timelineNext.total, timelineFirst.total);
    assert.notEqual(timelineNext.items[0]?.messageId, timelineFirst.items[0]?.messageId);
    await assert.rejects(
      shadow.getTimeline({
        projectId: markerHit.projectId,
        sessionId: markerHit.sessionId,
        branchKind: 'root',
        limit: 1,
        cursor: timelineFirst.nextCursor,
      }),
      /cursor/i,
    );

    assert.ok(markerHit.nativeProjectKey);
    assert.ok(markerHit.nativeSessionId);
    const legacyWorkflows = legacy.getSessionWorkflows(markerHit.nativeProjectKey, markerHit.nativeSessionId);
    assert.equal(legacyWorkflows.length, 1);
    const legacyWorkflow = legacyWorkflows[0]!;
    const workflows = await shadow.listWorkflows({
      projectId: markerHit.projectId,
      sessionId: markerHit.sessionId,
      limit: 1,
    });
    assert.equal(workflows.contractVersion, 1);
    assert.equal(workflows.items.length, 1);
    const workflow = workflows.items[0]!;
    assert.match(workflow.workflowId, /^workflow_v1_/);
    assert.equal(workflow.nativeWorkflowId, legacyWorkflow.workflowId);
    assert.equal(workflow.name, legacyWorkflow.name);
    assert.equal(workflow.nativeStatus, legacyWorkflow.status);
    assert.equal(workflow.agentCount, legacyWorkflow.agentCount);
    assert.equal(workflow.totalTokens, legacyWorkflow.totalTokens);
    assert.equal(workflow.totalToolCalls, legacyWorkflow.totalToolCalls);
    assert.equal(workflow.durationMs, legacyWorkflow.durationMs);
    assert.equal(workflow.snapshotStatus, 'present');
    assert.equal(workflow.resolutionStatus, 'resolved');
    assert.equal(workflow.observedMemberCount, 1);
    assert.equal(workflow.membershipCountStatus, 'different');

    const workflowDetails = await shadow.getWorkflow(workflow.workflowId);
    assert.equal(workflowDetails.workflow.workflowId, workflow.workflowId);
    assert.equal(workflowDetails.defaultModel, 'claude-test');
    assert.equal(workflowDetails.script, 'fixture-audit');
    assert.equal((workflowDetails.nativeSnapshot as { runId?: string })?.runId, legacyWorkflow.workflowId);
    assert.ok(workflowDetails.payloadBytes <= workflowDetails.payloadByteLimit);

    const legacyMembers = legacy.getWorkflowSubagents(
      markerHit.nativeProjectKey,
      markerHit.nativeSessionId,
      legacyWorkflow.workflowId,
    );
    assert.equal(legacyMembers.length, 1);
    const members = await shadow.listWorkflowMembers(workflow.workflowId, { limit: 1 });
    assert.equal(members.contractVersion, 1);
    assert.equal(members.items.length, 1);
    assert.equal(members.items[0]?.nativeAgentId, legacyMembers[0]?.agentId);
    assert.equal(members.items[0]?.messageCount, legacyMembers[0]?.messageCount);
    assert.equal(members.items[0]?.memberStatus, 'result_observed');
    assert.deepEqual(members.items[0]?.result, { summary: 'did the thing' });
    assert.equal(members.items[0]?.childRunPresent, true);
    assert.ok(members.payloadBytes <= members.payloadByteLimit);

    const workflowDelegations = await shadow.listDelegations({
      projectId: markerHit.projectId,
      sessionId: markerHit.sessionId,
      workflowId: workflow.workflowId,
      limit: 1,
    });
    assert.equal(workflowDelegations.contractVersion, 1);
    assert.equal(workflowDelegations.items.length, 1);
    assert.equal(workflowDelegations.items[0]?.nativeChildId, legacyMembers[0]?.agentId);
    assert.equal(workflowDelegations.items[0]?.messageCount, legacyMembers[0]?.messageCount);
    assert.equal(workflowDelegations.items[0]?.workflowMemberCount, 1);
    const standaloneDelegations = await shadow.listDelegations({
      projectId: markerHit.projectId,
      sessionId: markerHit.sessionId,
      standaloneOnly: true,
    });
    assert.equal(standaloneDelegations.items.length, 0);
    await assert.rejects(
      shadow.listDelegations({
        projectId: markerHit.projectId,
        sessionId: markerHit.sessionId,
        workflowId: workflow.workflowId,
        standaloneOnly: true,
      }),
      /cannot be combined/i,
    );

    const all = await shadow.search({ text: 'Add error handling to the parser', limit: 1 });
    assert.ok(all.total > 1);
    assert.equal(all.items.length, 1);
    assert.ok(all.nextCursor);
    const next = await shadow.search({
      text: 'Add error handling to the parser',
      limit: 1,
      cursor: all.nextCursor,
    });
    assert.equal(next.total, all.total);
    assert.notEqual(next.items[0]?.messageId, all.items[0]?.messageId);
    assert.equal(next.atCommitSeq, all.atCommitSeq);

    await assert.rejects(
      shadow.search({ text: 'searchable-wf-marker', branchKind: 'root', cursor: all.nextCursor }),
      /cursor/i,
    );
    await assert.rejects(shadow.search({ text: '   ' }), /search text/i);
    const controller = new AbortController();
    controller.abort();
    await assert.rejects(shadow.search({ text: 'searchable-wf-marker' }, controller.signal), /abort|cancel/i);
    await assert.rejects(
      shadow.getTimeline({ projectId: markerHit.projectId, sessionId: markerHit.sessionId }, controller.signal),
      /abort|cancel/i,
    );
    await assert.rejects(
      shadow.listWorkflows({ projectId: markerHit.projectId, sessionId: markerHit.sessionId }, controller.signal),
      /abort|cancel/i,
    );
  });

  test('matches normalized project/session summaries from the committed TypeScript oracle', async () => {
    const directory = mkdtempSync(path.join(tmpdir(), 'spaghetti-shadow-parity-'));
    tempDirs.push(directory);
    const productionDb = path.join(directory, 'legacy.db');
    const legacy = createSpaghettiService({
      rootDir: SMALL_CLAUDE_FIXTURE,
      dbPath: productionDb,
      engine: 'ts',
    });
    try {
      await legacy.initialize();
      const shadow = await openClaudeObservationShadow({
        productionDbPath: productionDb,
        roots: [SMALL_CLAUDE_FIXTURE],
        ownerLabel: 'sdk-shadow-query-parity-test',
      });
      shadows.push(shadow);

      const canonicalProjects = await allCanonicalProjects(shadow);
      const canonicalSessions = await allCanonicalSessions(shadow, canonicalProjects);
      const legacyProjects = legacy.getProjectList({ sourceId: 'claude-code' });
      const legacySessions = legacyProjects.flatMap((project) =>
        legacy.getSessionList(project, { sourceId: 'claude-code' }).map((session) => {
          const subagentMessageCount = legacy
            .getSessionSubagents(session.projectSlug, session.sessionId, {
              sourceId: 'claude-code',
              includeNested: true,
            })
            .reduce((total, agent) => total + agent.messageCount, 0);
          return {
            nativeProjectKey: session.projectSlug,
            nativeSessionId: session.sessionId,
            parentMessageCount: session.messageCount,
            subagentMessageCount,
          };
        }),
      );
      const legacyProjectSummaries = legacyProjects.map((project) => {
        const projectSessions = legacySessions.filter((session) => session.nativeProjectKey === project.slug);
        return {
          nativeProjectKey: project.slug,
          sessionCount: project.sessionCount,
          parentMessageCount: project.messageCount,
          subagentMessageCount: projectSessions.reduce((total, session) => total + session.subagentMessageCount, 0),
          hasMemory: project.hasMemory,
        };
      });

      const parity = compareClaudeObservationHistoryQueries(
        canonicalProjects,
        canonicalSessions,
        legacyProjectSummaries,
        legacySessions,
      );
      assert.equal(parity.exact, true, JSON.stringify(parity));
      assert.equal(parity.projects.compared, legacyProjects.length);
      assert.equal(parity.sessions.compared, legacySessions.length);
      assert.deepEqual(parity.projects.unexpectedCanonicalOnly, []);
      assert.deepEqual(parity.sessions.unexpectedCanonicalOnly, []);

      for (const legacyProject of legacyProjects) {
        const canonicalProject = canonicalProjects.find((project) => project.nativeProjectKey === legacyProject.slug);
        assert.ok(canonicalProject, `missing canonical usage project ${legacyProject.slug}`);
        const [canonicalTotals, canonicalActivity] = await Promise.all([
          shadow.getUsage({ projectId: canonicalProject.projectId }),
          shadow.getUsageActivity({
            projectId: canonicalProject.projectId,
            from: '2026-01-01',
            to: '2026-12-31',
          }),
        ]);
        const legacyActivity = legacy.getProjectTokenActivity(legacyProject, {
          sourceId: 'claude-code',
          from: '2026-01-01',
          to: '2026-12-31',
        });
        const usageParity = compareClaudeObservationUsage(canonicalTotals, canonicalActivity, {
          totals: legacyProject.tokenUsage,
          days: legacyActivity.days,
        });
        assert.equal(usageParity.exact, true, `${legacyProject.slug}: ${JSON.stringify(usageParity)}`);
        assert.equal(
          usageParity.activity.compared + usageParity.activity.acceptedLegacyZeroUsageDays.length,
          legacyActivity.days.length,
        );
      }
    } finally {
      await legacy.dispose();
    }
  });
});
