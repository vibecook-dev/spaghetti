#!/usr/bin/env -S tsx
/**
 * RFC 011 production-path ingestion benchmark.
 *
 * Unlike `bench-ingest.ts`, this opens the sole-owner observation host and
 * exercises the same adapter -> source driver -> fact projection -> SQLite
 * path used by applications. The default synthetic corpus is deterministic,
 * contains one intentionally large append object, and therefore catches
 * accidental per-commit scans of previously ingested rows.
 *
 * Usage:
 *   pnpm bench:observation
 *   pnpm bench:observation --records 32768 --runs 3 --warmup 1
 *   pnpm bench:observation --records 32768 --objects 1024 --runs 3 --warmup 1
 *   pnpm bench:observation --scenario warm-unchanged
 *   pnpm bench:observation --scenario live-append --append-records 64
 *   pnpm bench:observation --scenario warm-append --append-records 64
 *   pnpm bench:observation --fixture ~/.claude --report-json /tmp/report.json
 */

import { DatabaseSync } from 'node:sqlite';
import {
  appendFileSync,
  closeSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readdirSync,
  readSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import * as path from 'node:path';
import { parseArgs } from 'node:util';

import { openObservationHost } from '../packages/sdk/src/observation-host.js';
import type { SpaghettiEnginePerformanceStats } from '../packages/sdk/src/native.js';

type Scenario = 'cold' | 'live-append' | 'warm-unchanged' | 'warm-append';

/**
 * How often the benchmark asks the host whether history has converged. Readers
 * block WAL checkpoints, so this stays coarse enough that the measurement does
 * not hold the writer back.
 */
const CONVERGENCE_POLL_MS = 250;
/** How long to observe background convergence before driving repair passes. */
const CONVERGENCE_TIMEOUT_MS = 4 * 60 * 60 * 1000;
/** How long to wait for the deferred full-text structures after history. */
const SEARCH_READY_TIMEOUT_MS = 30 * 60 * 1000;

interface DatabaseMetrics {
  canonicalMessages: number;
  facts: number;
  commits: number;
  changeLogRows: number | null;
  changeLogPayloadBytes: number | null;
  databaseBytes: number;
  performance: SpaghettiEnginePerformanceStats | null;
}

interface Sample extends DatabaseMetrics {
  /** Time to catalog-visible: `openObservationHost` resolving after catalog-first startup. */
  readyMs: number;
  /** Time to history-complete: catalog plus every bounded repair pass to convergence. */
  durationMs: number;
  /** Time until the deferred full-text structures are queryable, when reached. */
  searchReadyMs: number | null;
  /** `refresh` passes the host needed after open to converge. */
  convergencePasses: number;
  inputBytes: number;
  inputRecords: number;
  mibPerSecond: number;
  recordsPerSecond: number;
}

interface Distribution {
  min: number;
  p50: number;
  median: number;
  p95: number;
  p99: number;
  mean: number;
  max: number;
}

interface Summary {
  scenario: Scenario;
  records: number;
  appendRecords: number;
  samples: Sample[];
  finalMetrics: DatabaseMetrics;
  durationMs: Distribution;
  readyMs: Distribution;
  medianConvergencePasses: number;
  medianMibPerSecond: number;
  medianRecordsPerSecond: number;
  memory: {
    baselineRssBytes: number;
    finalRssBytes: number;
    peakRssBytes: number;
    peakRssDeltaBytes: number;
  };
}

const { values } = parseArgs({
  options: {
    fixture: { type: 'string' },
    records: { type: 'string' },
    objects: { type: 'string' },
    'append-records': { type: 'string' },
    runs: { type: 'string' },
    warmup: { type: 'string' },
    scenario: { type: 'string' },
    'report-json': { type: 'string' },
    'keep-workspace': { type: 'boolean' },
  },
});

const records = positiveInteger(values.records ?? '8192', 'records');
const objects = positiveInteger(values.objects ?? '1', 'objects');
if (objects > records) fail(`--objects must not exceed --records (${objects} > ${records})`);
const appendRecords = positiveInteger(values['append-records'] ?? '64', 'append-records');
const runs = positiveInteger(values.runs ?? '3', 'runs');
const warmup = nonnegativeInteger(values.warmup ?? '1', 'warmup');
const scenario = (values.scenario ?? 'cold') as Scenario;
if (!['cold', 'live-append', 'warm-unchanged', 'warm-append'].includes(scenario)) {
  fail(`--scenario must be cold|live-append|warm-unchanged|warm-append, got: ${scenario}`);
}

const fixture = values.fixture ? path.resolve(expandTilde(values.fixture)) : undefined;
if (fixture && !existsSync(fixture)) fail(`fixture not found: ${fixture}`);
const reportPath = values['report-json'] ? path.resolve(expandTilde(values['report-json'])) : undefined;
const keepWorkspace = values['keep-workspace'] ?? false;
const ingestProfileSkip = process.env.SPAGHETTI_INGEST_PROFILE_SKIP?.trim() || undefined;
const workspace = mkdtempSync(path.join(tmpdir(), 'spaghetti-observation-bench-'));
const baselineRssBytes = process.memoryUsage().rss;
let peakRssBytes = baselineRssBytes;
const rssSampler = setInterval(() => {
  peakRssBytes = Math.max(peakRssBytes, process.memoryUsage().rss);
}, 5);
rssSampler.unref();

try {
  const sourceRoot = fixture ?? path.join(workspace, '.claude');
  const transcript = fixture ? undefined : createSyntheticCorpus(sourceRoot, records, objects);
  const inputBytes = directoryBytes(sourceRoot);
  const inputRecords = fixture ? countJsonLines(sourceRoot) : records;
  const samples: Sample[] = [];

  console.log(
    `RFC 011 observation benchmark: ${scenario}, ${inputRecords.toLocaleString()} records, ` +
      `${fixture ? 'fixture objects' : `${objects.toLocaleString()} objects`}, ${formatBytes(inputBytes)}` +
      (ingestProfileSkip ? `, skip=${ingestProfileSkip}` : ''),
  );

  if (scenario === 'live-append') {
    const liveResult = await runLiveAppendSeries({
      sourceRoot,
      transcript,
      inputRecords,
      appendRecords,
      warmup,
      runs,
      databasePath: path.join(workspace, 'observation-live.sqlite'),
    });
    samples.push(...liveResult.samples);
    const summary = summarize(scenario, inputRecords, appendRecords, samples, liveResult.finalMetrics);
    printSummary(summary, reportPath);
  } else {
    for (let index = 0; index < warmup + runs; index += 1) {
      const measured = index >= warmup;
      const sample = await runSample({
        sourceRoot,
        transcript,
        inputBytes,
        inputRecords,
        scenario,
        appendRecords,
        iteration: index,
        databasePath: path.join(workspace, `observation-${index}.sqlite`),
      });
      logSample(sample, measured ? `run ${index - warmup + 1}` : `warmup ${index + 1}`);
      if (measured) samples.push(sample);
    }
    const finalMetrics = requireLastSample(samples);
    const summary = summarize(scenario, inputRecords, appendRecords, samples, finalMetrics);
    printSummary(summary, reportPath);
  }
  if (keepWorkspace) console.log(`  workspace: ${workspace}`);
} finally {
  clearInterval(rssSampler);
  if (!keepWorkspace) rmSync(workspace, { recursive: true, force: true });
}

async function runLiveAppendSeries(options: {
  sourceRoot: string;
  transcript?: string;
  inputRecords: number;
  appendRecords: number;
  warmup: number;
  runs: number;
  databasePath: string;
}): Promise<{ samples: Sample[]; finalMetrics: DatabaseMetrics }> {
  if (!options.transcript) fail('live-append requires the synthetic corpus');
  const host = await timedOpen(options.sourceRoot, options.databasePath);
  const samples: Sample[] = [];
  try {
    await converge(host.value);
    for (let index = 0; index < options.warmup + options.runs; index += 1) {
      const measured = index >= options.warmup;
      const beforeBytes = statSync(options.transcript).size;
      appendSyntheticRecords(
        options.transcript,
        options.inputRecords + index * options.appendRecords,
        options.appendRecords,
      );
      const inputBytes = statSync(options.transcript).size - beforeBytes;
      const startedAt = performance.now();
      await host.value.refresh('claude-code');
      const readyMs = performance.now() - startedAt;
      const convergencePasses = await converge(host.value);
      const durationMs = performance.now() - startedAt;
      // Query through the native read pool while the owner is alive. Opening
      // node:sqlite here would load a second SQLite implementation into this
      // process; their process-scoped POSIX locks cannot safely coordinate.
      const metrics = await readHostMetrics(host.value);
      const expectedMessages = options.inputRecords + (index + 1) * options.appendRecords;
      if (!ingestProfileSkip && metrics.canonicalMessages !== expectedMessages) {
        throw new Error(
          `observation did not converge: expected ${expectedMessages} canonical messages, found ${metrics.canonicalMessages}`,
        );
      }
      const sample: Sample = {
        ...metrics,
        readyMs,
        durationMs,
        searchReadyMs: null,
        convergencePasses,
        inputBytes,
        inputRecords: options.appendRecords,
        mibPerSecond: durationMs === 0 ? 0 : inputBytes / 1024 / 1024 / (durationMs / 1000),
        recordsPerSecond: durationMs === 0 ? 0 : options.appendRecords / (durationMs / 1000),
      };
      logSample(sample, measured ? `run ${index - options.warmup + 1}` : `warmup ${index + 1}`);
      if (measured) samples.push(sample);
    }
  } finally {
    await host.value.dispose();
  }
  const finalMetrics = readDatabaseMetrics(options.databasePath);
  finalMetrics.performance = samples.at(-1)?.performance ?? null;
  assertSyntheticConvergence(
    options.inputRecords + (options.warmup + options.runs) * options.appendRecords,
    finalMetrics,
  );
  return { samples, finalMetrics };
}

async function readHostMetrics(host: Awaited<ReturnType<typeof openObservationHost>>): Promise<DatabaseMetrics> {
  const stats = await host.client.getStats();
  return {
    canonicalMessages: stats.searchableMessages,
    facts: stats.factRecords,
    commits: stats.ingestCommits,
    changeLogRows: null,
    changeLogPayloadBytes: null,
    databaseBytes: stats.allocatedDatabaseBytes,
    performance: stats.performance ?? null,
  };
}

function logSample(sample: Sample, label: string): void {
  const search = sample.searchReadyMs === null ? '' : `, ${formatDuration(sample.searchReadyMs)} search`;
  console.log(
    `  ${label}: ${formatDuration(sample.durationMs)} history ` +
      `(${formatDuration(sample.readyMs)} catalog${search}, ${sample.convergencePasses} repair passes), ` +
      `${sample.recordsPerSecond.toFixed(0)} records/s, ${sample.mibPerSecond.toFixed(2)} MiB/s, ` +
      `${sample.commits.toLocaleString()} commits, ${formatBytes(sample.databaseBytes)} DB`,
  );
}

async function runSample(options: {
  sourceRoot: string;
  transcript?: string;
  inputBytes: number;
  inputRecords: number;
  scenario: Scenario;
  appendRecords: number;
  iteration: number;
  databasePath: string;
}): Promise<Sample> {
  let sampleStartedAt = performance.now();
  let host = await timedOpen(options.sourceRoot, options.databasePath);
  let readyMs = host.durationMs;
  let benchmarkBytes = options.inputBytes;
  let benchmarkRecords = options.inputRecords;

  if (options.scenario !== 'cold') {
    await converge(host.value);
    if (options.scenario === 'warm-append' || options.scenario === 'live-append') {
      if (!options.transcript) fail('append scenarios require the synthetic corpus');
      const beforeBytes = statSync(options.transcript).size;
      appendSyntheticRecords(
        options.transcript,
        options.inputRecords + options.iteration * options.appendRecords,
        options.appendRecords,
      );
      benchmarkBytes = statSync(options.transcript).size - beforeBytes;
      benchmarkRecords = options.appendRecords;
    }
    sampleStartedAt = performance.now();
    if (options.scenario === 'live-append') {
      await host.value.refresh('claude-code');
      readyMs = performance.now() - sampleStartedAt;
    } else {
      await host.value.dispose();
      host = await timedOpen(options.sourceRoot, options.databasePath);
      readyMs = host.durationMs;
    }
  }

  const convergencePasses = await converge(host.value);
  const durationMs = performance.now() - sampleStartedAt;
  const searchReadyMs = (await awaitSearchReady(host.value)) ? performance.now() - sampleStartedAt : null;
  const metrics = await finishSample(host.value, options.databasePath);
  if (options.transcript && !ingestProfileSkip) {
    const appends = options.scenario === 'warm-append' || options.scenario === 'live-append';
    const expectedMessages = options.inputRecords + (appends ? (options.iteration + 1) * options.appendRecords : 0);
    assertSyntheticConvergence(expectedMessages, metrics);
  }
  return {
    ...metrics,
    readyMs,
    durationMs,
    searchReadyMs,
    convergencePasses,
    inputBytes: benchmarkBytes,
    inputRecords: benchmarkRecords,
    mibPerSecond: durationMs === 0 ? 0 : benchmarkBytes / 1024 / 1024 / (durationMs / 1000),
    recordsPerSecond: durationMs === 0 ? 0 : benchmarkRecords / (durationMs / 1000),
  };
}

async function finishSample(
  host: Awaited<ReturnType<typeof openObservationHost>>,
  databasePath: string,
): Promise<DatabaseMetrics> {
  const ownerMetrics = await readHostMetrics(host);
  await host.dispose();
  return { ...readDatabaseMetrics(databasePath), performance: ownerMetrics.performance };
}

function assertSyntheticConvergence(expectedMessages: number, metrics: DatabaseMetrics): void {
  if (metrics.canonicalMessages !== expectedMessages) {
    throw new Error(
      `observation did not converge: expected ${expectedMessages} canonical messages, found ${metrics.canonicalMessages}`,
    );
  }
}

async function timedOpen(sourceRoot: string, databasePath: string) {
  const startedAt = performance.now();
  const value = await openObservationHost({
    dbPath: databasePath,
    queryWorkers: 1,
    ownerLabel: 'observation-benchmark',
    sources: [{ adapterId: 'claude-code', roots: [sourceRoot] }],
  });
  try {
    assertCatalogVisible(value);
  } catch (error) {
    await value.dispose();
    throw error;
  }
  return { value, durationMs: performance.now() - startedAt };
}

/**
 * Wait for history to converge the way an application does.
 *
 * The supervisor drains its own backlog in the background after catalog-first
 * startup, so the benchmark observes that convergence instead of driving it:
 * `refresh` marks the whole adapter dirty and forces a full corpus rescan, so
 * a poll loop built on `refresh` measures repeated rescans rather than
 * ingestion. Bounded repair passes remain as a backstop for corpora the
 * supervisor cannot settle on its own, and their count is reported.
 */
async function converge(host: Awaited<ReturnType<typeof openObservationHost>>): Promise<number> {
  // Catalog-first startup returns before the supervisors are running; a
  // `refresh` issued before that fails with "supervisor is not running".
  await host.whenObserving();
  const deadline = performance.now() + CONVERGENCE_TIMEOUT_MS;
  while (performance.now() < deadline) {
    if (observationIsConverged(host) && (await historyIsReady(host))) return 0;
    await delay(CONVERGENCE_POLL_MS);
  }
  // The supervisor did not settle on its own; fall back to bounded repair.
  for (let pass = 1; pass <= 10_000; pass += 1) {
    await host.refresh('claude-code');
    if (observationIsConverged(host) && (await historyIsReady(host))) return pass;
  }
  throw new Error('observation did not converge after 10,000 bounded repair passes');
}

async function historyIsReady(host: Awaited<ReturnType<typeof openObservationHost>>): Promise<boolean> {
  const readiness = await host.readiness();
  return readiness.history.state === 'ready';
}

/**
 * Full-text structures are deferred during a cold ingest and rebuilt once, so
 * they become queryable after history does. Returns false if they never do.
 */
async function awaitSearchReady(host: Awaited<ReturnType<typeof openObservationHost>>): Promise<boolean> {
  const deadline = performance.now() + SEARCH_READY_TIMEOUT_MS;
  while (performance.now() < deadline) {
    const readiness = await host.readiness();
    if (readiness.search.state === 'ready') return true;
    await delay(CONVERGENCE_POLL_MS);
  }
  return false;
}

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, milliseconds);
  });
}

