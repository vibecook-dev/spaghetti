#!/usr/bin/env -S tsx
/**
 * bench-ingest.ts — wall-clock benchmark for the Rust and TS ingest paths.
 *
 * Runs each path multiple times against a given fixture (default: the
 * committed small fixture), reports min / median / max / mean, and prints
 * a side-by-side comparison.
 *
 * Usage:
 *   pnpm bench:ingest                           # small fixture, both paths, 3 runs
 *   pnpm bench:ingest --fixture ~/.claude       # your real claude dir
 *   pnpm bench:ingest --runs 10 --parallelism 4 # specific parallelism
 *   pnpm bench:ingest --only rust               # skip the TS path
 *   pnpm bench:ingest --warmup 0                # skip warmup (default is 1)
 *   pnpm bench:ingest --mode warm                # warm-start, both engines
 *   pnpm bench:ingest --mode warm --scenario growth   # a day of new messages
 *
 * `--mode warm` seeds each engine's DB with one full ingest, then measures
 * subsequent warm ingests. `--scenario` states what changed between runs:
 * `unchanged` (the fast path), `growth` (a day of new messages), `deletion`
 * (a removed session), or `repair` (a stale ingest-contract version, which
 * defeats the fast path and forces a full rebuild).
 *
 * Both engines are measured. RFC 008 Phase 4 accepts the Rust full-source
 * warm path when its median is no worse than max(2 x TS median, 3s), and that
 * comparison needs the TS incremental warm path on the same corpus.
 *
 *   pnpm bench:ingest --report-json <path>      # write a machine-readable report
 *   pnpm bench:ingest --compare-to <baseline>   # compare to baseline, exit 1 on regression
 *
 * Exit codes:
 *   0 — bench completed (and all compared metrics within threshold).
 *   1 — a run failed, or a compared metric regressed past its threshold.
 *   2 — bad args / fixture missing.
 */

