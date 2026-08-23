import assert from 'node:assert/strict';
import test from 'node:test';

import type { SessionListItem } from '@vibecook/spaghetti-sdk';
import type { SpaghettiCatalogSession } from '@vibecook/spaghetti-sdk/observation';
import type { CatalogProjectTarget } from './catalog-library.js';
import {
  allCatalogSessionSeeds,
  mergeCatalogSessions,
  mergeDecodedSessions,
  type LibrarySession,
} from './catalog-sessions.js';

function target(overrides: Partial<CatalogProjectTarget> = {}): CatalogProjectTarget {
  return {
    projectId: 'project-a',
    externalRef: '1:project-a',
    adapterId: 'claude-code',
    nativeProjectKey: '-workspace-a',
    ...overrides,
  };
}

function catalogSession(overrides: Partial<SpaghettiCatalogSession> = {}): SpaghettiCatalogSession {
  return {
    sessionId: 'canonical-session-a',
    projectId: 'project-a',
    externalRef: '1:session-a',
    adapterId: 'claude-code',
    nativeSessionId: 'native-session-a',
    title: 'Discovered title',
    catalogState: 'discovered',
    degraded: false,
    associationBasis: 'native',
    associationQuality: 'exact',
    associationProvenance: 'fixture',
    nativeCreatedAt: '2026-08-23T10:00:00.000Z',
    nativeUpdatedAt: '2026-08-23T10:05:00.000Z',
    decodedMessageCount: 0,
    transcriptPresent: true,
    identityConflicts: [],
    lastCommitSeq: 4,
    ...overrides,
  } as SpaghettiCatalogSession;
}

function decodedSession(overrides: Partial<SessionListItem> = {}): SessionListItem {
  return {
    sessionId: 'native-session-a',
    sourceId: 'claude-code',
    projectSlug: '-workspace-a',
    externalRef: '1:session-a',
    decoded: true,
    startTime: '2026-08-23T10:00:00.000Z',
    lastUpdate: '2026-08-23T10:06:00.000Z',
    lifespanMs: 360_000,
    tokenUsage: {
      inputTokens: 10,
      outputTokens: 5,
      cacheCreationTokens: 0,
      cacheReadTokens: 0,
      totalTokens: 15,
    },
    tokensEstimated: false,
    messageCount: 3,
    fullPath: '/fixture/session.jsonl',
    title: 'Decoded title',
    summary: 'Decoded summary',
    firstPrompt: 'Hello',
    gitBranch: 'main',
    todoCount: 1,
    planSlug: null,
    hasTask: true,
    isSidechain: false,
    ...overrides,
  };
}

test('pages every grouped catalog project and creates honest unreadable rows', async () => {
  const projects = [target(), target({ projectId: 'project-b', adapterId: 'codex', nativeProjectKey: '/workspace-a' })];
  const calls: Array<[string | undefined, string | undefined]> = [];
  const sessions = await allCatalogSessionSeeds(projects, async ({ projectId, cursor }) => {
    calls.push([projectId, cursor]);
    if (projectId === 'project-a' && !cursor) {
      return { sessions: [catalogSession({})], cursor: 'more-a', atCommitSeq: 4 };
    }
    if (projectId === 'project-a') {
      return {
        sessions: [
          catalogSession({
            sessionId: 'canonical-session-a2',
            externalRef: '1:session-a2',
            nativeSessionId: undefined,
            nativeUpdatedAt: '2026-08-23T09:00:00.000Z',
          }),
        ],
        atCommitSeq: 4,
      };
    }
    return {
      sessions: [
        catalogSession({
          sessionId: 'canonical-session-b',
          projectId: 'project-b',
          externalRef: '1:session-b',
          adapterId: 'codex',
          nativeSessionId: 'native-session-b',
          nativeUpdatedAt: '2026-08-23T11:00:00.000Z',
        }),
      ],
      atCommitSeq: 4,
    };
  });

  assert.deepEqual(calls, [
    ['project-a', undefined],
    ['project-b', undefined],
    ['project-a', 'more-a'],
  ]);
  assert.equal(sessions.length, 3);
  assert.equal(sessions[0]!.sourceId, 'codex', 'rows are sorted by best catalog activity time');
  assert.ok(sessions.every((session) => !session.decoded));
  assert.ok(sessions.every((session) => session.messageCount === 0 && session.tokenUsage.totalTokens === 0));
  assert.equal(sessions.find((session) => session.externalRef === '1:session-a2')!.sessionId, 'canonical-session-a2');
});

test('decoded rows replace catalog rows by external identity and keep catalog metadata', async () => {
  const catalog = await allCatalogSessionSeeds([target()], async () => ({
    sessions: [catalogSession({ nativeSessionId: undefined })],
    atCommitSeq: 4,
  }));
  const decoded = decodedSession({ sessionId: 'native-proven-later' });

  const catalogThenDecoded = mergeDecodedSessions(catalog, [decoded]);
  assert.equal(catalogThenDecoded.length, 1);
  assert.equal(catalogThenDecoded[0]!.decoded, true);
  assert.equal(catalogThenDecoded[0]!.messageCount, 3);
  assert.equal(catalogThenDecoded[0]!.catalogSessionId, 'canonical-session-a');

  const decodedThenCatalog = mergeCatalogSessions([decoded as LibrarySession], catalog);
  assert.equal(decodedThenCatalog.length, 1);
  assert.equal(decodedThenCatalog[0]!.decoded, true);
  assert.equal(decodedThenCatalog[0]!.messageCount, 3);
  assert.equal(decodedThenCatalog[0]!.catalogSessionId, 'canonical-session-a');
});

test('a catalog-only session remains beside decoded replacements', async () => {
  const catalog = await allCatalogSessionSeeds([target()], async () => ({
    sessions: [catalogSession({}), catalogSession({ sessionId: 'canonical-b', externalRef: '1:session-b' })],
    atCommitSeq: 4,
  }));
  const merged = mergeDecodedSessions(catalog, [decodedSession()]);
  assert.equal(merged.length, 2);
  assert.equal(merged.filter((session) => session.decoded).length, 1);
  assert.equal(merged.find((session) => session.externalRef === '1:session-b')!.decoded, false);
});
