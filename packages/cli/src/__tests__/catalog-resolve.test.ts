import { test, describe } from 'node:test';
import assert from 'node:assert';
import { resolveCatalogProject, suggestCatalogProjects } from '../lib/catalog.js';
import type { SpaghettiCatalogProject } from '@vibecook/spaghetti-sdk/observation';

function project(overrides: Partial<SpaghettiCatalogProject>): SpaghettiCatalogProject {
  return {
    projectId: 'project_v1_AAAA',
    externalRef: '1:AAAA',
    adapterId: 'claude-code',
    nativeProjectKey: '-tmp-alpha',
    displayName: 'alpha',
    displayPath: '/tmp/alpha',
    catalogState: 'discovered',
    degraded: false,
    sessionCount: 1,
    transcriptSessionCount: 0,
    hydratedSessionCount: 0,
    latestActivityAt: '2026-08-01T00:00:00.000Z',
    lastCommitSeq: 1,
    ...overrides,
  } as SpaghettiCatalogProject;
}

const projects = [
  project({}),
  project({
    projectId: 'project_v1_BBBB',
    externalRef: '1:BBBB',
    nativeProjectKey: '-tmp-beta',
    displayName: 'beta',
    displayPath: '/tmp/beta',
  }),
];

describe('resolveCatalogProject', () => {
  test('resolves the opaque project id a caller passes back', () => {
    assert.strictEqual(resolveCatalogProject('project_v1_BBBB', projects)?.displayName, 'beta');
  });

  test('resolves a persisted external reference', () => {
    assert.strictEqual(resolveCatalogProject('1:BBBB', projects)?.displayName, 'beta');
  });

  test('identifier matching is case-sensitive, because base64url is', () => {
    assert.strictEqual(resolveCatalogProject('PROJECT_V1_BBBB', projects), null);
  });

  test('resolves a 1-based index, a name, and a name prefix', () => {
    assert.strictEqual(resolveCatalogProject('1', projects)?.displayName, 'alpha');
    assert.strictEqual(resolveCatalogProject('beta', projects)?.displayName, 'beta');
    assert.strictEqual(resolveCatalogProject('al', projects)?.displayName, 'alpha');
  });

  test('resolves the native project key', () => {
    assert.strictEqual(resolveCatalogProject('-tmp-beta', projects)?.displayName, 'beta');
  });

  test('returns null when nothing matches, and suggests by name', () => {
    assert.strictEqual(resolveCatalogProject('gamma', projects), null);
    assert.deepStrictEqual(
      suggestCatalogProjects('bet', projects).map((entry) => entry.folderName),
      ['beta'],
    );
  });

  test('returns null for an empty catalog', () => {
    assert.strictEqual(resolveCatalogProject('.', []), null);
  });
});