/**
 * Catalog-first startup (RFC 012B) resolves `openObservationHost` once the
 * catalog is committed; history, usage and search converge afterwards. A host
 * that still needs recovery, however, never opened cleanly.
 */
function assertCatalogVisible(host: Awaited<ReturnType<typeof openObservationHost>>): void {
  const observation = host.status.observation;
  if (!observation.recoveryRequired) return;
  throw new Error(
    'observation host opened in recovery: ' +
      `state=${observation.state}, inFlight=${observation.reconcileInFlight}, ` +
      `full=${observation.fullReconcileRequired}, dirtyInstances=${observation.dirtyInstances}`,
  );
}

function observationIsConverged(host: Awaited<ReturnType<typeof openObservationHost>>): boolean {
  const observation = host.status.observation;
  return (
    !observation.reconcileInFlight &&
    !observation.recoveryRequired &&
    !observation.fullReconcileRequired &&
    observation.dirtyInstances === 0
  );
}

function createSyntheticCorpus(root: string, recordCount: number, objectCount: number): string {
  const project = path.join(root, 'projects', '-benchmark-project');
  mkdirSync(project, { recursive: true });
  let nextRecord = 0;
  let firstTranscript = '';
  for (let objectIndex = 0; objectIndex < objectCount; objectIndex += 1) {
    const suffix = (objectIndex + 1).toString(16).padStart(12, '0');
    const sessionId = `00000000-0000-4000-8000-${suffix}`;
    const transcript = path.join(project, `${sessionId}.jsonl`);
    if (!firstTranscript) firstTranscript = transcript;
    const count = Math.floor(recordCount / objectCount) + (objectIndex < recordCount % objectCount ? 1 : 0);
    const chunks: string[] = [];
    for (let local = 0; local < count; local += 1) {
      chunks.push(syntheticRecord(nextRecord, sessionId));
      nextRecord += 1;
    }
    writeFileSync(transcript, chunks.join(''));
  }
  return firstTranscript;
}

