/**
 * RFC 011 selected-topology benchmark.
 *
 * Build and run through Electron so this exercises MessageChannelMain and the
 * real SDK UtilityProcess rather than the in-process MessageChannel test shim.
 * Input is copied to scratch storage before adding the bounded payload probe.
 *
 *   pnpm bench:query-topology
 *   pnpm bench:query-topology -- --runs 30 --warmup 5
 *   pnpm bench:query-topology -- --payload-mib 0 --report-json /tmp/ipc.json
 */
import assert from 'node:assert/strict';
import { cpSync, existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { cpus, tmpdir, totalmem } from 'node:os';
import path from 'node:path';
import { monitorEventLoopDelay, performance } from 'node:perf_hooks';
import { parseArgs } from 'node:util';

import { app } from 'electron';
import type { SpaghettiClient } from '@vibecook/spaghetti-sdk/client';
import type { SdkHostDiagnostics } from '../shared/sdk-protocol.js';
import { readCanonicalStats } from '../main/canonical-queries.js';
import { SdkHostClient } from '../main/sdk-host-client.js';

process.once('uncaughtException', fatalStartupError);
process.once('unhandledRejection', fatalStartupError);

interface Distribution {
  min: number;
  p50: number;
  p95: number;
  p99: number;
  max: number;
  mean: number;
}

interface WorkloadMeasurement {
  latencyMs: Distribution;
  frames: FrameMeasurement;
  responseJsonBytes: number;
  eventLoopDelayMs: {
    mean: number | null;
    p95: number | null;
    p99: number | null;
    max: number | null;
  };
}

interface CancellationMeasurement {
  requests: number;
  rejected: number;
  fulfilled: number;
  elapsedMs: number;
  recoveryHits: number;
  frames: FrameMeasurement;
}

interface FrameCounters {
  sentCount: number;
  sentBytes: number;
  receivedCount: number;
  receivedBytes: number;
}

interface FrameMeasurement {
  sent: { count: number; bytes: number; bytesPerLogicalRequest: number };
  received: { count: number; bytes: number; bytesPerLogicalRequest: number };
}

interface PayloadFixture {
  nativeProjectKey: string;
  documents: number;
  bytes: number;
}

const { values } = parseArgs({
  args: process.argv.slice(2).filter((argument) => argument !== '--'),
  options: {
    fixture: { type: 'string' },
    runs: { type: 'string', default: '20' },
    warmup: { type: 'string', default: '3' },
    'cancel-burst': { type: 'string', default: '100' },
    'payload-mib': { type: 'string', default: '12' },
    search: { type: 'string', default: 'error handling' },
    'report-json': { type: 'string' },
    'keep-workdir': { type: 'boolean', default: false },
  },
  strict: true,
});

const repoRoot = path.resolve(import.meta.dirname, '../../../..');
const fixtureRoot = path.resolve(values.fixture ?? path.join(repoRoot, 'crates/spaghetti-napi/fixtures/small/.claude'));
const runs = positiveInteger(values.runs!, 'runs');
const warmup = nonNegativeInteger(values.warmup!, 'warmup');
const cancelBurst = positiveInteger(values['cancel-burst']!, 'cancel-burst');
const payloadMiB = boundedInteger(values['payload-mib']!, 'payload-mib', 0, 15);
const searchText = values.search!.trim();
const reportJsonPath = values['report-json'] ? path.resolve(values['report-json']) : undefined;
const keepWorkdir = values['keep-workdir'] ?? false;

function failArgs(message: string): never {
  console.error(message);
  app.exit(2);
  throw new Error(message);
}

function fatalStartupError(error: unknown): void {
  console.error(error);
  app.exit(1);
}

function positiveInteger(raw: string, name: string): number {
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value < 1) failArgs(`--${name} must be a positive integer`);
  return value;
}

function nonNegativeInteger(raw: string, name: string): number {
  return boundedInteger(raw, name, 0, Number.MAX_SAFE_INTEGER);
}

