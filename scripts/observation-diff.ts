#!/usr/bin/env -S tsx
/**
 * RFC 011 canonical-observation differential.
 *
 * Unlike ingest-diff.ts, this command drives the AgentAdapter/common-driver
 * coordinator and compares RFC 011 canonical state. It deliberately emits
 * only counts and SHA-256 digests, so a private-corpus report cannot disclose
 * transcript content.
 */

import { createHash } from 'node:crypto';
import {
  closeSync,
  cpSync,
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readSync,
  readdirSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
  writeSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { DatabaseSync } from 'node:sqlite';
import { parseArgs } from 'node:util';

import {
  openObservationShadow,
  type ObservationShadow,
} from '../packages/sdk/src/observation-shadow.js';

type Scenario = 'cold' | 'live' | 'reconcile' | 'generation' | 'restart';
type Adapter = 'claude-code' | 'codex' | 'grok';
type SqlValue = null | number | string | Uint8Array;
type NormalizedValue = null | number | string | boolean | NormalizedValue[] | { [key: string]: NormalizedValue };

interface SemanticSnapshot {
  tables: Record<string, Array<Record<string, NormalizedValue>>>;
  cursors: Array<Record<string, NormalizedValue>>;
}

interface ScenarioReport {
  scenario: Scenario;
  exact: boolean;
  commitSeq: number;
  canonicalTableCounts: Record<string, number>;
  canonicalTableDigests: Record<string, string>;
  cursorCount: number;
  cursorDigest: string;
  mismatchedTables: string[];
  cursorsMatch: boolean;
}

const volatileColumn = /^(?:last_commit_seq|source_object_id|source_stream_id|source_instance_id|source_generation|observed_at|cursor_start|cursor_end|decisive_evidence_id)$/;
const volatileProvenanceColumn = /(?:^|_)(?:fact_id|source_object_id|source_instance_id|source_generation)$/;

const { values } = parseArgs({
  options: {
    adapter: { type: 'string' },
    fixture: { type: 'string' },
    modes: { type: 'string' },
    'snapshot-json': { type: 'string' },
    keep: { type: 'boolean' },
  },
});

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const adapterId = parseAdapter(values.adapter);
const defaultFixtures: Record<Adapter, string> = {
  'claude-code': 'crates/spaghetti-napi/fixtures/small/.claude',
  codex: 'crates/spaghetti-napi/fixtures/small-codex/.codex',
  grok: 'crates/spaghetti-napi/fixtures/small-grok/.grok',
};
const rootNames: Record<Adapter, string> = {
  'claude-code': '.claude',
  codex: '.codex',
  grok: '.grok',
};
const fixture = path.resolve(
  values.fixture ?? path.join(repoRoot, defaultFixtures[adapterId]),
);
const allScenarios: Scenario[] = ['cold', 'live', 'reconcile', 'generation', 'restart'];
const scenarios = parseScenarios(values.modes);
if (!existsSync(fixture) || !lstatSync(fixture).isDirectory()) {
  throw new Error(`observation fixture does not exist or is not a directory: ${fixture}`);
}

const runRoot = mkdtempSync(path.join(tmpdir(), 'spaghetti-observation-diff-'));
let failed = false;
try {
  const snapshots = new Map<Scenario, SemanticSnapshot>();
  const commitSeqs = new Map<Scenario, number>();
  for (const scenario of scenarios) {
    const result = await runScenario(scenario, path.join(runRoot, scenario));
    snapshots.set(scenario, result.snapshot);
    commitSeqs.set(scenario, result.commitSeq);
  }

  const baselineScenario = scenarios.includes('cold') ? 'cold' : scenarios[0]!;
  const baseline = snapshots.get(baselineScenario)!;
  const reports = scenarios.map((scenario) =>
    reportScenario(scenario, snapshots.get(scenario)!, baseline, commitSeqs.get(scenario)!),
  );
  failed = reports.some((report) => !report.exact);
  const output = {
    contractVersion: 2,
    adapterId,
    fixture: path.relative(repoRoot, fixture) || '.',
    baselineScenario,
    exact: !failed,
    scenarios: reports,
  };
  console.log(JSON.stringify(output, null, 2));
  if (values['snapshot-json']) {
    const outputPath = path.resolve(values['snapshot-json']);
    mkdirSync(path.dirname(outputPath), { recursive: true });
    writeFileSync(outputPath, `${JSON.stringify(output, null, 2)}\n`);
  }
} finally {
  if (values.keep) console.error(`kept observation differential workspace: ${runRoot}`);
  else rmSync(runRoot, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 });
}