function appendSyntheticRecords(transcript: string, start: number, count: number): void {
  const chunks: string[] = [];
  for (let index = start; index < start + count; index += 1) chunks.push(syntheticRecord(index));
  appendFileSync(transcript, chunks.join(''));
}

function syntheticRecord(index: number, sessionId = '00000000-0000-4000-8000-000000000001'): string {
  const suffix = index.toString(16).padStart(12, '0');
  const uuid = `00000000-0000-4000-8000-${suffix}`;
  const common = {
    uuid,
    parentUuid: null,
    timestamp: new Date(Date.UTC(2026, 0, 1, 0, 0, index)).toISOString(),
    sessionId,
    cwd: '/benchmark/project',
    version: '1.0.0',
    gitBranch: 'main',
    isSidechain: false,
    userType: 'external',
  };
  const record =
    index % 2 === 0
      ? {
          type: 'user',
          ...common,
          message: { role: 'user', content: `benchmark prompt ${index}` },
        }
      : {
          type: 'assistant',
          ...common,
          requestId: `request-${index}`,
          message: {
            model: 'claude-benchmark',
            id: `message-${index}`,
            type: 'message',
            role: 'assistant',
            content: [{ type: 'text', text: `benchmark response ${index}` }],
            stop_reason: 'end_turn',
            stop_sequence: null,
            usage: {
              input_tokens: 100,
              output_tokens: 20,
              cache_creation_input_tokens: 0,
              cache_read_input_tokens: 0,
            },
          },
        };
  return `${JSON.stringify(record)}\n`;
}

