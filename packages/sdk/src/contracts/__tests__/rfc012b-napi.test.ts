import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createRequire } from 'node:module';
import test from 'node:test';

interface NativeCatalogEngine {
  getCatalogReadinessJson(requestJson: string): Promise<string>;
  listLibraryProjectsJson(requestJson: string): Promise<string>;
  listLibrarySessionsJson(requestJson: string): Promise<string>;
  resolveCatalogEntityJson(requestJson: string): Promise<string>;
  dispose(): Promise<unknown>;
}

interface NativeCatalogAddon {
  openSpaghettiEngine(options: { dbPath: string; ownerLabel: string }): Promise<NativeCatalogEngine>;
}

const native = createRequire(import.meta.url)('@vibecook/spaghetti-sdk-native') as NativeCatalogAddon;

test('RFC 012B native JSON boundary is bounded and does not echo attacker data', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'spaghetti-rfc012b-napi-'));
  const engine = await native.openSpaghettiEngine({
    dbPath: join(directory, 'catalog.db'),
    ownerLabel: 'rfc012b-napi-test',
  });
  try {
    const pathShapedRequest = JSON.stringify({ '/Users/alice/private/session.jsonl': 'secret' });
    const methods = [
      engine.getCatalogReadinessJson.bind(engine),
      engine.listLibraryProjectsJson.bind(engine),
      engine.listLibrarySessionsJson.bind(engine),
      engine.resolveCatalogEntityJson.bind(engine),
    ];
    for (const method of methods) {
      await assert.rejects(
        async () => method(pathShapedRequest),
        (error: unknown) => {
          assert.ok(error instanceof Error);
          assert.match(error.message, /invalid catalog query request/);
          assert.doesNotMatch(error.message, /\/Users\/|alice|private|session\.jsonl|secret/);
          return true;
        },
      );
      await assert.rejects(async () => method('x'.repeat(256 * 1024 + 1)), /invalid catalog query request/);
    }
  } finally {
    await engine.dispose();
    await rm(directory, { recursive: true, force: true });
  }
});
