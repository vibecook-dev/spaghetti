import assert from 'node:assert/strict';
import { afterEach, describe, test } from 'node:test';
import { cpSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  createObservationService,
  loadNativeAddon,
  openObservationHost,
  type ObservationHost,
  type ObservationHostSource,
  type ObservationService,
} from '../index.js';

const native = loadNativeAddon();
const here = path.dirname(fileURLToPath(import.meta.url));
const fixtureRoot = path.resolve(here, '../../../../crates/spaghetti-napi/fixtures');
const hosts: ObservationHost[] = [];
const services: ObservationService[] = [];
const tempDirs: string[] = [];

afterEach(async () => {
  for (const service of services.splice(0)) await service.dispose();
  for (const host of hosts.splice(0)) await host.dispose();
  for (const directory of tempDirs.splice(0)) {
    rmSync(directory, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 });
  }
});

function multiAdapterFixture(): { dbPath: string; sources: ObservationHostSource[] } {
  const directory = mkdtempSync(path.join(tmpdir(), 'spaghetti-observation-host-'));
  tempDirs.push(directory);
  const roots = {
    claude: path.join(directory, '.claude'),
    codex: path.join(directory, '.codex'),
    grok: path.join(directory, '.grok'),
  };
  cpSync(path.join(fixtureRoot, 'small/.claude'), roots.claude, { recursive: true, preserveTimestamps: true });
  cpSync(path.join(fixtureRoot, 'small-codex/.codex'), roots.codex, { recursive: true, preserveTimestamps: true });
  cpSync(path.join(fixtureRoot, 'small-grok/.grok'), roots.grok, { recursive: true, preserveTimestamps: true });
  return {
    dbPath: path.join(directory, 'spaghetti.db'),
    sources: [
      { adapterId: 'claude-code', roots: [roots.claude] },
      { adapterId: 'codex', roots: [roots.codex] },
      { adapterId: 'grok', roots: [roots.grok] },
    ],
  };
}

describe('observation host options', () => {
  test('rejects empty and duplicate adapter composition before opening storage', async () => {
    await assert.rejects(
      openObservationHost({ dbPath: '/tmp/not-opened.db', sources: [] }),
      /at least one configured adapter/i,
    );
    await assert.rejects(
      openObservationHost({
        dbPath: '/tmp/not-opened.db',
        sources: [
          { adapterId: 'codex', roots: ['/tmp/codex-a'] },
          { adapterId: 'codex', roots: ['/tmp/codex-b'] },
        ],
      }),
      /configured more than once/i,
    );
  });
});