function readDatabaseMetrics(databasePath: string): DatabaseMetrics {
  const database = new DatabaseSync(databasePath, { readOnly: true });
  try {
    const scalar = (sql: string): number => Number((database.prepare(sql).get() as { value: number }).value);
    return {
      canonicalMessages: scalar('SELECT COUNT(*) AS value FROM canonical_messages'),
      facts: scalar('SELECT COUNT(*) AS value FROM fact_records'),
      commits: scalar('SELECT COUNT(*) AS value FROM ingest_commits'),
      changeLogRows: scalar('SELECT COUNT(*) AS value FROM change_log'),
      changeLogPayloadBytes: scalar('SELECT COALESCE(SUM(length(payload)), 0) AS value FROM change_log'),
      databaseBytes: statSync(databasePath).size,
      performance: null,
    };
  } finally {
    database.close();
  }
}

function directoryBytes(root: string): number {
  let bytes = 0;
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    const item = path.join(root, entry.name);
    bytes += entry.isDirectory() ? directoryBytes(item) : statSync(item).size;
  }
  return bytes;
}

function countJsonLines(root: string): number {
  let count = 0;
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    const item = path.join(root, entry.name);
    if (entry.isDirectory()) count += countJsonLines(item);
    else if (entry.name.endsWith('.jsonl')) {
      const descriptor = openSync(item, 'r');
      const buffer = Buffer.allocUnsafe(64 * 1024);
      try {
        let bytesRead = 0;
        do {
          bytesRead = readSync(descriptor, buffer, 0, buffer.length, null);
          for (let index = 0; index < bytesRead; index += 1) {
            if (buffer[index] === 0x0a) count += 1;
          }
        } while (bytesRead > 0);
      } finally {
        closeSync(descriptor);
      }
    }
  }
  return count;
}

