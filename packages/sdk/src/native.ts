/**
 * Native addon loader — `@vibecook/spaghetti-sdk-native`.
 *
 * The Rust ingest core (RFC 003) ships as a separate native addon. This
 * module loads it opportunistically: if the addon is missing or fails
 * to load (unsupported platform, broken install), the SDK falls back
 * to the pure-TypeScript ingest path.
 *
 * As of Phase 4 (cutover, 0.7.0) the native path is the **default** —
 * set `SPAG_NATIVE_INGEST=0` to force the TS path.
 */

import { createRequire } from 'node:module';

import { resolveEngine, type IngestEngine } from './settings.js';

export interface NativeIngestOptions {
  /**
   * Agent data root on disk (e.g. `~/.claude` for Claude Code, `~/.codex` for Codex).
   * Paired with {@link sourceId} to select the native reader.
   */
  agentDir: string;
  dbPath: string;
  mode: 'cold' | 'warm';
  parallelism?: number;
  progressIntervalMs?: number;
  /**
   * Agent product id stamped on core rows (default `claude-code`).
   * Pass explicitly for multi-source native ingest (e.g. `codex`).
   */
  sourceId?: string;
  /**
   * Crash-safer bulk SQLite settings (WAL + synchronous=NORMAL) instead of
   * MEMORY + OFF. Prefer for long-lived desktop apps. Requires a native
   * addon that understands this field (ignored by older builds).
   */
  safeBulk?: boolean;
}

/**
 * Severity of one surfaced ingest error (RFC 008 Phase 2).
 *
 * - `record-skip` — a bad record inside an otherwise fine project. Reported;
 *   does not roll back or poison the project.
 * - `project-fatal` — unreadable project input. Rolls back that project; later
 *   projects still ingest.
 * - `source` — failed before any project identity existed (discovery, a
 *   pre-identity read). Has no slug, poisons nothing, but invalidates the
 *   source's success marker so the next warm run retries.
 *
 * Frozen in Phase 0, produced as of Phase 2 — see {@link NativeIngestError}.
 */
export type NativeIngestErrorSeverity = 'record-skip' | 'project-fatal' | 'source';

/**
 * A native ingest error.
 *
 * `slug` is optional because a failure that happens *before* a project slug
 * exists cannot name one, and inventing a fake slug is forbidden — it would
 * become a real row. Such failures used to be swallowed for exactly this
 * reason. `path` is mandatory in exchange, so every surfaced error can name a
 * file even when it cannot name a project.
 */
export interface NativeIngestError {
  /** Absent for `source` severity — no project identity existed yet. */
  slug?: string;
  /** Always present. The file the error is about. */
  path: string;
  severity: NativeIngestErrorSeverity;
  message: string;
}

/**
 * The error-reporting fields on ingest stats.
 *
 * `errors` is capped for display while `errorCount` stays uncapped, so a caller
 * can say "12 of 4,000 failures" instead of silently showing the first hundred
 * as if they were all of them. `errorsTruncated` makes that distinction
 * checkable rather than inferred from a length comparison.
 */
export interface NativeIngestErrorReport {
  /** First N errors, for display. Capped — do not use as a count. */
  errors: NativeIngestError[];
  /** Uncapped total, however many were kept for display. */
  errorCount: number;
  /** True when `errors.length < errorCount`. */
  errorsTruncated: boolean;
}

export interface NativeIngestStats extends NativeIngestErrorReport {
  durationMs: number;
  projectsProcessed: number;
  sessionsProcessed: number;
  messagesWritten: number;
  subagentsWritten: number;
}

export interface NativeIngestProgress {
  /** `scanning` | `parsing` | `finalizing` */
  phase: string;
  projectsDone: number;
  projectsTotal: number;
  elapsedMs: number;
}

export type NativeProgressCallback = (progress: NativeIngestProgress) => void;

/**
 * One row destined for the live-ingest path. Mirrors
 * `crates/spaghetti-napi/src/orchestrate/live_ingest.rs::LiveRow` — see that
 * module's category → payload table for the wire format.
 */
export interface NativeLiveRow {
  category: string;
  slug?: string;
  sessionId?: string;
  /** JSON-encoded payload whose shape is determined by `category`. */
  payloadJson: string;
}

export interface NativeLiveRowId {
  category: string;
  slug?: string;
  sessionId?: string;
  /**
   * Stable per-category identifier of the row that landed. Matches the
   * `row_key` computed on the Rust side (see
   * `crates/spaghetti-napi/src/orchestrate/live_ingest.rs::row_to_event`).
   */
  rowKey: string;
}

export interface NativeLiveBatchResult {
  writtenRows: NativeLiveRowId[];
  /** Wall-clock duration of the whole call (ms). */
  durationMs: number;
}

/** Options for the persistent RFC 011 engine shell. */
export interface SpaghettiEngineOpenOptions {
  dbPath: string;
  /** Persistent read-only SQLite workers. Defaults to 2; maximum 16. */
  queryWorkers?: number;
  /** Diagnostic host label written to the owner metadata sidecar. */
  ownerLabel?: string;
}

