/**
 * source-dimension.test.ts — the v5 source dimension (multi-agent groundwork).
 *
 * Ingests the `small` fixture through the real service and asserts that every
 * row is stamped with a source id, that the id is queryable and filterable,
 * and that an unknown source filters to empty. Claude Code is the only source
 * today, so the expected id is 'claude-code' everywhere.
 *
 * Run with `pnpm --filter @vibecook/spaghetti-sdk test`.
 */

import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { mkdtempSync, rmSync } from 'node:fs';

import { createSpaghettiService } from '../legacy-oracle.js';
import type { LegacySpaghettiAPI as SpaghettiAPI } from '../legacy-api.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE_ROOT_DIR = path.resolve(here, '../../../../crates/spaghetti-napi/fixtures/small/.claude');

describe('source dimension (schema v5)', () => {
  let spaghetti: SpaghettiAPI;
  let tempDir: string;

  before(async () => {
    tempDir = mkdtempSync(path.join(os.tmpdir(), 'spaghetti-source-dim-'));
    spaghetti = createSpaghettiService({
      rootDir: FIXTURE_ROOT_DIR,
      dbPath: path.join(tempDir, 'spaghetti.db'),
    });
    await spaghetti.initialize();
  });

  after(() => {
    spaghetti.shutdown();
    rmSync(tempDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  });

  test('getSourceIds() reports the one source present', () => {
    assert.deepEqual(spaghetti.getSourceIds(), ['claude-code']);
  });

  test('every project carries an exact locator and every session carries its source project', () => {
    const projects = spaghetti.getProjectList();
    assert.ok(projects.length > 0, 'fixture should yield projects');
    for (const project of projects) {
      assert.deepEqual(project.sourceIds, ['claude-code']);
      assert.ok(project.projectId.startsWith('path:'));
      assert.deepEqual(project.members, [{ sourceId: 'claude-code', slug: project.slug }]);
      for (const session of spaghetti.getSessionList(project)) {
        assert.equal(session.sourceId, 'claude-code');
        assert.equal(session.projectSlug, project.slug);
      }
    }
  });

  test('getProjectList filters by source', () => {
    const all = spaghetti.getProjectList();
    assert.deepEqual(
      spaghetti
        .getProjectList({ sourceId: 'claude-code' })
        .map((p) => p.slug)
        .sort(),
      all.map((p) => p.slug).sort(),
    );
    assert.deepEqual(spaghetti.getProjectList({ sourceId: 'codex' }), []);
  });
});