function summarize(
  selectedScenario: Scenario,
  selectedRecords: number,
  selectedAppendRecords: number,
  samples: Sample[],
  finalMetrics: DatabaseMetrics,
): Summary {
  const durationSamples = samples.map((sample) => sample.durationMs);
  const finalRssBytes = process.memoryUsage().rss;
  peakRssBytes = Math.max(peakRssBytes, finalRssBytes);
  return {
    scenario: selectedScenario,
    records: selectedRecords,
    appendRecords: selectedAppendRecords,
    samples,
    finalMetrics,
    durationMs: distribution(durationSamples),
    readyMs: distribution(samples.map((sample) => sample.readyMs)),
    medianConvergencePasses: median(samples.map((sample) => sample.convergencePasses)),
    medianMibPerSecond: median(samples.map((sample) => sample.mibPerSecond)),
    medianRecordsPerSecond: median(samples.map((sample) => sample.recordsPerSecond)),
    memory: {
      baselineRssBytes,
      finalRssBytes,
      peakRssBytes,
      peakRssDeltaBytes: Math.max(0, peakRssBytes - baselineRssBytes),
    },
  };
}

function printSummary(summary: Summary, selectedReportPath?: string): void {
  console.log(
    `  p50/p99: ${formatDuration(summary.durationMs.p50)} / ${formatDuration(summary.durationMs.p99)}, ` +
      `${summary.medianRecordsPerSecond.toFixed(0)} records/s, ` +
      `${summary.medianMibPerSecond.toFixed(2)} MiB/s, ` +
      `catalog p50 ${formatDuration(summary.readyMs.p50)}, ` +
      `${summary.medianConvergencePasses} convergence passes`,
  );
  printPerformanceSummary(summary.finalMetrics.performance);
  console.log(
    `  process memory: ${formatBytes(summary.memory.peakRssBytes)} peak RSS ` +
      `(+${formatBytes(summary.memory.peakRssDeltaBytes)} from harness baseline)`,
  );
  console.log(
    `  final: ${summary.finalMetrics.canonicalMessages.toLocaleString()} messages, ` +
      `${summary.finalMetrics.facts.toLocaleString()} facts, ` +
      `${summary.finalMetrics.commits.toLocaleString()} commits, ` +
      `${formatBytes(summary.finalMetrics.databaseBytes)} DB`,
  );
  if (selectedReportPath) {
    mkdirSync(path.dirname(selectedReportPath), { recursive: true });
    writeFileSync(selectedReportPath, `${JSON.stringify(summary, null, 2)}\n`);
    console.log(`  report: ${selectedReportPath}`);
  }
}