/** Structured metadata for the process that exclusively owns a database. */
export interface SpaghettiEngineOwner {
  protocolVersion: number;
  ownerId: string;
  ownerLabel: string;
  processId: number;
  startedAtUnixMs: number;
  databasePath: string;
  executable?: string;
  hostname?: string;
  engineVersion: string;
}

/** Engine-owned observation lifecycle and bounded recovery backlog. */
export interface SpaghettiEngineObservationStatus {
  state: 'idle' | 'scanning' | 'reconciling' | 'live' | 'dirty' | 'degraded' | 'stopped';
  reconcileInFlight: boolean;
  dirtyInstances: number;
  fullReconcileRequired: boolean;
  /** A known-loss or retry condition remains, even during an active repair pass. */
  recoveryRequired: boolean;
  supervisorsRunning: number;
  watchedInstances: number;
  /** Consolidated physical roots registered with native watcher backends. */
  watchRoots: number;
  reconcilesTotal: number;
  failedReconcilesTotal: number;
  retrySignalsTotal: number;
  queueOverflowsTotal: number;
  lastCommitSeq?: number;
  lastStartedAtUnixMs?: number;
  lastFinishedAtUnixMs?: number;
  lastError?: string;
}

export interface SpaghettiEngineStatus {
  state: 'running' | 'stopping' | 'stopped';
  databasePath: string;
  acceptingQueries: boolean;
  writerAlive: boolean;
  configuredQueryWorkers: number;
  aliveQueryWorkers: number;
  inFlightQueries: number;
  observation: SpaghettiEngineObservationStatus;
  owner?: SpaghettiEngineOwner;
}

export interface SpaghettiEngineHealth {
  status: SpaghettiEngineStatus;
  healthy: boolean;
  detail?: string;
}

/** First typed read model exposed by the persistent engine. */
export interface SpaghettiEngineOverview {
  schemaVersion: number;
  /** Latest durable ingest commit visible to the read-only query snapshot. */
  commitSeq: number;
  /** Transitional compatibility-table counts; not populated by RFC 011 observation. */
  projects: number;
  sessions: number;
  messages: number;
  /** Canonical history materialized by RFC 011 observation commits. */
  canonicalSessions: number;
  canonicalMessages: number;
  writerDataVersion: number;
  journalMode: string;
  queryOnly: boolean;
  readOnly: boolean;
}

export interface SpaghettiEngineHistoryPageOptions {
  /** Opaque keyset cursor returned by the preceding page. */
  cursor?: string;
  /** Page size. Defaults to 50 and must be between 1 and 200. */
  limit?: number;
}

export interface SpaghettiEngineHistorySessionPageOptions extends SpaghettiEngineHistoryPageOptions {
  /** Opaque project identity returned by {@link SpaghettiEngine.listHistoryProjects}. */
  projectId: string;
}

export type SpaghettiEngineHistoryActivitySource = 'message' | 'session' | 'session_index';

export interface SpaghettiEngineHistoryProjectIndex {
  status: string;
  originalPath?: string;
  entryCount: number;
  assertionCount: number;
  competingSnapshotCount: number;
  lastCommitSeq: number;
}

/** Rust-owned canonical project aggregation, without compatibility-table guesses. */
export interface SpaghettiEngineHistoryProject {
  projectId: string;
  adapterId: string;
  sourceInstanceId: number;
  nativeProjectKey: string;
  /** Transcript-backed sessions only; metadata-only index entries are not promoted to history. */
  transcriptSessionCount: number;
  messageCount: number;
  memoryDocumentCount: number;
  /** A native memory-index document exists; topic documents alone do not set this flag. */
  hasMemoryIndex: boolean;
  latestActivityAt?: string;
  latestActivitySource?: SpaghettiEngineHistoryActivitySource;
  index?: SpaghettiEngineHistoryProjectIndex;
  lastCommitSeq: number;
}

export interface SpaghettiEngineHistoryProjectPage {
  contractVersion: number;
  atCommitSeq: number;
  items: SpaghettiEngineHistoryProject[];
  nextCursor?: string;
}

export type SpaghettiEngineTimestampQuality =
  | 'native_exact'
  | 'native_approximate'
  | 'file_metadata_fallback'
  | 'derived';

export interface SpaghettiEngineHistorySessionIndex {
  fullPath: string;
  fileMtimeMs: number;
  firstPrompt: string;
  summary?: string;
  messageCount: number;
  createdAt: string;
  createdAtQuality: SpaghettiEngineTimestampQuality;
  modifiedAt: string;
  modifiedAtQuality: SpaghettiEngineTimestampQuality;
  gitBranch: string;
  projectPath: string;
  isSidechain: boolean;
  transcriptStatus: string;
  resolutionStatus: string;
  assertionCount: number;
  competingEntryCount: number;
  identityConflict: boolean;
  joinConflict: boolean;
  lastCommitSeq: number;
}

