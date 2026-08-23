/**
 * Opt-in RFC 011 Claude observation shadow.
 *
 * The shadow owns a persistent Rust engine and an isolated database. It is
 * deliberately not a query facade for the production service: callers use
 * its typed snapshots to collect parity evidence before selecting Rust as the
 * sole production writer.
 */

import { lstatSync, readlinkSync, realpathSync } from 'node:fs';
import { basename, dirname, extname, join, resolve } from 'node:path';

import {
  openSpaghettiEngine,
  type SpaghettiEngine,
  type SpaghettiEngineArtifactPage,
  type SpaghettiEngineCanonicalStats,
  type SpaghettiEngineDelegationPage,
  type SpaghettiEngineDelegationPageOptions,
  type SpaghettiEngineHealth,
  type SpaghettiEngineHistoryPageOptions,
  type SpaghettiEngineHistoryProject,
  type SpaghettiEngineHistoryProjectPage,
  type SpaghettiEngineHistorySession,
  type SpaghettiEngineHistorySessionPage,
  type SpaghettiEngineMemoryDocumentPage,
  type SpaghettiEngineMessagePage,
  type SpaghettiEngineMessagePageOptions,
  type SpaghettiEngineOverview,
  type SpaghettiEnginePlanPage,
  type SpaghettiEngineRunStateLookup,
  type SpaghettiEngineRuntimeSnapshot,
  type SpaghettiEngineRuntimeSnapshotOptions,
  type SpaghettiEngineSessionDetails,
  type SpaghettiEngineSearchPage,
  type SpaghettiEngineSearchPageOptions,
  type SpaghettiEngineSourcePage,
  type SpaghettiEngineStatus,
  type SpaghettiEngineTaskCollectionPage,
  type SpaghettiEngineTaskCollectionPageOptions,
  type SpaghettiEngineTaskPage,
  type SpaghettiEngineTimelinePage,
  type SpaghettiEngineTimelinePageOptions,
  type SpaghettiEngineTeamDetails,
  type SpaghettiEngineTeamInboxMessagePage,
  type SpaghettiEngineTeamInboxMessagePageOptions,
  type SpaghettiEngineTeamInboxPage,
  type SpaghettiEngineTeamPage,
  type SpaghettiEngineTeamPageOptions,
  type SpaghettiEngineToolResultPage,
  type SpaghettiEngineUsageActivity,
  type SpaghettiEngineUsageActivityOptions,
  type SpaghettiEngineUsageScopeOptions,
  type SpaghettiEngineUsageTotals,
  type SpaghettiEngineWorkflowDetails,
  type SpaghettiEngineWorkflowMemberPage,
  type SpaghettiEngineWorkflowPage,
  type SpaghettiEngineWorkflowPageOptions,
} from './native.js';
import {
  NapiTransport,
  openSpaghettiClient,
  serveSpaghettiIpc,
  type SpaghettiClient,
  type SpaghettiClientInfo,
  type SpaghettiIpcChannel,
  type SpaghettiIpcHost,
  type SpaghettiQueryOptions,
} from './client/index.js';

const OWNER_LOCK_SUFFIX = '.owner-lock.sqlite3';
const OWNER_METADATA_SUFFIX = '.owner.json';

export interface ClaudeObservationShadowOptions {
  /** Production compatibility database. The shadow is forbidden from opening it. */
  productionDbPath: string;
  /** Isolated RFC 011 database. Defaults beside the production database. */
  shadowDbPath?: string;
  /** Explicit Claude Code roots. No source root is inferred by this helper. */
  roots: string[];
  /** Persistent read-only workers used for shadow diagnostics. Defaults to one. */
  queryWorkers?: number;
  /** Diagnostic owner label written to the shadow owner sidecar. */
  ownerLabel?: string;
  /** Cancels startup observation; a partially opened engine is still disposed. */
  signal?: AbortSignal;
}

/** Adapter-neutral isolated observation host used by canonical parity gates. */
export interface ObservationShadowOptions extends ClaudeObservationShadowOptions {
  /** Open identifier of an adapter registered in the native composition root. */
  adapterId: string;
}

export interface ClaudeObservationShadowSnapshot {
  mode: 'shadow';
  databasePath: string;
  roots: string[];
  status: SpaghettiEngineStatus;
  health: SpaghettiEngineHealth;
  overview: SpaghettiEngineOverview;
}

export interface ClaudeLegacyHistoryCounts {
  /** Claude parent-session rows in the compatibility projection. */
  sessions: number;
  /** Claude parent transcript rows in the compatibility projection. */
  messages: number;
  /** Claude subagent transcript rows, excluding derived timeline rows. */
  subagentMessages: number;
}

export interface ClaudeObservationHistoryParity {
  atCommitSeq: number;
  exact: boolean;
  sessions: {
    legacy: number;
    canonical: number;
    delta: number;
    exact: boolean;
  };
  messages: {
    legacyParent: number;
    legacySubagent: number;
    legacyTotal: number;
    canonical: number;
    delta: number;
    exact: boolean;
  };
}

export interface ClaudeLegacyProjectSummary {
  nativeProjectKey: string;
  sessionCount: number;
  /** Compatibility parent-transcript count. Subagents are separate legacy rows. */
  parentMessageCount: number;
  /** Raw compatibility subagent transcript rows, including workflow-nested agents. */
  subagentMessageCount: number;
  hasMemory: boolean;
}

