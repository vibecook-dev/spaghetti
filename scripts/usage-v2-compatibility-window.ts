#!/usr/bin/env -S node --import tsx
/**
 * RFC 012C C3 compatibility-window collector.
 *
 * Runs the existing read-only getRuntimeUsageCompatibility sampler over a
 * representative external Claude corpus. It persists only aggregate,
 * privacy-reduced snapshots. Expected semantic divergence (legacy_higher /
 * different) is evidence, not a collector failure.
 *
 * This script does not promote a support release, select runtime.usage-v2,
 * or edit candidate package digests. It will not sample until an independent
 * census matches Ready v1 complete durable usage-v2 coverage.
 */

import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { constants, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { DatabaseSync } from 'node:sqlite';
import { parseArgs } from 'node:util';

import { openObservationHost } from '../packages/sdk/src/observation-host.js';
import type {
  SpaghettiEngineRuntimeUsageCompatibility,
  SpaghettiEngineRuntimeUsageCompatibilityBucket,
  SpaghettiEngineRuntimeUsageCompatibilityTelemetryStats,
  SpaghettiEngineRuntimeUsageTotalsSelectionScope,
  SpaghettiEngineRuntimeUsageV2Aggregate,
  SpaghettiEngineUsageAggregate,
  SpaghettiEngineUsageScopeOptions,
} from '../packages/sdk/src/native.js';

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

const CANDIDATE_DIR = 'agent-support/claude-code/candidate-2026-08-15';
const CANDIDATE_DOCUMENTS = [
  'ads.json',
  'source-declarations.json',
  'scope-programs.json',
  'evidence.json',
  'conformance.json',
  'support-release.json',
] as const;

const MAX_COMPATIBILITY_SCOPES = 128;
const PROJECT_PAGE_LIMIT = 200;
const SSD_ROOT = '/Volumes/SamsungRed/spaghetti-rfc012';
const COLLECTOR_SCRIPT = 'scripts/usage-v2-compatibility-window.ts';
const CENSUS_SCRIPT = 'scripts/runtime_observation_census/census.py';
const EXPECTED_USAGE_QUERY_PACK_ID = 'runtime.usage';
const EXPECTED_USAGE_QUERY_ID = 'legacy.usage';
const EXPECTED_USAGE_CONTRACT_VERSION = 1;
const EXPECTED_USAGE_V2_PROJECTION_ID = 'runtime.usage-v2';
const EXPECTED_USAGE_V2_VERSION = 1;
const EXPECTED_SELECTION_EPOCH = 0;
const MAX_MACHINE_CODE_BYTES = 64;
const PRIVACY_SCAN_PREFIX = 'compatibility report privacy scan failed:';

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

interface DurableSummary {
  usage: DurableUsageSummary;
  coverage: DurableCoverageSummary;
  providerDiagnostics: { records: number; codes: Record<string, number> };
  foreignKeyViolationsAfterBootstrap: number;
}

interface ParityCheck {
  name: string;
  expected: string | number | boolean | null;
  actual: string | number | boolean | null;
  exact: boolean;
}

interface PrivacyReducedBucket {
  legacyExactTokens: number;
  legacyEstimatedTokens: number;
  legacyCombinedTokens: number;
  v2KnownTokens: number;
  v2UnknownResponseCount: number;
  v2Completeness: string;
  relation: 'equal' | 'legacy_higher' | 'v2_higher' | 'incomparable';
  absoluteDeltaTokens: number | null;
}

interface PrivacyReducedSelection {
  querySelection: 'unselected-legacy-default';
  contractVersion: number;
  queryPackId: string;
  materialized: false;
  selected: { queryId: string; contractVersion: number };
  rollback: { queryId: string; contractVersion: number };
  selectionEpoch: number;
  memberCount: number;
  sessionCount: number;
  v2EligibleCount: number;
  adapters: Record<string, { memberCount: number; sessionCount: number }>;
  coverageStatus: Record<string, number>;
  projection: {
    readyCount: number;
    desiredVersion: typeof EXPECTED_USAGE_V2_VERSION;
    completedVersion: typeof EXPECTED_USAGE_V2_VERSION;
  };
}

interface PrivacyReducedSnapshot {
  batchIndex: number;
  batchSize: number;
  contractVersion: number;
  atCommitSeq: number;
  comparisonRef: string;
  comparisonRefOrderIndependent: boolean;
  status: string;
  comparisonStatus: string;
  selection: PrivacyReducedSelection;
  legacy: {
    exact: Record<string, number>;
    estimated: Record<string, number>;
    combined: Record<string, number>;
    quality: string;
    exactContributionCount: number;
    estimatedContributionCount: number;
    contributionCount: number;
    sessionCount: number;
  };
  usageV2: {
    responseCount: number;
    actorCount: number;
    inputTokens: Record<string, number | string>;
    outputTokens: Record<string, number | string>;
    cacheCreationInputTokens: Record<string, number | string>;
    cacheReadInputTokens: Record<string, number | string>;
  } | null;
  buckets: {
    inputTokens: PrivacyReducedBucket | null;
    outputTokens: PrivacyReducedBucket | null;
    cacheCreationInputTokens: PrivacyReducedBucket | null;
    cacheReadInputTokens: PrivacyReducedBucket | null;
  };
}

const { values } = parseArgs({
  options: {
    'claude-root': { type: 'string' },
    'live-source': { type: 'boolean' },
    'keep-workspace': { type: 'boolean' },
    report: { type: 'string' },
    json: { type: 'boolean' },
    'self-check': { type: 'boolean' },
  },
});

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

if (values['self-check']) {
  runSelfCheck();
  process.stdout.write('RFC 012C usage-v2 compatibility-window self-check passed.\n');
  process.exit(0);
}

if (!existsSync(SSD_ROOT)) {
  throw new Error('C3 collector requires /Volumes/SamsungRed/spaghetti-rfc012 to be mounted.');
}

const claudeRoot = path.resolve(values['claude-root'] ?? path.join(homedir(), '.claude'));
if (!existsSync(path.join(claudeRoot, 'projects'))) {
  throw new Error('Claude projects root does not exist.');
}

const reportPath = path.resolve(
  values.report ?? path.join(SSD_ROOT, 'build', 'c3-workspace', 'usage-v2-compatibility-window-v1.json'),
);
mkdirSync(path.dirname(reportPath), { recursive: true, mode: 0o700 });
const workspace = mkdtempSync(path.join(SSD_ROOT, 'build', 'c3-workspace', 'run-'));
const databasePath = path.join(workspace, 'observation.db');
const beforePath = path.join(workspace, 'census-before.json');
const afterPath = path.join(workspace, 'census-after.json');
const startedAt = performance.now();
const privacyForbidden = [claudeRoot, workspace, homedir(), SSD_ROOT];
let host: Awaited<ReturnType<typeof openObservationHost>> | undefined;
let runFailed = false;
let ingestChecks: ParityCheck[] | undefined;

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
    ownerLabel: 'usage-v2-compatibility-window',
  });
  const ingestElapsedMs = performance.now() - ingestStartedAt;

  runCensus(projectsRoot, afterPath);
  const after = readCensus(afterPath);
  const sourceStable =
    before.input.sourceSetDigest === after.input.sourceSetDigest &&
    before.input.changedDuringScan === 0 &&
    after.input.changedDuringScan === 0;
  const durable = readDurableSummary(databasePath);
  requireClosedMachineDetail(durable.coverage.detail);
  ingestChecks = buildIngestChecks(before, after, durable, sourceStable);
  if (durable.foreignKeyViolationsAfterBootstrap > 0) {
    throw new Error('foreign_key_check found at least one violation');
  }
  const failedIngest = ingestChecks.filter((check) => !check.exact).map((check) => check.name);
  if (failedIngest.length > 0) {
    throw new Error(`ingest parity failed: ${failedIngest.join(', ')}`);
  }

  const overview = await host.client.getOverview();
  const projectIds = await listEngineProjectIds(host.client);
  const batches = chunkScopes(projectIds, MAX_COMPATIBILITY_SCOPES);
  if (batches.length === 0) {
    throw new Error('Ingest produced no engine project scopes; refusing to fabricate a compatibility window.');
  }

  const snapshots: PrivacyReducedSnapshot[] = [];
  let windowSamples = 0;
  let orderIndependenceProbes = 0;
  for (const [batchIndex, batch] of batches.entries()) {
    const scopes: SpaghettiEngineUsageScopeOptions[] = batch.map((projectId) => ({ projectId }));
    const comparison = await host.client.getRuntimeUsageCompatibility({ scopes });
    windowSamples += 1;
    const reversed = await host.client.getRuntimeUsageCompatibility({
      scopes: [...scopes].reverse(),
    });
    orderIndependenceProbes += 1;
    if (comparison.comparisonRef !== reversed.comparisonRef) {
      throw new Error('comparisonRef changed under request reorder');
    }
    if (comparison.status !== 'ready') {
      throw new Error(`compatibility sampler status was ${comparison.status} after Ready ingest`);
    }
    assertUnselectedLegacyDefault(comparison.selectionVector);
    snapshots.push(
      reduceSnapshot({
        batchIndex,
        batchSize: batch.length,
        comparison,
        comparisonRefOrderIndependent: true,
      }),
    );
  }

  const engineTelemetry = redactTelemetry(
    (await host.client.getStats()).performance?.queries.runtimeUsageCompatibility,
  );
  await host.dispose();
  host = undefined;

  const report = {
    schemaVersion: 1,
    status: 'ready',
    experiment: 'rfc012c-usage-v2-compatibility-window',
    adapterId: 'claude-code',
    decoderContractVersion: readDecoderContractVersion(),
    sourceCapture: values['live-source'] ? 'live-checked-before-after' : 'ephemeral-isolated-clone',
    sourceSetDigestBefore: before.input.sourceSetDigest,
    sourceSetDigestAfter: after.input.sourceSetDigest,
    sourceStable,
    nativeArtifact: loadedNativeArtifactEvidence(),
    candidateDocuments: hashCandidateDocuments(),
    collectorEvidence: hashCollectorEvidence(),
    sampler: {
      contract: 'getRuntimeUsageCompatibility',
      contractVersion: EXPECTED_USAGE_CONTRACT_VERSION,
      maxScopes: MAX_COMPATIBILITY_SCOPES,
      promotion: false,
      querySelection: 'unselected-legacy-default',
      querySelectionProven: true,
      windowSamples,
      orderIndependenceProbes,
    },
    window: {
      engineProjectCount: projectIds.length,
      snapshotCount: snapshots.length,
      batchSizes: snapshots.map((snapshot) => snapshot.batchSize),
    },
    atCommitSeq: overview.commitSeq,
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
      usage: durable.usage,
      coverage: {
        sets: durable.coverage.sets,
        completeness: durable.coverage.completeness,
        readiness: durable.coverage.readiness,
        completedVersion: durable.coverage.completedVersion,
        detail: durable.coverage.detail,
        points: durable.coverage.points,
        absences: durable.coverage.absences,
        errors: durable.coverage.errors,
      },
      providerDiagnostics: machineCodeCounts(durable.providerDiagnostics),
      foreignKeyViolationsAfterBootstrap: durable.foreignKeyViolationsAfterBootstrap,
    },
    ingestChecks,
    snapshots,
    telemetry: {
      engine: engineTelemetry,
      windowSamples,
      orderIndependenceProbes,
    },
    privacy:
      'Aggregate counts, token totals, opaque comparison refs, coverage counts, artifact digest, and source metadata digest only. No native paths, identifiers, selection-scope refs, model values, prompts, answers, timestamps, timings, or raw payloads. Engine telemetry includes labeled order-independence probes.',
  };
  assertReportPrivacy(report, privacyForbidden);
  writeFileSync(reportPath, `${JSON.stringify(report, null, 2)}\n`);
  if (values.json) {
    process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
  } else {
    process.stdout.write(
      [
        'RFC 012C usage-v2 compatibility-window',
        `  status:      ready`,
        `  snapshots:   ${snapshots.length}`,
        `  projects:    ${projectIds.length}`,
        `  comparison:  ${snapshots.map((snapshot) => snapshot.comparisonStatus).join(',')}`,
        `  ingest:      ${ingestElapsedMs.toFixed(1)} ms`,
      ].join('\n') + '\n',
    );
    process.stderr.write(
      `wrote report ${reportPath}\n` +
        `sourceCapture ${roundMs(sourceCaptureElapsedMs)} ms ingest ${roundMs(ingestElapsedMs)} ms total ${roundMs(performance.now() - startedAt)} ms\n`,
    );
  }
} catch (error) {
  runFailed = true;
  const classified = classifyError(error);
  const failureReport = {
    schemaVersion: 1,
    status: 'failed',
    experiment: 'rfc012c-usage-v2-compatibility-window',
    adapterId: 'claude-code',
    decoderContractVersion: readDecoderContractVersion(),
    candidateDocuments: hashCandidateDocuments(),
    collectorEvidence: hashCollectorEvidence(),
    errorClass: classified.errorClass,
    errorMessage: classified.errorMessage,
    ingestChecks: ingestChecks ?? [],
    privacy:
      'Failure class, machine-safe message, and checked ingest fields only. No native paths, identifiers, payloads, timings, or workspace locations.',
  };
  assertReportPrivacy(failureReport, privacyForbidden);
  const failurePath = failureReportPath(reportPath);
  writeFileSync(failurePath, `${JSON.stringify(failureReport, null, 2)}\n`);
  process.exitCode = 1;
  process.stdout.write(
    ['RFC 012C usage-v2 compatibility-window', `  status:      failed`, `  errorClass:  ${classified.errorClass}`].join(
      '\n',
    ) + '\n',
  );
  process.stderr.write(
    `wrote failure artifact ${failurePath}\n` + `total ${roundMs(performance.now() - startedAt)} ms\n`,
  );
} finally {
  await host?.dispose().catch(() => undefined);
  const retainWorkspace = Boolean(values['keep-workspace'] || runFailed);
  if (retainWorkspace) {
    process.stderr.write(`retained diagnostic workspace ${workspace}\n`);
  } else {
    rmSync(workspace, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
}

function expectPrivacyRejected(report: Record<string, unknown>, label: string): void {
  let rejected = false;
  try {
    assertReportPrivacy(report, []);
  } catch (error) {
    if (!(error instanceof Error) || !error.message.startsWith(PRIVACY_SCAN_PREFIX)) throw error;
    rejected = true;
  }
  if (!rejected) {
    throw new Error(`expected scan to reject ${label}`);
  }
}

function expectSelectionRejected(vector: SpaghettiEngineRuntimeUsageTotalsSelectionScope[], label: string): void {
  let rejected = false;
  try {
    assertUnselectedLegacyDefault(vector);
  } catch (error) {
    if (!(error instanceof Error) || !error.message.includes('unselected legacy default')) throw error;
    rejected = true;
  }
  if (!rejected) {
    throw new Error(`expected legacy-default selection to reject a ${label}`);
  }
}

function chunkScopes<T>(items: readonly T[], size: number): T[][] {
  if (size <= 0) throw new Error('compatibility batch size must be positive');
  const batches: T[][] = [];
  for (let index = 0; index < items.length; index += size) {
    batches.push(items.slice(index, index + size));
  }
  return batches;
}

function runSelfCheck(): void {
  const ids = Array.from({ length: 130 }, (_, index) => `id-${String(index).padStart(3, '0')}`);
  const batches = chunkScopes(ids, MAX_COMPATIBILITY_SCOPES);
  if (batches.length !== 2 || batches[0]?.length !== 128 || batches[1]?.length !== 2) {
    throw new Error('chunkScopes did not preserve the 128-scope contract bound');
  }
  if (chunkScopes([], 128).length !== 0) {
    throw new Error('chunkScopes of an empty window must stay empty');
  }

  const leakyIdentity: Record<string, unknown> = {
    schemaVersion: 1,
    experiment: 'rfc012c-usage-v2-compatibility-window',
    scopes: [{ projectId: 'project_v1_abc' }],
  };
  expectPrivacyRejected(leakyIdentity, 'an engine project id');
  const leakyHome: Record<string, unknown> = {
    schemaVersion: 1,
    experiment: 'rfc012c-usage-v2-compatibility-window',
    source: '~/.claude',
  };
  expectPrivacyRejected(leakyHome, 'a ~/.claude path');

  const comparison = fakeCompatibility();
  assertUnselectedLegacyDefault(comparison.selectionVector);
  const reduced = reduceSnapshot({
    batchIndex: 0,
    batchSize: 2,
    comparison,
    comparisonRefOrderIndependent: true,
  });
  const reducedJson = JSON.stringify(reduced);
  if (reducedJson.includes('project_v1_') || reducedJson.includes('session_v1_')) {
    throw new Error('reduceSnapshot leaked engine identity');
  }
  if (reducedJson.includes('updatedAtUnixMs') || reducedJson.includes('sourceInstanceRef')) {
    throw new Error('reduceSnapshot leaked timestamp or source-instance fields');
  }
  if (reducedJson.includes('selectionScopeRef') || reducedJson.includes('"scopes"')) {
    throw new Error('reduceSnapshot retained request scopes or selection-scope refs');
  }
  if (reducedJson.includes('timing') || reducedJson.includes('ElapsedMs')) {
    throw new Error('reduceSnapshot retained timings');
  }
  if (reduced.buckets.inputTokens?.relation !== 'legacy_higher') {
    throw new Error('reduceSnapshot dropped bucket relations');
  }
  if (reduced.selection.querySelection !== 'unselected-legacy-default' || reduced.selection.memberCount !== 1) {
    throw new Error('reduceSnapshot dropped proven legacy-default selection');
  }
  const promoted = fakeCompatibility();
  promoted.selectionVector[0]!.querySelection.materialized = true;
  expectSelectionRejected(promoted.selectionVector, 'materialized vector');
  const ineligible = fakeCompatibility();
  ineligible.selectionVector[0]!.v2Eligible = false;
  expectSelectionRejected(ineligible.selectionVector, 'v2-ineligible member');
  const incomplete = fakeCompatibility();
  incomplete.selectionVector[0]!.coverageStatus = 'partial';
  expectSelectionRejected(incomplete.selectionVector, 'incomplete coverage member');
  const unready = fakeCompatibility();
  unready.selectionVector[0]!.projectionReadiness.state = 'pending';
  expectSelectionRejected(unready.selectionVector, 'non-ready projection member');
  const wrongVersion = fakeCompatibility();
  wrongVersion.selectionVector[0]!.projectionReadiness.completedVersion = 2;
  expectSelectionRejected(wrongVersion.selectionVector, 'non-v1 completed projection');

  const twoMembers = fakeCompatibility();
  const second = structuredClone(twoMembers.selectionVector[0]!);
  second.sessionCount = 5;
  twoMembers.selectionVector.push(second);
  const reducedTwo = reduceSelection(twoMembers.selectionVector);
  if (
    reducedTwo.memberCount !== 2 ||
    reducedTwo.sessionCount !== 7 ||
    reducedTwo.v2EligibleCount !== 2 ||
    reducedTwo.coverageStatus.complete !== 2 ||
    reducedTwo.projection.readyCount !== 2 ||
    reducedTwo.projection.desiredVersion !== EXPECTED_USAGE_V2_VERSION ||
    reducedTwo.projection.completedVersion !== EXPECTED_USAGE_V2_VERSION
  ) {
    throw new Error('reduceSelection did not derive the proven projection tuple');
  }

  let rejectedDetail = false;
  try {
    requireClosedMachineDetail('/Users/example/not-a-machine-code');
  } catch (error) {
    if (!(error instanceof Error) || !error.message.includes('closed machine code')) throw error;
    rejectedDetail = true;
  }
  if (!rejectedDetail) {
    throw new Error('expected non-machine coverage detail to be rejected');
  }
  requireClosedMachineDetail(null);
  requireClosedMachineDetail('replay_required');
  const maxMachineCode = `a${'b'.repeat(MAX_MACHINE_CODE_BYTES - 1)}`;
  const overMachineCode = `a${'b'.repeat(MAX_MACHINE_CODE_BYTES)}`;
  if (maxMachineCode.length !== MAX_MACHINE_CODE_BYTES || overMachineCode.length !== MAX_MACHINE_CODE_BYTES + 1) {
    throw new Error('machine-code fixtures are not the 64/65-byte bounds');
  }
  requireClosedMachineDetail(maxMachineCode);
  let rejectedOverDetail = false;
  try {
    requireClosedMachineDetail(overMachineCode);
  } catch (error) {
    if (!(error instanceof Error) || !error.message.includes('closed machine code')) throw error;
    rejectedOverDetail = true;
  }
  if (!rejectedOverDetail) {
    throw new Error('expected 65-byte coverage detail to be rejected');
  }
  const diagnosticCounts = machineCodeCounts({
    records: 3,
    codes: { replay_required: 1, [maxMachineCode]: 1, [overMachineCode]: 1 },
  });
  if (diagnosticCounts.codes[maxMachineCode] !== 1 || diagnosticCounts.codes.unclassified !== 1) {
    throw new Error('expected 65-byte diagnostic prefixes to map to unclassified');
  }
  assertReportPrivacy(
    {
      schemaVersion: 1,
      experiment: 'rfc012c-usage-v2-compatibility-window',
      adapterId: 'claude-code',
      snapshots: [reduced],
    },
    ['/Users/example', '/Volumes/SamsungRed/spaghetti-rfc012'],
  );

  const before = fakeCensus(10);
  const matching = fakeDurable(10);
  const passing = buildIngestChecks(before, before, matching, true);
  if (passing.some((check) => !check.exact)) {
    throw new Error('ingest checks rejected an exact durable match');
  }
  const mismatched = fakeDurable(10);
  mismatched.usage.totals.input_tokens += 1;
  const failing = buildIngestChecks(before, before, mismatched, true);
  if (!failing.some((check) => check.name === 'input_tokens total' && !check.exact)) {
    throw new Error('ingest checks accepted a token-total mismatch');
  }

  const classified = classifyError(
    new Error(
      'SQLite persist typed fact batch failed: UNIQUE constraint failed: fact_records.semantic_fact_revision_id',
    ),
  );
  if (classified.errorClass !== 'durable_semantic_revision_unique_constraint') {
    throw new Error('unique-constraint failure was not classified');
  }
  const fkClassified = classifyError(new Error('foreign_key_check found at least one violation'));
  if (
    fkClassified.errorClass !== 'bootstrap_foreign_key_violation' ||
    fkClassified.errorMessage !== 'foreign_key_check found at least one violation'
  ) {
    throw new Error('foreign-key violation was not classified');
  }
  const pathy = classifyError(new Error('ENOENT: /Users/example/.claude/projects/secret.jsonl'));
  if (pathy.errorMessage.includes('/Users/') || pathy.errorMessage.includes('.jsonl')) {
    throw new Error('classified error retained a native path');
  }
  const hashes = hashCollectorEvidence();
  if (!hashes[COLLECTOR_SCRIPT]?.startsWith('sha256:') || !hashes[CENSUS_SCRIPT]?.startsWith('sha256:')) {
    throw new Error('collector evidence hashes are missing');
  }
}

function fakeCensus(n: number): CensusReport {
  const totals = { input_tokens: n, output_tokens: n, cache_creation_input_tokens: n, cache_read_input_tokens: n };
  const zeros = { input_tokens: 0, output_tokens: 0, cache_creation_input_tokens: 0, cache_read_input_tokens: 0 };
  return {
    input: { sourceSetDigest: 'abc', files: n, changedDuringScan: 0 },
    usage: {
      fileScopedResponseGroups: n,
      usageActorFiles: n,
      usageSessions: n,
      rootResponseGroups: n,
      childResponseGroups: 0,
      rowsWithoutMessageId: 0,
      latestGroupsWithModel: n,
      latestResponseSnapshotTotal: totals,
      latestResponseUnknownGroups: zeros,
    },
  };
}

function fakeDurable(n: number): DurableSummary {
  const totals = { input_tokens: n, output_tokens: n, cache_creation_input_tokens: n, cache_read_input_tokens: n };
  const zeros = { input_tokens: 0, output_tokens: 0, cache_creation_input_tokens: 0, cache_read_input_tokens: 0 };
  return {
    usage: {
      responses: n,
      actors: n,
      sessions: n,
      rootResponses: n,
      childResponses: 0,
      fallbackResponses: 0,
      responsesWithModel: n,
      totals,
      unknownResponses: zeros,
    },
    coverage: {
      sets: 1,
      completeness: 'complete',
      readiness: 'ready',
      completedVersion: 1,
      detail: null,
      points: n,
      absences: 0,
      errors: 0,
      errorCodes: {},
    },
    providerDiagnostics: { records: 0, codes: {} },
    foreignKeyViolationsAfterBootstrap: 0,
  };
}

function fakeCompatibility(): SpaghettiEngineRuntimeUsageCompatibility {
  const tokens = {
    inputTokens: 10,
    outputTokens: 2,
    cacheCreationTokens: 0,
    cacheReadTokens: 0,
    componentTotalTokens: 12,
  };
  return {
    contractVersion: 1,
    atCommitSeq: 9,
    comparisonRef: 'v1:opaque',
    status: 'ready',
    comparisonStatus: 'different',
    scopes: [{ projectId: 'project_v1_should_not_leak', sessionId: 'session_v1_should_not_leak' }],
    selectionVector: [
      {
        selectionScopeRef: 'v1:scope',
        adapterId: 'claude-code',
        sessionCount: 2,
        v2Eligible: true,
        coverageStatus: 'complete',
        querySelection: {
          contractVersion: 1,
          queryPackId: 'runtime.usage',
          sourceInstanceRef: 'v1:instance',
          materialized: false,
          selected: { queryId: 'legacy.usage', contractVersion: 1 },
          rollback: { queryId: 'legacy.usage', contractVersion: 1 },
          selectionEpoch: 0,
          lastCommitSeq: 9,
          updatedAtUnixMs: 1_700_000_000_000,
        },
        projectionReadiness: {
          projectionId: 'runtime.usage-v2',
          desiredVersion: 1,
          completedVersion: 1,
          state: 'ready',
          lastCommitSeq: 9,
          updatedAtUnixMs: 1_700_000_000_000,
        },
      },
    ],
    legacy: {
      exact: tokens,
      estimated: { ...tokens, inputTokens: 0, outputTokens: 0, componentTotalTokens: 0 },
      combined: tokens,
      quality: 'exact',
      exactContributionCount: 3,
      estimatedContributionCount: 0,
      contributionCount: 3,
      sessionCount: 2,
    },
    usageV2: {
      responseCount: 2,
      actorCount: 1,
      inputTokens: {
        knownTokens: 8,
        knownResponseCount: 2,
        exactResponseCount: 2,
        nonExactResponseCount: 0,
        unknownResponseCount: 0,
        completeness: 'complete',
      },
      outputTokens: {
        knownTokens: 2,
        knownResponseCount: 2,
        exactResponseCount: 2,
        nonExactResponseCount: 0,
        unknownResponseCount: 0,
        completeness: 'complete',
      },
      cacheCreationInputTokens: {
        knownTokens: 0,
        knownResponseCount: 2,
        exactResponseCount: 2,
        nonExactResponseCount: 0,
        unknownResponseCount: 0,
        completeness: 'complete',
      },
      cacheReadInputTokens: {
        knownTokens: 0,
        knownResponseCount: 2,
        exactResponseCount: 2,
        nonExactResponseCount: 0,
        unknownResponseCount: 0,
        completeness: 'complete',
      },
    },
    inputTokens: {
      legacyExactTokens: 10,
      legacyEstimatedTokens: 0,
      legacyCombinedTokens: 10,
      v2KnownTokens: 8,
      v2UnknownResponseCount: 0,
      v2Completeness: 'complete',
      relation: 'legacy_higher',
      absoluteDeltaTokens: 2,
    },
  };
}

async function listEngineProjectIds(client: {
  listHistoryProjects: (request: { cursor?: string; limit: number }) => Promise<{
    items: Array<{ projectId: string }>;
    nextCursor?: string;
  }>;
}): Promise<string[]> {
  const ids = new Set<string>();
  let cursor: string | undefined;
  do {
    const page = await client.listHistoryProjects({ cursor, limit: PROJECT_PAGE_LIMIT });
    for (const item of page.items) {
      if (typeof item.projectId !== 'string' || !item.projectId.startsWith('project_v1_')) {
        throw new Error('listHistoryProjects returned a non-opaque project identity');
      }
      ids.add(item.projectId);
    }
    cursor = page.nextCursor;
  } while (cursor);
  return [...ids].sort();
}

function reduceSnapshot(input: {
  batchIndex: number;
  batchSize: number;
  comparison: SpaghettiEngineRuntimeUsageCompatibility;
  comparisonRefOrderIndependent: boolean;
}): PrivacyReducedSnapshot {
  return {
    batchIndex: input.batchIndex,
    batchSize: input.batchSize,
    contractVersion: input.comparison.contractVersion,
    atCommitSeq: input.comparison.atCommitSeq,
    comparisonRef: input.comparison.comparisonRef,
    comparisonRefOrderIndependent: input.comparisonRefOrderIndependent,
    status: input.comparison.status,
    comparisonStatus: input.comparison.comparisonStatus,
    selection: reduceSelection(input.comparison.selectionVector),
    legacy: reduceLegacyAggregate(input.comparison.legacy),
    usageV2: input.comparison.usageV2 ? reduceUsageV2(input.comparison.usageV2) : null,
    buckets: {
      inputTokens: reduceBucket(input.comparison.inputTokens),
      outputTokens: reduceBucket(input.comparison.outputTokens),
      cacheCreationInputTokens: reduceBucket(input.comparison.cacheCreationInputTokens),
      cacheReadInputTokens: reduceBucket(input.comparison.cacheReadInputTokens),
    },
  };
}

function assertUnselectedLegacyDefault(vector: SpaghettiEngineRuntimeUsageTotalsSelectionScope[]): void {
  if (vector.length === 0) {
    throw new Error('selection vector is empty; cannot prove unselected legacy default');
  }
  for (const scope of vector) {
    const selection = scope.querySelection;
    const readiness = scope.projectionReadiness;
    if (
      selection.contractVersion !== EXPECTED_USAGE_CONTRACT_VERSION ||
      selection.queryPackId !== EXPECTED_USAGE_QUERY_PACK_ID ||
      selection.materialized !== false ||
      selection.selected.queryId !== EXPECTED_USAGE_QUERY_ID ||
      selection.selected.contractVersion !== EXPECTED_USAGE_CONTRACT_VERSION ||
      selection.rollback.queryId !== EXPECTED_USAGE_QUERY_ID ||
      selection.rollback.contractVersion !== EXPECTED_USAGE_CONTRACT_VERSION ||
      selection.selectionEpoch !== EXPECTED_SELECTION_EPOCH
    ) {
      throw new Error('selection vector is not the unselected legacy default');
    }
    if (
      scope.v2Eligible !== true ||
      scope.coverageStatus !== 'complete' ||
      readiness.projectionId !== EXPECTED_USAGE_V2_PROJECTION_ID ||
      readiness.state !== 'ready' ||
      readiness.desiredVersion !== EXPECTED_USAGE_V2_VERSION ||
      readiness.completedVersion !== EXPECTED_USAGE_V2_VERSION
    ) {
      throw new Error('selection vector is not the unselected legacy default');
    }
    requireClosedMachineDetail(readiness.detail ?? null);
  }
}

function reduceSelection(vector: SpaghettiEngineRuntimeUsageTotalsSelectionScope[]): PrivacyReducedSelection {
  assertUnselectedLegacyDefault(vector);
  const adapters: Record<string, { memberCount: number; sessionCount: number }> = {};
  let sessionCount = 0;
  for (const scope of vector) {
    sessionCount += scope.sessionCount;
    const adapter = adapters[scope.adapterId] ?? { memberCount: 0, sessionCount: 0 };
    adapter.memberCount += 1;
    adapter.sessionCount += scope.sessionCount;
    adapters[scope.adapterId] = adapter;
  }
  return {
    querySelection: 'unselected-legacy-default',
    contractVersion: EXPECTED_USAGE_CONTRACT_VERSION,
    queryPackId: EXPECTED_USAGE_QUERY_PACK_ID,
    materialized: false,
    selected: { queryId: EXPECTED_USAGE_QUERY_ID, contractVersion: EXPECTED_USAGE_CONTRACT_VERSION },
    rollback: { queryId: EXPECTED_USAGE_QUERY_ID, contractVersion: EXPECTED_USAGE_CONTRACT_VERSION },
    selectionEpoch: EXPECTED_SELECTION_EPOCH,
    memberCount: vector.length,
    sessionCount,
    v2EligibleCount: vector.length,
    adapters,
    coverageStatus: { complete: vector.length },
    projection: {
      readyCount: vector.length,
      desiredVersion: EXPECTED_USAGE_V2_VERSION,
      completedVersion: EXPECTED_USAGE_V2_VERSION,
    },
  };
}

function reduceLegacyAggregate(aggregate: SpaghettiEngineUsageAggregate): PrivacyReducedSnapshot['legacy'] {
  return {
    exact: tokenValues(aggregate.exact),
    estimated: tokenValues(aggregate.estimated),
    combined: tokenValues(aggregate.combined),
    quality: aggregate.quality,
    exactContributionCount: aggregate.exactContributionCount,
    estimatedContributionCount: aggregate.estimatedContributionCount,
    contributionCount: aggregate.contributionCount,
    sessionCount: aggregate.sessionCount,
  };
}

function reduceUsageV2(
  aggregate: SpaghettiEngineRuntimeUsageV2Aggregate,
): NonNullable<PrivacyReducedSnapshot['usageV2']> {
  return {
    responseCount: aggregate.responseCount,
    actorCount: aggregate.actorCount,
    inputTokens: v2Bucket(aggregate.inputTokens),
    outputTokens: v2Bucket(aggregate.outputTokens),
    cacheCreationInputTokens: v2Bucket(aggregate.cacheCreationInputTokens),
    cacheReadInputTokens: v2Bucket(aggregate.cacheReadInputTokens),
  };
}

function reduceBucket(bucket: SpaghettiEngineRuntimeUsageCompatibilityBucket | undefined): PrivacyReducedBucket | null {
  if (!bucket) return null;
  return {
    legacyExactTokens: bucket.legacyExactTokens,
    legacyEstimatedTokens: bucket.legacyEstimatedTokens,
    legacyCombinedTokens: bucket.legacyCombinedTokens,
    v2KnownTokens: bucket.v2KnownTokens,
    v2UnknownResponseCount: bucket.v2UnknownResponseCount,
    v2Completeness: bucket.v2Completeness,
    relation: bucket.relation,
    absoluteDeltaTokens: bucket.absoluteDeltaTokens ?? null,
  };
}

function tokenValues(value: {
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  componentTotalTokens: number;
}): Record<string, number> {
  return {
    inputTokens: value.inputTokens,
    outputTokens: value.outputTokens,
    cacheCreationTokens: value.cacheCreationTokens,
    cacheReadTokens: value.cacheReadTokens,
    componentTotalTokens: value.componentTotalTokens,
  };
}

function v2Bucket(bucket: SpaghettiEngineRuntimeUsageV2Aggregate['inputTokens']): Record<string, number | string> {
  return {
    knownTokens: bucket.knownTokens,
    knownResponseCount: bucket.knownResponseCount,
    exactResponseCount: bucket.exactResponseCount,
    nonExactResponseCount: bucket.nonExactResponseCount,
    unknownResponseCount: bucket.unknownResponseCount,
    completeness: bucket.completeness,
  };
}

function redactTelemetry(
  telemetry: SpaghettiEngineRuntimeUsageCompatibilityTelemetryStats | undefined,
): Record<string, number | null> {
  return {
    samples: telemetry?.samples ?? 0,
    readySamples: telemetry?.readySamples ?? 0,
    notReadySamples: telemetry?.notReadySamples ?? 0,
    equalSamples: telemetry?.equalSamples ?? 0,
    differentSamples: telemetry?.differentSamples ?? 0,
    incomparableSamples: telemetry?.incomparableSamples ?? 0,
    equalBuckets: telemetry?.equalBuckets ?? 0,
    legacyHigherBuckets: telemetry?.legacyHigherBuckets ?? 0,
    v2HigherBuckets: telemetry?.v2HigherBuckets ?? 0,
    incomparableBuckets: telemetry?.incomparableBuckets ?? 0,
    sampledAbsoluteDeltaTokens: telemetry?.sampledAbsoluteDeltaTokens ?? 0,
    maxAbsoluteDeltaTokens: telemetry?.maxAbsoluteDeltaTokens ?? 0,
    firstAtCommitSeq: telemetry?.firstAtCommitSeq ?? null,
    lastAtCommitSeq: telemetry?.lastAtCommitSeq ?? null,
  };
}

function isClosedMachineCode(value: string): boolean {
  return /^[a-z][a-z0-9_]*$/.test(value) && value.length <= MAX_MACHINE_CODE_BYTES;
}

function machineCodeCounts(value: { records: number; codes: Record<string, number> }): {
  records: number;
  codes: Record<string, number>;
} {
  const codes: Record<string, number> = {};
  let unclassified = 0;
  for (const [code, count] of Object.entries(value.codes)) {
    if (isClosedMachineCode(code)) codes[code] = count;
    else unclassified += count;
  }
  if (unclassified > 0) codes.unclassified = (codes.unclassified ?? 0) + unclassified;
  return { records: value.records, codes };
}

function assertReportPrivacy(report: Record<string, unknown>, forbiddenSubstrings: string[]): void {
  const encoded = JSON.stringify(report);
  const needles = [
    'project_v1_',
    'session_v1_',
    '/Users/',
    '/Volumes/',
    '/home/',
    '.claude/',
    '~/.claude',
    'originalPath',
    'nativeProjectKey',
    'fullPath',
    'updatedAtUnixMs',
    'selectionScopeRef',
    ...forbiddenSubstrings.filter((value) => value.length > 0),
  ];
  for (const needle of needles) {
    if (encoded.includes(needle)) {
      throw new Error(`${PRIVACY_SCAN_PREFIX} ${needle}`);
    }
  }
}

function classifyError(error: unknown): { errorClass: string; errorMessage: string } {
  const raw = error instanceof Error ? error.message : 'unknown error';
  if (raw.includes('UNIQUE constraint failed: fact_records.semantic_fact_revision_id')) {
    return {
      errorClass: 'durable_semantic_revision_unique_constraint',
      errorMessage: 'UNIQUE constraint failed: fact_records.semantic_fact_revision_id',
    };
  }
  if (raw.includes('foreign_key_check found at least one violation')) {
    return {
      errorClass: 'bootstrap_foreign_key_violation',
      errorMessage: 'foreign_key_check found at least one violation',
    };
  }
  if (raw.startsWith('ingest parity failed:')) {
    return { errorClass: 'ingest_parity_failed', errorMessage: raw };
  }
  if (raw.includes('Source changed during')) {
    return { errorClass: 'source_unstable', errorMessage: 'source changed during the run' };
  }
  if (raw.includes('comparisonRef changed')) {
    return {
      errorClass: 'comparison_ref_order_dependent',
      errorMessage: 'comparisonRef changed under request reorder',
    };
  }
  if (raw.includes('no engine project scopes')) {
    return { errorClass: 'empty_engine_catalog', errorMessage: 'ingest produced no engine project scopes' };
  }
  if (raw.startsWith('compatibility sampler status was')) {
    return { errorClass: 'sampler_not_ready', errorMessage: 'compatibility sampler was not ready after Ready ingest' };
  }
  if (raw.includes('unselected legacy default') || raw.includes('selection vector is empty')) {
    return {
      errorClass: 'selection_not_unselected_legacy_default',
      errorMessage: 'selection vector is not the unselected legacy default',
    };
  }
  if (raw.includes('closed machine code')) {
    return {
      errorClass: 'durable_coverage_detail_not_machine_code',
      errorMessage: 'durable coverage detail is not a closed machine code',
    };
  }
  return {
    errorClass: 'collector_failed',
    errorMessage: 'collector failed without a classified persist or ingest gate error',
  };
}

function failureReportPath(successPath: string): string {
  return successPath.endsWith('.json')
    ? `${successPath.slice(0, -'.json'.length)}.failure.json`
    : `${successPath}.failure.json`;
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

function hashRepoFile(relativePath: string): string {
  const bytes = readFileSync(path.join(repoRoot, relativePath));
  return `sha256:${createHash('sha256').update(bytes).digest('hex')}`;
}

function hashCandidateDocuments(): Record<string, string> {
  const hashes: Record<string, string> = {};
  for (const filename of CANDIDATE_DOCUMENTS) {
    hashes[filename] = hashRepoFile(`${CANDIDATE_DIR}/${filename}`);
  }
  return hashes;
}

function hashCollectorEvidence(): Record<string, string> {
  return {
    [COLLECTOR_SCRIPT]: hashRepoFile(COLLECTOR_SCRIPT),
    [CENSUS_SCRIPT]: hashRepoFile(CENSUS_SCRIPT),
  };
}

function requireClosedMachineDetail(detail: string | null): string | null {
  if (detail == null) return null;
  if (!isClosedMachineCode(detail)) {
    throw new Error('durable coverage detail is not a closed machine code');
  }
  return detail;
}

function readDecoderContractVersion(): number {
  const release = JSON.parse(readFileSync(path.join(repoRoot, CANDIDATE_DIR, 'support-release.json'), 'utf8')) as {
    versions?: { decoder_contract?: number };
  };
  const version = release.versions?.decoder_contract;
  if (typeof version !== 'number') {
    throw new Error('candidate support-release.json is missing decoder_contract');
  }
  return version;
}

function runCensus(projects: string, output: string): void {
  execFileSync('python3', [path.join(repoRoot, CENSUS_SCRIPT), '--claude-projects', projects, '--out', output], {
    cwd: repoRoot,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
}

function readCensus(filePath: string): CensusReport {
  return JSON.parse(readFileSync(filePath, 'utf8')) as CensusReport;
}

function buildIngestChecks(
  before: CensusReport,
  after: CensusReport,
  durable: DurableSummary,
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

function readDurableSummary(databasePath: string): DurableSummary {
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
      foreignKeyViolationsAfterBootstrap: database.prepare('PRAGMA foreign_key_check').all().length,
    };
  } finally {
    database.close();
  }
}

function roundMs(value: number): number {
  return Math.round(value * 1_000) / 1_000;
}