/** Transcript-backed canonical session with separately sourced native-index enrichment. */
export interface SpaghettiEngineHistorySession {
  sessionId: string;
  projectId: string;
  nativeSessionId: string;
  nativeProjectKey: string;
  cwd?: string;
  gitBranch?: string;
  firstPrompt?: string;
  aiTitle?: string;
  customTitle?: string;
  messageCount: number;
  firstMessageAt?: string;
  firstMessageTimeQuality?: SpaghettiEngineTimestampQuality;
  lastMessageAt?: string;
  lastMessageTimeQuality?: SpaghettiEngineTimestampQuality;
  latestActivityAt?: string;
  latestActivitySource?: SpaghettiEngineHistoryActivitySource;
  index?: SpaghettiEngineHistorySessionIndex;
  lastCommitSeq: number;
}

export interface SpaghettiEngineHistorySessionPage {
  contractVersion: number;
  atCommitSeq: number;
  projectId: string;
  items: SpaghettiEngineHistorySession[];
  nextCursor?: string;
}

export interface SpaghettiEngineMessagePageOptions extends SpaghettiEngineHistoryPageOptions {
  /** Opaque project identity returned by {@link SpaghettiEngine.listHistoryProjects}. */
  projectId: string;
  /** Opaque session identity returned by {@link SpaghettiEngine.listHistorySessions}. */
  sessionId: string;
}

/** Exact transcript-backed session lookup plus counts from one committed snapshot. */
export interface SpaghettiEngineSessionDetail {
  sessionId: string;
  projectId: string;
  adapterId: string;
  sourceInstanceId: number;
  nativeSessionId: string;
  nativeProjectKey: string;
  cwd?: string;
  gitBranch?: string;
  firstPrompt?: string;
  aiTitle?: string;
  customTitle?: string;
  sourceTime?: string;
  sourceTimeQuality?: SpaghettiEngineTimestampQuality;
  messageCount: number;
  runCount: number;
  presenceCount: number;
  taskCollectionCount: number;
  artifactCount: number;
  workflowCount: number;
  persistedToolResultCount: number;
  projectMemoryDocumentCount: number;
  index?: SpaghettiEngineHistorySessionIndex;
  decisiveFactId: string;
  observedAtUnixMs: number;
  sourceObjectId: number;
  sourceGeneration: number;
  lastCommitSeq: number;
}

export interface SpaghettiEngineSessionDetails {
  contractVersion: number;
  atCommitSeq: number;
  /** Absent when a well-formed opaque identity is not present. */
  session?: SpaghettiEngineSessionDetail;
}

/** Canonical message fields with the lossless source record kept separately. */
export interface SpaghettiEngineMessageDetail {
  messageId: string;
  sessionId: string;
  projectId: string;
  adapterId: string;
  sourceInstanceId: number;
  nativeSessionId: string;
  nativeProjectKey: string;
  nativeMessageId?: string;
  nativeKind: string;
  role: string;
  content: unknown;
  nativePayload: unknown;
  sourceTime?: string;
  sourceTimeQuality?: SpaghettiEngineTimestampQuality;
  parentNativeMessageId?: string;
  model?: string;
  searchText?: string;
  decisiveFactId: string;
  observedAtUnixMs: number;
  sourceObjectId: number;
  sourceGeneration: number;
  lastCommitSeq: number;
}

export interface SpaghettiEngineMessagePage {
  contractVersion: number;
  atCommitSeq: number;
  projectId: string;
  sessionId: string;
  items: SpaghettiEngineMessageDetail[];
  /** UTF-8 bytes in canonical content JSON plus native payload JSON. */
  payloadBytes: number;
  payloadByteLimit: number;
  nextCursor?: string;
}

export interface SpaghettiEngineSourceSummary {
  sourceId: string;
  sourceInstanceId: number;
  adapterId: string;
  displayName: string;
  adapterContractVersion: number;
  discoveredAtUnixMs: number;
  lastSeenAtUnixMs: number;
  streamCount: number;
  unavailableStreamCount: number;
  objectCount: number;
  activeObjectCount: number;
  recordErrorCount: number;
  factCount: number;
  commitCount: number;
  lastCommitSeq?: number;
}

export interface SpaghettiEngineSourcePage {
  contractVersion: number;
  atCommitSeq: number;
  items: SpaghettiEngineSourceSummary[];
  nextCursor?: string;
}

export interface SpaghettiEngineNamedCount {
  name: string;
  count: number;
}

/** Canonical/catalog statistics; compatibility-cache tables are excluded. */
export interface SpaghettiEngineCanonicalStats {
  contractVersion: number;
  atCommitSeq: number;
  schemaVersion: number;
  sourceInstances: number;
  sourceStreams: number;
  sourceObjects: number;
  activeSourceObjects: number;
  sourceRecordErrors: number;
  ingestCommits: number;
  factRecords: number;
  searchableMessages: number;
  entities: SpaghettiEngineNamedCount[];
  sourceStreamStates: SpaghettiEngineNamedCount[];
  projectionReadiness: SpaghettiEngineNamedCount[];
  databasePageCount: number;
  databasePageSizeBytes: number;
  allocatedDatabaseBytes: number;
}