export interface ClaudeLegacySessionSummary {
  nativeProjectKey: string;
  nativeSessionId: string;
  /** Compatibility parent-transcript count. */
  parentMessageCount: number;
  /** Raw compatibility subagent transcript rows scoped to this parent session. */
  subagentMessageCount: number;
}

export interface ClaudeObservationHistoryQueryParity {
  exact: boolean;
  projects: {
    compared: number;
    missingCanonical: string[];
    /** Metadata/memory projects with no legacy compatibility project row. */
    acceptedCanonicalOnly: string[];
    /** Canonical-only projects that unexpectedly contain transcript history. */
    unexpectedCanonicalOnly: string[];
    mismatched: Array<{
      nativeProjectKey: string;
      fields: string[];
    }>;
  };
  sessions: {
    compared: number;
    missingCanonical: string[];
    /** Always unexpected: canonical sessions require transcript evidence. */
    unexpectedCanonicalOnly: string[];
    mismatched: Array<{
      nativeSessionId: string;
      fields: string[];
    }>;
  };
  /**
   * Canonical message counts include parent and subagent transcript rows, so
   * comparison normalizes legacy parent and raw subagent counts together.
   */
  acceptedDifferences: readonly ['canonical_message_count_includes_subagents'];
}

/** Source-native Claude token fields with equivalent legacy/canonical meaning. */
export interface ClaudeLegacyUsageValues {
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
}

export interface ClaudeLegacyUsageDay {
  date: string;
  tokenUsage: ClaudeLegacyUsageValues;
  /** Legacy sessions with any transcript row; not a usage-contributor count. */
  sessionCount: number;
}

export interface ClaudeLegacyUsageReport {
  totals: ClaudeLegacyUsageValues;
  days: readonly ClaudeLegacyUsageDay[];
}

export interface ClaudeObservationUsageParity {
  exact: boolean;
  scope: {
    mismatchedFields: string[];
  };
  totals: {
    mismatchedFields: string[];
    unexpectedEstimatedContributionCount: number;
    unexpectedEstimatedFields: string[];
  };
  activity: {
    compared: number;
    /** Legacy transcript days with no token evidence have no canonical usage row. */
    acceptedLegacyZeroUsageDays: string[];
    missingCanonical: string[];
    unexpectedCanonical: string[];
    mismatched: Array<{
      date: string;
      fields: string[];
    }>;
    unexpectedEstimatedContributionCount: number;
    unexpectedEstimatedFields: string[];
    unexpectedUntimedContributionCount: number;
    unexpectedUntimedFields: string[];
  };
  /**
   * Legacy provider totals and transcript-row counts have no source-neutral
   * equality claim against the canonical component/contribution fields.
   */
  acceptedDifferences: readonly [
    'canonical_component_total_is_not_provider_billing_total',
    'canonical_contribution_count_is_not_legacy_message_count',
    'canonical_session_count_is_not_legacy_transcript_session_count',
    'canonical_days_require_usage_evidence',
  ];
}