function boundedInteger(raw: string, name: string, min: number, max: number): number {
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value < min || value > max) {
    failArgs(`--${name} must be an integer from ${min} through ${max}`);
  }
  return value;
}

function round(value: number): number {
  return Number(value.toFixed(3));
}

function distribution(samples: readonly number[]): Distribution {
  assert.ok(samples.length > 0);
  const sorted = [...samples].sort((left, right) => left - right);
  const percentile = (fraction: number): number =>
    sorted[Math.min(sorted.length - 1, Math.max(0, Math.ceil(sorted.length * fraction) - 1))]!;
  return {
    min: round(sorted[0]!),
    p50: round(percentile(0.5)),
    p95: round(percentile(0.95)),
    p99: round(percentile(0.99)),
    max: round(sorted.at(-1)!),
    mean: round(sorted.reduce((total, value) => total + value, 0) / sorted.length),
  };
}

function delayMs(value: number): number | null {
  return Number.isFinite(value) ? round(value / 1_000_000) : null;
}

async function measure(operation: () => Promise<unknown>, frames: FrameCounters): Promise<WorkloadMeasurement> {
  for (let index = 0; index < warmup; index += 1) await operation();
  resetFrames(frames);

  const delay = monitorEventLoopDelay({ resolution: 1 });
  const samples: number[] = [];
  let result: unknown;
  delay.enable();
  for (let index = 0; index < runs; index += 1) {
    const started = performance.now();
    result = await operation();
    samples.push(performance.now() - started);
    await new Promise((resolve) => setImmediate(resolve));
  }
  delay.disable();
  return {
    latencyMs: distribution(samples),
    frames: frameMeasurement(frames, runs),
    responseJsonBytes: Buffer.byteLength(JSON.stringify(result)),
    eventLoopDelayMs: {
      mean: delayMs(delay.mean),
      p95: delayMs(delay.percentile(95)),
      p99: delayMs(delay.percentile(99)),
      max: delayMs(delay.max),
    },
  };
}

async function cancellation(
  client: SpaghettiClient,
  frames: FrameCounters,
  expectedHits: number,
): Promise<CancellationMeasurement> {
  resetFrames(frames);
  const controllers = Array.from({ length: cancelBurst }, () => new AbortController());
  const started = performance.now();
  const pending = controllers.map((controller) =>
    client.search({ text: searchText, limit: 50 }, { signal: controller.signal }),
  );
  for (const controller of controllers) controller.abort();
  const settled = await Promise.allSettled(pending);
  const recovery = await client.search({ text: searchText, limit: 1 });
  assert.equal(recovery.total, expectedHits, 'query pool did not recover to the same search result');
  return {
    requests: settled.length,
    rejected: settled.filter(({ status }) => status === 'rejected').length,
    fulfilled: settled.filter(({ status }) => status === 'fulfilled').length,
    elapsedMs: round(performance.now() - started),
    recoveryHits: recovery.total,
    frames: frameMeasurement(frames, settled.length + 1),
  };
}

function resetFrames(frames: FrameCounters): void {
  frames.sentCount = 0;
  frames.sentBytes = 0;
  frames.receivedCount = 0;
  frames.receivedBytes = 0;
}

function frameMeasurement(frames: FrameCounters, logicalRequests: number): FrameMeasurement {
  return {
    sent: {
      count: frames.sentCount,
      bytes: frames.sentBytes,
      bytesPerLogicalRequest: round(frames.sentBytes / logicalRequests),
    },
    received: {
      count: frames.receivedCount,
      bytes: frames.receivedBytes,
      bytesPerLogicalRequest: round(frames.receivedBytes / logicalRequests),
    },
  };
}