if (failed) process.exitCode = 1;

function parseAdapter(raw: string | undefined): Adapter {
  const value = raw ?? 'claude-code';
  if (value === 'claude-code' || value === 'codex' || value === 'grok') return value;
  throw new Error('--adapter must be one of: claude-code, codex, grok');
}

function parseScenarios(raw: string | undefined): Scenario[] {
  if (!raw) return allScenarios;
  const parsed = raw
    .split(',')
    .map((value) => value.trim())
    .filter((value): value is Scenario => allScenarios.includes(value as Scenario));
  const invalid = raw
    .split(',')
    .map((value) => value.trim())
    .filter((value) => value && !allScenarios.includes(value as Scenario));
  if (invalid.length > 0 || parsed.length === 0) {
    throw new Error(`--modes must contain: ${allScenarios.join(', ')}; invalid: ${invalid.join(', ')}`);
  }
  return [...new Set(parsed)];
}

async function runScenario(
  scenario: Scenario,
  scenarioRoot: string,
): Promise<{ snapshot: SemanticSnapshot; commitSeq: number }> {
  // Every scenario represents the same logical installation. Keep its
  // canonical root path stable so deterministic source namespaces do not turn
  // independent temp-directory spellings into false semantic differences.
  const sourceRoot = path.join(path.dirname(scenarioRoot), 'shared-source', rootNames[adapterId]);
  const stagingRoot = path.join(scenarioRoot, 'staging');
  const databasePath = path.join(scenarioRoot, 'observation.db');
  mkdirSync(scenarioRoot, { recursive: true });
  rmSync(sourceRoot, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 });
  mkdirSync(path.dirname(sourceRoot), { recursive: true });
  cpSync(fixture, sourceRoot, { recursive: true, dereference: false, preserveTimestamps: true });

  let liveFiles: StagedAppend[] = [];
  if (scenario === 'live') liveFiles = stageAppendTails(sourceRoot, stagingRoot);

  let shadow: ObservationShadow | undefined;
  try {
    shadow = await openObservationShadow({
      adapterId,
      productionDbPath: path.join(scenarioRoot, 'production-sentinel.db'),
      shadowDbPath: databasePath,
      roots: [sourceRoot],
      ownerLabel: `observation-diff-${scenario}`,
    });

    if (scenario === 'live') {
      restoreAppendTails(liveFiles);
      await shadow.refresh();
    } else if (scenario === 'reconcile') {
      await shadow.refresh();
      await shadow.refresh();
    } else if (scenario === 'generation') {
      const replacements = stageGenerationReplacement(sourceRoot, stagingRoot);
      await shadow.refresh();
      restoreGenerationReplacement(replacements);
      await shadow.refresh();
    } else if (scenario === 'restart') {
      await shadow.dispose();
      shadow = await openObservationShadow({
        adapterId,
        productionDbPath: path.join(scenarioRoot, 'production-sentinel.db'),
        shadowDbPath: databasePath,
        roots: [sourceRoot],
        ownerLabel: 'observation-diff-restart-2',
      });
      await shadow.refresh();
    }

    const commitSeq = (await shadow.snapshot()).overview.commitSeq;
    await shadow.dispose();
    shadow = undefined;
    return { snapshot: dumpSemanticSnapshot(databasePath), commitSeq };
  } finally {
    await shadow?.dispose().catch(() => undefined);
  }
}

interface StagedAppend {
  stagedPath: string;
  livePath: string;
  tailStart: number;
  size: number;
}