export interface SpaghettiEngineUsageScopeOptions {
  /** Opaque project identity returned by {@link SpaghettiEngine.listHistoryProjects}. */
  projectId: string;
  /** Optional opaque session identity returned by {@link SpaghettiEngine.listHistorySessions}. */
  sessionId?: string;
}

export interface SpaghettiEngineUsageActivityOptions extends SpaghettiEngineUsageScopeOptions {
  /** Inclusive calendar date in YYYY-MM-DD form. */
  from: string;
  /** Inclusive calendar date in YYYY-MM-DD form. */
  to: string;
}

export interface SpaghettiEngineUsageTokenValues {
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  /** Arithmetic sum of preserved components, not a provider billing normalization. */
  componentTotalTokens: number;
}

export type SpaghettiEngineUsageQuality = 'exact' | 'estimated' | 'mixed' | 'unavailable';
export type SpaghettiEngineUsageScope = 'record' | 'message' | 'turn' | 'run' | 'session' | 'team' | 'project';
export type SpaghettiEngineUsageAccounting = 'delta' | 'cumulative' | 'snapshot';
export type SpaghettiEngineValueQuality = 'native_exact' | 'native_approximate' | 'derived_exact' | 'estimated';

export interface SpaghettiEngineUsageAggregate {
  exact: SpaghettiEngineUsageTokenValues;
  estimated: SpaghettiEngineUsageTokenValues;
  combined: SpaghettiEngineUsageTokenValues;
  quality: SpaghettiEngineUsageQuality;
  exactContributionCount: number;
  estimatedContributionCount: number;
  contributionCount: number;
  sessionCount: number;
}

export interface SpaghettiEngineUsageCoverage {
  scope: SpaghettiEngineUsageScope;
  accounting: SpaghettiEngineUsageAccounting;
  valueQuality: SpaghettiEngineValueQuality;
  qualityBucket: 'exact' | 'estimated';
  model?: string;
  sourceTimeQuality?: SpaghettiEngineTimestampQuality;
  contributionCount: number;
  tokens: SpaghettiEngineUsageTokenValues;
}

export interface SpaghettiEngineUsageTotals {
  contractVersion: number;
  atCommitSeq: number;
  projectId: string;
  sessionId?: string;
  aggregate: SpaghettiEngineUsageAggregate;
  coverage: SpaghettiEngineUsageCoverage[];
  firstSourceTime?: string;
  lastSourceTime?: string;
  firstObservedAtUnixMs?: number;
  lastObservedAtUnixMs?: number;
  lastCommitSeq?: number;
}

export interface SpaghettiEngineUsageActivityDay {
  date: string;
  aggregate: SpaghettiEngineUsageAggregate;
  firstSourceTime: string;
  lastSourceTime: string;
  firstObservedAtUnixMs: number;
  lastObservedAtUnixMs: number;
  lastCommitSeq: number;
}

export interface SpaghettiEngineUntimedUsage {
  aggregate: SpaghettiEngineUsageAggregate;
  coverage: SpaghettiEngineUsageCoverage[];
  firstObservedAtUnixMs?: number;
  lastObservedAtUnixMs?: number;
  lastCommitSeq?: number;
}

export interface SpaghettiEngineUsageActivity {
  contractVersion: number;
  atCommitSeq: number;
  projectId: string;
  sessionId?: string;
  from: string;
  to: string;
  days: SpaghettiEngineUsageActivityDay[];
  aggregate: SpaghettiEngineUsageAggregate;
  coverage: SpaghettiEngineUsageCoverage[];
  untimed: SpaghettiEngineUntimedUsage;
  firstObservedAtUnixMs?: number;
  lastObservedAtUnixMs?: number;
  lastCommitSeq?: number;
}

export interface SpaghettiEngineRuntimeSnapshotOptions extends SpaghettiEngineHistoryPageOptions {
  /** Optional project scope. Omit it to retain orphan run/presence evidence. */
  projectId?: string;
  /** Optional session scope. Membership is validated when projectId is also supplied. */
  sessionId?: string;
}

export type SpaghettiEngineRunState =
  | 'declared'
  | 'active'
  | 'waiting'
  | 'succeeded'
  | 'failed'
  | 'cancelled'
  | 'unknown';

export interface SpaghettiEngineRuntimeRunEvidence {
  evidenceId: string;
  kind: string;
  strength: string;
  nativeState?: string;
  sourceTime?: string;
  sourceTimeQuality?: SpaghettiEngineTimestampQuality;
  observedAtUnixMs: number;
  sourceObjectId: number;
  lastCommitSeq: number;
}

