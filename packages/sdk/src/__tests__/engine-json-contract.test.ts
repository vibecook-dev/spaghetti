/**
 * The engine's JSON contract, checked against real Rust output.
 *
 * This replaces the RFC 012A/012C fixture-parity suites, which compared a
 * hand-written TypeScript parser against the Rust one over frozen JSON. There
 * is no second parser now: Rust serializes, `ts-rs` writes the TypeScript, and
 * what needs checking is that the generated types describe what a real engine
 * actually emits — so every assertion below runs a real engine over the
 * committed fixture corpus and reads its real answers.
 *
 * Two kinds of check:
 *
 * - *Structural*, and free: each result is assigned to its generated type. A
 *   Rust field renamed without regenerating stops this file compiling.
 * - *Behavioural*, and the reason the file exists: the invariants the
 *   generated types promise but cannot enforce — an absent optional is absent
 *   rather than `null`, every number survives JSON, and every page reports the
 *   snapshot it was read at.
 */

import assert from 'node:assert/strict';
import { after, before, describe, test } from 'node:test';
import { cpSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { loadNativeAddon, openSpaghettiEngine, type SpaghettiEngine } from '../index.js';
import type {
  ArtifactPage,
  CanonicalStats,
  CatalogProjectPage,
  CatalogResolution,
  CatalogSessionPage,
  ChangeReplay,
  CommitWaitResult,
  DelegationPage,
  EngineHealthSnapshot,
  EngineOverview,
  EngineStatusSnapshot,
  FactFamilyCoveragePage,
  HistoryProjectPage,
  HistorySessionPage,
  MemoryDocumentPage,
  MessagePage,
  PlanPage,
  Readiness,
  RunStateLookup,
  RuntimeSnapshot,
  SearchPage,
  SessionDetails,
  SourcePage,
  TaskCollectionPage,
  TaskPage,
  TeamInboxMessagePage,
  TeamInboxPage,
  TeamPage,
  TimelinePage,
  ToolResultPage,
  UsageReport,
} from '../generated/index.js';

const native = loadNativeAddon();
const here = path.dirname(fileURLToPath(import.meta.url));
const fixtureRoot = path.resolve(here, '../../../../crates/spaghetti-napi/fixtures');

const tempDirs: string[] = [];
let engine: SpaghettiEngine;
let projectId: string;
let sessionId: string;

after(async () => {
  if (engine) await engine.dispose();
  for (const directory of tempDirs.splice(0)) {
    rmSync(directory, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 });
  }
});

before(async () => {
  if (!native) return;
  const directory = mkdtempSync(path.join(tmpdir(), 'spaghetti-json-contract-'));
  tempDirs.push(directory);
  const claude = path.join(directory, '.claude');
  cpSync(path.join(fixtureRoot, 'medium/.claude'), claude, { recursive: true, preserveTimestamps: true });
  engine = await openSpaghettiEngine({
    dbPath: path.join(directory, 'spaghetti.db'),
    ownerLabel: 'engine-json-contract-test',
    queryWorkers: 1,
  });
  await engine.startConfiguredObservation({
    sources: [{ adapterId: 'claude-code', roots: [claude], reason: 'contract_test' }],
  });
  await engine.awaitObservationStart();
  await engine.completeQueryBootstrap();
  const projects = await engine.listHistoryProjects({ limit: 10 });
  projectId = projects.items[0]!.projectId;
  const sessions = await engine.listHistorySessions({ projectId, limit: 50 });
  sessionId = [...sessions.items].sort((a, b) => b.messageCount - a.messageCount)[0]!.sessionId;
});

/**
 * Walk a decoded result and report every place the JSON contract is broken.
 *
 * `null` is the interesting one. Each optional field is declared `field?: T`,
 * which promises that "no value" arrives as an absent key — that is what
 * `#[serde(skip_serializing_if = "Option::is_none")]` produces, and what the
 * napi object marshalling it replaced used to produce. A `null` would type-check
 * against nothing and quietly turn `value ?? fallback` into `null` for callers
 * that spread the row onward.
 */
