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
 *   pnpm bench:ingest --mode warm --only rust   # warm-start (0 changes) fast path
 *
 * `--mode warm` benchmarks the warm-start fast path: seeds the DB with
 * one cold run, then measures subsequent warm ingests without modifying
 * the fixture between runs. Only supported for --only rust.
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
import { existsSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
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

if (mode === 'warm' && only !== 'rust') {
  console.error(`--mode warm requires --only rust (TS warm-start path is not exposed to this bench)`);
  process.exit(2);
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
    agentDir: fixtureRootDir,
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
    agentDir: fixtureRootDir,
    dbPath,
    mode: 'cold',
    parallelism,
  });
}

async function runTsOnce(dbPath: string): Promise<number> {
  cleanDb(dbPath);
  const svc = createSpaghettiService({ rootDir: fixtureRootDir, dbPath });
  const t0 = performance.now();
  await svc.initialize();
  svc.shutdown();
  return performance.now() - t0;
}

async function runBench(label: string, fn: (dbPath: string) => Promise<number>): Promise<Summary> {
  const dbPath = path.join(tmpdir(), `bench-ingest-${label.toLowerCase()}.db`);

  // For warm mode we seed the DB with one cold run before any warm
  // measurement can be meaningful.
  if (mode === 'warm' && label === 'rust') {
    await seedWarmDb(dbPath);
  }

  for (let i = 0; i < warmup; i++) {
    await fn(dbPath);
  }
  const samples: number[] = [];
  for (let i = 0; i < runs; i++) {
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
    cold: { rust?: ReportSummary; ts?: ReportSummary };
    warm: { rust?: ReportSummary };
  } = {
    runs,
    warmup,
    fixture: fixtureRootDir,
    native: native.nativeVersion(),
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