export interface SpaghettiEngineRuntimeRun {
  runId: string;
  sessionId: string;
  projectId?: string;
  adapterId: string;
  sourceInstanceId: number;
  nativeRunId: string;
  parentRunId?: string;
  nativeSessionId?: string;
  nativeProjectKey?: string;
  sessionPresent: boolean;
  state?: SpaghettiEngineRunState;
  decisiveEvidence?: SpaghettiEngineRuntimeRunEvidence;
  evidenceCount: number;
  lastActivityAt?: string;
  terminalAt?: string;
  /** Current registry-object evidence count, not a PID-liveness claim. */
  presenceCount: number;
  conflictingPresenceCount: number;
  lastCommitSeq: number;
}

export interface SpaghettiEngineRuntimePresence {
  presenceId: string;
  sessionId: string;
  runId: string;
  projectId?: string;
  adapterId: string;
  sourceInstanceId: number;
  nativeSessionId: string;
  nativePid: number;
  cwd: string;
  startedAt: string;
  startedAtQuality: SpaghettiEngineTimestampQuality;
  nativeKind?: string;
  entrypoint?: string;
  name?: string;
  nativeStatus?: string;
  updatedAt?: string;
  updatedAtQuality?: SpaghettiEngineTimestampQuality;
  statusUpdatedAt?: string;
  statusUpdatedAtQuality?: SpaghettiEngineTimestampQuality;
  nativeProcessStartedAt?: string;
  version?: string;
  peerProtocol?: number;
  nameSource?: string;
  bridgeSessionId?: string;
  messagingSocketPath?: string;
  presenceStatus: 'resolved' | 'conflicting';
  decisiveFactId: string;
  assertionCount: number;
  competingAssertionCount: number;
  observedAtUnixMs: number;
  sessionPresent: boolean;
  runPresent: boolean;
  lastCommitSeq: number;
}

export type SpaghettiEngineRuntimeEntry =
  | { kind: 'run'; run: SpaghettiEngineRuntimeRun; presence?: never }
  | { kind: 'presence'; run?: never; presence: SpaghettiEngineRuntimePresence };

export interface SpaghettiEngineRuntimeSnapshot {
  contractVersion: number;
  atCommitSeq: number;
  projectId?: string;
  sessionId?: string;
  entries: SpaghettiEngineRuntimeEntry[];
  nextCursor?: string;
}

export interface SpaghettiEngineRunStateLookup {
  contractVersion: number;
  atCommitSeq: number;
  /** Absent when a well-formed opaque identity is not present. */
  run?: SpaghettiEngineRuntimeRun;
}

export type SpaghettiEngineTeamPageOptions = SpaghettiEngineHistoryPageOptions;

export interface SpaghettiEngineTeamScopedPageOptions extends SpaghettiEngineHistoryPageOptions {
  teamId: string;
}

export interface SpaghettiEngineTeamInboxMessagePageOptions extends SpaghettiEngineHistoryPageOptions {
  inboxId: string;
}

export interface SpaghettiEngineTeamConfig {
  name: string;
  description?: string;
  createdAt: string;
  createdAtQuality: SpaghettiEngineTimestampQuality;
  leadMemberId?: string;
  leadMemberPresent: boolean;
  nativeLeadAgentId: string;
  leadSessionId: string;
  leadSessionPresent: boolean;
  nativeLeadSessionId: string;
  configStatus: 'resolved' | 'conflicting';
  decisiveFactId: string;
  assertionCount: number;
  competingSnapshotCount: number;
  memberCount: number;
  lastCommitSeq: number;
}

export interface SpaghettiEngineTeamSummary {
  teamId: string;
  adapterId: string;
  sourceInstanceId: number;
  nativeTeamId: string;
  config?: SpaghettiEngineTeamConfig;
  inboxCount: number;
  messageCount: number;
  unreadMessageCount: number;
  conflictingInboxCount: number;
  conflictingMessageCount: number;
  lastCommitSeq: number;
}

export interface SpaghettiEngineTeamPage {
  contractVersion: number;
  atCommitSeq: number;
  items: SpaghettiEngineTeamSummary[];
  nextCursor?: string;
}

export interface SpaghettiEngineTeamMember {
  memberId: string;
  teamId: string;
  memberOrdinal: number;
  nativeAgentId: string;
  nativeName: string;
  agentType?: string;
  model?: string;
  prompt?: string;
  color?: string;
  planModeRequired?: boolean;
  joinedAt: string;
  joinedAtQuality: SpaghettiEngineTimestampQuality;
  tmuxPaneId: string;
  cwd: string;
  subscriptions: string[];
  backendType?: string;
  membershipStatus: 'resolved' | 'conflicting';
  decisiveFactId: string;
  assertionCount: number;
  competingMembershipCount: number;
  lastCommitSeq: number;
}

export interface SpaghettiEngineTeamDetails {
  contractVersion: number;
  atCommitSeq: number;
  team: SpaghettiEngineTeamSummary;
  members: SpaghettiEngineTeamMember[];
}

