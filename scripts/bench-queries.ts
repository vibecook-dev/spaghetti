#!/usr/bin/env -S tsx
/**
 * Conformance and end-to-end query benchmark for RFC 011 Phase 9.
 *
 * The harness always copies its input corpus into a unique temporary
 * directory. Both databases and the live-refresh mutation therefore stay
 * isolated from checked-in fixtures and real agent data.
 *
 * Usage:
 *   pnpm bench:queries
 *   pnpm bench:queries --fixture crates/spaghetti-napi/fixtures/medium/.claude
 *   pnpm bench:queries --runs 30 --warmup 5 --query-workers 4
 *   pnpm bench:queries --payload-mib 0 # disable the scratch-only 12 MiB boundary document
 *   pnpm bench:queries --mode conformance
 *   pnpm bench:queries --report-json /tmp/query-benchmark.json
 */

import assert from 'node:assert/strict';
import {
  appendFileSync,
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { arch, cpus, platform, tmpdir, totalmem } from 'node:os';
import path from 'node:path';
import { monitorEventLoopDelay, performance } from 'node:perf_hooks';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

import {
  loadNativeAddon,
  type SpaghettiEngineHistoryProject,
  type SpaghettiEngineHistorySession,
  type SpaghettiEngineRuntimeEntry,
} from '../packages/sdk/src/index.js';
import {
  compareClaudeObservationHistoryQueries,
  compareClaudeObservationUsage,
  createSpaghettiService,
  openClaudeObservationShadow,
  type ClaudeObservationShadow,
  type LegacySpaghettiAPI,
} from '../packages/sdk/src/legacy-oracle.js';

type Mode = 'all' | 'conformance';
type LegacyProject = ReturnType<LegacySpaghettiAPI['getProjectList']>[number];
type LegacySession = ReturnType<LegacySpaghettiAPI['getSessionList']>[number];

interface LegacySessionOracle {
  project: LegacyProject;
  session: LegacySession;
  parentMessageCount: number;
  subagentMessageCount: number;
}

interface PayloadObservation {
  query: string;
  bytes: number;
  limit: number;
  utilization: number;
}

interface ConformanceReport {
  exact: true;
  atCommitSeq: number;
  schemaVersion: number;
  projects: number;
  sessions: number;
  messages: number;
  searchText: string;
  searchHits: number;
  checks: string[];
  capabilities: Record<string, number>;
  payloads: PayloadObservation[];
  acceptedDifferences: string[];
}

interface Distribution {
  min: number;
  p50: number;
  p95: number;
  p99: number;
  max: number;
  mean: number;
}

interface QueryMeasurement {
  endToEndMs: Distribution;
  responseBytes: number;
  heapDeltaBytes: number;
  rssDeltaBytes: number;
  eventLoopDelayMs: {
    mean: number | null;
    p95: number | null;
    p99: number | null;
    max: number | null;
  };
  apiCallsPerLogicalRequest: number;
}

interface BenchmarkRow {
  workload: string;
  legacy: QueryMeasurement;
  rustNapi: QueryMeasurement;
}

interface CancellationReport {
  requests: number;
  rejected: number;
  fulfilled: number;
  elapsedMs: number;
  recoverySearchHits: number;
}

interface ConcurrentRefreshReport {
  readers: number;
  baselineReaderMs: Distribution;
  concurrentReaderMs: Distribution;
  p95Ratio: number | null;
  refreshMs: number;
  beforeCommitSeq: number;
  afterCommitSeq: number;
  markerHits: number;
  walBytes: number;
}

interface BenchmarkReport {
  rows: BenchmarkRow[];
  cancellation: CancellationReport;
  concurrentRefresh: ConcurrentRefreshReport;
}

interface CorpusStats {
  files: number;
  jsonlFiles: number;
  bytes: number;
}

interface PayloadBoundaryFixture {
  nativeProjectKey: string;
  documents: number;
  bytes: number;
}

interface CompleteSurfaceFixture {
  nativeProjectKey: string;
  nativeSessionId: string;
  nativePid: number;
  nativeTeamId: string;
  nativeRecipientName: string;
  inboxMessages: number;
}

interface HarnessContext {
  legacy: LegacySpaghettiAPI;
  shadow: ClaudeObservationShadow;
  workRoot: string;
  shadowDbPath: string;
  canonicalProjects: SpaghettiEngineHistoryProject[];
  canonicalSessions: SpaghettiEngineHistorySession[];
  legacyProjects: LegacyProject[];
  legacySessions: LegacySessionOracle[];
  primaryProject: SpaghettiEngineHistoryProject;
  primarySession: SpaghettiEngineHistorySession;
  messageProject: SpaghettiEngineHistoryProject;
  messageSession: SpaghettiEngineHistorySession;
  messageLegacy: LegacySessionOracle;
  searchText: string;
  usageFrom: string;
  usageTo: string;
  atCommitSeq: number;
  payloadBoundary?: PayloadBoundaryFixture;
  completeSurface: CompleteSurfaceFixture;
  workflowScope?: {
    project: SpaghettiEngineHistoryProject;
    session: SpaghettiEngineHistorySession;
    legacy: LegacySessionOracle;
  };
  delegationScope?: {
    project: SpaghettiEngineHistoryProject;
    session: SpaghettiEngineHistorySession;
    legacy: LegacySessionOracle;
  };
  memoryScope?: {
    project: SpaghettiEngineHistoryProject;
    legacy: LegacyProject;
  };
}

interface QueryCase {
  workload: string;
  legacyCalls: number;
  rustCalls: number;
  legacy: () => unknown | Promise<unknown>;
  rust: () => unknown | Promise<unknown>;
}

const { values } = parseArgs({
  options: {
    fixture: { type: 'string' },
    runs: { type: 'string', default: '20' },
    warmup: { type: 'string', default: '3' },
    'query-workers': { type: 'string', default: '4' },
    readers: { type: 'string', default: '10' },
    'cancel-burst': { type: 'string', default: '100' },
    'payload-mib': { type: 'string', default: '12' },
    search: { type: 'string' },
    mode: { type: 'string', default: 'all' },
    'report-json': { type: 'string' },
    'keep-workdir': { type: 'boolean', default: false },
  },
  strict: true,
});

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const defaultFixture = path.join(repoRoot, 'crates/spaghetti-napi/fixtures/small/.claude');
const fixtureRoot = path.resolve(expandTilde(values.fixture ?? defaultFixture));
const runs = positiveInteger(values.runs!, 'runs');
const warmup = nonNegativeInteger(values.warmup!, 'warmup');
const queryWorkers = boundedInteger(values['query-workers']!, 'query-workers', 1, 16);
const readers = positiveInteger(values.readers!, 'readers');
const cancelBurst = positiveInteger(values['cancel-burst']!, 'cancel-burst');
const payloadMiB = boundedInteger(values['payload-mib']!, 'payload-mib', 0, 15);
const mode = values.mode as Mode;
const requestedSearch = values.search?.trim();
const reportJsonPath = values['report-json'] ? path.resolve(expandTilde(values['report-json'])) : undefined;
const keepWorkdir = values['keep-workdir'] ?? false;

if (!['all', 'conformance'].includes(mode)) failArgs(`--mode must be all|conformance, got ${mode}`);
if (!existsSync(fixtureRoot) || !statSync(fixtureRoot).isDirectory()) {
  failArgs(`fixture directory does not exist: ${fixtureRoot}`);
}
if (requestedSearch !== undefined && requestedSearch.length === 0) failArgs('--search must not be blank');

function failArgs(message: string): never {
  console.error(message);
  process.exit(2);
}

function positiveInteger(raw: string, name: string): number {
  return boundedInteger(raw, name, 1, Number.MAX_SAFE_INTEGER);
}

function nonNegativeInteger(raw: string, name: string): number {
  return boundedInteger(raw, name, 0, Number.MAX_SAFE_INTEGER);
}

function boundedInteger(raw: string, name: string, min: number, max: number): number {
  const parsed = Number(raw);
  if (!Number.isSafeInteger(parsed) || parsed < min || parsed > max) {
    failArgs(`--${name} must be an integer from ${min} through ${max}, got ${raw}`);
  }
  return parsed;
}

function expandTilde(candidate: string): string {
  if (candidate === '~') return process.env.HOME ?? candidate;
  if (candidate.startsWith('~/')) return path.join(process.env.HOME ?? '~', candidate.slice(2));
  return candidate;
}

function round(value: number): number {
  return Number(value.toFixed(3));
}

function encodedBytes(value: unknown): number {
  const json = JSON.stringify(value);
  return json === undefined ? 0 : Buffer.byteLength(json);
}

function stableJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(',')}]`;
  if (value !== null && typeof value === 'object') {
    return `{${Object.entries(value)
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([key, entry]) => `${JSON.stringify(key)}:${stableJson(entry)}`)
      .join(',')}}`;
  }
  return JSON.stringify(value) ?? 'undefined';
}

function percentile(sorted: readonly number[], fraction: number): number {
  assert.ok(sorted.length > 0, 'cannot calculate a percentile over no samples');
  return sorted[Math.min(sorted.length - 1, Math.max(0, Math.ceil(sorted.length * fraction) - 1))]!;
}

function distribution(samples: readonly number[]): Distribution {
  const sorted = [...samples].sort((left, right) => left - right);
  assert.ok(sorted.length > 0, 'benchmark produced no samples');
  return {
    min: round(sorted[0]!),
    p50: round(percentile(sorted, 0.5)),
    p95: round(percentile(sorted, 0.95)),
    p99: round(percentile(sorted, 0.99)),
    max: round(sorted[sorted.length - 1]!),
    mean: round(sorted.reduce((total, value) => total + value, 0) / sorted.length),
  };
}

function finiteDelay(value: number): number | null {
  return Number.isFinite(value) ? round(value / 1_000_000) : null;
}

function immediate(): Promise<void> {
  return new Promise((resolve) => setImmediate(resolve));
}

function corpusStats(root: string): CorpusStats {
  const result: CorpusStats = { files: 0, jsonlFiles: 0, bytes: 0 };
  const pending = [root];
  while (pending.length > 0) {
    const current = pending.pop()!;
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const absolute = path.join(current, entry.name);
      if (entry.isDirectory()) pending.push(absolute);
      else if (entry.isFile()) {
        result.files += 1;
        result.bytes += statSync(absolute).size;
        if (entry.name.endsWith('.jsonl')) result.jsonlFiles += 1;
      }
    }
  }
  return result;
}

function installPayloadBoundaryFixture(root: string): PayloadBoundaryFixture | undefined {
  if (payloadMiB === 0) return undefined;
  const projectsRoot = path.join(root, 'projects');
  const projects = readdirSync(projectsRoot, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .sort();
  assert.ok(projects.length > 0, 'fixture needs a project for the payload-boundary document');
  const nativeProjectKey =
    projects.find((project) => existsSync(path.join(projectsRoot, project, 'memory', 'MEMORY.md'))) ?? projects[0]!;
  const memoryRoot = path.join(projectsRoot, nativeProjectKey, 'memory');
  mkdirSync(memoryRoot, { recursive: true });
  const documentBytes = 1024 * 1024;
  let totalBytes = 0;
  for (let index = 0; index < payloadMiB; index += 1) {
    const memoryPath = path.join(memoryRoot, `query-payload-boundary-${String(index).padStart(2, '0')}.md`);
    const prefix = `# Query payload boundary ${index}\n\n`;
    const content = `${prefix}${'x'.repeat(documentBytes - Buffer.byteLength(prefix))}`;
    writeFileSync(memoryPath, content);
    const bytes = statSync(memoryPath).size;
    assert.equal(bytes, documentBytes);
    totalBytes += bytes;
  }
  return { nativeProjectKey, documents: payloadMiB, bytes: totalBytes };
}