function stageAppendTails(sourceRoot: string, stagingRoot: string): StagedAppend[] {
  return jsonlFiles(sourceRoot).flatMap((livePath) => {
    const size = statSync(livePath).size;
    if (size === 0) return [];
    const tailStart = lastRecordStart(livePath, size);
    const relative = path.relative(sourceRoot, livePath);
    const stagedPath = path.join(stagingRoot, 'live', relative);
    mkdirSync(path.dirname(stagedPath), { recursive: true });
    renameSync(livePath, stagedPath);
    copyRange(stagedPath, livePath, 0, tailStart, false);
    return [{ stagedPath, livePath, tailStart, size }];
  });
}

function restoreAppendTails(files: readonly StagedAppend[]): void {
  for (const file of files) copyRange(file.stagedPath, file.livePath, file.tailStart, file.size, true);
}

interface StagedGeneration {
  stagedPath: string;
  livePath: string;
}

function stageGenerationReplacement(sourceRoot: string, stagingRoot: string): StagedGeneration[] {
  return jsonlFiles(sourceRoot).map((livePath) => {
    const relative = path.relative(sourceRoot, livePath);
    const stagedPath = path.join(stagingRoot, 'generation', relative);
    mkdirSync(path.dirname(stagedPath), { recursive: true });
    renameSync(livePath, stagedPath);
    writeFileSync(livePath, '');
    return { stagedPath, livePath };
  });
}

function restoreGenerationReplacement(files: readonly StagedGeneration[]): void {
  for (const file of files) {
    rmSync(file.livePath, { force: true });
    renameSync(file.stagedPath, file.livePath);
  }
}

function jsonlFiles(root: string): string[] {
  const files: string[] = [];
  const visit = (directory: string): void => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const entryPath = path.join(directory, entry.name);
      if (entry.isSymbolicLink()) continue;
      if (entry.isDirectory()) visit(entryPath);
      else if (entry.isFile() && entry.name.endsWith('.jsonl')) files.push(entryPath);
    }
  };
  visit(root);
  return files.sort();
}

function lastRecordStart(filePath: string, size: number): number {
  const fd = openSync(filePath, 'r');
  try {
    const chunk = Buffer.allocUnsafe(64 * 1024);
    let position = size;
    let trailing = true;
    while (position > 0) {
      const start = Math.max(0, position - chunk.byteLength);
      const length = position - start;
      readSync(fd, chunk, 0, length, start);
      for (let index = length - 1; index >= 0; index -= 1) {
        const byte = chunk[index]!;
        if (trailing && (byte === 0x0a || byte === 0x0d)) continue;
        trailing = false;
        if (byte === 0x0a) return start + index + 1;
      }
      position = start;
    }
    return 0;
  } finally {
    closeSync(fd);
  }
}

function copyRange(source: string, destination: string, start: number, end: number, append: boolean): void {
  const input = openSync(source, 'r');
  const output = openSync(destination, append ? 'a' : 'w');
  const buffer = Buffer.allocUnsafe(1024 * 1024);
  let position = start;
  try {
    while (position < end) {
      const requested = Math.min(buffer.byteLength, end - position);
      const read = readSync(input, buffer, 0, requested, position);
      if (read === 0) throw new Error(`unexpected EOF while staging ${source}`);
      let written = 0;
      while (written < read) written += writeSync(output, buffer, written, read - written);
      position += read;
    }
  } finally {
    closeSync(input);
    closeSync(output);
  }
}

function dumpSemanticSnapshot(databasePath: string): SemanticSnapshot {
  const db = new DatabaseSync(databasePath, { readOnly: true });
  try {
    const names = db
      .prepare(
        `SELECT name FROM sqlite_master
         WHERE type = 'table'
           AND (name LIKE 'canonical_%' OR name IN ('observed_run_states', 'usage_totals'))
           AND name NOT LIKE '%_fts%'
         ORDER BY name`,
      )
      .all()
      .map((row) => String((row as { name: unknown }).name));
    const tables: SemanticSnapshot['tables'] = {};
    for (const name of names) tables[name] = dumpSemanticTable(db, name);
    return { tables, cursors: dumpLogicalCursors(db) };
  } finally {
    db.close();
  }
}