export interface SpaghettiEngineTeamInbox {
  inboxId: string;
  teamId: string;
  recipientId: string;
  recipientPresent: boolean;
  nativeTeamId: string;
  nativeRecipientName: string;
  inboxStatus: 'resolved' | 'conflicting';
  decisiveFactId: string;
  assertionCount: number;
  competingSnapshotCount: number;
  messageCount: number;
  unreadMessageCount: number;
  conflictingMessageCount: number;
  lastCommitSeq: number;
}

export interface SpaghettiEngineTeamInboxPage {
  contractVersion: number;
  atCommitSeq: number;
  teamId: string;
  items: SpaghettiEngineTeamInbox[];
  nextCursor?: string;
}

export interface SpaghettiEngineTeamInboxMessage {
  messageId: string;
  inboxId: string;
  senderId: string;
  senderPresent: boolean;
  messageOrdinal: number;
  nativeMessageId?: string;
  nativeKind?: string;
  nativeVersion?: number;
  nativeSenderName: string;
  text: string;
  summary?: string;
  color?: string;
  sourceTime: string;
  sourceTimeQuality: SpaghettiEngineTimestampQuality;
  read: boolean;
  messageStatus: 'resolved' | 'conflicting';
  decisiveFactId: string;
  assertionCount: number;
  competingMessageCount: number;
  lastCommitSeq: number;
}

export interface SpaghettiEngineTeamInboxMessagePage {
  contractVersion: number;
  atCommitSeq: number;
  inboxId: string;
  teamId: string;
  nativeTeamId: string;
  nativeRecipientName: string;
  items: SpaghettiEngineTeamInboxMessage[];
  nextCursor?: string;
}

export interface SpaghettiEngineReconcileOptions {
  /** Configured Claude Code data roots, such as `~/.claude`. */
  roots: string[];
  /** Durable ingest reason. Defaults to `manual_reconcile`. */
  reason?: string;
}

export interface SpaghettiEngineObservationOptions {
  /** Configured Claude Code data roots, such as `~/.claude`. */
  roots: string[];
  /** Durable reason prefix. Defaults to `native_watch`. */
  reason?: string;
}

export interface SpaghettiEngineReconcileResult {
  instancesDiscovered: number;
  streamsReconciled: number;
  streamsUnavailable: number;
  objectsDiscovered: number;
  objectsRegistered: number;
  objectsChanged: number;
  objectsUnchanged: number;
  objectsRemoved: number;
  recordsDecoded: number;
  recordsQuarantined: number;
  retriesRequired: number;
  commits: number;
  lastCommitSeq?: number;
}