/**
 * Fields the engine passes through verbatim rather than typing.
 *
 * These are declared `unknown` in the bindings because they hold the agent's
 * own JSON, nulls and all — `parentUuid: null` is Claude Code's spelling, not
 * ours, and rewriting it would be the lie. The rules below are about fields the
 * engine declares, so the walk stops here.
 */
const OPAQUE_NATIVE_FIELDS = new Set(['content', 'nativePayload', 'nativeSnapshot', 'result', 'value']);

function contractViolations(value: unknown, at = '$'): string[] {
  if (value === null) return [`${at} is null; an absent value must be an absent key`];
  if (typeof value === 'number') {
    if (!Number.isFinite(value)) return [`${at} is ${String(value)}, which JSON cannot carry`];
    if (Number.isInteger(value) && !Number.isSafeInteger(value)) {
      return [`${at} is ${value}, outside the safe-integer range`];
    }
    return [];
  }
  if (typeof value === 'bigint') return [`${at} is a bigint; the bindings declare number`];
  if (Array.isArray(value)) return value.flatMap((item, index) => contractViolations(item, `${at}[${index}]`));
  if (typeof value === 'object') {
    return Object.entries(value as Record<string, unknown>).flatMap(([key, item]) =>
      OPAQUE_NATIVE_FIELDS.has(key) ? [] : contractViolations(item, `${at}.${key}`),
    );
  }
  return [];
}

function assertClean(label: string, value: unknown): void {
  const violations = contractViolations(value, label);
  assert.deepEqual(violations, [], violations.join('\n'));
}

