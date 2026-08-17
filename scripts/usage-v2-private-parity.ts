#!/usr/bin/env -S node --import tsx
/**
 * RFC 012C private-corpus usage-v2 parity gate.
 *
 * The independent Python census runs before and after a fresh durable ingest.
 * This command compares only aggregate response/actor/session counts, qualified
 * bucket totals, and normalized coverage. It never emits native paths,
 * identifiers, model values, prompts, answers, or raw payloads. The temporary
 * database is removed even when the comparison fails.
 */

import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { constants, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { DatabaseSync } from 'node:sqlite';
import { parseArgs } from 'node:util';

import { openObservationHost } from '../packages/sdk/src/observation-host.js';

const TOKEN_BUCKETS = [
  'input_tokens',
  'output_tokens',
  'cache_creation_input_tokens',
  'cache_read_input_tokens',
] as const;

const OBSERVED_SOURCE_ENTRIES = [
  'projects',
  'teams',
  'sessions',
  'todos',
  'tasks',
  'plans',
  'file-history',
  'settings.json',
  'settings.local.json',
] as const;

type TokenBucket = (typeof TOKEN_BUCKETS)[number];
type TokenValues = Record<TokenBucket, number>;

interface CensusReport {
  input: {
    sourceSetDigest: string;
    files: number;
    changedDuringScan: number;
  };
  usage: {
    fileScopedResponseGroups: number;
    usageActorFiles: number;
    usageSessions: number;
    rootResponseGroups: number;
    childResponseGroups: number;
    rowsWithoutMessageId: number;
    latestGroupsWithModel: number;
    latestResponseSnapshotTotal: TokenValues;
    latestResponseUnknownGroups: TokenValues;
  };
}

interface DurableUsageSummary {
  responses: number;
  actors: number;
  sessions: number;
  rootResponses: number;
  childResponses: number;
  fallbackResponses: number;
  responsesWithModel: number;
  totals: TokenValues;
  unknownResponses: TokenValues;
}

interface DurableCoverageSummary {
  sets: number;
  completeness: string | null;
  readiness: string | null;
  completedVersion: number | null;
  detail: string | null;
  points: number;
  absences: number;
  errors: number;
  errorCodes: Record<string, number>;
}

interface DurableDiagnosticSummary {
  records: number;
  codes: Record<string, number>;
}

interface ParityCheck {
  name: string;
  expected: string | number | boolean | null;
  actual: string | number | boolean | null;
  exact: boolean;
}

const { values } = parseArgs({
  options: {
    'claude-root': { type: 'string' },
    'live-source': { type: 'boolean' },
    'keep-workspace': { type: 'boolean' },
    report: { type: 'string' },
    json: { type: 'boolean' },
  },
});

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const claudeRoot = path.resolve(values['claude-root'] ?? path.join(homedir(), '.claude'));
if (!existsSync(path.join(claudeRoot, 'projects'))) {
  throw new Error('Claude projects root does not exist.');
}

const reportPath = path.resolve(values.report ?? path.join(tmpdir(), 'spaghetti-usage-v2-private-parity.json'));
const workspace = mkdtempSync(path.join(tmpdir(), 'spaghetti-usage-v2-private-parity-'));
const databasePath = path.join(workspace, 'observation.db');
const beforePath = path.join(workspace, 'census-before.json');
const afterPath = path.join(workspace, 'census-after.json');
const startedAt = performance.now();
let host: Awaited<ReturnType<typeof openObservationHost>> | undefined;
let report: Record<string, unknown> | undefined;

try {
  const sourceCaptureStartedAt = performance.now();
  const observedClaudeRoot = values['live-source']
    ? claudeRoot
    : cloneObservedSource(claudeRoot, path.join(workspace, 'source', '.claude'));
  const sourceCaptureElapsedMs = performance.now() - sourceCaptureStartedAt;
  const projectsRoot = path.join(observedClaudeRoot, 'projects');
  runCensus(projectsRoot, beforePath);
  const before = readCensus(beforePath);

  const ingestStartedAt = performance.now();
  host = await openObservationHost({
    dbPath: databasePath,
    sources: [{ adapterId: 'claude-code', roots: [observedClaudeRoot] }],
    queryWorkers: 1,
    ownerLabel: 'usage-v2-private-parity',
  });
  const ingestElapsedMs = performance.now() - ingestStartedAt;
  const commitSeq = (await host.client.getOverview()).commitSeq;
  await host.dispose();
  host = undefined;

  runCensus(projectsRoot, afterPath);
  const after = readCensus(afterPath);
  const durable = readDurableSummary(databasePath);
  const sourceStable =
    before.input.sourceSetDigest === after.input.sourceSetDigest &&
    before.input.changedDuringScan === 0 &&
    after.input.changedDuringScan === 0;
  const checks = buildChecks(before, after, durable, sourceStable);
  const exact = checks.every((check) => check.exact);
  report = {
    schemaVersion: 1,
    experiment: 'rfc012c-usage-v2-private-corpus-parity',
    adapterId: 'claude-code',
    source: claudeRoot === path.join(homedir(), '.claude') ? '~/.claude' : '<provided-root>',
    sourceCapture: values['live-source'] ? 'live-checked-before-after' : 'ephemeral-isolated-clone',
    sourceSetDigestBefore: before.input.sourceSetDigest,
    sourceSetDigestAfter: after.input.sourceSetDigest,
    sourceStable,
    exact,
    atCommitSeq: commitSeq,
    nativeArtifact: loadedNativeArtifactEvidence(),
    independent: {
      responseGroups: before.usage.fileScopedResponseGroups,
      actorFiles: before.usage.usageActorFiles,
      sessions: before.usage.usageSessions,
      rootResponseGroups: before.usage.rootResponseGroups,
      childResponseGroups: before.usage.childResponseGroups,
      fallbackResponses: before.usage.rowsWithoutMessageId,
      responsesWithModel: before.usage.latestGroupsWithModel,
      totals: before.usage.latestResponseSnapshotTotal,
      unknownResponses: before.usage.latestResponseUnknownGroups,
      declaredTranscriptObjects: before.input.files,
    },
    durable: {
      ...durable.usage,
      coverage: durable.coverage,
      providerDiagnostics: durable.providerDiagnostics,
      foreignKeyViolationsAfterBootstrap: durable.foreignKeyViolationsAfterBootstrap,
    },
    checks,
    timing: {
      sourceCaptureElapsedMs: Math.round(sourceCaptureElapsedMs * 1_000) / 1_000,
      ingestElapsedMs: Math.round(ingestElapsedMs * 1_000) / 1_000,
      totalElapsedMs: Math.round((performance.now() - startedAt) * 1_000) / 1_000,
    },
    privacy:
      'Aggregate counts, token totals, readiness, coverage counts, and source metadata digest only. No native paths, identifiers, model values, prompts, answers, or raw payloads.',
  };
  writeFileSync(reportPath, `${JSON.stringify(report, null, 2)}\n`);
  if (values.json) {
    process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
  } else {
    process.stdout.write(
      [
        'RFC 012C usage-v2 private-corpus parity',
        `  exact:       ${exact}`,
        `  responses:   ${before.usage.fileScopedResponseGroups.toLocaleString('en-US')}`,
        `  actors:      ${before.usage.usageActorFiles.toLocaleString('en-US')}`,
        `  sessions:    ${before.usage.usageSessions.toLocaleString('en-US')}`,
        `  ingest:      ${ingestElapsedMs.toFixed(1)} ms`,
        `  wrote ${reportPath}`,
      ].join('\n') + '\n',
    );
  }
  if (!exact) process.exitCode = 1;
} finally {
  await host?.dispose().catch(() => undefined);
  if (values['keep-workspace']) {
    process.stderr.write(`retained diagnostic workspace ${workspace}\n`);
  } else {
    rmSync(workspace, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
}

function cloneObservedSource(source: string, destination: string): string {
  mkdirSync(destination, { recursive: true, mode: 0o700 });
  for (const entry of OBSERVED_SOURCE_ENTRIES) {
    const sourceEntry = path.join(source, entry);
    if (!existsSync(sourceEntry)) continue;
    cpSync(sourceEntry, path.join(destination, entry), {
      recursive: true,
      dereference: false,
      preserveTimestamps: true,
      errorOnExist: true,
      mode: constants.COPYFILE_FICLONE,
    });
  }
  return destination;
}

function loadedNativeArtifactEvidence(): { fileName: string; sha256: string; bytes: number } {
  const require = createRequire(import.meta.url);
  const candidates = Object.keys(require.cache).filter(
    (candidate) => candidate.endsWith('.node') && path.basename(candidate).startsWith('spaghetti.'),
  );
  if (candidates.length !== 1) {
    throw new Error(`Expected one loaded Spaghetti native artifact, found ${candidates.length}.`);
  }
  const artifact = readFileSync(candidates[0]);
  return {
    fileName: path.basename(candidates[0]),
    sha256: createHash('sha256').update(artifact).digest('hex'),
    bytes: artifact.byteLength,
  };
}

function runCensus(projects: string, output: string): void {
  execFileSync(
    'python3',
    [
      path.join(repoRoot, 'scripts/runtime_observation_census/census.py'),
      '--claude-projects',
      projects,
      '--out',
      output,
    ],
    { cwd: repoRoot, stdio: ['ignore', 'pipe', 'pipe'] },
  );
}

function readCensus(filePath: string): CensusReport {
  return JSON.parse(readFileSync(filePath, 'utf8')) as CensusReport;
}

function readDurableSummary(databasePath: string): {
  usage: DurableUsageSummary;
  coverage: DurableCoverageSummary;
  providerDiagnostics: DurableDiagnosticSummary;
  foreignKeyViolationsAfterBootstrap: number;
} {
  const database = new DatabaseSync(databasePath, { readOnly: true });
  try {
    const row = database
      .prepare(
        `
        SELECT COUNT(*) AS responses,
               COUNT(DISTINCT usage.source_object_id) AS actors,
               COUNT(DISTINCT hex(usage.session_key)) AS sessions,
               SUM(CASE WHEN streams.stream_key = 'session-transcripts' THEN 1 ELSE 0 END) AS root_responses,
               SUM(CASE WHEN streams.stream_key = 'subagent-transcripts' THEN 1 ELSE 0 END) AS child_responses,
               SUM(CASE WHEN usage.response_identity = 'source_record_fallback' THEN 1 ELSE 0 END) AS fallback_responses,
               SUM(CASE WHEN usage.model IS NOT NULL THEN 1 ELSE 0 END) AS responses_with_model,
               COALESCE(SUM(usage.input_tokens), 0) AS input_tokens,
               COALESCE(SUM(usage.output_tokens), 0) AS output_tokens,
               COALESCE(SUM(usage.cache_creation_input_tokens), 0) AS cache_creation_input_tokens,
               COALESCE(SUM(usage.cache_read_input_tokens), 0) AS cache_read_input_tokens,
               SUM(CASE WHEN usage.input_tokens IS NULL THEN 1 ELSE 0 END) AS input_unknown,
               SUM(CASE WHEN usage.output_tokens IS NULL THEN 1 ELSE 0 END) AS output_unknown,
               SUM(CASE WHEN usage.cache_creation_input_tokens IS NULL THEN 1 ELSE 0 END) AS cache_creation_unknown,
               SUM(CASE WHEN usage.cache_read_input_tokens IS NULL THEN 1 ELSE 0 END) AS cache_read_unknown
        FROM usage_v2_response_contributions AS usage
        JOIN source_objects AS objects ON objects.source_object_id = usage.source_object_id
        JOIN source_streams AS streams ON streams.source_stream_id = objects.source_stream_id
        `,
      )
      .get() as Record<string, number>;
    const coverage = database
      .prepare(
        `
        SELECT COUNT(*) AS sets,
               MIN(coverage.completeness) AS completeness,
               MIN(projection.readiness) AS readiness,
               MIN(projection.completed_version) AS completed_version,
               MIN(projection.detail) AS detail,
               (SELECT COUNT(*) FROM source_coverage_points AS points
                 JOIN source_coverage_sets AS parent ON parent.coverage_set_id = points.coverage_set_id
                WHERE parent.owner_id = 'runtime.usage-v2'
                  AND parent.domain_kind = 'fact_family'
                  AND parent.domain_name = 'runtime.usage-v2'
                  AND parent.domain_version = 1) AS points,
               (SELECT COUNT(*) FROM source_coverage_absences AS absences
                 JOIN source_coverage_sets AS parent ON parent.coverage_set_id = absences.coverage_set_id
                WHERE parent.owner_id = 'runtime.usage-v2'
                  AND parent.domain_kind = 'fact_family'
                  AND parent.domain_name = 'runtime.usage-v2'
                  AND parent.domain_version = 1) AS absences,
               (SELECT COUNT(*) FROM source_coverage_errors AS errors
                 JOIN source_coverage_sets AS parent ON parent.coverage_set_id = errors.coverage_set_id
                WHERE parent.owner_id = 'runtime.usage-v2'
                  AND parent.domain_kind = 'fact_family'
                  AND parent.domain_name = 'runtime.usage-v2'
                  AND parent.domain_version = 1) AS errors
        FROM source_coverage_sets AS coverage
        JOIN projection_versions AS projection
          ON projection.projection_id = coverage.owner_id
         AND projection.scope_key = coverage.owner_scope_key
        WHERE coverage.owner_id = 'runtime.usage-v2'
          AND coverage.domain_kind = 'fact_family'
          AND coverage.domain_name = 'runtime.usage-v2'
          AND coverage.domain_version = 1
        `,
      )
      .get() as Record<string, number | string | null>;
    const errorCodes = Object.fromEntries(
      database
        .prepare(
          `
          SELECT errors.error_code, COUNT(*) AS error_count
          FROM source_coverage_errors AS errors
          JOIN source_coverage_sets AS parent ON parent.coverage_set_id = errors.coverage_set_id
          WHERE parent.owner_id = 'runtime.usage-v2'
            AND parent.domain_kind = 'fact_family'
            AND parent.domain_name = 'runtime.usage-v2'
            AND parent.domain_version = 1
          GROUP BY errors.error_code
          ORDER BY errors.error_code
          `,
        )
        .all()
        .map((error) => [
          String((error as Record<string, unknown>).error_code),
          Number((error as Record<string, unknown>).error_count),
        ]),
    );
    const diagnosticRows = database
      .prepare(
        `
        SELECT CASE
                 WHEN instr(errors.error_message, ':') > 0
                   THEN substr(errors.error_message, 1, instr(errors.error_message, ':') - 1)
                 ELSE 'common_driver_record_error'
               END AS diagnostic_code,
               COUNT(*) AS diagnostic_count
        FROM source_record_errors AS errors
        JOIN source_objects AS objects ON objects.source_object_id = errors.source_object_id
        JOIN source_streams AS streams ON streams.source_stream_id = objects.source_stream_id
        WHERE streams.stream_key IN ('session-transcripts', 'subagent-transcripts')
        GROUP BY diagnostic_code
        ORDER BY diagnostic_code
        `,
      )
      .all() as Array<Record<string, unknown>>;
    const diagnosticCodes = Object.fromEntries(
      diagnosticRows.map((diagnostic) => [String(diagnostic.diagnostic_code), Number(diagnostic.diagnostic_count)]),
    );
    const foreignKeyViolationsAfterBootstrap = database.prepare('PRAGMA foreign_key_check').all().length;
    return {
      usage: {
        responses: row.responses,
        actors: row.actors,
        sessions: row.sessions,
        rootResponses: row.root_responses,
        childResponses: row.child_responses,
        fallbackResponses: row.fallback_responses,
        responsesWithModel: row.responses_with_model,
        totals: {
          input_tokens: row.input_tokens,
          output_tokens: row.output_tokens,
          cache_creation_input_tokens: row.cache_creation_input_tokens,
          cache_read_input_tokens: row.cache_read_input_tokens,
        },
        unknownResponses: {
          input_tokens: row.input_unknown,
          output_tokens: row.output_unknown,
          cache_creation_input_tokens: row.cache_creation_unknown,
          cache_read_input_tokens: row.cache_read_unknown,
        },
      },
      coverage: {
        sets: coverage.sets as number,
        completeness: coverage.completeness as string | null,
        readiness: coverage.readiness as string | null,
        completedVersion: coverage.completed_version as number | null,
        detail: coverage.detail as string | null,
        points: coverage.points as number,
        absences: coverage.absences as number,
        errors: coverage.errors as number,
        errorCodes,
      },
      providerDiagnostics: {
        records: diagnosticRows.reduce((total, diagnostic) => total + Number(diagnostic.diagnostic_count), 0),
        codes: diagnosticCodes,
      },
      foreignKeyViolationsAfterBootstrap,
    };
  } finally {
    database.close();
  }
}

function buildChecks(
  before: CensusReport,
  after: CensusReport,
  durable: {
    usage: DurableUsageSummary;
    coverage: DurableCoverageSummary;
    foreignKeyViolationsAfterBootstrap: number;
  },
  sourceStable: boolean,
): ParityCheck[] {
  const checks: ParityCheck[] = [];
  const add = (name: string, expected: ParityCheck['expected'], actual: ParityCheck['actual']): void => {
    checks.push({ name, expected, actual, exact: expected === actual });
  };
  add('source snapshot stable', true, sourceStable);
  add('response groups', before.usage.fileScopedResponseGroups, durable.usage.responses);
  add('actor files', before.usage.usageActorFiles, durable.usage.actors);
  add('sessions', before.usage.usageSessions, durable.usage.sessions);
  add('root response groups', before.usage.rootResponseGroups, durable.usage.rootResponses);
  add('child response groups', before.usage.childResponseGroups, durable.usage.childResponses);
  add('fallback responses', before.usage.rowsWithoutMessageId, durable.usage.fallbackResponses);
  add('responses with model', before.usage.latestGroupsWithModel, durable.usage.responsesWithModel);
  for (const bucket of TOKEN_BUCKETS) {
    add(`${bucket} total`, before.usage.latestResponseSnapshotTotal[bucket], durable.usage.totals[bucket]);
    add(
      `${bucket} unknown responses`,
      before.usage.latestResponseUnknownGroups[bucket],
      durable.usage.unknownResponses[bucket],
    );
  }
  add('coverage sets', 1, durable.coverage.sets);
  add('coverage completeness', 'complete', durable.coverage.completeness);
  add('projection readiness', 'ready', durable.coverage.readiness);
  add('projection completed version', 1, durable.coverage.completedVersion);
  add('projection detail', null, durable.coverage.detail);
  add('coverage points', after.input.files, durable.coverage.points);
  add('coverage absences', 0, durable.coverage.absences);
  add('coverage errors', 0, durable.coverage.errors);
  add('foreign key violations after bootstrap', 0, durable.foreignKeyViolationsAfterBootstrap);
  return checks;
}