function installCompleteSurfaceFixture(root: string): CompleteSurfaceFixture {
  const projectsRoot = path.join(root, 'projects');
  const projects = readdirSync(projectsRoot, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .sort();
  const nativeProjectKey = projects.find((project) =>
    readdirSync(path.join(projectsRoot, project)).some((entry) => entry.endsWith('.jsonl')),
  );
  assert.ok(nativeProjectKey, 'fixture needs a transcript-backed project for complete-surface evidence');
  const nativeSessionId = path.basename(
    readdirSync(path.join(projectsRoot, nativeProjectKey))
      .filter((entry) => entry.endsWith('.jsonl'))
      .sort()[0]!,
    '.jsonl',
  );
  const nativePid = 424_242;
  const nativeTeamId = 'query-benchmark-team';
  const nativeRecipientName = 'lead';
  const fixedMillis = Date.parse('2026-04-01T00:00:00.000Z');

  const sessionsRoot = path.join(root, 'sessions');
  mkdirSync(sessionsRoot, { recursive: true });
  writeFileSync(
    path.join(sessionsRoot, `${nativePid}.json`),
    JSON.stringify({
      pid: nativePid,
      sessionId: nativeSessionId,
      cwd: '/tmp/spaghetti-query-benchmark',
      startedAt: fixedMillis,
      kind: 'local',
      entrypoint: 'cli',
      name: 'query-benchmark',
      status: 'working',
      updatedAt: fixedMillis + 1_000,
      statusUpdatedAt: fixedMillis + 1_000,
      procStart: 'query-benchmark-process-start',
      version: '1.0.0',
      peerProtocol: 1,
      nameSource: 'native',
      bridgeSessionId: 'query-benchmark-bridge',
      messagingSocketPath: '/tmp/spaghetti-query-benchmark.sock',
    }),
  );

  const teamRoot = path.join(root, 'teams', nativeTeamId);
  const inboxRoot = path.join(teamRoot, 'inboxes');
  mkdirSync(inboxRoot, { recursive: true });
  writeFileSync(
    path.join(teamRoot, 'config.json'),
    JSON.stringify({
      name: 'Query Benchmark Team',
      description: 'scratch-only complete query surface fixture',
      createdAt: fixedMillis,
      leadAgentId: `lead@${nativeTeamId}`,
      leadSessionId: nativeSessionId,
      members: [
        {
          agentId: `lead@${nativeTeamId}`,
          name: nativeRecipientName,
          agentType: 'team-lead',
          model: 'claude-test',
          prompt: 'coordinate the query benchmark',
          color: 'blue',
          planModeRequired: true,
          joinedAt: fixedMillis,
          tmuxPaneId: 'query-benchmark-pane',
          cwd: '/tmp/spaghetti-query-benchmark',
          subscriptions: ['changes'],
          backendType: 'in-process',
        },
      ],
    }),
  );
  const messages = [
    {
      from: 'worker',
      text: 'query benchmark inbox message one',
      summary: 'first',
      timestamp: '2026-04-01T00:00:01.000Z',
      color: 'green',
      read: false,
      msg_id: 'query-benchmark-message-1',
      msgV: 1,
      type: 'message',
    },
    {
      from: nativeRecipientName,
      text: 'query benchmark inbox message two',
      timestamp: '2026-04-01T00:00:02.000Z',
      read: true,
      msg_id: 'query-benchmark-message-2',
      msgV: 1,
      type: 'message',
    },
  ];
  writeFileSync(path.join(inboxRoot, `${nativeRecipientName}.json`), JSON.stringify(messages));

  return {
    nativeProjectKey,
    nativeSessionId,
    nativePid,
    nativeTeamId,
    nativeRecipientName,
    inboxMessages: messages.length,
  };
}

function assertVersioned(
  value: { contractVersion: number; atCommitSeq: number },
  label: string,
  atCommitSeq: number,
): void {
  assert.equal(value.contractVersion, 1, `${label}: contractVersion`);
  assert.equal(value.atCommitSeq, atCommitSeq, `${label}: atCommitSeq`);
}

function observePayload(
  observations: PayloadObservation[],
  label: string,
  value: { payloadBytes: number; payloadByteLimit: number },
): void {
  assert.ok(value.payloadBytes >= 0, `${label}: payload bytes must be non-negative`);
  assert.ok(value.payloadBytes <= value.payloadByteLimit, `${label}: payload bound exceeded`);
  observations.push({
    query: label,
    bytes: value.payloadBytes,
    limit: value.payloadByteLimit,
    utilization: round(value.payloadByteLimit === 0 ? 0 : value.payloadBytes / value.payloadByteLimit),
  });
}

async function allCanonicalProjects(
  shadow: ClaudeObservationShadow,
  atCommitSeq: number,
): Promise<SpaghettiEngineHistoryProject[]> {
  const items: SpaghettiEngineHistoryProject[] = [];
  let cursor: string | undefined;
  do {
    const page = await shadow.listHistoryProjects({ limit: 200, cursor });
    assertVersioned(page, 'listHistoryProjects', atCommitSeq);
    items.push(...page.items);
    cursor = page.nextCursor;
  } while (cursor);
  return items;
}

async function allCanonicalSessions(
  shadow: ClaudeObservationShadow,
  projects: readonly SpaghettiEngineHistoryProject[],
  atCommitSeq: number,
): Promise<SpaghettiEngineHistorySession[]> {
  const items: SpaghettiEngineHistorySession[] = [];
  for (const project of projects) {
    let cursor: string | undefined;
    do {
      const page = await shadow.listHistorySessions(project.projectId, { limit: 200, cursor });
      assertVersioned(page, 'listHistorySessions', atCommitSeq);
      assert.equal(page.projectId, project.projectId);
      items.push(...page.items);
      cursor = page.nextCursor;
    } while (cursor);
  }
  return items;
}

async function allRuntimeEntries(
  shadow: ClaudeObservationShadow,
  atCommitSeq: number,
): Promise<SpaghettiEngineRuntimeEntry[]> {
  const entries: SpaghettiEngineRuntimeEntry[] = [];
  let cursor: string | undefined;
  do {
    const page = await shadow.getRuntimeSnapshot({ limit: 200, cursor });
    assertVersioned(page, 'getRuntimeSnapshot', atCommitSeq);
    entries.push(...page.entries);
    cursor = page.nextCursor;
  } while (cursor);
  return entries;
}

function canonicalScopeForLegacy(
  canonicalProjects: readonly SpaghettiEngineHistoryProject[],
  canonicalSessions: readonly SpaghettiEngineHistorySession[],
  legacy: LegacySessionOracle,
): { project: SpaghettiEngineHistoryProject; session: SpaghettiEngineHistorySession } {
  const project = canonicalProjects.find((item) => item.nativeProjectKey === legacy.session.projectSlug);
  assert.ok(project, `missing canonical project ${legacy.session.projectSlug}`);
  const session = canonicalSessions.find(
    (item) => item.projectId === project.projectId && item.nativeSessionId === legacy.session.sessionId,
  );
  assert.ok(session, `missing canonical session ${legacy.session.sessionId}`);
  return { project, session };
}

function latestUsageRange(sessions: readonly SpaghettiEngineHistorySession[]): { from: string; to: string } {
  const latest = sessions
    .flatMap((session) => [session.latestActivityAt, session.lastMessageAt, session.firstMessageAt])
    .filter((value): value is string => value !== undefined)
    .sort()
    .at(-1);
  const year = latest?.slice(0, 4);
  assert.match(year ?? '', /^\d{4}$/, 'fixture must expose at least one dated session');
  return { from: `${year}-01-01`, to: `${year}-12-31` };
}

async function prepareContext(
  legacy: LegacySpaghettiAPI,
  shadow: ClaudeObservationShadow,
  workRoot: string,
  shadowDbPath: string,
  payloadBoundary: PayloadBoundaryFixture | undefined,
  completeSurface: CompleteSurfaceFixture,
): Promise<HarnessContext> {
  const snapshot = await shadow.snapshot();
  assert.equal(snapshot.health.healthy, true, snapshot.health.detail);
  assert.equal(snapshot.overview.queryOnly, true);
  assert.equal(snapshot.overview.readOnly, true);
  const atCommitSeq = snapshot.overview.commitSeq;
  const canonicalProjects = await allCanonicalProjects(shadow, atCommitSeq);
  const canonicalSessions = await allCanonicalSessions(shadow, canonicalProjects, atCommitSeq);
  assert.ok(canonicalProjects.length > 0, 'fixture has no canonical projects');
  assert.ok(canonicalSessions.length > 0, 'fixture has no canonical sessions');

  const legacyProjects = legacy.getProjectList({ sourceId: 'claude-code' });
  const legacySessions: LegacySessionOracle[] = legacyProjects.flatMap((project) =>
    legacy.getSessionList(project, { sourceId: 'claude-code' }).map((session) => ({
      project,
      session,
      parentMessageCount: session.messageCount,
      subagentMessageCount: legacy
        .getSessionSubagents(session.projectSlug, session.sessionId, {
          sourceId: 'claude-code',
          includeNested: true,
        })
        .reduce((total, child) => total + child.messageCount, 0),
    })),
  );

  const primaryProject = [...canonicalProjects].sort((left, right) => right.messageCount - left.messageCount)[0]!;
  const primarySession = [...canonicalSessions]
    .filter((session) => session.projectId === primaryProject.projectId)
    .sort((left, right) => right.messageCount - left.messageCount)[0]!;
  const messageLegacy = [...legacySessions]
    .filter((item) => item.parentMessageCount > 0 && item.subagentMessageCount === 0)
    .sort((left, right) => right.parentMessageCount - left.parentMessageCount)[0];
  assert.ok(messageLegacy, 'fixture needs one non-delegated transcript for exact message paging');
  const messageScope = canonicalScopeForLegacy(canonicalProjects, canonicalSessions, messageLegacy);

  const workflowLegacy = legacySessions.find(
    (item) => legacy.getSessionWorkflows(item.session.projectSlug, item.session.sessionId).length > 0,
  );
  const delegationLegacy = legacySessions.find(
    (item) =>
      legacy.getSessionSubagents(item.session.projectSlug, item.session.sessionId, {
        sourceId: 'claude-code',
        includeNested: true,
      }).length > 0,
  );
  const memoryProject =
    legacyProjects.find((project) => project.slug === payloadBoundary?.nativeProjectKey) ??
    legacyProjects.find((project) => project.hasMemory);
  const usageRange = latestUsageRange(canonicalSessions);

  const markerText = 'searchable-wf-marker';
  const markerLegacy = legacy.search({ text: markerText, limit: 1 });
  const markerCanonical = await shadow.search({ text: markerText, limit: 1 });
  assertVersioned(markerCanonical, 'search(marker probe)', atCommitSeq);
  assert.equal(markerCanonical.total, markerLegacy.total, 'marker search total parity');
  const searchText = requestedSearch ?? (markerCanonical.total > 0 ? markerText : 'error handling');

  return {
    legacy,
    shadow,
    workRoot,
    shadowDbPath,
    canonicalProjects,
    canonicalSessions,
    legacyProjects,
    legacySessions,
    primaryProject,
    primarySession,
    messageProject: messageScope.project,
    messageSession: messageScope.session,
    messageLegacy,
    searchText,
    usageFrom: usageRange.from,
    usageTo: usageRange.to,
    atCommitSeq,
    payloadBoundary,
    completeSurface,
    workflowScope: workflowLegacy
      ? { ...canonicalScopeForLegacy(canonicalProjects, canonicalSessions, workflowLegacy), legacy: workflowLegacy }
      : undefined,
    delegationScope: delegationLegacy
      ? { ...canonicalScopeForLegacy(canonicalProjects, canonicalSessions, delegationLegacy), legacy: delegationLegacy }
      : undefined,
    memoryScope: memoryProject
      ? {
          project: canonicalProjects.find((item) => item.nativeProjectKey === memoryProject.slug)!,
          legacy: memoryProject,
        }
      : undefined,
  };
}

async function runConformance(context: HarnessContext): Promise<ConformanceReport> {
  const { legacy, shadow, canonicalProjects, canonicalSessions, legacyProjects, legacySessions, atCommitSeq } = context;
  const checks: string[] = [];
  const payloads: PayloadObservation[] = [];
  const capabilities: Record<string, number> = {};
  const before = await shadow.snapshot();

  const projectParity = compareClaudeObservationHistoryQueries(
    canonicalProjects,
    canonicalSessions,
    legacyProjects.map((project) => {
      const sessions = legacySessions.filter((item) => item.session.projectSlug === project.slug);
      return {
        nativeProjectKey: project.slug,
        sessionCount: project.sessionCount,
        parentMessageCount: project.messageCount,
        subagentMessageCount: sessions.reduce((total, session) => total + session.subagentMessageCount, 0),
        hasMemory: project.hasMemory,
      };
    }),
    legacySessions.map((item) => ({
      nativeProjectKey: item.session.projectSlug,
      nativeSessionId: item.session.sessionId,
      parentMessageCount: item.parentMessageCount,
      subagentMessageCount: item.subagentMessageCount,
    })),
  );
  assert.equal(projectParity.exact, true, JSON.stringify(projectParity));
  const aggregateParity = await shadow.compareHistory({
    sessions: legacySessions.length,
    messages: legacySessions.reduce((total, session) => total + session.parentMessageCount, 0),
    subagentMessages: legacySessions.reduce((total, session) => total + session.subagentMessageCount, 0),
  });
  assert.equal(aggregateParity.exact, true, JSON.stringify(aggregateParity));
  assert.equal(aggregateParity.atCommitSeq, atCommitSeq);
  checks.push('normalized project/session history parity');

  const sessionDetails = new Map<string, Awaited<ReturnType<ClaudeObservationShadow['getSession']>>>();
  for (const session of canonicalSessions) {
    const details = await shadow.getSession(session.sessionId);
    assertVersioned(details, 'getSession', atCommitSeq);
    assert.equal(details.session?.sessionId, session.sessionId);
    sessionDetails.set(session.sessionId, details);
  }
  checks.push('session detail coverage');

  const canonicalMessages = await shadow.getMessages(
    context.messageProject.projectId,
    context.messageSession.sessionId,
    { limit: 200 },
  );
  assertVersioned(canonicalMessages, 'getMessages', atCommitSeq);
  observePayload(payloads, 'getMessages', canonicalMessages);
  const legacyMessages = legacy.getSessionMessages(
    context.messageLegacy.session.projectSlug,
    context.messageLegacy.session.sessionId,
    200,
    0,
    { sourceId: 'claude-code' },
  );
  assert.equal(canonicalMessages.items.length, legacyMessages.messages.length);
  assert.deepEqual(
    canonicalMessages.items.map((message) => stableJson(message.nativePayload)).sort(),
    legacyMessages.messages.map(stableJson).sort(),
  );
  checks.push('lossless root message payload parity');

  const search = await shadow.search({ text: context.searchText, limit: 200 });
  const legacySearch = legacy.search({ text: context.searchText, limit: 200 });
  assertVersioned(search, 'search', atCommitSeq);
  observePayload(payloads, 'search', search);
  assert.equal(search.querySyntax, 'literal_phrase_v1');
  assert.equal(search.totalIsExact, true);
  assert.equal(search.total, legacySearch.total, 'exact FTS hit-count parity');
  assert.ok(search.total > 0, `search text has no hits: ${context.searchText}`);
  checks.push('literal FTS total parity and shared score-domain contract');

  const searchableHit = search.items.find((hit) => hit.projectId !== undefined);
  assert.ok(searchableHit?.projectId, 'search result must resolve a project for timeline conformance');
  const timeline = await shadow.getTimeline({
    projectId: searchableHit.projectId,
    sessionId: searchableHit.sessionId,
    limit: 200,
  });
  assertVersioned(timeline, 'getTimeline', atCommitSeq);
  observePayload(payloads, 'getTimeline', timeline);
  assert.equal(timeline.total, timeline.facets.totalMessages);
  assert.equal(timeline.order, 'newest_first');
  if (timeline.nextCursor) {
    const next = await shadow.getTimeline({
      projectId: searchableHit.projectId,
      sessionId: searchableHit.sessionId,
      limit: 200,
      cursor: timeline.nextCursor,
    });
    assertVersioned(next, 'getTimeline(next)', atCommitSeq);
  }
  checks.push('timeline totals, facets, payload, and cursor contract');

  const delegationScope = context.delegationScope ?? {
    project: context.primaryProject,
    session: context.primarySession,
    legacy: legacySessions.find((item) => item.session.sessionId === context.primarySession.nativeSessionId)!,
  };
  const delegations = await shadow.listDelegations({
    projectId: delegationScope.project.projectId,
    sessionId: delegationScope.session.sessionId,
    limit: 200,
  });
  assertVersioned(delegations, 'listDelegations', atCommitSeq);
  const legacyDelegations = legacy.getSessionSubagents(
    delegationScope.legacy.session.projectSlug,
    delegationScope.legacy.session.sessionId,
    { sourceId: 'claude-code', includeNested: true },
  );
  assert.deepEqual(
    delegations.items.map((item) => item.nativeChildId).sort(),
    legacyDelegations.map((item) => item.agentId).sort(),
  );
  capabilities.delegations = delegations.items.length;
  checks.push('delegation identity parity');

  const workflowScope = context.workflowScope ?? delegationScope;
  const workflows = await shadow.listWorkflows({
    projectId: workflowScope.project.projectId,
    sessionId: workflowScope.session.sessionId,
    limit: 200,
  });
  assertVersioned(workflows, 'listWorkflows', atCommitSeq);
  const legacyWorkflows = legacy.getSessionWorkflows(
    workflowScope.legacy.session.projectSlug,
    workflowScope.legacy.session.sessionId,
  );
  assert.equal(workflows.items.length, legacyWorkflows.length);
  for (const workflow of workflows.items) {
    const legacyWorkflow = legacyWorkflows.find((item) => item.workflowId === workflow.nativeWorkflowId);
    assert.ok(legacyWorkflow, `missing legacy workflow ${workflow.nativeWorkflowId}`);
    assert.deepEqual(
      {
        name: workflow.name,
        status: workflow.nativeStatus,
        agentCount: workflow.agentCount,
        totalTokens: workflow.totalTokens,
        totalToolCalls: workflow.totalToolCalls,
        durationMs: workflow.durationMs,
      },
      {
        name: legacyWorkflow.name,
        status: legacyWorkflow.status,
        agentCount: legacyWorkflow.agentCount,
        totalTokens: legacyWorkflow.totalTokens,
        totalToolCalls: legacyWorkflow.totalToolCalls,
        durationMs: legacyWorkflow.durationMs,
      },
    );
    const details = await shadow.getWorkflow(workflow.workflowId);
    assertVersioned(details, 'getWorkflow', atCommitSeq);
    observePayload(payloads, 'getWorkflow', details);
    const members = await shadow.listWorkflowMembers(workflow.workflowId, { limit: 200 });
    assertVersioned(members, 'listWorkflowMembers', atCommitSeq);
    observePayload(payloads, 'listWorkflowMembers', members);
    const legacyMembers = legacy.getWorkflowSubagents(
      workflowScope.legacy.session.projectSlug,
      workflowScope.legacy.session.sessionId,
      workflow.nativeWorkflowId,
      { sourceId: 'claude-code' },
    );
    assert.deepEqual(
      members.items.map((item) => item.nativeAgentId).sort(),
      legacyMembers.map((item) => item.agentId).sort(),
    );
  }
  capabilities.workflows = workflows.items.length;
  checks.push('workflow summaries, details, and member identity parity');

  const memoryProject = context.memoryScope?.project ?? context.primaryProject;
  const memory = await shadow.listMemoryDocuments(memoryProject.projectId, { limit: 200 });
  assertVersioned(memory, 'listMemoryDocuments', atCommitSeq);
  observePayload(payloads, 'listMemoryDocuments', memory);
  if (context.memoryScope) {
    assert.equal(
      memory.items.find((document) => document.isIndex)?.content ?? null,
      legacy.getProjectMemory(context.memoryScope.legacy, { sourceId: 'claude-code' }),
    );
  }
  if (context.payloadBoundary) {
    const boundaryItems = memory.items.filter((document) =>
      document.nativeDocumentPath.startsWith('memory/query-payload-boundary-'),
    );
    assert.equal(boundaryItems.length, context.payloadBoundary.documents, 'payload-boundary document discovery');
    assert.equal(
      boundaryItems.reduce((total, document) => total + Buffer.byteLength(document.content), 0),
      context.payloadBoundary.bytes,
      'payload-boundary content bytes',
    );
    assert.ok(memory.payloadBytes >= context.payloadBoundary.bytes, 'payload page did not include boundary content');
    assert.ok(
      memory.payloadBytes / memory.payloadByteLimit >= 0.7,
      'payload page did not exercise at least 70% of its bound',
    );
  }
  capabilities.memoryDocuments = memory.items.length;

  const taskCollections = await shadow.listTaskCollections({ limit: 200 });
  assertVersioned(taskCollections, 'listTaskCollections', atCommitSeq);
  capabilities.taskCollections = taskCollections.items.length;
  let taskCount = 0;
  for (const collection of taskCollections.items) {
    const tasks = await shadow.listTasks(collection.collectionId, { limit: 200 });
    assertVersioned(tasks, 'listTasks', atCommitSeq);
    observePayload(payloads, 'listTasks', tasks);
    taskCount += tasks.items.length;
  }
  capabilities.tasks = taskCount;

  const plans = await shadow.listPlans({ limit: 200 });
  assertVersioned(plans, 'listPlans', atCommitSeq);
  observePayload(payloads, 'listPlans', plans);
  capabilities.plans = plans.items.length;

  let toolResultsCount = 0;
  let artifactsCount = 0;
  for (const session of canonicalSessions) {
    const details = sessionDetails.get(session.sessionId)?.session;
    if (details?.persistedToolResultCount) {
      const toolResults = await shadow.listToolResults(session.projectId, session.sessionId, { limit: 200 });
      assertVersioned(toolResults, 'listToolResults', atCommitSeq);
      observePayload(payloads, 'listToolResults', toolResults);
      const legacySession = legacySessions.find((item) => item.session.sessionId === session.nativeSessionId);
      assert.ok(legacySession);
      for (const result of toolResults.items) {
        assert.equal(
          result.content,
          legacy.getToolResult(
            legacySession.session.projectSlug,
            legacySession.session.sessionId,
            result.nativeToolUseId,
          ),
        );
      }
      toolResultsCount += toolResults.items.length;
    }
    if (details?.artifactCount) {
      const artifacts = await shadow.listArtifacts(session.sessionId, { limit: 200 });
      assertVersioned(artifacts, 'listArtifacts', atCommitSeq);
      observePayload(payloads, 'listArtifacts', artifacts);
      artifactsCount += artifacts.items.length;
    }
  }
  capabilities.toolResults = toolResultsCount;
  capabilities.artifacts = artifactsCount;
  checks.push('capability pages and bounded lossless payloads');

  const sources = await shadow.listSources({ limit: 200 });
  assertVersioned(sources, 'listSources', atCommitSeq);
  assert.deepEqual(sources.items.map((source) => source.adapterId).sort(), legacy.getSourceIds().sort());
  capabilities.sources = sources.items.length;
  const stats = await shadow.getStats();
  assertVersioned(stats, 'getStats', atCommitSeq);
  assert.equal(stats.sourceInstances, sources.items.length);

  for (const legacyProject of legacyProjects) {
    const project = canonicalProjects.find((item) => item.nativeProjectKey === legacyProject.slug);
    assert.ok(project, `missing usage project ${legacyProject.slug}`);
    const [totals, activity] = await Promise.all([
      shadow.getUsage({ projectId: project.projectId }),
      shadow.getUsageActivity({
        projectId: project.projectId,
        from: context.usageFrom,
        to: context.usageTo,
      }),
    ]);
    assertVersioned(totals, 'getUsage', atCommitSeq);
    assertVersioned(activity, 'getUsageActivity', atCommitSeq);
    const legacyActivity = legacy.getProjectTokenActivity(legacyProject, {
      sourceId: 'claude-code',
      from: context.usageFrom,
      to: context.usageTo,
    });
    const parity = compareClaudeObservationUsage(totals, activity, {
      totals: legacyProject.tokenUsage,
      days: legacyActivity.days,
    });
    assert.equal(parity.exact, true, `${legacyProject.slug}: ${JSON.stringify(parity)}`);
  }
  const sessionUsage = await shadow.getUsage({
    projectId: context.messageProject.projectId,
    sessionId: context.messageSession.sessionId,
  });
  assertVersioned(sessionUsage, 'getUsage(session)', atCommitSeq);
  checks.push('exact usage components and activity parity');

  const runtimeEntries = await allRuntimeEntries(shadow, atCommitSeq);
  capabilities.runtimeEntries = runtimeEntries.length;
  const fixturePresence = runtimeEntries.find(
    (entry) => entry.kind === 'presence' && entry.presence.nativePid === context.completeSurface.nativePid,
  );
  assert.ok(fixturePresence?.presence, 'scratch presence was not returned by the runtime snapshot');
  assert.equal(fixturePresence.presence.nativeSessionId, context.completeSurface.nativeSessionId);
  const runEntry = runtimeEntries.find((entry) => entry.kind === 'run');
  assert.ok(runEntry?.run, 'runtime snapshot must expose a run for getRunState conformance');
  const run = await shadow.getRunState(runEntry.run.runId);
  assertVersioned(run, 'getRunState', atCommitSeq);
  assert.equal(run.run?.runId, runEntry.run.runId);

  const teams = await shadow.listTeams({ limit: 200 });
  assertVersioned(teams, 'listTeams', atCommitSeq);
  const legacyTeams = legacy.getTeams();
  assert.deepEqual(teams.items.map((team) => team.nativeTeamId).sort(), legacyTeams.map((team) => team.teamId).sort());
  capabilities.teams = teams.items.length;
  let inboxCount = 0;
  let inboxMessageCount = 0;
  for (const team of teams.items) {
    const details = await shadow.getTeam(team.teamId);
    assertVersioned(details, 'getTeam', atCommitSeq);
    const inboxes = await shadow.listTeamInboxes(team.teamId, { limit: 200 });
    assertVersioned(inboxes, 'listTeamInboxes', atCommitSeq);
    inboxCount += inboxes.items.length;
    for (const inbox of inboxes.items) {
      const messages = await shadow.listTeamInboxMessages(inbox.inboxId, { limit: 200 });
      assertVersioned(messages, 'listTeamInboxMessages', atCommitSeq);
      inboxMessageCount += messages.items.length;
    }
  }
  const fixtureTeam = teams.items.find((team) => team.nativeTeamId === context.completeSurface.nativeTeamId);
  assert.ok(fixtureTeam, 'scratch team was not returned by listTeams');
  assert.equal(fixtureTeam.inboxCount, 1);
  assert.equal(fixtureTeam.messageCount, context.completeSurface.inboxMessages);
  capabilities.teamInboxes = inboxCount;
  capabilities.teamInboxMessages = inboxMessageCount;
  checks.push('source, stats, runtime, run, team, and inbox contracts');

  const after = await shadow.snapshot();
  assert.equal(after.overview.commitSeq, before.overview.commitSeq, 'queries must not advance the writer commit');
  assert.equal(
    after.overview.writerDataVersion,
    before.overview.writerDataVersion,
    'queries must not mutate the canonical database',
  );
  assert.equal(after.overview.queryOnly, true);
  assert.equal(after.overview.readOnly, true);
  checks.push('read-only/query-only purity across the complete surface');

  const aborted = new AbortController();
  aborted.abort();
  await assert.rejects(
    shadow.search({ text: context.searchText, limit: 1 }, aborted.signal),
    /abort|cancel/i,
    'pre-aborted query must not enter the native queue',
  );
  checks.push('pre-queue cancellation');

  return {
    exact: true,
    atCommitSeq,
    schemaVersion: before.overview.schemaVersion,
    projects: canonicalProjects.length,
    sessions: canonicalSessions.length,
    messages: canonicalSessions.reduce((total, session) => total + session.messageCount, 0),
    searchText: context.searchText,
    searchHits: search.total,
    checks,
    capabilities,
    payloads: payloads.sort((left, right) => right.bytes - left.bytes),
    acceptedDifferences: [
      'canonical message counts include delegated transcripts',
      'equal-timestamp message ties use canonical identity rather than legacy transcript ordinal',
      'timeline facets count canonical envelopes and content blocks, not legacy display rows',
      'canonical token totals are additive components, not provider billing totals',
      'canonical stats exclude compatibility-cache rows',
      'workflow state, member evidence, and child run state remain separate',
    ],
  };
}

async function measure(fn: () => unknown | Promise<unknown>, callsPerRequest: number): Promise<QueryMeasurement> {
  for (let index = 0; index < warmup; index += 1) {
    await fn();
    await immediate();
  }

  const delay = monitorEventLoopDelay({ resolution: 1 });
  delay.enable();
  const memoryBefore = process.memoryUsage();
  const samples: number[] = [];
  let lastResult: unknown;
  for (let index = 0; index < runs; index += 1) {
    const started = performance.now();
    lastResult = await fn();
    samples.push(performance.now() - started);
    await immediate();
  }
  const memoryAfter = process.memoryUsage();
  delay.disable();

  return {
    endToEndMs: distribution(samples),
    responseBytes: encodedBytes(lastResult),
    heapDeltaBytes: memoryAfter.heapUsed - memoryBefore.heapUsed,
    rssDeltaBytes: memoryAfter.rss - memoryBefore.rss,
    eventLoopDelayMs: {
      mean: finiteDelay(delay.mean),
      p95: finiteDelay(delay.percentile(95)),
      p99: finiteDelay(delay.percentile(99)),
      max: finiteDelay(delay.max),
    },
    apiCallsPerLogicalRequest: callsPerRequest,
  };
}

async function cursorBeforeMessage(
  shadow: ClaudeObservationShadow,
  projectId: string,
  sessionId: string,
  offset: number,
): Promise<string | undefined> {
  let cursor: string | undefined;
  for (let index = 0; index < offset; index += 1) {
    const page = await shadow.getMessages(projectId, sessionId, { limit: 1, cursor });
    cursor = page.nextCursor;
    if (!cursor) return undefined;
  }
  return cursor;
}

async function queryCases(context: HarnessContext): Promise<QueryCase[]> {
  const { legacy, shadow } = context;
  const cases: QueryCase[] = [
    {
      workload: 'warm metadata lookup',
      legacyCalls: 1,
      rustCalls: 1,
      legacy: () => legacy.getStats(),
      rust: () => shadow.getStats(),
    },
    {
      workload: 'project list aggregation',
      legacyCalls: 1,
      rustCalls: 1,
      legacy: () => legacy.getProjectList({ sourceId: 'claude-code' }).slice(0, 50),
      rust: () => shadow.listHistoryProjects({ limit: 50 }),
    },
    {
      workload: 'session list aggregation',
      legacyCalls: 1,
      rustCalls: 1,
      legacy: () => legacy.getSessionList(context.messageLegacy.project, { sourceId: 'claude-code' }).slice(0, 50),
      rust: () => shadow.listHistorySessions(context.messageProject.projectId, { limit: 50 }),
    },
    {
      workload: 'message page 50',
      legacyCalls: 1,
      rustCalls: 1,
      legacy: () =>
        legacy.getSessionMessages(
          context.messageLegacy.session.projectSlug,
          context.messageLegacy.session.sessionId,
          50,
          0,
          { sourceId: 'claude-code' },
        ),
      rust: () => shadow.getMessages(context.messageProject.projectId, context.messageSession.sessionId, { limit: 50 }),
    },
    {
      workload: 'FTS top 50 parent + delegated',
      legacyCalls: 1,
      rustCalls: 1,
      legacy: () => legacy.search({ text: context.searchText, limit: 50 }),
      rust: () => shadow.search({ text: context.searchText, limit: 50 }),
    },
    {
      workload: 'timeline page + facets + total',
      legacyCalls: 2,
      rustCalls: 1,
      legacy: () => ({
        page: legacy.getSessionTimeline(
          context.messageLegacy.session.projectSlug,
          context.messageLegacy.session.sessionId,
          { sourceId: 'claude-code', limit: 50 },
        ),
        facets: legacy.getSessionTimelineFacets(
          context.messageLegacy.session.projectSlug,
          context.messageLegacy.session.sessionId,
          { sourceId: 'claude-code' },
        ),
      }),
      rust: () =>
        shadow.getTimeline({
          projectId: context.messageProject.projectId,
          sessionId: context.messageSession.sessionId,
          limit: 50,
        }),
    },
    {
      workload: 'project usage activity',
      legacyCalls: 1,
      rustCalls: 1,
      legacy: () =>
        legacy.getProjectTokenActivity(context.messageLegacy.project, {
          sourceId: 'claude-code',
          from: context.usageFrom,
          to: context.usageTo,
        }),
      rust: () =>
        shadow.getUsageActivity({
          projectId: context.messageProject.projectId,
          from: context.usageFrom,
          to: context.usageTo,
        }),
    },
  ];

  if (context.delegationScope) {
    const scope = context.delegationScope;
    cases.push({
      workload: 'delegation list',
      legacyCalls: 1,
      rustCalls: 1,
      legacy: () =>
        legacy.getSessionSubagents(scope.legacy.session.projectSlug, scope.legacy.session.sessionId, {
          sourceId: 'claude-code',
          includeNested: true,
        }),
      rust: () =>
        shadow.listDelegations({ projectId: scope.project.projectId, sessionId: scope.session.sessionId, limit: 50 }),
    });
  }

  if (context.workflowScope) {
    const scope = context.workflowScope;
    cases.push({
      workload: 'workflow list',
      legacyCalls: 1,
      rustCalls: 1,
      legacy: () => legacy.getSessionWorkflows(scope.legacy.session.projectSlug, scope.legacy.session.sessionId),
      rust: () =>
        shadow.listWorkflows({ projectId: scope.project.projectId, sessionId: scope.session.sessionId, limit: 50 }),
    });
  }

  if (context.memoryScope) {
    const scope = context.memoryScope;
    cases.push({
      workload: 'bounded memory detail payload',
      legacyCalls: 1,
      rustCalls: 1,
      legacy: () => legacy.getProjectMemory(scope.legacy, { sourceId: 'claude-code' }),
      rust: () => shadow.listMemoryDocuments(scope.project.projectId, { limit: 50 }),
    });
  }

  const deepOffset = Math.min(25, context.messageLegacy.parentMessageCount - 1);
  if (deepOffset > 0) {
    const cursor = await cursorBeforeMessage(
      shadow,
      context.messageProject.projectId,
      context.messageSession.sessionId,
      deepOffset,
    );
    assert.ok(cursor, `could not prepare keyset cursor at offset ${deepOffset}`);
    cases.push({
      workload: `deep message page at row ${deepOffset + 1}`,
      legacyCalls: 1,
      rustCalls: 1,
      legacy: () =>
        legacy.getSessionMessages(
          context.messageLegacy.session.projectSlug,
          context.messageLegacy.session.sessionId,
          1,
          deepOffset,
          { sourceId: 'claude-code' },
        ),
      rust: () =>
        shadow.getMessages(context.messageProject.projectId, context.messageSession.sessionId, {
          limit: 1,
          cursor,
        }),
    });
  }

  cases.push({
    workload: `${readers} reader burst`,
    legacyCalls: readers,
    rustCalls: readers,
    legacy: () =>
      Promise.all(Array.from({ length: readers }, () => legacy.search({ text: context.searchText, limit: 50 }))),
    rust: () =>
      Promise.all(Array.from({ length: readers }, () => shadow.search({ text: context.searchText, limit: 50 }))),
  });

  return cases;
}

async function runCancellation(context: HarnessContext): Promise<CancellationReport> {
  const controllers = Array.from({ length: cancelBurst }, () => new AbortController());
  const started = performance.now();
  const pending = controllers.map((controller) =>
    context.shadow.search({ text: context.searchText, limit: 50 }, controller.signal),
  );
  for (const controller of controllers) controller.abort();
  const settled = await Promise.allSettled(pending);
  const elapsedMs = performance.now() - started;
  const rejected = settled.filter((result) => result.status === 'rejected').length;
  const recovery = await context.shadow.search({ text: context.searchText, limit: 1 });
  assert.equal(recovery.total > 0, true, 'query pool did not recover after cancellation burst');
  return {
    requests: cancelBurst,
    rejected,
    fulfilled: settled.length - rejected,
    elapsedMs: round(elapsedMs),
    recoverySearchHits: recovery.total,
  };
}

async function timedReaders(context: HarnessContext): Promise<number[]> {
  return Promise.all(
    Array.from({ length: readers }, async () => {
      const started = performance.now();
      await context.shadow.search({ text: context.searchText, limit: 50 });
      return performance.now() - started;
    }),
  );
}

async function runConcurrentRefresh(context: HarnessContext): Promise<ConcurrentRefreshReport> {
  const baselineSamples = await timedReaders(context);
  const transcript = path.join(
    context.workRoot,
    'projects',
    context.messageSession.nativeProjectKey,
    `${context.messageSession.nativeSessionId}.jsonl`,
  );
  assert.equal(existsSync(transcript), true, `missing scratch transcript ${transcript}`);
  const marker = 'query-conformance-live-refresh-marker';
  appendFileSync(
    transcript,
    `${JSON.stringify({
      type: 'user',
      uuid: '00000000-0000-4000-8000-000000000001',
      parentUuid: null,
      timestamp: `${context.usageFrom.slice(0, 4)}-12-31T23:59:59.999Z`,
      sessionId: context.messageSession.nativeSessionId,
      cwd: context.messageSession.cwd ?? '/tmp/spaghetti-query-bench',
      version: '1.0.0',
      gitBranch: context.messageSession.gitBranch ?? 'main',
      isSidechain: false,
      userType: 'external',
      message: { role: 'user', content: marker },
    })}\n`,
  );

  const refreshStarted = performance.now();
  const refreshPromise = context.shadow.refresh();
  const concurrentSamplesPromise = timedReaders(context);
  const [, concurrentSamples] = await Promise.all([refreshPromise, concurrentSamplesPromise]);
  const refreshMs = performance.now() - refreshStarted;
  const after = await context.shadow.snapshot();
  assert.ok(after.overview.commitSeq > context.atCommitSeq, 'live refresh did not advance the durable commit');
  const markerSearch = await context.shadow.search({ text: marker, limit: 1 });
  assert.equal(markerSearch.total, 1, 'live refresh marker did not become queryable exactly once');
  const baseline = distribution(baselineSamples);
  const concurrent = distribution(concurrentSamples);
  const walPath = `${context.shadowDbPath}-wal`;

  return {
    readers,
    baselineReaderMs: baseline,
    concurrentReaderMs: concurrent,
    p95Ratio: baseline.p95 === 0 ? null : round(concurrent.p95 / baseline.p95),
    refreshMs: round(refreshMs),
    beforeCommitSeq: context.atCommitSeq,
    afterCommitSeq: after.overview.commitSeq,
    markerHits: markerSearch.total,
    walBytes: existsSync(walPath) ? statSync(walPath).size : 0,
  };
}

async function runBenchmark(context: HarnessContext): Promise<BenchmarkReport> {
  const rows: BenchmarkRow[] = [];
  for (const queryCase of await queryCases(context)) {
    process.stdout.write(`  ${queryCase.workload}... `);
    const legacy = await measure(queryCase.legacy, queryCase.legacyCalls);
    const rustNapi = await measure(queryCase.rust, queryCase.rustCalls);
    rows.push({ workload: queryCase.workload, legacy, rustNapi });
    console.log('done');
  }

  const beforeRefresh = await context.shadow.snapshot();
  assert.equal(
    beforeRefresh.overview.commitSeq,
    context.atCommitSeq,
    'benchmark queries mutated the canonical database',
  );
  assert.equal(beforeRefresh.overview.queryOnly, true);
  assert.equal(beforeRefresh.overview.readOnly, true);

  process.stdout.write('  cancellation burst... ');
  const cancellation = await runCancellation(context);
  console.log('done');
  process.stdout.write('  concurrent readers + live refresh... ');
  const concurrentRefresh = await runConcurrentRefresh(context);
  console.log('done');
  return { rows, cancellation, concurrentRefresh };
}

function printBenchmark(report: BenchmarkReport): void {
  console.log('');
  console.log('workload                                  TS p50/p95/p99     Rust p50/p95/p99   bytes TS/Rust');
  console.log('----------------------------------------  -----------------  -----------------  -------------');
  for (const row of report.rows) {
    const legacy = row.legacy.endToEndMs;
    const rust = row.rustNapi.endToEndMs;
    const legacyTimes = `${legacy.p50}/${legacy.p95}/${legacy.p99}`.padStart(17);
    const rustTimes = `${rust.p50}/${rust.p95}/${rust.p99}`.padStart(17);
    const bytes = `${row.legacy.responseBytes}/${row.rustNapi.responseBytes}`.padStart(13);
    console.log(`${row.workload.padEnd(40)}  ${legacyTimes}  ${rustTimes}  ${bytes}`);
  }
  console.log('');
  console.log(
    `cancellation: ${report.cancellation.rejected}/${report.cancellation.requests} rejected in ` +
      `${report.cancellation.elapsedMs} ms; recovery search returned ${report.cancellation.recoverySearchHits} hits`,
  );
  console.log(
    `live refresh:  readers p95 ${report.concurrentRefresh.baselineReaderMs.p95} → ` +
      `${report.concurrentRefresh.concurrentReaderMs.p95} ms; refresh ${report.concurrentRefresh.refreshMs} ms; ` +
      `commit ${report.concurrentRefresh.beforeCommitSeq} → ${report.concurrentRefresh.afterCommitSeq}`,
  );
}

function hostInfo(): Record<string, unknown> {
  const cpu = cpus();
  return {
    platform: platform(),
    arch: arch(),
    cpuModel: cpu[0]?.model.trim() ?? 'unknown',
    cpuCount: cpu.length,
    totalMemoryBytes: totalmem(),
    node: process.version,
  };
}

async function main(): Promise<void> {
  const native = loadNativeAddon();
  if (!native)
    throw new Error(
      'native addon is unavailable; build it with pnpm --filter @vibecook/spaghetti-sdk-native build:debug',
    );

  const scratch = mkdtempSync(path.join(tmpdir(), 'spaghetti-query-bench-'));
  const workRoot = path.join(scratch, '.claude');
  const legacyDbPath = path.join(scratch, 'legacy.db');
  const shadowDbPath = path.join(scratch, 'observation.db');
  mkdirSync(path.dirname(workRoot), { recursive: true });
  cpSync(fixtureRoot, workRoot, { recursive: true });
  const completeSurface = installCompleteSurfaceFixture(workRoot);
  const payloadBoundary = installPayloadBoundaryFixture(workRoot);
  const corpus = corpusStats(workRoot);
  let legacy: LegacySpaghettiAPI | undefined;
  let shadow: ClaudeObservationShadow | undefined;

  console.log(`fixture:       ${fixtureRoot}`);
  console.log(`scratch copy:  ${workRoot}`);
  console.log(`corpus:        ${corpus.files} files, ${corpus.jsonlFiles} JSONL, ${corpus.bytes} bytes`);
  console.log(`surface probe: runtime presence + team/inbox (scratch only)`);
  if (payloadBoundary) {
    console.log(
      `payload probe: ${payloadBoundary.documents} × 1 MiB memory documents, ` +
        `${payloadBoundary.bytes} bytes total (scratch only)`,
    );
  }
  console.log(`mode:          ${mode}`);
  console.log(`queries:       ${runs} runs (+ ${warmup} warmup), ${queryWorkers} Rust workers`);
  console.log(`native:        ${native.nativeVersion()}`);

  try {
    legacy = createSpaghettiService({ rootDir: workRoot, dbPath: legacyDbPath, engine: 'ts' });
    process.stdout.write('initializing TypeScript oracle... ');
    await legacy.initialize();
    console.log('done');
    process.stdout.write('initializing Rust observation engine... ');
    shadow = await openClaudeObservationShadow({
      productionDbPath: legacyDbPath,
      shadowDbPath,
      roots: [workRoot],
      queryWorkers,
      ownerLabel: 'phase-9-query-conformance-benchmark',
    });
    console.log('done');

    const context = await prepareContext(legacy, shadow, workRoot, shadowDbPath, payloadBoundary, completeSurface);
    process.stdout.write('query conformance... ');
    const conformance = await runConformance(context);
    console.log(`pass (${conformance.checks.length} groups)`);
    console.log(
      `canonical:     ${conformance.projects} projects, ${conformance.sessions} sessions, ` +
        `${conformance.messages} messages at commit ${conformance.atCommitSeq}`,
    );
    console.log(`search:        ${JSON.stringify(conformance.searchText)} → ${conformance.searchHits} hits`);
    const largestPayload = conformance.payloads[0];
    if (largestPayload) {
      console.log(
        `payload max:   ${largestPayload.query} ${largestPayload.bytes}/${largestPayload.limit} bytes ` +
          `(${round(largestPayload.utilization * 100)}%)`,
      );
    }

    const benchmark = mode === 'conformance' ? undefined : await runBenchmark(context);
    if (benchmark) printBenchmark(benchmark);

    const report = {
      reportVersion: 1,
      generatedAt: new Date().toISOString(),
      fixture: fixtureRoot,
      corpus,
      completeSurface,
      payloadBoundary: payloadBoundary
        ? {
            nativeProjectKey: payloadBoundary.nativeProjectKey,
            documents: payloadBoundary.documents,
            bytes: payloadBoundary.bytes,
          }
        : null,
      mode,
      runs,
      warmup,
      queryWorkers,
      nativeVersion: native.nativeVersion(),
      host: hostInfo(),
      conformance,
      benchmark,
      measurementNotes: [
        'End-to-end timings include JavaScript-to-N-API conversion and response materialization.',
        'Response bytes are JSON-encoded JavaScript result bytes; query payload fields enforce their separate Rust bounds.',
        'Heap and RSS deltas are process observations without forced GC and are diagnostic, not allocation counts.',
        'Rust allocation, SQLite execution, queue, and conversion sub-timings require native telemetry not yet exposed.',
        'IPC topology is not measured because the Phase 10 field-native/daemon transport does not exist yet.',
        'No latency regression threshold is inferred from a single host; use report JSON to establish reviewed baselines.',
      ],
    };

    if (reportJsonPath) {
      mkdirSync(path.dirname(reportJsonPath), { recursive: true });
      writeFileSync(reportJsonPath, `${JSON.stringify(report, null, 2)}\n`);
      console.log(`report:        ${reportJsonPath}`);
    }
  } finally {
    if (shadow) await shadow.dispose();
    if (legacy) await legacy.dispose();
    if (keepWorkdir) console.log(`kept scratch:  ${scratch}`);
    else rmSync(scratch, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 });
  }
}

main().catch((error: unknown) => {
  console.error(error);
  process.exitCode = 1;
});