export interface ClaudeObservationShadow {
  readonly databasePath: string;
  readonly roots: readonly string[];
  readonly status: SpaghettiEngineStatus;
  /** Negotiated query boundary used by every shadow read. */
  readonly clientInfo: SpaghettiClientInfo;
  /** Serve one framed IPC client without transferring ownership of the Rust engine. */
  serveIpc(channel: SpaghettiIpcChannel, transportKind?: string): SpaghettiIpcHost;
  snapshot(signal?: AbortSignal): Promise<ClaudeObservationShadowSnapshot>;
  compareHistory(legacy: ClaudeLegacyHistoryCounts, signal?: AbortSignal): Promise<ClaudeObservationHistoryParity>;
  /** Query Rust-owned canonical project summaries without opening either database in TypeScript. */
  listHistoryProjects(
    options?: SpaghettiEngineHistoryPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineHistoryProjectPage>;
  /** Query transcript-backed sessions for one opaque canonical project identity. */
  listHistorySessions(
    projectId: string,
    options?: SpaghettiEngineHistoryPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineHistorySessionPage>;
  /** Read one canonical session by its opaque identity. */
  getSession(sessionId: string, signal?: AbortSignal): Promise<SpaghettiEngineSessionDetails>;
  /** Page one canonical session's messages with bounded payload bytes. */
  getMessages(
    projectId: string,
    sessionId: string,
    options?: SpaghettiEngineHistoryPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineMessagePage>;
  /** Search root and delegated canonical messages in one Rust-owned score domain. */
  search(options: SpaghettiEngineSearchPageOptions, signal?: AbortSignal): Promise<SpaghettiEngineSearchPage>;
  /** Read root and delegated canonical messages plus exact session facets. */
  getTimeline(options: SpaghettiEngineTimelinePageOptions, signal?: AbortSignal): Promise<SpaghettiEngineTimelinePage>;
  /** Page current child-run delegation relations for one verified session. */
  listDelegations(
    options: SpaghettiEngineDelegationPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineDelegationPage>;
  /** Page canonical workflow containers for one verified session. */
  listWorkflows(
    options: SpaghettiEngineWorkflowPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineWorkflowPage>;
  /** Read one workflow plus its bounded native snapshot. */
  getWorkflow(workflowId: string, signal?: AbortSignal): Promise<SpaghettiEngineWorkflowDetails>;
  /** Page explicit workflow-member journal evidence. */
  listWorkflowMembers(
    workflowId: string,
    options?: SpaghettiEngineHistoryPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineWorkflowMemberPage>;
  /** Page canonical project-memory documents, index first, with exact content. */
  listMemoryDocuments(
    projectId: string,
    options?: SpaghettiEngineHistoryPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineMemoryDocumentPage>;
  /** Page task collections globally or under one trusted session/run/team relation. */
  listTaskCollections(
    options?: SpaghettiEngineTaskCollectionPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineTaskCollectionPage>;
  /** Page canonical task items for one opaque collection. */
  listTasks(
    collectionId: string,
    options?: SpaghettiEngineHistoryPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineTaskPage>;
  /** Page global plan documents without inventing session ownership. */
  listPlans(options?: SpaghettiEngineHistoryPageOptions, signal?: AbortSignal): Promise<SpaghettiEnginePlanPage>;
  /** Page persisted tool-result sidecars for one verified project/session. */
  listToolResults(
    projectId: string,
    sessionId: string,
    options?: SpaghettiEngineHistoryPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineToolResultPage>;
  /** Page binary-safe file-history artifacts for one session. */
  listArtifacts(
    sessionId: string,
    options?: SpaghettiEngineHistoryPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineArtifactPage>;
  /** List configured source instances and their durable ingest inventory. */
  listSources(options?: SpaghettiEngineHistoryPageOptions, signal?: AbortSignal): Promise<SpaghettiEngineSourcePage>;
  /** Return canonical/catalog statistics, excluding compatibility-cache rows. */
  getStats(signal?: AbortSignal): Promise<SpaghettiEngineCanonicalStats>;
  /** Query canonical all-time usage for one project or verified session. */
  getUsage(options: SpaghettiEngineUsageScopeOptions, signal?: AbortSignal): Promise<SpaghettiEngineUsageTotals>;
  /** Query inclusive daily usage plus separately reported untimed contributions. */
  getUsageActivity(
    options: SpaghettiEngineUsageActivityOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineUsageActivity>;
  /** Query durable run state and registry presence without PID liveness inference. */
  getRuntimeSnapshot(
    options?: SpaghettiEngineRuntimeSnapshotOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineRuntimeSnapshot>;
  /** Read one canonical run without PID-liveness inference. */
  getRunState(runId: string, signal?: AbortSignal): Promise<SpaghettiEngineRunStateLookup>;
  /** List canonical teams, including inbox-only team identities. */
  listTeams(options?: SpaghettiEngineTeamPageOptions, signal?: AbortSignal): Promise<SpaghettiEngineTeamPage>;
  /** Read one team config/member snapshot. */
  getTeam(teamId: string, signal?: AbortSignal): Promise<SpaghettiEngineTeamDetails>;
  /** Page inbox metadata without returning message bodies. */
  listTeamInboxes(
    teamId: string,
    options?: SpaghettiEngineHistoryPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineTeamInboxPage>;
  /** Page one inbox's messages in native snapshot order. */
  listTeamInboxMessages(
    inboxId: string,
    options?: SpaghettiEngineHistoryPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineTeamInboxMessagePage>;
  refresh(signal?: AbortSignal): Promise<SpaghettiEngineStatus>;
  dispose(): Promise<SpaghettiEngineStatus>;
}

/** Canonical query/lifecycle surface shared by every registered adapter. */
export type ObservationShadow = Omit<ClaudeObservationShadow, 'compareHistory'>;
export type ObservationShadowSnapshot = ClaudeObservationShadowSnapshot;

/** Derive a sibling database without changing or opening either path. */
export function defaultClaudeObservationShadowDbPath(productionDbPath: string): string {
  const absolute = resolveDatabaseCandidate(productionDbPath, 'production');
  const extension = extname(absolute);
  const stem = basename(absolute, extension);
  return join(dirname(absolute), `${stem}.observation-shadow${extension || '.db'}`);
}

/**
 * Compare like-for-like Claude history counts.
 *
 * The caller must scope legacy counts to Claude. Whole-store counts from a
 * multi-source production database are not a valid parity oracle.
 */
export function compareClaudeObservationHistory(
  overview: SpaghettiEngineOverview,
  legacy: ClaudeLegacyHistoryCounts,
): ClaudeObservationHistoryParity {
  assertCount('sessions', legacy.sessions);
  assertCount('messages', legacy.messages);
  assertCount('subagentMessages', legacy.subagentMessages);
  const legacyMessages = legacy.messages + legacy.subagentMessages;
  assertCount('total messages', legacyMessages);
  const sessionDelta = overview.canonicalSessions - legacy.sessions;
  const messageDelta = overview.canonicalMessages - legacyMessages;
  return {
    atCommitSeq: overview.commitSeq,
    exact: sessionDelta === 0 && messageDelta === 0,
    sessions: {
      legacy: legacy.sessions,
      canonical: overview.canonicalSessions,
      delta: sessionDelta,
      exact: sessionDelta === 0,
    },
    messages: {
      legacyParent: legacy.messages,
      legacySubagent: legacy.subagentMessages,
      legacyTotal: legacyMessages,
      canonical: overview.canonicalMessages,
      delta: messageDelta,
      exact: messageDelta === 0,
    },
  };
}

/**
 * Compare the fields with equivalent legacy/canonical meaning.
 *
 * Index-only and memory-only projects are legitimate canonical evidence and
 * therefore appear as `canonicalOnly`, not as fabricated transcript rows.
 */
export function compareClaudeObservationHistoryQueries(
  canonicalProjects: readonly SpaghettiEngineHistoryProject[],
  canonicalSessions: readonly SpaghettiEngineHistorySession[],
  legacyProjects: readonly ClaudeLegacyProjectSummary[],
  legacySessions: readonly ClaudeLegacySessionSummary[],
): ClaudeObservationHistoryQueryParity {
  const canonicalProjectMap = uniqueBy(canonicalProjects, (project) => project.nativeProjectKey, 'canonical project');
  const legacyProjectMap = uniqueBy(legacyProjects, (project) => project.nativeProjectKey, 'legacy project');
  const canonicalSessionMap = uniqueBy(canonicalSessions, (session) => session.nativeSessionId, 'canonical session');
  const legacySessionMap = uniqueBy(legacySessions, (session) => session.nativeSessionId, 'legacy session');
  const projectMismatch: ClaudeObservationHistoryQueryParity['projects']['mismatched'] = [];
  const sessionMismatch: ClaudeObservationHistoryQueryParity['sessions']['mismatched'] = [];

  for (const [nativeProjectKey, legacy] of legacyProjectMap) {
    assertCount('legacy project sessionCount', legacy.sessionCount);
    assertCount('legacy project parentMessageCount', legacy.parentMessageCount);
    assertCount('legacy project subagentMessageCount', legacy.subagentMessageCount);
    assertCount('legacy project totalMessageCount', legacy.parentMessageCount + legacy.subagentMessageCount);
    const canonical = canonicalProjectMap.get(nativeProjectKey);
    if (!canonical) continue;
    const fields: string[] = [];
    if (canonical.transcriptSessionCount !== legacy.sessionCount) fields.push('sessionCount');
    if (canonical.messageCount !== legacy.parentMessageCount + legacy.subagentMessageCount) fields.push('messageCount');
    if (canonical.hasMemoryIndex !== legacy.hasMemory) fields.push('hasMemory');
    if (fields.length > 0) projectMismatch.push({ nativeProjectKey, fields });
  }
  for (const [nativeSessionId, legacy] of legacySessionMap) {
    assertCount('legacy session parentMessageCount', legacy.parentMessageCount);
    assertCount('legacy session subagentMessageCount', legacy.subagentMessageCount);
    assertCount('legacy session totalMessageCount', legacy.parentMessageCount + legacy.subagentMessageCount);
    const canonical = canonicalSessionMap.get(nativeSessionId);
    if (!canonical) continue;
    const fields: string[] = [];
    if (canonical.nativeProjectKey !== legacy.nativeProjectKey) fields.push('nativeProjectKey');
    if (canonical.messageCount !== legacy.parentMessageCount + legacy.subagentMessageCount) fields.push('messageCount');
    if (fields.length > 0) sessionMismatch.push({ nativeSessionId, fields });
  }

  const missingProjects = [...legacyProjectMap.keys()].filter((key) => !canonicalProjectMap.has(key)).sort();
  const canonicalOnlyProjects = [...canonicalProjectMap.entries()].filter(([key]) => !legacyProjectMap.has(key));
  const acceptedCanonicalOnlyProjects = canonicalOnlyProjects
    .filter(([, project]) => project.transcriptSessionCount === 0 && project.messageCount === 0)
    .map(([key]) => key)
    .sort();
  const unexpectedCanonicalOnlyProjects = canonicalOnlyProjects
    .filter(([, project]) => project.transcriptSessionCount > 0 || project.messageCount > 0)
    .map(([key]) => key)
    .sort();
  const missingSessions = [...legacySessionMap.keys()].filter((key) => !canonicalSessionMap.has(key)).sort();
  const canonicalOnlySessions = [...canonicalSessionMap.keys()].filter((key) => !legacySessionMap.has(key)).sort();
  const exact =
    missingProjects.length === 0 &&
    unexpectedCanonicalOnlyProjects.length === 0 &&
    projectMismatch.length === 0 &&
    missingSessions.length === 0 &&
    canonicalOnlySessions.length === 0 &&
    sessionMismatch.length === 0;
  return {
    exact,
    projects: {
      compared: legacyProjectMap.size,
      missingCanonical: missingProjects,
      acceptedCanonicalOnly: acceptedCanonicalOnlyProjects,
      unexpectedCanonicalOnly: unexpectedCanonicalOnlyProjects,
      mismatched: projectMismatch,
    },
    sessions: {
      compared: legacySessionMap.size,
      missingCanonical: missingSessions,
      unexpectedCanonicalOnly: canonicalOnlySessions,
      mismatched: sessionMismatch,
    },
    acceptedDifferences: ['canonical_message_count_includes_subagents'],
  };
}

/**
 * Compare Claude's four additive native token components across query engines.
 *
 * This deliberately does not compare legacy `totalTokens` or `messageCount`:
 * those fields encode provider normalization and transcript rows respectively,
 * while the canonical API exposes a component sum and usage contributions.
 */
export function compareClaudeObservationUsage(
  canonicalTotals: SpaghettiEngineUsageTotals,
  canonicalActivity: SpaghettiEngineUsageActivity,
  legacy: ClaudeLegacyUsageReport,
): ClaudeObservationUsageParity {
  assertLegacyUsageValues('totals', legacy.totals);
  const allLegacyDays = uniqueBy(legacy.days, (day) => day.date, 'legacy usage day');
  const canonicalDays = uniqueBy(canonicalActivity.days, (day) => day.date, 'canonical usage day');
  const scopeMismatch: string[] = [];
  if (canonicalTotals.projectId !== canonicalActivity.projectId) scopeMismatch.push('projectId');
  if ((canonicalTotals.sessionId ?? null) !== (canonicalActivity.sessionId ?? null)) scopeMismatch.push('sessionId');
  const totalMismatch = usageValueMismatches(canonicalTotals.aggregate.exact, legacy.totals);
  const activityMismatch: ClaudeObservationUsageParity['activity']['mismatched'] = [];

  for (const [date, legacyDay] of allLegacyDays) {
    assertIsoDate('legacy usage day', date);
    assertLegacyUsageValues(`day ${date}`, legacyDay.tokenUsage);
    assertCount(`usage day ${date} sessionCount`, legacyDay.sessionCount);
  }
  const acceptedLegacyZeroUsageDays = [...allLegacyDays.entries()]
    .filter(([, day]) => usageValuesAreZero(day.tokenUsage))
    .map(([date]) => date)
    .sort();
  const legacyDays = new Map([...allLegacyDays.entries()].filter(([, day]) => !usageValuesAreZero(day.tokenUsage)));

  for (const [date, legacyDay] of legacyDays) {
    const canonicalDay = canonicalDays.get(date);
    if (!canonicalDay) continue;
    const fields = usageValueMismatches(canonicalDay.aggregate.exact, legacyDay.tokenUsage);
    if (fields.length > 0) activityMismatch.push({ date, fields });
  }
  for (const date of canonicalDays.keys()) assertIsoDate('canonical usage day', date);

  const missingCanonical = [...legacyDays.keys()].filter((date) => !canonicalDays.has(date)).sort();
  const unexpectedCanonical = [...canonicalDays.keys()].filter((date) => !legacyDays.has(date)).sort();
  const totalsEstimated = canonicalTotals.aggregate.estimatedContributionCount;
  const totalsEstimatedFields = nonzeroUsageFields(canonicalTotals.aggregate.estimated);
  const activityEstimated = canonicalActivity.aggregate.estimatedContributionCount;
  const activityEstimatedFields = nonzeroUsageFields(canonicalActivity.aggregate.estimated);
  const untimed = canonicalActivity.untimed.aggregate.contributionCount;
  const untimedFields = nonzeroUsageFields(canonicalActivity.untimed.aggregate.combined);
  const exact =
    scopeMismatch.length === 0 &&
    totalMismatch.length === 0 &&
    totalsEstimated === 0 &&
    totalsEstimatedFields.length === 0 &&
    missingCanonical.length === 0 &&
    unexpectedCanonical.length === 0 &&
    activityMismatch.length === 0 &&
    activityEstimated === 0 &&
    activityEstimatedFields.length === 0 &&
    untimed === 0 &&
    untimedFields.length === 0;

  return {
    exact,
    scope: { mismatchedFields: scopeMismatch },
    totals: {
      mismatchedFields: totalMismatch,
      unexpectedEstimatedContributionCount: totalsEstimated,
      unexpectedEstimatedFields: totalsEstimatedFields,
    },
    activity: {
      compared: legacyDays.size,
      acceptedLegacyZeroUsageDays,
      missingCanonical,
      unexpectedCanonical,
      mismatched: activityMismatch,
      unexpectedEstimatedContributionCount: activityEstimated,
      unexpectedEstimatedFields: activityEstimatedFields,
      unexpectedUntimedContributionCount: untimed,
      unexpectedUntimedFields: untimedFields,
    },
    acceptedDifferences: [
      'canonical_component_total_is_not_provider_billing_total',
      'canonical_contribution_count_is_not_legacy_message_count',
      'canonical_session_count_is_not_legacy_transcript_session_count',
      'canonical_days_require_usage_evidence',
    ],
  };
}

/** Open one isolated engine for any registered adapter and complete its initial scan. */
export async function openObservationShadow(options: ObservationShadowOptions): Promise<ObservationShadow> {
  const adapterId = options.adapterId.trim();
  if (adapterId.length === 0) throw new Error('Observation shadow adapterId must not be empty.');
  return openObservationShadowInternal(options, adapterId, adapterId);
}

/** Claude compatibility wrapper around the adapter-neutral observation host. */
export async function openClaudeObservationShadow(
  options: ClaudeObservationShadowOptions,
): Promise<ClaudeObservationShadow> {
  return openObservationShadowInternal(options, 'claude-code', 'Claude');
}

async function openObservationShadowInternal(
  options: ClaudeObservationShadowOptions,
  adapterId: string,
  displayName: string,
): Promise<ClaudeObservationShadow> {
  options.signal?.throwIfAborted();
  const productionPath = canonicalPotentialPath(options.productionDbPath, 'production');
  const requestedShadowPath = options.shadowDbPath ?? defaultClaudeObservationShadowDbPath(productionPath);
  const shadowPath = canonicalPotentialPath(requestedShadowPath, 'shadow');
  assertIsolatedDatabaseArtifacts(productionPath, shadowPath);
  const roots = normalizeRoots(options.roots);

  const engine = await openSpaghettiEngine({
    dbPath: shadowPath,
    queryWorkers: options.queryWorkers ?? 1,
    ownerLabel:
      options.ownerLabel ??
      (adapterId === 'claude-code' ? 'sdk-claude-observation-shadow' : `sdk-${adapterId}-observation-shadow`),
  });
  let client: SpaghettiClient | undefined;
  try {
    await engine.startObservation({ adapterId, roots, reason: 'shadow_observation' }, options.signal);
    client = await openSpaghettiClient({
      transport: new NapiTransport({ engine, ownsEngine: false }),
      clientName: `${adapterId}-observation-shadow`,
    });
    return new NativeClaudeObservationShadow(engine, client, roots, adapterId, displayName);
  } catch (error) {
    await client?.dispose().catch(() => undefined);
    await engine.dispose();
    throw error;
  }
}

class NativeClaudeObservationShadow implements ClaudeObservationShadow {
  readonly databasePath: string;
  readonly roots: readonly string[];
  private readonly ipcHosts = new Set<SpaghettiIpcHost>();
  private disposePromise: Promise<SpaghettiEngineStatus> | null = null;

  constructor(
    private readonly engine: SpaghettiEngine,
    private readonly client: SpaghettiClient,
    roots: string[],
    private readonly adapterId: string,
    private readonly displayName: string,
  ) {
    this.databasePath = engine.status.databasePath;
    this.roots = Object.freeze([...roots]);
  }

  get status(): SpaghettiEngineStatus {
    return this.engine.status;
  }

  get clientInfo(): SpaghettiClientInfo {
    return this.client.info;
  }

  serveIpc(channel: SpaghettiIpcChannel, transportKind = 'ipc'): SpaghettiIpcHost {
    if (this.disposePromise) throw new Error(`${this.displayName} observation shadow is stopping.`);

    let host: SpaghettiIpcHost | undefined;
    let unsubscribeClose = (): void => undefined;
    unsubscribeClose = channel.onClose(() => {
      if (host) this.ipcHosts.delete(host);
      unsubscribeClose();
    });
    try {
      host = serveSpaghettiIpc({
        channel,
        transport: new NapiTransport({ engine: this.engine, ownsEngine: false }),
        ownsTransport: true,
        transportKind,
      });
      this.ipcHosts.add(host);
      return host;
    } catch (error) {
      unsubscribeClose();
      void channel.close().catch(() => undefined);
      throw error;
    }
  }

  async snapshot(signal?: AbortSignal): Promise<ClaudeObservationShadowSnapshot> {
    const queryOptions = clientQueryOptions(signal);
    const [health, overview] = await Promise.all([
      this.client.getHealth(queryOptions),
      this.client.getOverview(queryOptions),
    ]);
    return {
      mode: 'shadow',
      databasePath: this.databasePath,
      roots: [...this.roots],
      status: health.status,
      health,
      overview,
    };
  }

  async compareHistory(
    legacy: ClaudeLegacyHistoryCounts,
    signal?: AbortSignal,
  ): Promise<ClaudeObservationHistoryParity> {
    return compareClaudeObservationHistory(await this.client.getOverview(clientQueryOptions(signal)), legacy);
  }

  listHistoryProjects(
    options?: SpaghettiEngineHistoryPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineHistoryProjectPage> {
    return this.client.listProjects(options, clientQueryOptions(signal));
  }

  listHistorySessions(
    projectId: string,
    options?: SpaghettiEngineHistoryPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineHistorySessionPage> {
    return this.client.listSessions({ projectId, ...options }, clientQueryOptions(signal));
  }

  getSession(sessionId: string, signal?: AbortSignal): Promise<SpaghettiEngineSessionDetails> {
    return this.client.getSession({ sessionId }, clientQueryOptions(signal));
  }

  getMessages(
    projectId: string,
    sessionId: string,
    options?: SpaghettiEngineHistoryPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineMessagePage> {
    const nativeOptions: SpaghettiEngineMessagePageOptions = { projectId, sessionId, ...options };
    return this.client.getMessages(nativeOptions, clientQueryOptions(signal));
  }

  search(options: SpaghettiEngineSearchPageOptions, signal?: AbortSignal): Promise<SpaghettiEngineSearchPage> {
    return this.client.search(options, clientQueryOptions(signal));
  }

  getTimeline(options: SpaghettiEngineTimelinePageOptions, signal?: AbortSignal): Promise<SpaghettiEngineTimelinePage> {
    return this.client.getTimeline(options, clientQueryOptions(signal));
  }

  listDelegations(
    options: SpaghettiEngineDelegationPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineDelegationPage> {
    return this.client.listDelegations(options, clientQueryOptions(signal));
  }

  listWorkflows(
    options: SpaghettiEngineWorkflowPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineWorkflowPage> {
    return this.client.listWorkflows(options, clientQueryOptions(signal));
  }

  getWorkflow(workflowId: string, signal?: AbortSignal): Promise<SpaghettiEngineWorkflowDetails> {
    return this.client.getWorkflow({ workflowId }, clientQueryOptions(signal));
  }

  listWorkflowMembers(
    workflowId: string,
    options?: SpaghettiEngineHistoryPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineWorkflowMemberPage> {
    return this.client.listWorkflowMembers({ workflowId, ...options }, clientQueryOptions(signal));
  }

  listMemoryDocuments(
    projectId: string,
    options?: SpaghettiEngineHistoryPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineMemoryDocumentPage> {
    return this.client.listMemoryDocuments({ projectId, ...options }, clientQueryOptions(signal));
  }

  listTaskCollections(
    options?: SpaghettiEngineTaskCollectionPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineTaskCollectionPage> {
    return this.client.listTaskCollections(options, clientQueryOptions(signal));
  }

  listTasks(
    collectionId: string,
    options?: SpaghettiEngineHistoryPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineTaskPage> {
    return this.client.listTasks({ collectionId, ...options }, clientQueryOptions(signal));
  }

  listPlans(options?: SpaghettiEngineHistoryPageOptions, signal?: AbortSignal): Promise<SpaghettiEnginePlanPage> {
    return this.client.listPlans(options, clientQueryOptions(signal));
  }

  listToolResults(
    projectId: string,
    sessionId: string,
    options?: SpaghettiEngineHistoryPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineToolResultPage> {
    return this.client.listToolResults({ projectId, sessionId, ...options }, clientQueryOptions(signal));
  }

  listArtifacts(
    sessionId: string,
    options?: SpaghettiEngineHistoryPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineArtifactPage> {
    return this.client.listArtifacts({ sessionId, ...options }, clientQueryOptions(signal));
  }

  listSources(options?: SpaghettiEngineHistoryPageOptions, signal?: AbortSignal): Promise<SpaghettiEngineSourcePage> {
    return this.client.listSources(options, clientQueryOptions(signal));
  }

  getStats(signal?: AbortSignal): Promise<SpaghettiEngineCanonicalStats> {
    return this.client.getStats(clientQueryOptions(signal));
  }

  getUsage(options: SpaghettiEngineUsageScopeOptions, signal?: AbortSignal): Promise<SpaghettiEngineUsageTotals> {
    return this.client.getUsage(options, clientQueryOptions(signal));
  }

  getUsageActivity(
    options: SpaghettiEngineUsageActivityOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineUsageActivity> {
    return this.client.getUsageActivity(options, clientQueryOptions(signal));
  }

  getRuntimeSnapshot(
    options?: SpaghettiEngineRuntimeSnapshotOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineRuntimeSnapshot> {
    return this.client.getRuntimeSnapshot(options, clientQueryOptions(signal));
  }

  getRunState(runId: string, signal?: AbortSignal): Promise<SpaghettiEngineRunStateLookup> {
    return this.client.getRunState({ runId }, clientQueryOptions(signal));
  }

  listTeams(options?: SpaghettiEngineTeamPageOptions, signal?: AbortSignal): Promise<SpaghettiEngineTeamPage> {
    return this.client.listTeams(options, clientQueryOptions(signal));
  }

  getTeam(teamId: string, signal?: AbortSignal): Promise<SpaghettiEngineTeamDetails> {
    return this.client.getTeam({ teamId }, clientQueryOptions(signal));
  }

  listTeamInboxes(
    teamId: string,
    options?: SpaghettiEngineHistoryPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineTeamInboxPage> {
    return this.client.listTeamInboxes({ teamId, ...options }, clientQueryOptions(signal));
  }

  listTeamInboxMessages(
    inboxId: string,
    options?: SpaghettiEngineHistoryPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineTeamInboxMessagePage> {
    const nativeOptions: SpaghettiEngineTeamInboxMessagePageOptions = { inboxId, ...options };
    return this.client.listTeamInboxMessages(nativeOptions, clientQueryOptions(signal));
  }

  refresh(signal?: AbortSignal): Promise<SpaghettiEngineStatus> {
    return this.engine.refreshObservation(this.adapterId, signal);
  }

  async dispose(): Promise<SpaghettiEngineStatus> {
    if (!this.disposePromise) {
      this.disposePromise = (async () => {
        const ipcHosts = [...this.ipcHosts];
        this.ipcHosts.clear();
        await Promise.allSettled(ipcHosts.map((host) => host.dispose()));
        await this.client.dispose();
        return await this.engine.dispose();
      })();
    }
    return await this.disposePromise;
  }
}

function clientQueryOptions(signal: AbortSignal | undefined): SpaghettiQueryOptions | undefined {
  return signal ? { signal } : undefined;
}

function normalizeRoots(roots: string[]): string[] {
  if (!Array.isArray(roots) || roots.length === 0) {
    throw new Error('Claude observation shadow requires at least one explicit source root.');
  }
  const normalized = roots.map((root) => canonicalPotentialPath(root, 'source root'));
  return [...new Set(normalized)];
}

function resolveDatabaseCandidate(value: string, label: string): string {
  if (typeof value !== 'string' || value.trim() === '' || value === ':memory:') {
    throw new Error(`Claude observation ${label} must be a non-empty file-backed path.`);
  }
  return resolve(value);
}

/** Resolve symlinked ancestors even when the leaf does not exist yet. */
function canonicalPotentialPath(value: string, label: string): string {
  const absolute = resolveDatabaseCandidate(value, label);
  return canonicalPotentialAbsolutePath(absolute, label, new Set());
}

function canonicalPotentialAbsolutePath(absolute: string, label: string, seenLinks: Set<string>): string {
  let existing = absolute;
  const missing: string[] = [];
  while (true) {
    const entry = lstatSync(existing, { throwIfNoEntry: false });
    if (entry?.isSymbolicLink()) {
      const linkIdentity = pathIdentity(existing);
      if (seenLinks.has(linkIdentity)) {
        throw new Error(`Claude observation ${label} contains a symbolic-link cycle: ${absolute}`);
      }
      seenLinks.add(linkIdentity);
      const target = resolve(dirname(existing), readlinkSync(existing));
      return resolve(canonicalPotentialAbsolutePath(target, label, seenLinks), ...missing);
    }
    if (entry) {
      if (missing.length > 0 && !entry.isDirectory()) {
        throw new Error(`Claude observation ${label} has a non-directory ancestor: ${existing}`);
      }
      return resolve(realpathSync.native(existing), ...missing);
    }
    const parent = dirname(existing);
    if (parent === existing) {
      throw new Error(`Claude observation ${label} could not be resolved: ${absolute}`);
    }
    missing.unshift(basename(existing));
    existing = parent;
  }
}

function artifactFamily(databasePath: string): Set<string> {
  return new Set(
    [
      databasePath,
      `${databasePath}-wal`,
      `${databasePath}-shm`,
      `${databasePath}-journal`,
      `${databasePath}${OWNER_LOCK_SUFFIX}`,
      `${databasePath}${OWNER_METADATA_SUFFIX}`,
    ].map(pathIdentity),
  );
}

function assertIsolatedDatabaseArtifacts(productionPath: string, shadowPath: string): void {
  const productionArtifacts = artifactFamily(productionPath);
  const shadowArtifacts = artifactFamily(shadowPath);
  for (const artifact of shadowArtifacts) {
    if (productionArtifacts.has(artifact)) {
      throw new Error(
        `Claude observation shadow database must be isolated from the production database: ${shadowPath}`,
      );
    }
  }
}

function pathIdentity(value: string): string {
  // Windows paths are case-insensitive. Default macOS volumes are too; being
  // conservative on a case-sensitive macOS volume only rejects a risky alias.
  return process.platform === 'win32' || process.platform === 'darwin' ? value.toLocaleLowerCase('en-US') : value;
}

function assertCount(label: string, value: number): void {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new Error(`Claude legacy ${label} count must be a non-negative safe integer.`);
  }
}

function assertLegacyUsageValues(label: string, values: ClaudeLegacyUsageValues): void {
  assertCount(`${label} inputTokens`, values.inputTokens);
  assertCount(`${label} outputTokens`, values.outputTokens);
  assertCount(`${label} cacheCreationTokens`, values.cacheCreationTokens);
  assertCount(`${label} cacheReadTokens`, values.cacheReadTokens);
}

function usageValueMismatches(
  canonical: SpaghettiEngineUsageTotals['aggregate']['exact'],
  legacy: ClaudeLegacyUsageValues,
): string[] {
  const fields: string[] = [];
  if (canonical.inputTokens !== legacy.inputTokens) fields.push('inputTokens');
  if (canonical.outputTokens !== legacy.outputTokens) fields.push('outputTokens');
  if (canonical.cacheCreationTokens !== legacy.cacheCreationTokens) fields.push('cacheCreationTokens');
  if (canonical.cacheReadTokens !== legacy.cacheReadTokens) fields.push('cacheReadTokens');
  return fields;
}

function usageValuesAreZero(values: ClaudeLegacyUsageValues): boolean {
  return (
    values.inputTokens === 0 &&
    values.outputTokens === 0 &&
    values.cacheCreationTokens === 0 &&
    values.cacheReadTokens === 0
  );
}

function nonzeroUsageFields(values: ClaudeLegacyUsageValues): string[] {
  const fields: string[] = [];
  if (values.inputTokens !== 0) fields.push('inputTokens');
  if (values.outputTokens !== 0) fields.push('outputTokens');
  if (values.cacheCreationTokens !== 0) fields.push('cacheCreationTokens');
  if (values.cacheReadTokens !== 0) fields.push('cacheReadTokens');
  return fields;
}

function assertIsoDate(label: string, value: string): void {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(value)) throw new Error(`${label} must use YYYY-MM-DD form: ${value}`);
}

function uniqueBy<T>(values: readonly T[], keyOf: (value: T) => string, label: string): Map<string, T> {
  const output = new Map<string, T>();
  for (const value of values) {
    const key = keyOf(value);
    if (typeof key !== 'string' || key.trim() === '') throw new Error(`${label} identity must be a non-empty string.`);
    if (output.has(key)) throw new Error(`${label} identity is duplicated: ${key}`);
    output.set(key, value);
  }
  return output;
}
