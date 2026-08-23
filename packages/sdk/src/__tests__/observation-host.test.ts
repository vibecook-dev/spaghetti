import assert from 'node:assert/strict';
import { afterEach, describe, test } from 'node:test';
import { cpSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  createObservationService,
  loadNativeAddon,
  openObservationHost,
  type ObservationHost,
  type ObservationHostProgress,
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
  test('explicit deferred bootstrap serves catalog queries while search is incomplete', async () => {
    const fixture = multiAdapterFixture();
    const startup: ObservationHostProgress[] = [];
    const host = await openObservationHost({
      dbPath: fixture.dbPath,
      sources: [fixture.sources[0]!],
      queryWorkers: 1,
      ownerLabel: 'bootstrap-host-test',
      deferQueryStructures: true,
      onProgress: (progress) => startup.push(progress),
    });
    hosts.push(host);

    const ready = startup.filter((progress) => progress.stage === 'ready');
    assert.ok(ready.length > 0);
    assert.equal(host.status.acceptingQueries, true);
    assert.equal(host.status.aliveQueryWorkers, 1);

    // Catalog-first: the library is listable while search is still pending.
    const readiness = await host.readiness();
    assert.equal(readiness.catalog.state, 'ready');
    assert.equal(readiness.search.state, 'pending');
    assert.ok(host.catalog.catalogProjects > 0, 'discovery committed projects');
    assert.ok(host.catalog.catalogSessions > 0, 'discovery committed sessions');
    assert.deepEqual(host.catalog.degradedSources, []);
    const catalogProjects = await host.client.listCatalogProjects();
    assert.ok(catalogProjects.projects.length > 0, 'projects are listable before search');

    const sources = await host.client.listSources();
    assert.ok(sources.items.length >= 1);
    const started = Date.now();
    while (host.status.state === 'bootstrapping' && Date.now() - started < 120_000) {
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    assert.equal(host.status.state, 'running');
    assert.ok((await host.client.getStats()).searchableMessages >= 0);
    assert.equal(startup.at(-1)?.stage, 'ready');
  });

  test('owns one database, runs all registered adapters, and reopens without duplication', async () => {
    const fixture = multiAdapterFixture();
    const startup: ObservationHostProgress[] = [];
    const host = await openObservationHost({
      ...fixture,
      queryWorkers: 2,
      ownerLabel: 'multi-adapter-host-test',
      onProgress: (progress) => startup.push(progress),
    });
    hosts.push(host);

    assert.equal(host.status.state, 'running');
    // Catalog-first: the host returns before watchers finish starting, so a
    // caller that needs history says so explicitly.
    await host.whenObserving();
    assert.equal(host.status.observation.supervisorsRunning, 3);
    assert.equal(host.clientInfo.transportKind, 'napi');
    assert.equal((await host.snapshot()).health.healthy, true);
    assert.deepEqual(
      startup.map((progress) => progress.stage),
      ['opening', 'discovering-catalog', 'catalog-ready', 'ready'],
    );
    const committed = startup.find((progress) => progress.stage === 'catalog-ready')?.catalog;
    assert.ok(committed, 'catalog-ready reports what discovery committed');
    assert.equal(committed.supervisorsStarted, 3);
    assert.equal(committed.historyBackground, true);

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
    const stats = await host.client.getStats();
    const performance = stats.performance;
    assert.ok(performance, 'native owner performance snapshot');
    assert.equal(performance.writer.committed, stats.ingestCommits);
    assert.equal(performance.writer.failed, 0);
    assert.ok(performance.writer.factsCommitted >= stats.factRecords);
    assert.ok(performance.writer.changesPublished > 0);
    assert.ok(performance.writer.sqliteRowsChanged >= performance.writer.factsCommitted);
    assert.equal(
      performance.writer.checkpoint.completed +
        performance.writer.checkpoint.blocked +
        performance.writer.checkpoint.failures,
      performance.writer.checkpoint.attempts,
    );
    assert.equal(
      performance.source.totals.recordsDecoded +
        performance.source.totals.decodeRetries +
        performance.source.totals.decodeFailures,
      performance.source.totals.decodeAttempts,
    );
    assert.ok(performance.source.totals.recordsRead >= performance.source.totals.recordsDecoded);
    assert.ok(performance.source.totals.factsEmitted >= performance.writer.factsCommitted);
    assert.equal(performance.source.dimensionOverflowAssignments, 0);
    assert.ok(performance.source.dimensions.length > 0);
    assert.ok(performance.source.dimensions.length <= performance.source.dimensionCapacity);
    assert.equal(
      performance.source.totals.timings.find((timing) => timing.name === 'adapter_decode')?.latency.samples,
      performance.source.totals.decodeAttempts,
    );
    assert.equal(
      performance.writer.timings.find((timing) => timing.name === 'writer_total')?.latency.samples,
      performance.writer.committed,
    );
    assert.ok(performance.queries.requestsCompleted > 0);
    assert.ok(performance.storage.databaseFileBytes > 0);

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

  // This owner-only mutation cannot run until an independently reviewed
  // Claude support release is promoted and can mint typed durable authority.
  test.skip('authorizes fact-family replay against current coverage and configured roots', async () => {
    const fixture = multiAdapterFixture();
    const host = await openObservationHost({
      dbPath: fixture.dbPath,
      sources: [fixture.sources[0]!],
      ownerLabel: 'fact-family-replay-host-test',
    });
    hosts.push(host);
    assert.equal(
      (host.clientInfo.methods as readonly string[]).includes('replayFactFamily'),
      false,
      'the transport-neutral client must remain read-only',
    );
    assert.equal(
      (host.clientInfo.methods as readonly string[]).includes('selectRuntimeUsageQuery'),
      false,
      'query selection must remain an owner-only mutation',
    );

    const projects = await host.client.listCatalogProjects({ limit: 10 });
    const projectId = projects.projects[0]?.projectId;
    assert.ok(projectId);
    const sessions = await host.client.listCatalogSessions({ projectId, limit: 10 });
    const sessionId = sessions.sessions[0]?.sessionId;
    assert.ok(sessionId);
    const coverage = await host.client.getFactFamilyCoverage({
      projectId,
      sessionId,
      ownerId: 'runtime.usage-v2',
      family: 'runtime.usage-v2',
      familyVersion: 1,
      limit: 1,
    });
    assert.equal(coverage.status, 'materialized');
    assert.ok(coverage.coverage);

    await assert.rejects(
      host.replayFactFamily({
        adapterId: 'codex',
        projectId,
        sessionId,
        ownerId: 'runtime.usage-v2',
        family: 'runtime.usage-v2',
        familyVersion: 1,
        expectedSourceInstanceRef: coverage.coverage.sourceInstanceRef,
        expectedContentDigestRef: coverage.coverage.contentDigestRef,
        expectedCoverageLastCommitSeq: coverage.coverage.lastCommitSeq,
        reason: 'unconfigured adapter must fail',
      }),
      /not configured/i,
    );
    const request = {
      adapterId: 'claude-code',
      projectId,
      sessionId,
      ownerId: 'runtime.usage-v2',
      family: 'runtime.usage-v2',
      familyVersion: 1,
      expectedSourceInstanceRef: coverage.coverage.sourceInstanceRef,
      expectedContentDigestRef: coverage.coverage.contentDigestRef,
      expectedCoverageLastCommitSeq: coverage.coverage.lastCommitSeq,
      reason: 'host explicit replay contract test',
    };
    const replay = await host.replayFactFamily(request);
    assert.equal(replay.contractVersion, 1);
    assert.equal(replay.authorizedCoverageLastCommitSeq, request.expectedCoverageLastCommitSeq);
    assert.ok(replay.outcome.recordsDecoded > 0);
    await assert.rejects(host.replayFactFamily(request), /authorization is stale/i);

    const usage = await host.client.getRuntimeUsageV2({ projectId, sessionId, limit: 1 });
    const promoted = await host.selectRuntimeUsageQuery({
      projectId,
      sessionId,
      targetQueryId: 'runtime.usage-v2',
      expectedMaterialized: usage.querySelection.materialized,
      expectedSelectedQueryId: usage.querySelection.selected.queryId,
      expectedSelectedContractVersion: usage.querySelection.selected.contractVersion,
      expectedSelectionEpoch: usage.querySelection.selectionEpoch,
      reason: 'observation host usage-v2 promotion smoke test',
    });
    assert.equal(promoted.selection.selected.queryId, 'runtime.usage-v2');
    assert.equal(
      (await host.client.getRuntimeUsageV2({ projectId, sessionId, limit: 1 })).projectionStatus,
      'selected',
    );
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

    const lateProgress: Array<{ message: string }> = [];
    const stopLateProgress = service.onProgress((progress) => lateProgress.push(progress));
    await new Promise((resolve) => setImmediate(resolve));
    stopLateProgress();

    assert.equal(service.isReady(), true);
    assert.equal(lateProgress.at(-1)?.message, 'Rust observation service is ready.');
    assert.deepEqual(await service.getSourceIds(), ['claude-code', 'codex', 'grok']);
    const projects = await service.getProjectList();
    assert.deepEqual([...new Set(projects.flatMap((project) => project.sourceIds))].sort(), [
      'claude-code',
      'codex',
      'grok',
    ]);
    for (const project of projects) {
      if (!project.absolutePath) continue;
      assert.equal(project.folderName, path.basename(project.absolutePath));
      assert.ok(!project.folderName.startsWith('-tmp-'), `${project.folderName} is a display name, not an encoded cwd`);
    }

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
    const timelineSession = [...codexSessions].sort((a, b) => b.messageCount - a.messageCount)[0]!;
    const firstTimelinePage = await service.getSessionTimeline(timelineSession.projectSlug, timelineSession.sessionId, {
      sourceId: 'codex',
      limit: 1,
    });
    assert.ok(firstTimelinePage.messages.length > 0);
    assert.ok(firstTimelinePage.facets);
    assert.equal(firstTimelinePage.facets.total, firstTimelinePage.total);
    assert.equal(typeof firstTimelinePage.nextCursor, 'string', 'production timelines use Rust keyset cursors');
    const secondTimelinePage = await service.getSessionTimeline(
      timelineSession.projectSlug,
      timelineSession.sessionId,
      { sourceId: 'codex', limit: 1, before: firstTimelinePage.nextCursor },
    );
    assert.ok(secondTimelinePage.messages.length > 0);
    assert.notEqual(secondTimelinePage.messages[0]?.timelineId, firstTimelinePage.messages[0]?.timelineId);
    const facets = await service.getSessionTimelineFacets(timelineSession.projectSlug, timelineSession.sessionId, {
      sourceId: 'codex',
    });
    assert.ok(facets.total >= firstTimelinePage.total);

    const activity = await service.getProjectTokenActivity(codexProject, {
      sourceId: 'codex',
      from: '2025-08-14',
      to: '2026-08-14',
    });
    assert.ok(activity.days.length > 0, 'the maximum RFC activity window remains queryable');

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

/**
 * The catalog must not disturb what existing callers already read.
 *
 * `listProjects` / `listSessions` are the RFC 011 history queries the
 * playground and CLI have always used. The baseline below was captured
 * through the public client at `3db39a7` — before any catalog code existed —
 * on this same committed fixture corpus, so this compares against a genuine
 * "before" rather than against the implementation it is meant to police.
 *
 * Identity fields (`projectId`, `sessionId`, `sourceInstanceId`) derive from
 * the source root, which is a fresh temp directory on every run, so they are
 * normalized to their stable native keys. Everything else — field set, values,
 * and row order — is compared exactly.
 */
describe('history query contract survives the catalog', { skip: !native }, () => {
  test('listProjects and listSessions return their pre-catalog fields unchanged', async () => {
    const fixture = multiAdapterFixture();
    const host = await openObservationHost({
      ...fixture,
      queryWorkers: 1,
      ownerLabel: 'history-contract-test',
    });
    hosts.push(host);
    await host.whenObserving();

    const baseline = JSON.parse(
      readFileSync(new URL('./fixtures/history-contract-baseline.json', import.meta.url), 'utf8'),
    ) as HistoryBaseline;

    const projects = await host.client.listProjects({ limit: 200 });
    assert.equal(projects.contractVersion, baseline.projectsContractVersion);
    assert.deepEqual(
      projects.items.map((row) => normalizeProject(row as unknown as Record<string, unknown>)),
      baseline.projects,
      'every listProjects field, value, and row position must be what it was before the catalog',
    );

    for (const project of projects.items) {
      const key = projectKey(project);
      const expected = baseline.sessionsByProject[key];
      assert.ok(expected, `baseline is missing ${key}`);
      const page = await host.client.listSessions({ projectId: project.projectId, limit: 200 });
      assert.equal(page.contractVersion, expected.contractVersion);
      const rows = page.items.map((row) =>
        normalizeSession(row as unknown as Record<string, unknown>, project.adapterId),
      );
      assert.deepEqual(
        rows.map(withoutFirstPrompt),
        expected.items.map(withoutFirstPrompt),
        `every listSessions field, value, and row position for ${key} must be unchanged`,
      );
      // `firstPrompt` is compared only when present. Its population races in
      // the RFC 011 decode path — measured at 31 or 32 of 34 fixture sessions
      // across repeated runs of the pre-catalog tree — so pinning it exactly
      // would assert a pre-existing bug rather than this lane's behavior.
      rows.forEach((row, index) => {
        const before = expected.items[index]?.firstPrompt;
        if (row.firstPrompt !== undefined && before !== undefined) {
          assert.equal(row.firstPrompt, before, `firstPrompt changed for ${key}`);
        }
      });
    }

    // `lastCommitSeq` keeps its contract even though its value moved: a
    // committed row is never ahead of the page's watermark.
    for (const project of projects.items) {
      assert.ok(project.lastCommitSeq > 0);
      assert.ok(project.lastCommitSeq <= projects.atCommitSeq);
    }

    // The other half of "additive": the new fields are actually populated.
    for (const project of projects.items) {
      assert.ok(project.externalRef, 'a decoded project carries its external reference');
      assert.ok(
        ['transcript_backed', 'hydrated', 'searchable'].includes(project.catalogState ?? ''),
        `a decoded project reports a decoded catalog state, got ${project.catalogState}`,
      );
    }
  });
});

/** Drop the one field the history path does not populate deterministically. */
function withoutFirstPrompt(row: Record<string, unknown>): Record<string, unknown> {
  const { firstPrompt: _prompt, ...rest } = row;
  return rest;
}

/** Remove `lastCommitSeq` wherever it appears, including on nested index rows. */
function stripWatermarks(value: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, item] of Object.entries(value)) {
    if (key === 'lastCommitSeq') continue;
    out[key] =
      item && typeof item === 'object' && !Array.isArray(item)
        ? stripWatermarks(item as Record<string, unknown>)
        : item;
  }
  return out;
}

interface HistoryBaseline {
  projectsContractVersion: number;
  projects: Array<Record<string, unknown>>;
  sessionsByProject: Record<string, { contractVersion: number; items: Array<Record<string, unknown>> }>;
}

function projectKey(project: { adapterId: string; nativeProjectKey: string }): string {
  return `#project:${project.adapterId}:${project.nativeProjectKey}`;
}

/**
 * Drop the fields the catalog added, pin run-varying identity, and drop
 * `lastCommitSeq` — a watermark whose absolute value moves whenever the
 * number of commits changes. Its real contract is asserted separately.
 */
function normalizeProject(row: Record<string, unknown>): Record<string, unknown> {
  const { externalRef: _ref, catalogState: _state, ...rest } = stripWatermarks(row);
  return {
    ...rest,
    projectId: `#project:${String(row.adapterId)}:${String(row.nativeProjectKey)}`,
    sourceInstanceId: `#source:${String(row.adapterId)}`,
  };
}

function normalizeSession(row: Record<string, unknown>, adapterId: string): Record<string, unknown> {
  const { externalRef: _ref, catalogState: _state, ...rest } = stripWatermarks(row);
  return {
    ...rest,
    sessionId: `#session:${String(row.nativeSessionId)}`,
    projectId: `#project:${adapterId}:${String(row.nativeProjectKey)}`,
  };
}