/** Async handle backed by one persistent Rust engine lifecycle. */
export interface SpaghettiEngine {
  readonly status: SpaghettiEngineStatus;
  health(signal?: AbortSignal): Promise<SpaghettiEngineHealth>;
  overview(signal?: AbortSignal): Promise<SpaghettiEngineOverview>;
  listHistoryProjects(
    options?: SpaghettiEngineHistoryPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineHistoryProjectPage>;
  listHistorySessions(
    options: SpaghettiEngineHistorySessionPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineHistorySessionPage>;
  getSession(sessionId: string, signal?: AbortSignal): Promise<SpaghettiEngineSessionDetails>;
  getMessages(options: SpaghettiEngineMessagePageOptions, signal?: AbortSignal): Promise<SpaghettiEngineMessagePage>;
  listSources(options?: SpaghettiEngineHistoryPageOptions, signal?: AbortSignal): Promise<SpaghettiEngineSourcePage>;
  getStats(signal?: AbortSignal): Promise<SpaghettiEngineCanonicalStats>;
  getUsage(options: SpaghettiEngineUsageScopeOptions, signal?: AbortSignal): Promise<SpaghettiEngineUsageTotals>;
  getUsageActivity(
    options: SpaghettiEngineUsageActivityOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineUsageActivity>;
  getRuntimeSnapshot(
    options?: SpaghettiEngineRuntimeSnapshotOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineRuntimeSnapshot>;
  getRunState(runId: string, signal?: AbortSignal): Promise<SpaghettiEngineRunStateLookup>;
  listTeams(options?: SpaghettiEngineTeamPageOptions, signal?: AbortSignal): Promise<SpaghettiEngineTeamPage>;
  getTeam(teamId: string, signal?: AbortSignal): Promise<SpaghettiEngineTeamDetails>;
  listTeamInboxes(
    options: SpaghettiEngineTeamScopedPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineTeamInboxPage>;
  listTeamInboxMessages(
    options: SpaghettiEngineTeamInboxMessagePageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineTeamInboxMessagePage>;
  reconcileClaude(
    options: SpaghettiEngineReconcileOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineReconcileResult>;
  /** Register native watchers before the initial scan and supervise changes in Rust. */
  startClaudeObservation(
    options: SpaghettiEngineObservationOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineStatus>;
  /** Force the running supervisor through its common reconcile path. */
  refreshClaudeObservation(signal?: AbortSignal): Promise<SpaghettiEngineStatus>;
  /** Stop Claude watch registration without disposing the engine. */
  stopClaudeObservation(signal?: AbortSignal): Promise<SpaghettiEngineStatus>;
  cancelPendingQueries(): number;
  dispose(): Promise<SpaghettiEngineStatus>;
}

export interface NativeAddon {
  /** Returns the semver of the loaded native addon. */
  nativeVersion(): string;
  /**
   * Run a full ingest and resolve to the stats. Optionally receives a
   * progress callback invoked from the libuv worker thread (safe from
   * any thread — caller need not synchronise).
   */
  ingest(opts: NativeIngestOptions, onProgress?: NativeProgressCallback): Promise<NativeIngestStats>;
  /**
   * Write a batch of live-update rows to the SQLite DB at `dbPath`.
   * Wraps `writer::write_batch_with_tx` (RFC 005 Phase 4 C4.1) so the
   * live-ingest path shares the cold-start writer's transaction +
   * UPSERT semantics.
   *
   * Synchronous on the Rust side (the whole batch is one BEGIN
   * IMMEDIATE / COMMIT) — the TS caller wraps it in a Promise at the
   * call site if it wants to interop with the rest of the async
   * live-updates pipeline.
   *
   * Throws on any single-row failure (bad JSON, unknown category,
   * SQLite error); the whole batch is rolled back and the TS side
   * falls back to its own writer.
   */
  /**
   * Write a live batch. Optional `sourceId` defaults to `claude-code` on
   * the Rust side when omitted.
   */
  liveIngestBatch(dbPath: string, rows: NativeLiveRow[], sourceId?: string): NativeLiveBatchResult;
  /** Open the persistent RFC 011 engine shell off the JavaScript thread. */
  openSpaghettiEngine(options: SpaghettiEngineOpenOptions): Promise<SpaghettiEngine>;
}

/**
 * Why the native addon did not load, in terms a user can act on.
 *
 * The loader used to swallow the require failure entirely, so "the addon is
 * missing" and "the addon exists but this platform has no prebuilt binary" and
 * "the binary is there but its glibc is too old" all looked identical: the
 * engine silently became `ts` and nothing said why. RFC 008 Phase 4 requires
 * the missing-addon path to be loud and actionable.
 */
export class EngineUnavailableError extends Error {
  /** `process.platform` — `linux`, `darwin`, `win32`. */
  readonly platform: string;
  /** `process.arch` — `x64`, `arm64`. */
  readonly arch: string;
  /**
   * `glibc`, `musl`, or `null` off Linux.
   *
   * Worth carrying because it is the difference between "unsupported
   * platform" and "wrong artifact for a supported one" — an Alpine container
   * is x64 Linux and still cannot load a GNU build.
   */
  readonly libc: 'glibc' | 'musl' | null;
  /** Version of the addon package the SDK expects. */
  readonly expectedVersion: string;
  /** The underlying resolution failure. */
  readonly cause: unknown;

  constructor(init: {
    platform: string;
    arch: string;
    libc: 'glibc' | 'musl' | null;
    expectedVersion: string;
    cause: unknown;
  }) {
    const target = [init.platform, init.arch, init.libc].filter(Boolean).join('-');
    super(
      `Native ingest addon unavailable for ${target}. ` +
        `Falling back to the TypeScript engine, which is slower but produces the same index. ` +
        `${installHint(init.platform, init.libc)}`,
    );
    this.name = 'EngineUnavailableError';
    this.platform = init.platform;
    this.arch = init.arch;
    this.libc = init.libc;
    this.expectedVersion = init.expectedVersion;
    this.cause = init.cause;
  }
}

function installHint(platform: string, libc: 'glibc' | 'musl' | null): string {
  if (platform === 'linux' && libc === 'musl') {
    return (
      'On Alpine and other musl systems, install the musl build: ' +
      '`npm i @vibecook/spaghetti-sdk-native-linux-x64-musl` (or `-arm64-musl`).'
    );
  }
  if (platform === 'linux') {
    return (
      'Check that your glibc meets the documented minimum, or reinstall to fetch ' +
      'the prebuilt binary: `npm i @vibecook/spaghetti-sdk-native`.'
    );
  }
  return 'Reinstall to fetch the prebuilt binary: `npm i @vibecook/spaghetti-sdk-native`.';
}

/**
 * Which C library this Node was built against.
 *
 * `glibcVersionRuntime` is present in the process report on glibc and absent
 * on musl — the detection Node's own ecosystem uses, and it needs no child
 * process or filesystem probe.
 */
export function detectLibc(): 'glibc' | 'musl' | null {
  if (process.platform !== 'linux') return null;
  try {
    const header = (process.report?.getReport() as { header?: Record<string, unknown> } | undefined)?.header;
    return header && 'glibcVersionRuntime' in header ? 'glibc' : 'musl';
  } catch {
    return null;
  }
}

let cached: NativeAddon | null | undefined;
let loadFailure: EngineUnavailableError | null = null;

/**
 * Load the native addon, returning null if unavailable.
 *
 * Result is memoized — a missing addon won't be retried on subsequent calls.
 * When it fails, the reason is kept and available from
 * {@link nativeLoadFailure} rather than discarded.
 */
export function loadNativeAddon(): NativeAddon | null {
  if (cached !== undefined) return cached;

  try {
    const require = createRequire(import.meta.url);
    cached = require('@vibecook/spaghetti-sdk-native') as NativeAddon;
    loadFailure = null;
  } catch (err) {
    cached = null;
    loadFailure = new EngineUnavailableError({
      platform: process.platform,
      arch: process.arch,
      libc: detectLibc(),
      expectedVersion: expectedAddonVersion(),
      cause: err,
    });
  }

  return cached;
}

/**
 * Open the persistent Rust observation/query engine.
 *
 * This is the low-level persistent lifecycle surface, not yet a replacement
 * for {@link createSpaghettiService}. Use `openClaudeObservationShadow()` for
 * the isolated staged-observation mode while query parity is built.
 */
export function openSpaghettiEngine(options: SpaghettiEngineOpenOptions): Promise<SpaghettiEngine> {
  const addon = loadNativeAddon();
  if (!addon) {
    const failure = nativeLoadFailure();
    if (failure) throw failure;
    throw new Error('Persistent SpaghettiEngine requires the native addon, but it could not be loaded.');
  }
  return addon.openSpaghettiEngine(options);
}

/**
 * Version of the addon this SDK expects.
 *
 * The two are released in lockstep, so the SDK's own version is the answer.
 * Read at call time from the package manifest rather than baked in, because a
 * baked constant drifts silently the moment a release bumps one and not the
 * other — and this value exists to help diagnose exactly that kind of skew.
 */
function expectedAddonVersion(): string {
  try {
    const require = createRequire(import.meta.url);
    return (require('../package.json') as { version?: string }).version ?? 'unknown';
  } catch {
    return 'unknown';
  }
}

/**
 * Why the native addon is unavailable, or `null` when it loaded.
 *
 * Call after {@link loadNativeAddon}. Surfacing this is what turns a silent
 * fallback into an actionable one — the engine still works, so this is a
 * diagnostic rather than a thrown error.
 */
export function nativeLoadFailure(): EngineUnavailableError | null {
  loadNativeAddon();
  return loadFailure;
}

/**
 * Whether the native ingest path is enabled.
 *
 * Resolves via the shared `resolveEngine()` helper — honours (in order)
 * `SPAG_ENGINE=ts|rs`, legacy `SPAG_NATIVE_INGEST=0|1`, the persisted
 * engine setting in `~/.spaghetti/config.json`, and the default (`rs`).
 *
 * If the addon itself is missing or fails to load, the SDK falls back
 * to the TS path regardless of this setting. This helper only gates
 * the *preference*; actual resolution is
 * `isNativeIngestEnabled() && loadNativeAddon() !== null`.
 */
export function isNativeIngestEnabled(): boolean {
  return resolveEngine() === 'rs';
}

/** Effective ingest engine after native-addon fallback — see {@link resolveActiveEngine}. */
export interface ActiveEngineInfo {
  /**
   * The engine actually used at runtime. `'rs'` only when the `rs`
   * preference is set AND the native addon loads on this platform;
   * otherwise `'ts'` (either the preference was `ts`, or it was `rs`
   * but the addon is missing and the SDK fell back).
   */
  engine: IngestEngine;
  /** The configured preference alone (env → legacy env → config → default `rs`). */
  preference: IngestEngine;
  /** Whether the native addon (`@vibecook/spaghetti-sdk-native`) loaded. */
  nativeAvailable: boolean;
  /** Loaded native addon semver, or `null` when unavailable. */
  nativeVersion: string | null;
}

/**
 * Resolve the ingest engine that a service will *actually* run — the
 * single source of truth for the native-fallback rule mirrored in
 * `LifecycleOwner.initialize()` (`engine === 'rs' ? loadNativeAddon() : null`,
 * then native-or-TS). Consumers that need to *display* the active engine
 * (CLI badge, `spag engine`, doctor) should call this rather than
 * `resolveEngine()`, which only reports the preference and so reads `rs`
 * even when the addon is missing and the run silently falls back to `ts`.
 *
 * Pure + cheap: `loadNativeAddon()` is memoized, so this is safe to call
 * from a hot render path.
 */
export function resolveActiveEngine(): ActiveEngineInfo {
  const preference = resolveEngine();
  const native = loadNativeAddon();
  const nativeAvailable = native !== null;
  const engine: IngestEngine = preference === 'rs' && nativeAvailable ? 'rs' : 'ts';
  return {
    engine,
    preference,
    nativeAvailable,
    nativeVersion: native?.nativeVersion() ?? null,
  };
}