function dumpSemanticTable(db: DatabaseSync, table: string): Array<Record<string, NormalizedValue>> {
  const quoted = `"${table.replaceAll('"', '""')}"`;
  const columns = db
    .prepare(`PRAGMA table_info(${quoted})`)
    .all()
    .map((row) => String((row as { name: unknown }).name))
    .filter((column) => !volatileColumn.test(column) && !volatileProvenanceColumn.test(column));
  const selected = columns.map((column) => `"${column.replaceAll('"', '""')}"`).join(', ');
  const rows = db.prepare(`SELECT ${selected} FROM ${quoted}`).all() as Array<Record<string, SqlValue>>;
  return rows
    .map((row) =>
      Object.fromEntries(columns.map((column) => [column, normalizeValue(row[column] ?? null)])),
    )
    .sort((left, right) => stableJson(left).localeCompare(stableJson(right)));
}

function dumpLogicalCursors(db: DatabaseSync): Array<Record<string, NormalizedValue>> {
  const rows = db
    .prepare(
      `SELECT ss.stream_key, so.object_key, so.display_path,
              so.committed_cursor, so.decoder_state, so.state
       FROM source_objects so
       JOIN source_streams ss ON ss.source_stream_id = so.source_stream_id
       ORDER BY ss.stream_key, so.object_key`,
    )
    .all() as Array<Record<string, SqlValue>>;
  return rows.map((row) =>
    Object.fromEntries(
      Object.entries(row).map(([key, value]) => [key, normalizeLogicalCursorValue(key, value)]),
    ),
  );
}

function normalizeLogicalCursorValue(key: string, value: SqlValue): NormalizedValue {
  // Directory cursors intentionally include filesystem identity so a moved
  // root starts a new generation. Independent fixture copies therefore have
  // different opaque hashes even when their logical membership is identical.
  // Canonical state and the object inventory compare that membership; retain
  // the cursor kind here without pretending the identity-bearing bytes match.
  if (key === 'committed_cursor' && value instanceof Uint8Array && value[1] === 3) {
    return 'directory-snapshot';
  }
  return normalizeValue(value);
}

function normalizeValue(value: SqlValue): NormalizedValue {
  if (value === null) return null;
  if (typeof value === 'number') return value;
  if (typeof value !== 'string') return `base64:${Buffer.from(value).toString('base64')}`;
  const trimmed = value.trim();
  if (trimmed.startsWith('{') || trimmed.startsWith('[')) {
    try {
      return normalizeJson(JSON.parse(trimmed) as unknown);
    } catch {
      // Native source text that merely starts like JSON remains ordinary text.
    }
  }
  return value;
}

function normalizeJson(value: unknown): NormalizedValue {
  if (value === null) return null;
  if (typeof value === 'string') return value;
  if (typeof value === 'boolean') return value;
  if (typeof value === 'number') return value;
  if (Array.isArray(value)) return value.map(normalizeJson);
  if (typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>)
        .sort(([left], [right]) => left.localeCompare(right))
        .map(([key, item]) => [key, normalizeJson(item)]),
    );
  }
  return String(value);
}

function stableJson(value: unknown): string {
  return JSON.stringify(value);
}

function digest(value: unknown): string {
  return createHash('sha256').update(stableJson(value)).digest('hex');
}

function reportScenario(
  scenario: Scenario,
  snapshot: SemanticSnapshot,
  baseline: SemanticSnapshot,
  commitSeq: number,
): ScenarioReport {
  const tableNames = [...new Set([...Object.keys(baseline.tables), ...Object.keys(snapshot.tables)])].sort();
  const mismatchedTables = tableNames.filter(
    (table) => stableJson(snapshot.tables[table] ?? []) !== stableJson(baseline.tables[table] ?? []),
  );
  const cursorsMatch = stableJson(snapshot.cursors) === stableJson(baseline.cursors);
  return {
    scenario,
    exact: mismatchedTables.length === 0 && cursorsMatch,
    commitSeq,
    canonicalTableCounts: Object.fromEntries(
      Object.entries(snapshot.tables).map(([table, rows]) => [table, rows.length]),
    ),
    canonicalTableDigests: Object.fromEntries(
      Object.entries(snapshot.tables).map(([table, rows]) => [table, digest(rows)]),
    ),
    cursorCount: snapshot.cursors.length,
    cursorDigest: digest(snapshot.cursors),
    mismatchedTables,
    cursorsMatch,
  };
}
