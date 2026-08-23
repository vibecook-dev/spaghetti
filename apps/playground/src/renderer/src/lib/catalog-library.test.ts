import assert from 'node:assert/strict';
import test from 'node:test';

import type { SpaghettiCatalogProject } from '@vibecook/spaghetti-sdk/observation';
import { catalogLibrary, catalogWorkspaceKey } from './catalog-library.js';

function project(overrides: Partial<SpaghettiCatalogProject>): SpaghettiCatalogProject {
  return {
    projectId: 'project_v1_AAAA',
    externalRef: '1:AAAA',
    adapterId: 'claude-code',
    nativeProjectKey: '-Users-test-alpha',
    displayPath: '/Users/test/alpha',
    catalogState: 'discovered',
    degraded: false,
    sessionCount: 3,
    transcriptSessionCount: 0,
    hydratedSessionCount: 0,
    latestActivityAt: '2026-04-01T10:00:00.000Z',
    lastCommitSeq: 1,
    ...overrides,
  } as SpaghettiCatalogProject;
}

test('a workspace is keyed by its path, exactly as the decoded row will be', () => {
  assert.equal(catalogWorkspaceKey(project({})), 'path:/Users/test/alpha');
  assert.equal(
    catalogWorkspaceKey(project({ displayPath: undefined, nativeProjectKey: '/workspace/beta' })),
    'path:/workspace/beta',
  );
  assert.equal(catalogWorkspaceKey(project({ displayPath: '/Users/test/alpha/' })), 'path:/Users/test/alpha');
});

test('a project with no authoritative path is keyed by source and slug', () => {
  const key = catalogWorkspaceKey(project({ displayPath: undefined, nativeProjectKey: '-Users-test-alpha' }));
  assert.equal(key, 'member:{"sourceId":"claude-code","slug":"-Users-test-alpha"}');
});

test('two agents in one workspace become one row carrying both', () => {
  const rows = catalogLibrary([
    project({}),
    project({
      projectId: 'project_v1_BBBB',
      adapterId: 'codex',
      nativeProjectKey: '/Users/test/alpha',
      displayPath: '/Users/test/alpha',
      sessionCount: 4,
      latestActivityAt: '2026-04-02T10:00:00.000Z',
    }),
  ]);

  assert.equal(rows.length, 1);
  const [row] = rows;
  assert.equal(row!.projectId, 'path:/Users/test/alpha');
  assert.deepEqual(row!.sourceIds, ['claude-code', 'codex']);
  assert.equal(row!.members.length, 2);
  assert.equal(row!.sessionCount, 7, 'discovered sessions sum across the agents in the workspace');
  assert.equal(row!.lastActiveAt, '2026-04-02T10:00:00.000Z', 'the workspace is as recent as its most recent agent');
  assert.equal(row!.slug, '/Users/test/alpha', 'the slug follows the most recently active member');
  assert.equal(row!.folderName, 'alpha');
});

test('rows are ordered most recently active first, ties broken by identity', () => {
  const rows = catalogLibrary([
    project({ displayPath: '/w/old', nativeProjectKey: '/w/old', latestActivityAt: '2026-04-01T00:00:00.000Z' }),
    project({ displayPath: '/w/new', nativeProjectKey: '/w/new', latestActivityAt: '2026-04-09T00:00:00.000Z' }),
    project({ displayPath: '/w/a', nativeProjectKey: '/w/a', latestActivityAt: '2026-04-01T00:00:00.000Z' }),
  ]);
  assert.deepEqual(
    rows.map((row) => row.folderName),
    ['new', 'a', 'old'],
  );
});

test('a seeded row never claims decoded numbers it does not have', () => {
  const [row] = catalogLibrary([project({ latestActivityAt: undefined })]);
  assert.equal(row!.decoded, false);
  assert.equal(row!.messageCount, 0);
  assert.equal(row!.tokenUsage.totalTokens, 0);
  assert.equal(row!.lastActiveAt, '');
  assert.equal(row!.latestPrompt, '');
  assert.equal(row!.hasMemory, false);
});