import { createRequire } from 'node:module';
import {
  appendFileSync,
  cpSync,
  existsSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { arch, cpus, homedir, platform, tmpdir, totalmem } from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

import { createSpaghettiService } from '../packages/sdk/dist/index.js';

// Resolve the native addon from the SDK package — under pnpm's strict
// workspace layout, `@vibecook/spaghetti-sdk-native` is only hoisted into
// `packages/sdk/node_modules/`, so a plain require from `scripts/` misses it.
const sdkPkgJson = new URL('../packages/sdk/package.json', import.meta.url);
const require = createRequire(sdkPkgJson);

// ─── CLI ────────────────────────────────────────────────────────────────────

const { values } = parseArgs({
  options: {
    fixture: { type: 'string' },
    runs: { type: 'string' },
    warmup: { type: 'string' },
    parallelism: { type: 'string' },
    only: { type: 'string' }, // 'rust' | 'ts'
    mode: { type: 'string' }, // 'cold' | 'warm'
    scenario: { type: 'string' }, // 'unchanged' | 'growth' | 'deletion' | 'repair'
    'report-json': { type: 'string' },
    'compare-to': { type: 'string' },
  },
});

// `fileURLToPath`, not `new URL(...).pathname` — see the note in
// scripts/ingest-diff.ts; the latter resolves to `D:\D:\repo` on Windows.
const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const defaultFixture = path.join(repoRoot, 'crates/spaghetti-napi/fixtures/small/.claude');
const fixtureRootDir = path.resolve(expandTilde(values.fixture ?? defaultFixture));

const runs = parseIntOrDie(values.runs ?? '3', 'runs');
const warmup = parseIntOrDie(values.warmup ?? '1', 'warmup');
const parallelism = values.parallelism ? parseIntOrDie(values.parallelism, 'parallelism') : undefined;
const only = values.only as 'rust' | 'ts' | undefined;
const mode = (values.mode ?? 'cold') as 'cold' | 'warm';
const reportJsonPath = values['report-json'] ? path.resolve(expandTilde(values['report-json'])) : undefined;
const compareToPath = values['compare-to'] ? path.resolve(expandTilde(values['compare-to'])) : undefined;

if (only && only !== 'rust' && only !== 'ts') {
  console.error(`--only must be 'rust' or 'ts', got: ${only}`);
  process.exit(2);
}

if (mode !== 'cold' && mode !== 'warm') {
  console.error(`--mode must be 'cold' or 'warm', got: ${mode}`);
  process.exit(2);
}

// ─── Warm scenarios (RFC 008 Phase 4) ───────────────────────────────────────
//
// A warm benchmark is only meaningful against a stated change. "Warm" alone
// conflates the fast path (nothing changed, ~60ms) with a full rebuild
// (something changed, seconds) — numbers that differ by two orders of
// magnitude and answer different questions.
//
// Every scenario mutates a working copy of the fixture, never the committed
// one, and runs before each measured iteration so every sample sees the same
// kind of change.

type Scenario = 'unchanged' | 'growth' | 'deletion' | 'repair';

const scenario = (values.scenario ?? 'unchanged') as Scenario;
if (!['unchanged', 'growth', 'deletion', 'repair'].includes(scenario)) {
  console.error(`--scenario must be unchanged|growth|deletion|repair, got: ${scenario}`);
  process.exit(2);
}
if (scenario !== 'unchanged' && mode !== 'warm') {
  console.error(`--scenario ${scenario} only applies to --mode warm`);
  process.exit(2);
}

/**
 * The tree the benchmark actually reads.
 *
 * Cold runs and the unchanged scenario read the fixture in place. Anything
 * that mutates works on a copy, so a benchmark can never leave the committed
 * fixture — or a developer's real `~/.claude` — modified.
 */
const benchDir = scenario === 'unchanged' ? fixtureRootDir : path.join(tmpdir(), 'bench-ingest-worktree');

if (scenario !== 'unchanged') {
  rmSync(benchDir, { recursive: true, force: true });
  cpSync(fixtureRootDir, benchDir, { recursive: true });
}

/** Every session JSONL under the bench tree, sorted for determinism. */
function sessionFiles(): string[] {
  const out: string[] = [];
  const projects = path.join(benchDir, 'projects');
  if (!existsSync(projects)) return out;
  for (const slug of readdirSync(projects).sort()) {
    const dir = path.join(projects, slug);
    if (!statSync(dir).isDirectory()) continue;
    for (const f of readdirSync(dir).sort()) {
      if (f.endsWith('.jsonl')) out.push(path.join(dir, f));
    }
  }
  return out;
}

let scenarioCursor = 0;

/**
 * One day of activity: append ~20 messages to a session, moving to a
 * different session each iteration so repeated runs are not all re-reading
 * the same hot file.
 */
function applyGrowth(): void {
  const files = sessionFiles();
  if (files.length === 0) return;
  const target = files[scenarioCursor % files.length];
  scenarioCursor += 1;
  const sessionId = path.basename(target, '.jsonl');
  let body = '';
  for (let i = 0; i < 20; i++) {
    body +=
      JSON.stringify({
        type: 'user',
        uuid: `bench-${scenarioCursor}-${i}`,
        timestamp: new Date().toISOString(),
        sessionId,
        isSidechain: false,
        userType: 'external',
        cwd: '/',
        version: '1',
        gitBranch: 'main',
        message: { role: 'user', content: `bench growth ${scenarioCursor}-${i}` },
      }) + '\n';
  }
  appendFileSync(target, body);
}

/** A session the user deleted — the case that must not leave orphan rows. */
function applyDeletion(): void {
  const files = sessionFiles();
  if (files.length === 0) return;
  rmSync(files[scenarioCursor % files.length], { force: true });
  scenarioCursor += 1;
}

/**
 * Forced upgrade repair: every file matches, but the stored ingest-contract
 * version is stale, so the warm fast path must be defeated and the source
 * rebuilt (RFC 008 Phase 1.3). This is the worst warm case by construction.
 */
function applyRepair(dbPath?: string): void {
  if (!dbPath) return;
  if (!existsSync(dbPath)) return;
  // Required lazily: `require` is built from the SDK package above, and
  // this runs only in the repair scenario.
  const { DatabaseSync } = require('node:sqlite') as typeof import('node:sqlite');
  const db = new DatabaseSync(dbPath);
  // Loud on purpose. The first version of this wrote `WHERE materialization =`
  // — not a column — inside a try/catch, so the UPDATE errored, the scenario
  // silently did nothing, and the benchmark reported fast-path timings as if
  // they were a forced rebuild. A scenario that quietly fails to apply is
  // worse than one that crashes.
  const res = db
    .prepare(`UPDATE source_materializations SET version = 0 WHERE projection = ?`)
    .run('rust-ingest-contract');
  db.close();
  if (res.changes === 0) {
    throw new Error(
      'repair scenario: no rust-ingest-contract row to invalidate — the DB was ' +
        'not seeded by the Rust engine, so the fast path would not be defeated',
    );
  }
}

let currentDbPath: string | undefined;

function applyScenario(): void {
  switch (scenario) {
    case 'growth':
      return applyGrowth();
    case 'deletion':
      return applyDeletion();
    case 'repair':
      return applyRepair(currentDbPath);
    case 'unchanged':
      return;
  }
}

if (!existsSync(fixtureRootDir)) {
  console.error(`fixture not found: ${fixtureRootDir}`);
  process.exit(2);
}

// ─── Helpers ────────────────────────────────────────────────────────────────

function expandTilde(p: string): string {
  // `homedir()`, not `process.env.HOME` — HOME is not set on Windows
  // outside of shells that emulate it, where the empty fallback would
  // silently turn `~/x` into a relative path.
  if (p.startsWith('~/')) return path.join(homedir(), p.slice(2));
  return p;
}

function parseIntOrDie(s: string, name: string): number {
  const n = Number.parseInt(s, 10);
  if (!Number.isFinite(n) || n < 0) {
    console.error(`--${name} must be a non-negative integer, got: ${s}`);
    process.exit(2);
  }
  return n;
}

function cleanDb(p: string): void {
  for (const suffix of ['', '-wal', '-shm', '-journal']) {
    if (existsSync(p + suffix)) rmSync(p + suffix, { force: true });
  }
}

function summarize(label: string, msSamples: number[]): Summary {
  const sorted = [...msSamples].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  const median = sorted.length % 2 === 0 ? (sorted[mid - 1] + sorted[mid]) / 2 : sorted[mid];
  const min = sorted[0];
  const max = sorted[sorted.length - 1];
  const mean = msSamples.reduce((a, b) => a + b, 0) / msSamples.length;
  return { label, samples: msSamples, min, median, mean, max };
}

interface Summary {
  label: string;
  samples: number[];
  min: number;
  median: number;
  mean: number;
  max: number;
}

function formatMs(n: number): string {
  if (n < 10) return `${n.toFixed(2)}ms`;
  if (n < 1_000) return `${n.toFixed(1)}ms`;
  return `${(n / 1_000).toFixed(2)}s`;
}

function printSummary(s: Summary): void {
  console.log(
    `  ${s.label.padEnd(6)}  min ${formatMs(s.min).padStart(8)}   med ${formatMs(s.median).padStart(8)}   mean ${formatMs(s.mean).padStart(8)}   max ${formatMs(s.max).padStart(8)}`,
  );
  const samples = s.samples.map(formatMs).join('  ');
  console.log(`          samples: ${samples}`);
}

// ─── Runners ────────────────────────────────────────────────────────────────

interface NativeAddon {
  ingest(opts: {
    agentDir: string;
    dbPath: string;
    mode: 'cold' | 'warm';
    parallelism?: number;
    sourceId?: string;
  }): Promise<{ durationMs: number }>;
  nativeVersion(): string;
}

async function runRustOnce(dbPath: string): Promise<number> {
  // Cold mode: fresh DB on every run. Warm mode: reuse the seeded DB.
  if (mode === 'cold') cleanDb(dbPath);
  const native = require('@vibecook/spaghetti-sdk-native') as NativeAddon;
  const t0 = performance.now();
  await native.ingest({
    agentDir: benchDir,
    dbPath,
    mode,
    parallelism,
  });
  return performance.now() - t0;
}

async function seedWarmDb(dbPath: string): Promise<void> {
  cleanDb(dbPath);
  const native = require('@vibecook/spaghetti-sdk-native') as NativeAddon;
  await native.ingest({
    agentDir: benchDir,
    dbPath,
    mode: 'cold',
    parallelism,
  });
}

async function runTsOnce(dbPath: string): Promise<number> {
  // Warm mode reuses the seeded DB on purpose. `initialize()` takes the TS
  // incremental warm path when fingerprints already exist, which is the path
  // RFC 008 Phase 4 compares the Rust full-source path against. Cleaning here
  // would have measured a cold start and called it warm.
  if (mode === 'cold') cleanDb(dbPath);
  const svc = createSpaghettiService({ rootDir: benchDir, dbPath });
  const t0 = performance.now();
  await svc.initialize();
  svc.shutdown();
  return performance.now() - t0;
}

async function seedTsWarmDb(dbPath: string): Promise<void> {
  cleanDb(dbPath);
  const svc = createSpaghettiService({ rootDir: benchDir, dbPath });
  await svc.initialize();
  svc.shutdown();
}

async function runBench(label: string, fn: (dbPath: string) => Promise<number>): Promise<Summary> {
  const dbPath = path.join(tmpdir(), `bench-ingest-${label.toLowerCase()}.db`);
  currentDbPath = dbPath;

  // For warm mode we seed the DB with one cold run before any warm
  // measurement can be meaningful.
  if (mode === 'warm') {
    if (label === 'rust') await seedWarmDb(dbPath);
    else await seedTsWarmDb(dbPath);
  }

  for (let i = 0; i < warmup; i++) {
    if (mode === 'warm') applyScenario();
    await fn(dbPath);
  }
  const samples: number[] = [];
  for (let i = 0; i < runs; i++) {
    // Mutate before each measured run so every sample sees the same *kind* of
    // change. Without this the second run of a growth scenario would find
    // nothing changed and time the fast path instead.
    if (mode === 'warm') applyScenario();
    samples.push(await fn(dbPath));
  }
  cleanDb(dbPath);
  return summarize(label, samples);
}

// ─── Report + compare ───────────────────────────────────────────────────────

interface BaselineEntry {
  target: number | null;
  regression_threshold_pct: number;
}

interface BaselineFile {
  cold_start_ms_p50?: BaselineEntry;
  warm_start_ms_p50?: BaselineEntry;
}

type ReportSummary = Pick<Summary, 'min' | 'median' | 'mean' | 'max' | 'samples'>;

function toReportSummary(s: Summary): ReportSummary {
  return { min: s.min, median: s.median, mean: s.mean, max: s.max, samples: s.samples };
}

/**
 * Machine identity for the report.
 *
 * RFC 008 Phase 4 accepts the Rust warm path at `max(2 x TS median, 3s)`. The
 * ratio half is self-normalizing — both engines run back to back here, so
 * hardware cancels. The 3-second floor does not, and neither does any archived
 * number read months later. Record what produced them.
 */
function hostInfo(): {
  platform: string;
  arch: string;
  cpuModel: string;
  cpuCount: number;
  totalMemBytes: number;
  node: string;
} {
  const cpu = cpus();
  return {
    platform: platform(),
    arch: arch(),
    cpuModel: cpu[0]?.model?.trim() ?? 'unknown',
    cpuCount: cpu.length,
    totalMemBytes: totalmem(),
    node: process.version,
  };
}

function writeReport(results: Summary[]): void {
  if (!reportJsonPath) return;
  const native = require('@vibecook/spaghetti-sdk-native') as NativeAddon;
  const rust = results.find((r) => r.label === 'rust');
  const ts = results.find((r) => r.label === 'ts');
  const report: {
    runs: number;
    warmup: number;
    fixture: string;
    native: string;
    host: ReturnType<typeof hostInfo>;
    cold: { rust?: ReportSummary; ts?: ReportSummary };
    warm: { rust?: ReportSummary };
  } = {
    runs,
    warmup,
    fixture: fixtureRootDir,
    native: native.nativeVersion(),
    host: hostInfo(),
    cold: {},
    warm: {},
  };
  if (mode === 'cold') {
    if (rust) report.cold.rust = toReportSummary(rust);
    if (ts) report.cold.ts = toReportSummary(ts);
  } else {
    if (rust) report.warm.rust = toReportSummary(rust);
  }
  writeFileSync(reportJsonPath, JSON.stringify(report, null, 2));
  console.log('');
  console.log(`report:        ${reportJsonPath}`);
}

interface CompareRow {
  metric: string;
  baseline: number | null;
  current: number;
  deltaPct: number | null;
  thresholdPct: number;
  verdict: 'pass' | 'fail' | 'skip';
}

function compareToBaseline(results: Summary[]): boolean {
  if (!compareToPath) return true;
  if (!existsSync(compareToPath)) {
    console.error(`--compare-to file not found: ${compareToPath}`);
    process.exit(2);
  }
  const baseline = JSON.parse(readFileSync(compareToPath, 'utf8')) as BaselineFile;
  const rust = results.find((r) => r.label === 'rust');

  const rows: CompareRow[] = [];

  // Only compare what actually ran. Cold → cold_start_ms_p50; warm → warm_start_ms_p50.
  // We compare Rust only (TS path is not gated in this first iteration).
  if (rust && mode === 'cold' && baseline.cold_start_ms_p50) {
    const entry = baseline.cold_start_ms_p50;
    rows.push(buildCompareRow('cold_start_ms_p50 (rust)', entry, rust.median));
  }
  if (rust && mode === 'warm' && baseline.warm_start_ms_p50) {
    const entry = baseline.warm_start_ms_p50;
    rows.push(buildCompareRow('warm_start_ms_p50 (rust)', entry, rust.median));
  }

  if (rows.length === 0) {
    console.log('');
    console.log(`baseline:      ${compareToPath}`);
    console.log('no matching baseline entries for this run — skipping comparison');
    return true;
  }

  console.log('');
  console.log(`baseline:      ${compareToPath}`);
  console.log('');
  console.log('  metric                        baseline   current     delta   verdict');
  console.log('  ----------------------------  ---------  --------  --------  -------');
  for (const row of rows) {
    const baselineStr = row.baseline === null ? '    null' : formatMs(row.baseline).padStart(9);
    const currentStr = formatMs(row.current).padStart(8);
    const deltaStr =
      row.deltaPct === null ? '     n/a' : `${row.deltaPct >= 0 ? '+' : ''}${row.deltaPct.toFixed(1)}%`.padStart(8);
    console.log(`  ${row.metric.padEnd(28)}  ${baselineStr}  ${currentStr}  ${deltaStr}  ${row.verdict}`);
  }

  const failed = rows.some((r) => r.verdict === 'fail');
  return !failed;
}

function buildCompareRow(metric: string, entry: BaselineEntry, current: number): CompareRow {
  if (entry.target === null) {
    return {
      metric,
      baseline: null,
      current,
      deltaPct: null,
      thresholdPct: entry.regression_threshold_pct,
      verdict: 'skip',
    };
  }
  const deltaPct = ((current - entry.target) / entry.target) * 100;
  const verdict = deltaPct > entry.regression_threshold_pct ? 'fail' : 'pass';
  return {
    metric,
    baseline: entry.target,
    current,
    deltaPct,
    thresholdPct: entry.regression_threshold_pct,
    verdict,
  };
}

// ─── Main ───────────────────────────────────────────────────────────────────

async function main(): Promise<void> {
  const native = require('@vibecook/spaghetti-sdk-native') as NativeAddon;

  console.log(`fixture:       ${fixtureRootDir}`);
  console.log(`mode:          ${mode}`);
  console.log(`runs:          ${runs} (+ ${warmup} warmup)`);
  if (parallelism !== undefined) console.log(`parallelism:   ${parallelism}`);
  console.log(`native:        ${native.nativeVersion()}`);
  console.log('');

  const results: Summary[] = [];

  if (only !== 'ts') {
    process.stdout.write('Rust ingest... ');
    results.push(await runBench('rust', runRustOnce));
    console.log('done');
  }

  if (only !== 'rust') {
    process.stdout.write('TS ingest...   ');
    results.push(await runBench('ts', runTsOnce));
    console.log('done');
  }

  console.log('');
  for (const r of results) printSummary(r);

  // Speedup summary when both ran.
  const rust = results.find((r) => r.label === 'rust');
  const ts = results.find((r) => r.label === 'ts');
  if (rust && ts) {
    console.log('');
    const speedup = ts.median / rust.median;
    console.log(
      `speedup (median): ${speedup.toFixed(2)}×   (TS ${formatMs(ts.median)} → Rust ${formatMs(rust.median)})`,
    );
  }

  writeReport(results);

  const ok = compareToBaseline(results);
  if (!ok) {
    console.log('');
    console.error('FAIL: one or more metrics regressed past their threshold.');
    process.exit(1);
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