function requireLastSample(samples: Sample[]): DatabaseMetrics {
  const sample = samples.at(-1);
  if (!sample) fail('benchmark produced no measured samples');
  return {
    canonicalMessages: sample.canonicalMessages,
    facts: sample.facts,
    commits: sample.commits,
    changeLogRows: sample.changeLogRows,
    changeLogPayloadBytes: sample.changeLogPayloadBytes,
    databaseBytes: sample.databaseBytes,
    performance: sample.performance,
  };
}

function printPerformanceSummary(performance: SpaghettiEnginePerformanceStats | null): void {
  if (!performance) return;
  const writerTimings = new Map(performance.writer.timings.map((timing) => [timing.name, timing.latency]));
  const stageNames = [
    'prepare',
    'canonical_projection',
    'runtime_projection',
    'usage_projection',
    'change_log',
    'sqlite_commit',
    'bootstrap_finalize',
  ];
  const stages = stageNames
    .map((name) => {
      const latency = writerTimings.get(name);
      return latency ? `${name}=${latency.totalMs.toFixed(1)}ms` : null;
    })
    .filter((value): value is string => value !== null)
    .join(', ');
  console.log(
    `  native writer: ${performance.writer.sqliteRowsChanged.toLocaleString()} SQLite row changes, ` +
      `queue high-water ${performance.writer.queueHighWatermark}; ${stages}`,
  );
  const sourceTimings = new Map(performance.source.totals.timings.map((timing) => [timing.name, timing.latency]));
  console.log(
    `  native source: ${performance.source.totals.recordsRead.toLocaleString()} records / ` +
      `${formatBytes(performance.source.totals.payloadBytesRead)} read, ` +
      `${performance.source.totals.factsEmitted.toLocaleString()} facts, ` +
      `${performance.source.totals.readContinuations.toLocaleString()} bounded continuations / ` +
      `${performance.source.totals.readRetries.toLocaleString()} retries; ` +
      `read=${(sourceTimings.get('source_read')?.totalMs ?? 0).toFixed(1)}ms, ` +
      `decode=${(sourceTimings.get('decode_total')?.totalMs ?? 0).toFixed(1)}ms, ` +
      `adapter=${(sourceTimings.get('adapter_decode')?.totalMs ?? 0).toFixed(1)}ms, ` +
      `fact-build=${(sourceTimings.get('fact_build')?.totalMs ?? 0).toFixed(1)}ms`,
  );
  const projectors = performance.writer.timings
    .filter((timing) => timing.name.startsWith('projector.'))
    .sort((left, right) => right.latency.totalMs - left.latency.totalMs)
    .slice(0, 5)
    .map((timing) => `${timing.name.slice('projector.'.length)}=${timing.latency.totalMs.toFixed(1)}ms`)
    .join(', ');
  console.log(`  native projectors: ${projectors}`);
  console.log(
    `  native pressure: ${formatBytes(performance.storage.walFileBytes)} WAL, ` +
      `${performance.queries.queueHighWatermark} query queue high-water, ` +
      `${performance.queries.oldestActiveMs.toFixed(1)}ms oldest active reader`,
  );
  const checkpoint = performance.writer.checkpoint;
  console.log(
    `  native checkpoints: ${checkpoint.completed}/${checkpoint.attempts} completed, ` +
      `${checkpoint.blocked} reader-blocked, ${checkpoint.lastRemainingFrames.toLocaleString()} frames remaining, ` +
      `${checkpoint.blockedByReaderMs.toFixed(1)}ms blocked time`,
  );
}