describe('multi-adapter observation host', { skip: !native }, () => {
  test('owns one database, runs all registered adapters, and reopens without duplication', async () => {
    const fixture = multiAdapterFixture();
    const host = await openObservationHost({
      ...fixture,
      queryWorkers: 2,
      ownerLabel: 'multi-adapter-host-test',
    });
    hosts.push(host);

    assert.equal(host.status.state, 'running');
    assert.equal(host.status.observation.supervisorsRunning, 3);
    assert.equal(host.clientInfo.transportKind, 'napi');
    assert.equal((await host.snapshot()).health.healthy, true);

    const sources = await host.client.listSources();
    assert.deepEqual(sources.items.map((source) => source.adapterId).sort(), ['claude-code', 'codex', 'grok']);
    for (const source of sources.items) {
      assert.match(source.adapterVersion, /^\d+\.\d+\.\d+/);
      assert.ok(source.sourceSchemaVersions.length > 0, `${source.adapterId} source schemas`);
      assert.ok(source.capabilities.length > 0, `${source.adapterId} capabilities`);
      for (const capability of source.capabilities) {
        assert.ok(capability.id.length > 0);
        assert.ok(['native', 'derived', 'estimated', 'unsupported'].includes(capability.supportLevel));
        assert.ok(capability.granularity.length > 0);
        assert.ok(['live', 'eventually_live', 'completion_only', 'backfill_only'].includes(capability.availability));
      }
    }
    const overview = await host.client.getOverview();
    assert.deepEqual([overview.canonicalSessions, overview.canonicalMessages], [34, 346]);

    await assert.rejects(openObservationHost({ ...fixture, ownerLabel: 'competing-host' }), /already owned/i);
    const beforeRefresh = overview.commitSeq;
    await host.refresh();
    assert.equal((await host.client.getOverview()).commitSeq, beforeRefresh);

    await host.dispose();
    const reopened = await openObservationHost({ ...fixture, ownerLabel: 'reopened-host' });
    hosts.push(reopened);
    const afterRestart = await reopened.client.getOverview();
    assert.deepEqual([afterRestart.canonicalSessions, afterRestart.canonicalMessages], [34, 346]);
    assert.equal(afterRestart.commitSeq, beforeRefresh);
  });

  test('serves one source-neutral product API for Claude, Codex, and Grok', async () => {
    const fixture = multiAdapterFixture();
    const service = createObservationService({
      ...fixture,
      ownerLabel: 'multi-adapter-service-test',
      live: false,
    });
    services.push(service);
    await service.initialize();

    assert.equal(service.isReady(), true);
    assert.deepEqual(await service.getSourceIds(), ['claude-code', 'codex', 'grok']);
    const projects = await service.getProjectList();
    assert.deepEqual([...new Set(projects.flatMap((project) => project.sourceIds))].sort(), [
      'claude-code',
      'codex',
      'grok',
    ]);

    for (const sourceId of ['claude-code', 'codex', 'grok']) {
      const project = projects.find((candidate) => candidate.sourceIds.includes(sourceId));
      assert.ok(project, `${sourceId} project`);
      const sessions = await service.getSessionList(project, { sourceId });
      assert.ok(sessions.length > 0, `${sourceId} sessions`);
      const page = await service.getSessionMessages(sessions[0]!.projectSlug, sessions[0]!.sessionId, 200, 0, {
        sourceId,
      });
      assert.ok(page.messages.length > 0, `${sourceId} messages`);
      for (const message of page.messages as unknown as Array<Record<string, unknown>>) {
        assert.equal('payload' in message, false);
        assert.equal('nativePayload' in message, false);
        const envelope = message.message as { content?: unknown } | undefined;
        if (Array.isArray(envelope?.content)) {
          for (const block of envelope.content as Array<Record<string, unknown>>) {
            assert.equal('kind' in block, false, `${sourceId} leaked canonical storage block`);
            assert.equal(typeof block.type, 'string', `${sourceId} compatibility block type`);
          }
        }
      }
    }

    const codexProject = projects.find((project) => project.sourceIds.includes('codex'))!;
    const codexSessions = await service.getSessionList(codexProject, { sourceId: 'codex' });
    const rename = codexSessions.find((session) => session.sessionId.includes('019c0001'))!;
    assert.ok(rename);
    const scoped = await service.search({
      text: 'Rename the parser module.',
      projectSlug: rename.projectSlug,
      sessionId: rename.sessionId,
      sourceId: 'codex',
    });
    assert.ok(scoped.results.length > 0);
    assert.ok(scoped.results.every((result) => result.sourceId === 'codex' && result.sessionId === rename.sessionId));
    assert.deepEqual(await service.search({ text: 'Rename the parser module.', projectSlug: 'missing-project' }), {
      results: [],
      total: 0,
      hasMore: false,
    });
  });

  test('releases ownership after initialization failure', async () => {
    const fixture = multiAdapterFixture();
    const failed = createObservationService({
      dbPath: fixture.dbPath,
      sources: [{ adapterId: 'not-registered', roots: [path.dirname(fixture.dbPath)] }],
      live: false,
    });
    services.push(failed);
    await assert.rejects(failed.initialize(), /adapter|registered/i);
    assert.equal(failed.isReady(), false);

    const recovered = createObservationService({ ...fixture, ownerLabel: 'post-failure-owner', live: false });
    services.push(recovered);
    await recovered.initialize();
    assert.equal(recovered.isReady(), true);
  });
});
