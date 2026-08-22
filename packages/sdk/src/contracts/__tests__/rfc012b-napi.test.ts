import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createRequire } from 'node:module';
import test from 'node:test';

interface NativeCatalogEngine {
  getCatalogReadinessJson(requestJson: string): Promise<string>;
  listLibraryProjectsJson(requestJson: string): Promise<string>;
  listLibrarySessionsJson(requestJson: string): Promise<string>;
  resolveCatalogEntityJson(requestJson: string): Promise<string>;
  requestCatalogHydrationJson(requestJson: string): Promise<string>;
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
      engine.requestCatalogHydrationJson.bind(engine),
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
    const hydrationFixture = JSON.parse(
      await readFile(
        new URL(
          '../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012b-catalog-hydration-v1.json',
          import.meta.url,
        ),
        'utf8',
      ),
    ) as { command: Record<string, unknown> };
    const queryFixture = JSON.parse(
      await readFile(
        new URL(
          '../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012b-catalog-query-v1.json',
          import.meta.url,
        ),
        'utf8',
      ),
    ) as { contract_request: unknown };
    const command = hydrationFixture.command;
    const authorization = command.authorization as {
      handoff: { selected_base_session_ref: unknown; locator_claim_key: unknown };
    };
    await assert.rejects(
      engine.requestCatalogHydrationJson(
        JSON.stringify({
          contract_request: queryFixture.contract_request,
          coverage_plan_id: (command.snapshot_id as { coverage_plan_id: unknown }).coverage_plan_id,
          snapshot_id: command.snapshot_id,
          selected_base_session_ref: authorization.handoff.selected_base_session_ref,
          locator_claim_key: authorization.handoff.locator_claim_key,
          stable_request_token: '/Users/alice/private/session.jsonl',
        }),
      ),
      (error: unknown) => {
        assert.ok(error instanceof Error);
        assert.match(error.message, /invalid catalog query request/);
        assert.doesNotMatch(error.message, /\/Users\/|alice|private|session\.jsonl/);
        return true;
      },
    );
  } finally {
    await engine.dispose();
    await rm(directory, { recursive: true, force: true });
  }
});