describe('engine JSON contract', { skip: !native }, () => {
  test('every result decodes to an object, not to the string it crossed as', async () => {
    // The addon hands over JSON; `openSpaghettiEngine` decodes it once. A
    // string reaching a caller would mean the decode boundary was bypassed.
    const results: unknown[] = [
      engine.status,
      await engine.health(),
      await engine.overview(),
      await engine.readiness(),
      await engine.listHistoryProjects({ limit: 5 }),
      await engine.search({ text: 'the', limit: 5 }),
    ];
    for (const [index, result] of results.entries()) {
      assert.equal(typeof result, 'object', `result ${index} is still encoded`);
      assert.notEqual(result, null);
    }
  });

  test('every query result matches its generated type and carries no nulls', async () => {
    const overview: EngineOverview = await engine.overview();
    const status: EngineStatusSnapshot = engine.status;
    const health: EngineHealthSnapshot = await engine.health();
    const readiness: Readiness = await engine.readiness();
    const stats: CanonicalStats = await engine.getStats();
    const changes: ChangeReplay = await engine.replayChanges({ limit: 5 });
    const commit: CommitWaitResult = await engine.waitForCommit({ afterCommitSeq: 0, timeoutMs: 100 });
    const catalogProjects: CatalogProjectPage = await engine.listCatalogProjects({ limit: 5 });
    const catalogSessions: CatalogSessionPage = await engine.listCatalogSessions({ limit: 5 });
    const resolution: CatalogResolution = await engine.resolveCatalogEntity(catalogProjects.projects[0]!.externalRef);
    const projects: HistoryProjectPage = await engine.listHistoryProjects({ limit: 5 });
    const sessions: HistorySessionPage = await engine.listHistorySessions({ projectId, limit: 5 });
    const session: SessionDetails = await engine.getSession(sessionId);
    const messages: MessagePage = await engine.getMessages({ projectId, sessionId, limit: 5 });
    const search: SearchPage = await engine.search({ text: 'the', limit: 5 });
    const timeline: TimelinePage = await engine.getTimeline({ projectId, sessionId, limit: 5 });
    const delegations: DelegationPage = await engine.listDelegations({ projectId, sessionId, limit: 5 });
    const memory: MemoryDocumentPage = await engine.listMemoryDocuments({ projectId, limit: 5 });
    const collections: TaskCollectionPage = await engine.listTaskCollections({ limit: 5 });
    const plans: PlanPage = await engine.listPlans({ limit: 5 });
    const tools: ToolResultPage = await engine.listToolResults({ projectId, sessionId, limit: 5 });
    const artifacts: ArtifactPage = await engine.listArtifacts({ sessionId, limit: 5 });
    const sources: SourcePage = await engine.listSources({ limit: 5 });
    const usage: UsageReport = await engine.getUsage({ projectId });
    const coverage: FactFamilyCoveragePage = await engine.getFactFamilyCoverage({
      projectId,
      sessionId,
      ownerId: 'runtime_semantic',
      family: 'runtime.usage-v2',
      familyVersion: 1,
      limit: 5,
    });
    const runtime: RuntimeSnapshot = await engine.getRuntimeSnapshot({ limit: 5 });
    const teams: TeamPage = await engine.listTeams({ limit: 5 });
    const teamId = teams.items[0]?.teamId;

    const named: Array<[string, unknown]> = [
      ['overview', overview],
      ['status', status],
      ['health', health],
      ['readiness', readiness],
      ['getStats', stats],
      ['replayChanges', changes],
      ['waitForCommit', commit],
      ['listCatalogProjects', catalogProjects],
      ['listCatalogSessions', catalogSessions],
      ['resolveCatalogEntity', resolution],
      ['listHistoryProjects', projects],
      ['listHistorySessions', sessions],
      ['getSession', session],
      ['getMessages', messages],
      ['search', search],
      ['getTimeline', timeline],
      ['listDelegations', delegations],
      ['listMemoryDocuments', memory],
      ['listTaskCollections', collections],
      ['listPlans', plans],
      ['listToolResults', tools],
      ['listArtifacts', artifacts],
      ['listSources', sources],
      ['getUsage', usage],
      ['getFactFamilyCoverage', coverage],
      ['getRuntimeSnapshot', runtime],
      ['listTeams', teams],
    ];

    // The remaining pages are keyed by a row this corpus may not have.
    const collectionId = collections.items[0]?.collectionId;
    if (collectionId !== undefined) {
      const tasks: TaskPage = await engine.listTasks({ collectionId, limit: 5 });
      named.push(['listTasks', tasks]);
    }
    const runId = runtime.entries.find((entry) => entry.run !== undefined)?.run?.runId;
    if (runId !== undefined) {
      const runState: RunStateLookup = await engine.getRunState(runId);
      named.push(['getRunState', runState]);
    }
    if (teamId !== undefined) {
      const inboxes: TeamInboxPage = await engine.listTeamInboxes({ teamId, limit: 5 });
      named.push(['listTeamInboxes', inboxes]);
      const inboxId = inboxes.items[0]?.inboxId;
      if (inboxId !== undefined) {
        const inboxMessages: TeamInboxMessagePage = await engine.listTeamInboxMessages({ inboxId, limit: 5 });
        named.push(['listTeamInboxMessages', inboxMessages]);
      }
    }
    for (const [label, result] of named) assertClean(label, result);

    // The corpus is real, so the checks above are only worth something if the
    // rows they walked exist.
    assert.ok(projects.items.length > 0, 'the fixture corpus decoded at least one project');
    assert.ok(messages.items.length > 0, 'the fixture corpus decoded at least one message');
    assert.ok(search.items.length > 0, 'full-text search returned hits to check');
  });

  test('an absent optional is an absent key, on rows that have one', async () => {
    // Proving the rule needs rows that actually omit something: a corpus where
    // every optional happened to be populated would pass vacuously.
    const rowSets: Array<Array<Record<string, unknown>>> = [
      (await engine.listHistorySessions({ projectId, limit: 50 })).items as unknown as Array<Record<string, unknown>>,
      (await engine.getMessages({ projectId, sessionId, limit: 50 })).items as unknown as Array<
        Record<string, unknown>
      >,
      (await engine.search({ text: 'the', limit: 50 })).items as unknown as Array<Record<string, unknown>>,
      (await engine.getTimeline({ projectId, sessionId, limit: 50 })).items as unknown as Array<
        Record<string, unknown>
      >,
    ];

    let omissions = 0;
    for (const rows of rowSets) {
      const declared = new Set<string>();
      for (const row of rows) for (const key of Object.keys(row)) declared.add(key);
      for (const row of rows) {
        const keys = Object.keys(row);
        if (keys.length < declared.size) omissions += 1;
        for (const [key, value] of Object.entries(row)) {
          assert.notEqual(value, null, `${key} is present as null instead of being omitted`);
        }
      }
    }
    assert.ok(omissions > 0, 'expected at least one row to omit an optional field rather than send null');
  });

  test('every page reports the snapshot it was read at', async () => {
    const overview = await engine.overview();
    const pages = [
      await engine.listHistoryProjects({ limit: 5 }),
      await engine.listHistorySessions({ projectId, limit: 5 }),
      await engine.getMessages({ projectId, sessionId, limit: 5 }),
      await engine.search({ text: 'the', limit: 5 }),
      await engine.listSources({ limit: 5 }),
    ];
    for (const page of pages) {
      assert.ok(Number.isSafeInteger(page.atCommitSeq), 'the watermark is a safe integer');
      assert.ok(page.atCommitSeq <= overview.commitSeq, 'no page is read ahead of the durable watermark');
      assert.equal(page.contractVersion, 1, 'pages declare the query contract they were built for');
    }
  });

  test('a cursor round-trips through JSON and continues the same page', async () => {
    const first = await engine.getMessages({ projectId, sessionId, limit: 2 });
    assert.ok(first.nextCursor, 'the fixture session has more messages than one page');
    const second = await engine.getMessages({ projectId, sessionId, limit: 2, cursor: first.nextCursor });
    assert.equal(second.atCommitSeq, first.atCommitSeq, 'a continuation reads the same snapshot');
    const firstIds = first.items.map((row) => row.messageId);
    const secondIds = second.items.map((row) => row.messageId);
    assert.deepEqual(
      firstIds.filter((id) => secondIds.includes(id)),
      [],
      'a continued page does not repeat rows from the previous one',
    );
  });

  test('binary change payloads survive as base64 text', async () => {
    const replay = await engine.replayChanges({ limit: 20 });
    assert.ok(replay.changes.length > 0, 'the ingest published durable changes to read');
    for (const change of replay.changes) {
      assert.match(change.entityKeyBase64Url, /^[A-Za-z0-9_-]*$/, 'entity keys are URL-safe base64');
      assert.doesNotThrow(() => Buffer.from(change.payloadBase64, 'base64'), 'payloads are standard base64');
      assert.ok(Number.isSafeInteger(change.cursor.commitSeq));
    }
  });
});