function installPayloadFixture(root: string): PayloadFixture | undefined {
  if (payloadMiB === 0) return undefined;
  const projectsRoot = path.join(root, 'projects');
  const nativeProjectKey = readdirSync(projectsRoot, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .sort()[0];
  assert.ok(nativeProjectKey, 'fixture needs at least one project for the payload probe');
  const memoryRoot = path.join(projectsRoot, nativeProjectKey, 'memory');
  mkdirSync(memoryRoot, { recursive: true });
  const documentBytes = 1024 * 1024;
  for (let index = 0; index < payloadMiB; index += 1) {
    const prefix = `# IPC topology payload ${index}\n\n`;
    writeFileSync(
      path.join(memoryRoot, `ipc-topology-payload-${String(index).padStart(2, '0')}.md`),
      `${prefix}${'x'.repeat(documentBytes - Buffer.byteLength(prefix))}`,
    );
  }
  return { nativeProjectKey, documents: payloadMiB, bytes: payloadMiB * documentBytes };
}

function memoryDelta(after: SdkHostDiagnostics, before: SdkHostDiagnostics): SdkHostDiagnostics['memory'] {
  return {
    rss: after.memory.rss - before.memory.rss,
    heapTotal: after.memory.heapTotal - before.memory.heapTotal,
    heapUsed: after.memory.heapUsed - before.memory.heapUsed,
    external: after.memory.external - before.memory.external,
    arrayBuffers: after.memory.arrayBuffers - before.memory.arrayBuffers,
  };
}

async function run(): Promise<void> {
  if (!existsSync(fixtureRoot) || !statSync(fixtureRoot).isDirectory())
    failArgs(`fixture directory not found: ${fixtureRoot}`);
  if (!searchText) failArgs('--search must not be blank');

  const scratch = mkdtempSync(path.join(tmpdir(), 'spaghetti-ipc-topology-'));
  const root = path.join(scratch, '.claude');
  const legacyDbPath = path.join(scratch, 'legacy.db');
  const shadowDbPath = path.join(scratch, 'observation.db');
  cpSync(fixtureRoot, root, { recursive: true });
  const payloadFixture = installPayloadFixture(root);
  const frames: FrameCounters = { sentCount: 0, sentBytes: 0, receivedCount: 0, receivedBytes: 0 };
  const host = new SdkHostClient({
    dbPath: legacyDbPath,
    engine: 'ts',
    rootDir: root,
    observationShadow: { dbPath: shadowDbPath },
    detectAdditionalSources: false,
    onEvent: () => undefined,
  });
  let client: SpaghettiClient | undefined;

  try {
    const coldStarted = performance.now();
    const [openedClient, concurrentClient] = await Promise.all([
      host.getObservationClient({
        clientName: 'playground-ipc-topology-benchmark',
        onFrame: ({ direction, byteLength }) => {
          if (direction === 'sent') {
            frames.sentCount += 1;
            frames.sentBytes += byteLength;
          } else {
            frames.receivedCount += 1;
            frames.receivedBytes += byteLength;
          }
        },
      }),
      host.getObservationClient(),
    ]);
    client = openedClient;
    assert.equal(concurrentClient, client, 'concurrent product reads must share one client opener');
    assert.equal(await host.getObservationClient(), client, 'product reads must reuse one negotiated client');
    const coldStartMs = round(performance.now() - coldStarted);
    const handshakeFrames = frameMeasurement(frames, 1);
    const memoryBefore = await host.getHostDiagnostics();
    const overview = await client.getOverview();
    const canonicalStats = await readCanonicalStats(host);
    const projects = await client.listProjects({ limit: 50 });
    const search = await client.search({ text: searchText, limit: 50 });
    assert.equal(overview.canonicalSessions > 0, true);
    assert.equal(canonicalStats.atCommitSeq, overview.commitSeq);
    assert.equal(projects.items.length > 0, true);
    const project = projects.items.find((item) => item.messageCount > 0);
    assert.ok(project, 'fixture needs a project with canonical messages');
    const sessions = await client.listSessions({ projectId: project.projectId, limit: 50 });
    const session = sessions.items.find((item) => item.messageCount > 0);
    assert.ok(session, 'fixture needs a canonical session with messages');

    const operations: Record<string, () => Promise<unknown>> = {
      overview: () => client!.getOverview(),
      projects50: () => client!.listProjects({ limit: 50 }),
      sessions50: () => client!.listSessions({ projectId: project.projectId, limit: 50 }),
      messages50: () => client!.getMessages({ projectId: project.projectId, sessionId: session.sessionId, limit: 50 }),
      search50: () => client!.search({ text: searchText, limit: 50 }),
      timeline50: () => client!.getTimeline({ projectId: project.projectId, sessionId: session.sessionId, limit: 50 }),
    };
    let payloadProbe:
      | (PayloadFixture & { pagePayloadBytes: number; pagePayloadByteLimit: number; matchedDocuments: number })
      | undefined;
    if (payloadFixture) {
      const memoryProject = projects.items.find((item) => item.nativeProjectKey === payloadFixture.nativeProjectKey);
      assert.ok(memoryProject, `missing payload project ${payloadFixture.nativeProjectKey}`);
      const page = await client.listMemoryDocuments({ projectId: memoryProject.projectId, limit: 50 });
      const matchedDocuments = page.items.filter((item) =>
        item.nativeDocumentPath.includes('ipc-topology-payload-'),
      ).length;
      assert.equal(matchedDocuments, payloadFixture.documents);
      assert.ok(page.payloadBytes >= payloadFixture.bytes);
      assert.ok(page.payloadBytes <= page.payloadByteLimit);
      payloadProbe = {
        ...payloadFixture,
        pagePayloadBytes: page.payloadBytes,
        pagePayloadByteLimit: page.payloadByteLimit,
        matchedDocuments,
      };
      operations.memoryPayload = () => client!.listMemoryDocuments({ projectId: memoryProject.projectId, limit: 50 });
    }
    const workloads: Record<string, WorkloadMeasurement> = {};
    for (const [name, operation] of Object.entries(operations)) workloads[name] = await measure(operation, frames);

    const cancellationBurst = await cancellation(client, frames, search.total);
    const memoryAfter = await host.getHostDiagnostics();
    const report = {
      reportVersion: 1,
      generatedAt: new Date().toISOString(),
      topology: 'electron-utility-message-port',
      fixture: fixtureRoot,
      runs,
      warmup,
      searchText,
      payloadProbe,
      coldStartMs,
      handshakeFrames,
      negotiated: client.info,
      canonical: {
        commitSeq: overview.commitSeq,
        projects: projects.items.length,
        sessions: overview.canonicalSessions,
        messages: overview.canonicalMessages,
        searchHits: search.total,
      },
      workloads,
      cancellation: cancellationBurst,
      utilityHost: {
        pid: memoryAfter.pid,
        uptimeSeconds: round(memoryAfter.uptimeSeconds),
        memoryBefore: memoryBefore.memory,
        memoryAfter: memoryAfter.memory,
        memoryDelta: memoryDelta(memoryAfter, memoryBefore),
      },
      benchmarkProcess: {
        electron: process.versions.electron,
        node: process.version,
        cpuModel: cpus()[0]?.model ?? 'unknown',
        cpuCount: cpus().length,
        totalMemoryBytes: totalmem(),
      },
    };

    console.log(JSON.stringify(report, null, 2));
    if (reportJsonPath) {
      mkdirSync(path.dirname(reportJsonPath), { recursive: true });
      writeFileSync(reportJsonPath, `${JSON.stringify(report, null, 2)}\n`);
    }
  } finally {
    await client?.dispose();
    await host.dispose();
    if (keepWorkdir) console.error(`kept scratch: ${scratch}`);
    else rmSync(scratch, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 });
  }
}

void app
  .whenReady()
  .then(run)
  .then(() => app.exit(0))
  .catch(fatalStartupError);