function distribution(values: number[]): Distribution {
  return {
    min: Math.min(...values),
    p50: percentile(values, 0.5),
    median: median(values),
    p95: percentile(values, 0.95),
    p99: percentile(values, 0.99),
    mean: values.reduce((sum, value) => sum + value, 0) / values.length,
    max: Math.max(...values),
  };
}

function median(values: number[]): number {
  return percentile(values, 0.5);
}

function percentile(values: number[], quantile: number): number {
  const sorted = [...values].sort((left, right) => left - right);
  const rank = (sorted.length - 1) * quantile;
  const lower = Math.floor(rank);
  const upper = Math.ceil(rank);
  return sorted[lower] + (sorted[upper] - sorted[lower]) * (rank - lower);
}

function positiveInteger(value: string, name: string): number {
  const parsed = Number.parseInt(value, 10);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) fail(`--${name} must be a positive integer`);
  return parsed;
}

function nonnegativeInteger(value: string, name: string): number {
  const parsed = Number.parseInt(value, 10);
  if (!Number.isSafeInteger(parsed) || parsed < 0) fail(`--${name} must be a non-negative integer`);
  return parsed;
}

function expandTilde(value: string): string {
  return value.startsWith('~/') ? path.join(homedir(), value.slice(2)) : value;
}

function formatDuration(milliseconds: number): string {
  return milliseconds < 1_000 ? `${milliseconds.toFixed(1)}ms` : `${(milliseconds / 1_000).toFixed(2)}s`;
}

function formatBytes(bytes: number): string {
  return bytes < 1024 * 1024 ? `${(bytes / 1024).toFixed(1)} KiB` : `${(bytes / 1024 / 1024).toFixed(1)} MiB`;
}

function fail(message: string): never {
  console.error(message);
  process.exit(2);
}