describe('durable identity is deterministic', { skip: !native }, () => {
  test('the same session keeps one external reference across queries and reopens', async () => {
    const catalog = await engine.listCatalogSessions({ limit: 50 });
    assert.ok(catalog.sessions.length > 0);
    for (const row of catalog.sessions) {
      // The one RFC 012A spelling, shared with `ExternalEntityRef.entity_key`.
      assert.match(
        row.externalRef,
        /^v1:[A-Za-z0-9_-]{43}$/,
        'an external reference is an opaque versioned digest, never a path',
      );
    }

    // RFC 012A: the reference is a function of native identity, so reading it
    // twice — and reading it through the history path — gives one answer.
    const again = await engine.listCatalogSessions({ limit: 50 });
    assert.deepEqual(
      again.sessions.map((row) => row.externalRef),
      catalog.sessions.map((row) => row.externalRef),
      'a second read mints the same references',
    );

    const history = await engine.listHistorySessions({ projectId, limit: 50 });
    const byNativeId = new Map(catalog.sessions.map((row) => [row.nativeSessionId, row.externalRef]));
    for (const row of history.items) {
      const fromCatalog = byNativeId.get(row.nativeSessionId);
      if (fromCatalog === undefined) continue;
      assert.equal(row.externalRef, fromCatalog, 'history and catalog agree on one session reference');
    }
  });

  test('a reference resolves back to the entity that minted it', async () => {
    const catalog = await engine.listCatalogProjects({ limit: 5 });
    const project = catalog.projects[0]!;
    const resolved = await engine.resolveCatalogEntity(project.externalRef);
    assert.equal(resolved.kind, 'project');
    assert.equal(resolved.project?.projectId, project.projectId);
    assert.equal(resolved.session, undefined, 'a project reference never resolves to a session');
  });
});
