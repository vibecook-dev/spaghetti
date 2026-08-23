/**
 * Native addon loader — `@vibecook/spaghetti-sdk-native`.
 *
 * The Rust RFC 011 observation/query engine ships as a separate native addon.
 * The production SDK requires it; a missing or incompatible binary is an
 * actionable startup error and never selects a second ingest authority.
 */

import { createRequire } from 'node:module';

import type { IngestEngine } from './settings.js';

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
  /** Internal cold-start hint used by the production observation host. */
  bootstrapQueryStructures?: boolean;
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
  state: 'bootstrapping' | 'running' | 'stopping' | 'stopped';
  databasePath: string;
  acceptingQueries: boolean;
  catalogQueryReady: boolean;
  searchAvailable: boolean;
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
  /** Oldest durable change still resumable without taking a new snapshot. */
  changeLogOldestCursor?: SpaghettiEngineChangeCursor;
  changeLogPrunedThroughSeq: number;
  changeLogRetainedChanges: number;
  changeLogRetainedPayloadBytes: number;
  writerDataVersion: number;
  journalMode: string;
  queryOnly: boolean;
  readOnly: boolean;
}

/** Exact durable position in the ordered projection change log. */
export interface SpaghettiEngineChangeCursor {
  commitSeq: number;
  ordinal: number;
}

export interface SpaghettiEngineChangeReplayOptions {
  /** Return changes strictly after this cursor. Omit to start at retained history. */
  after?: SpaghettiEngineChangeCursor;
  /** Empty or omitted means all stable change topics. */
  topics?: string[];
  /** Defaults to 100 and must be between 1 and 1,000. */
  limit?: number;
}

/** Lossless projection-level change; binary fields stay explicitly encoded. */
export interface SpaghettiEngineDurableChange {
  cursor: SpaghettiEngineChangeCursor;
  topic: string;
  schemaVersion: number;
  entityKeyBase64Url: string;
  operation: string;
  payloadBase64: string;
}

export interface SpaghettiEngineChangeReplay {
  contractVersion: number;
  /** Watermark read in the same SQLite snapshot as this page. */
  atCommitSeq: number;
  oldestAvailable?: SpaghettiEngineChangeCursor;
  changes: SpaghettiEngineDurableChange[];
  /** Cursor of the last returned change, including on a final non-empty page. */
  nextCursor?: SpaghettiEngineChangeCursor;
  hasMore: boolean;
  payloadBytes: number;
  payloadByteLimit: number;
}

export interface SpaghettiEngineCommitWaitOptions {
  /** Resolve after a commit newer than this sequence is published. */
  afterCommitSeq: number;
  /** Maximum wait in milliseconds. Defaults to 30 seconds. */
  timeoutMs?: number;
}

export interface SpaghettiEngineCommitWaitResult {
  /** Latest commit observed by the engine when the wait resolved. */
  observedCommitSeq: number;
  reason: 'commit' | 'timeout';
  waitedMs: number;
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

export type SpaghettiEngineSearchBranchKind = 'all' | 'root' | 'delegated' | 'unknown';

export interface SpaghettiEngineSearchPageOptions extends SpaghettiEngineHistoryPageOptions {
  /** Search text is treated as one literal FTS phrase, not as raw FTS syntax. */
  text: string;
  projectId?: string;
  sessionId?: string;
  adapterIds?: string[];
  roles?: string[];
  nativeKinds?: string[];
  branchKind?: SpaghettiEngineSearchBranchKind;
}

export interface SpaghettiEngineSearchHit {
  messageId: string;
  /** Absent while the referenced canonical session endpoint is unresolved. */
  projectId?: string;
  sessionId: string;
  runId: string;
  parentRunId?: string;
  branchKind: Exclude<SpaghettiEngineSearchBranchKind, 'all'>;
  adapterId: string;
  sourceInstanceId: number;
  nativeProjectKey?: string;
  nativeSessionId?: string;
  nativeRunId?: string;
  nativeChildId?: string;
  nativeTaskId?: string;
  delegationStatus?: string;
  nativeMessageId?: string;
  nativeKind: string;
  role: string;
  model?: string;
  sourceTime?: string;
  sourceTimeQuality?: SpaghettiEngineTimestampQuality;
  /** Plain text with excerpts separated by ` … `; the engine adds no markup. */
  snippet: string;
  /** SQLite FTS5 BM25 rank. Lower values sort first. */
  score: number;
  decisiveFactId: string;
  observedAtUnixMs: number;
  sourceObjectId: number;
  sourceGeneration: number;
  lastCommitSeq: number;
}

export interface SpaghettiEngineSearchPage {
  contractVersion: number;
  atCommitSeq: number;
  querySyntax: 'literal_phrase_v1';
  scoreDirection: 'lower_is_better';
  totalIsExact: true;
  total: number;
  items: SpaghettiEngineSearchHit[];
  /** UTF-8 bytes in returned snippets. */
  payloadBytes: number;
  payloadByteLimit: number;
  nextCursor?: string;
}

export type SpaghettiEngineTimelineContentKind =
  | 'text'
  | 'thinking'
  | 'tool_call'
  | 'tool_result'
  | 'image'
  | 'document'
  | 'native';

export interface SpaghettiEngineTimelinePageOptions extends SpaghettiEngineHistoryPageOptions {
  projectId: string;
  sessionId: string;
  roles?: string[];
  nativeKinds?: string[];
  /** Content-kind and tool-name includes are ORed within one solo filter. */
  includeContentKinds?: SpaghettiEngineTimelineContentKind[];
  includeToolNames?: string[];
  /** A message is excluded if any block matches either exclusion dimension. */
  excludeContentKinds?: SpaghettiEngineTimelineContentKind[];
  excludeToolNames?: string[];
  /** Blank text disables search; other text is one literal FTS phrase. */
  search?: string;
  branchKind?: SpaghettiEngineSearchBranchKind;
}

export interface SpaghettiEngineTimelineFacets {
  /** Unfiltered canonical message envelopes in the verified session. */
  totalMessages: number;
  /** Message-envelope counts. */
  roles: SpaghettiEngineNamedCount[];
  /** Message-envelope counts. */
  nativeKinds: SpaghettiEngineNamedCount[];
  /** Canonical content-block counts. */
  contentKinds: SpaghettiEngineNamedCount[];
  /** Canonical tool-call block counts. */
  toolNames: SpaghettiEngineNamedCount[];
  /** Message-envelope counts. */
  branchKinds: SpaghettiEngineNamedCount[];
}

export interface SpaghettiEngineTimelineMessage {
  messageId: string;
  projectId: string;
  sessionId: string;
  runId: string;
  parentRunId?: string;
  branchKind: Exclude<SpaghettiEngineSearchBranchKind, 'all'>;
  /** Exact parent message from the decisive native spawn correlation. */
  branchAnchorMessageId?: string;
  adapterId: string;
  sourceInstanceId: number;
  nativeProjectKey: string;
  nativeSessionId: string;
  nativeRunId?: string;
  nativeChildId?: string;
  nativeTaskId?: string;
  delegationKind?: string;
  delegationStrength?: string;
  delegationStatus?: string;
  branchToolName?: string;
  branchLabel?: string;
  requestedAgentType?: string;
  nativeMessageId?: string;
  nativeKind: string;
  role: string;
  /** Ordered canonical common blocks; raw/native payload is a detail concern. */
  content: unknown;
  contentKinds: SpaghettiEngineTimelineContentKind[];
  toolNames: string[];
  sourceTime?: string;
  sourceTimeQuality?: SpaghettiEngineTimestampQuality;
  parentNativeMessageId?: string;
  model?: string;
  decisiveFactId: string;
  observedAtUnixMs: number;
  sourceObjectId: number;
  sourceGeneration: number;
  lastCommitSeq: number;
}

export interface SpaghettiEngineTimelinePage {
  contractVersion: number;
  atCommitSeq: number;
  projectId: string;
  sessionId: string;
  order: 'newest_first';
  searchSyntax: 'literal_phrase_v1';
  totalIsExact: true;
  /** Filtered messages before cursor pagination. */
  total: number;
  /** Always describes the unfiltered verified session. */
  facets: SpaghettiEngineTimelineFacets;
  items: SpaghettiEngineTimelineMessage[];
  /** UTF-8 bytes in returned canonical content JSON. */
  payloadBytes: number;
  payloadByteLimit: number;
  nextCursor?: string;
}

export interface SpaghettiEngineDelegationPageOptions extends SpaghettiEngineHistoryPageOptions {
  projectId: string;
  sessionId: string;
  /** Include only delegations named by this canonical workflow. */
  workflowId?: string;
  /** Exclude delegations that are current members of any workflow. */
  standaloneOnly?: boolean;
}

export interface SpaghettiEngineDelegation {
  runId: string;
  parentRunId?: string;
  projectId: string;
  sessionId: string;
  adapterId: string;
  sourceInstanceId: number;
  nativeRunId?: string;
  nativeChildId?: string;
  nativeTaskId?: string;
  agentType?: string;
  description?: string;
  nativeName?: string;
  spawnDepth?: number;
  label?: string;
  prompt?: string;
  cwd?: string;
  worktreePath?: string;
  relationKind: string;
  relationStrength: string;
  relationStatus: string;
  metadataStatus?: string;
  spawnStatus?: string;
  branchToolName?: string;
  requestedAgentType?: string;
  branchAnchorMessageId?: string;
  childPresent: boolean;
  parentPresent: boolean;
  metadataRunPresent?: boolean;
  observedRunState?: SpaghettiEngineRunState;
  messageCount: number;
  workflowMemberCount: number;
  sourceTime?: string;
  sourceTimeQuality?: SpaghettiEngineTimestampQuality;
  decisiveRelationFactId?: string;
  decisiveSpawnFactId?: string;
  decisiveMetadataFactId?: string;
  assertionCount: number;
  competingRelationCount: number;
  observedAtUnixMs: number;
  sourceObjectId: number;
  sourceGeneration: number;
  lastCommitSeq: number;
}

export interface SpaghettiEngineDelegationPage {
  contractVersion: number;
  atCommitSeq: number;
  projectId: string;
  sessionId: string;
  workflowId?: string;
  standaloneOnly: boolean;
  items: SpaghettiEngineDelegation[];
  nextCursor?: string;
}

export interface SpaghettiEngineWorkflowPageOptions extends SpaghettiEngineHistoryPageOptions {
  projectId: string;
  sessionId: string;
}

export interface SpaghettiEngineWorkflowSummary {
  workflowId: string;
  projectId: string;
  sessionId: string;
  adapterId: string;
  sourceInstanceId: number;
  nativeWorkflowId: string;
  nativeTaskId?: string;
  name?: string;
  nativeStatus?: string;
  /** Container state only; never inherited by member child runs. */
  workflowStatus?: 'pending' | 'running' | 'succeeded' | 'failed' | 'cancelled' | 'other';
  startedAt?: string;
  startedAtQuality?: SpaghettiEngineTimestampQuality;
  finishedAt?: string;
  finishedAtQuality?: SpaghettiEngineTimestampQuality;
  durationMs?: number;
  agentCount?: number;
  totalTokens?: number;
  totalToolCalls?: number;
  snapshotStatus: 'present' | 'missing';
  resolutionStatus: 'resolved' | 'incomplete' | 'conflicting';
  decisiveSnapshotFactId?: string;
  /** Snapshot fact, or one deterministic member fact for journal-only rows. */
  provenanceFactId: string;
  snapshotAssertionCount: number;
  competingSnapshotCount: number;
  observedMemberCount: number;
  startedMemberCount: number;
  resultMemberCount: number;
  unresolvedMemberCount: number;
  conflictingMemberCount: number;
  membershipCountStatus: 'unobserved' | 'snapshot_missing' | 'matched' | 'different';
  joinConflict: boolean;
  observedAtUnixMs: number;
  sourceObjectId: number;
  sourceGeneration: number;
  lastCommitSeq: number;
}

export interface SpaghettiEngineWorkflowPage {
  contractVersion: number;
  atCommitSeq: number;
  projectId: string;
  sessionId: string;
  items: SpaghettiEngineWorkflowSummary[];
  nextCursor?: string;
}

export interface SpaghettiEngineWorkflowDetails {
  contractVersion: number;
  atCommitSeq: number;
  workflow: SpaghettiEngineWorkflowSummary;
  defaultModel?: string;
  script?: string;
  scriptPath?: string;
  args?: string;
  summary?: string;
  error?: string;
  nativeSnapshot?: unknown;
  payloadBytes: number;
  payloadByteLimit: number;
}

export interface SpaghettiEngineWorkflowMemberPageOptions extends SpaghettiEngineHistoryPageOptions {
  workflowId: string;
}

export interface SpaghettiEngineWorkflowMember {
  memberId: string;
  workflowId: string;
  projectId: string;
  sessionId: string;
  childRunId: string;
  childRunPresent: boolean;
  adapterId: string;
  sourceInstanceId: number;
  nativeWorkflowId: string;
  nativeAgentId: string;
  nativeEventKey: string;
  nativeRunId?: string;
  agentType?: string;
  description?: string;
  nativeName?: string;
  worktreePath?: string;
  memberStatus: 'started' | 'result_observed' | 'orphan_result';
  /** Native result value; it is not child success/failure evidence. */
  result?: unknown;
  resolutionStatus: 'resolved' | 'conflicting';
  observedRunState?: SpaghettiEngineRunState;
  delegationStatus?: string;
  messageCount: number;
  decisiveStartedFactId?: string;
  decisiveResultFactId?: string;
  startedObservedAtUnixMs?: number;
  resultObservedAtUnixMs?: number;
  startedAssertionCount: number;
  competingStartedCount: number;
  resultAssertionCount: number;
  competingResultCount: number;
  eventKeyConflict: boolean;
  identityConflict: boolean;
  sourceObjectId: number;
  sourceGeneration: number;
  lastCommitSeq: number;
}

export interface SpaghettiEngineWorkflowMemberPage {
  contractVersion: number;
  atCommitSeq: number;
  workflowId: string;
  projectId: string;
  sessionId: string;
  items: SpaghettiEngineWorkflowMember[];
  payloadBytes: number;
  payloadByteLimit: number;
  nextCursor?: string;
}

export type SpaghettiEngineCapabilityPageOptions = SpaghettiEngineHistoryPageOptions;

export interface SpaghettiEngineMemoryDocumentPageOptions extends SpaghettiEngineHistoryPageOptions {
  projectId: string;
}

export interface SpaghettiEngineMemoryDocument {
  documentId: string;
  projectId: string;
  adapterId: string;
  sourceInstanceId: number;
  nativeProjectKey: string;
  nativeDocumentPath: string;
  title: string;
  content: string;
  sizeBytes: number;
  isIndex: boolean;
  resolutionStatus: string;
  decisiveFactId: string;
  assertionCount: number;
  competingDocumentCount: number;
  observedAtUnixMs: number;
  sourceObjectId: number;
  sourceGeneration: number;
  lastCommitSeq: number;
}

export interface SpaghettiEngineMemoryDocumentPage {
  contractVersion: number;
  atCommitSeq: number;
  projectId: string;
  items: SpaghettiEngineMemoryDocument[];
  /** UTF-8 bytes in returned document content. */
  payloadBytes: number;
  payloadByteLimit: number;
  nextCursor?: string;
}

export interface SpaghettiEngineTaskCollectionPageOptions extends SpaghettiEngineHistoryPageOptions {
  /** At most one scope identity may be supplied. Omit all three for global discovery. */
  sessionId?: string;
  runId?: string;
  teamId?: string;
}

export interface SpaghettiEngineTaskCollection {
  collectionId: string;
  projectId?: string;
  sessionId?: string;
  runId?: string;
  teamId?: string;
  adapterId: string;
  sourceInstanceId: number;
  nativeCollectionId: string;
  nativeOwnerId?: string;
  collectionKind: string;
  nativeCollectionKind: string;
  resolutionStatus: string;
  decisiveFactId: string;
  assertionCount: number;
  competingMetadataCount: number;
  completeSnapshotCount: number;
  itemDocumentCount: number;
  itemCount: number;
  observedAtUnixMs: number;
  sourceObjectId: number;
  sourceGeneration: number;
  lastCommitSeq: number;
}

export interface SpaghettiEngineTaskCollectionPage {
  contractVersion: number;
  atCommitSeq: number;
  sessionId?: string;
  runId?: string;
  teamId?: string;
  items: SpaghettiEngineTaskCollection[];
  nextCursor?: string;
}

export interface SpaghettiEngineTaskPageOptions extends SpaghettiEngineHistoryPageOptions {
  collectionId: string;
}

export interface SpaghettiEngineTask {
  taskId: string;
  collectionId: string;
  adapterId: string;
  sourceInstanceId: number;
  itemOrdinal: number;
  nativeTaskId?: string;
  subject: string;
  description?: string;
  activeForm?: string;
  nativeOwner?: string;
  taskStatus: string;
  nativeStatus: string;
  blocks: string[];
  blockedBy: string[];
  resolutionStatus: string;
  decisiveFactId: string;
  assertionCount: number;
  competingItemCount: number;
  observedAtUnixMs: number;
  sourceObjectId: number;
  sourceGeneration: number;
  lastCommitSeq: number;
}

export interface SpaghettiEngineTaskPage {
  contractVersion: number;
  atCommitSeq: number;
  collectionId: string;
  items: SpaghettiEngineTask[];
  payloadBytes: number;
  payloadByteLimit: number;
  nextCursor?: string;
}

export interface SpaghettiEnginePlan {
  planId: string;
  adapterId: string;
  sourceInstanceId: number;
  nativePlanId: string;
  title: string;
  content: string;
  sizeBytes: number;
  sourceTime?: string;
  sourceTimeQuality?: SpaghettiEngineTimestampQuality;
  resolutionStatus: string;
  decisiveFactId: string;
  assertionCount: number;
  competingPlanCount: number;
  observedAtUnixMs: number;
  sourceObjectId: number;
  sourceGeneration: number;
  lastCommitSeq: number;
}

export interface SpaghettiEnginePlanPage {
  contractVersion: number;
  atCommitSeq: number;
  items: SpaghettiEnginePlan[];
  payloadBytes: number;
  payloadByteLimit: number;
  nextCursor?: string;
}

export interface SpaghettiEngineToolResultPageOptions extends SpaghettiEngineHistoryPageOptions {
  projectId: string;
  sessionId: string;
}

export interface SpaghettiEngineToolResult {
  resultId: string;
  projectId: string;
  sessionId: string;
  adapterId: string;
  sourceInstanceId: number;
  nativeProjectKey: string;
  nativeSessionId: string;
  nativeToolUseId: string;
  nativeDocumentPath: string;
  content: string;
  sizeBytes: number;
  resolutionStatus: string;
  correlationStatus: string;
  toolCallMessageId?: string;
  toolResultMessageId?: string;
  decisiveFactId: string;
  assertionCount: number;
  competingResultCount: number;
  toolCallMatchCount: number;
  toolResultMatchCount: number;
  joinConflict: boolean;
  observedAtUnixMs: number;
  sourceObjectId: number;
  sourceGeneration: number;
  lastCommitSeq: number;
}

export interface SpaghettiEngineToolResultPage {
  contractVersion: number;
  atCommitSeq: number;
  projectId: string;
  sessionId: string;
  items: SpaghettiEngineToolResult[];
  payloadBytes: number;
  payloadByteLimit: number;
  nextCursor?: string;
}

export interface SpaghettiEngineArtifactPageOptions extends SpaghettiEngineHistoryPageOptions {
  sessionId: string;
}

export interface SpaghettiEngineArtifact {
  artifactId: string;
  sessionId: string;
  projectId?: string;
  nativeArtifactId?: string;
  nativeFileHash?: string;
  version: number;
  trackingPath?: string;
  realParentDir?: string;
  backupTime?: string;
  backupTimeQuality?: SpaghettiEngineTimestampQuality;
  captureStatus: string;
  /** Exact arbitrary bytes, encoded only for the JS transport. */
  contentBase64?: string;
  sizeBytes?: number;
  contentDigestBase64url?: string;
  contentStatus: string;
  resolutionStatus: string;
  metadataFactId?: string;
  contentFactId?: string;
  metadataAdapterId?: string;
  metadataSourceInstanceId?: number;
  metadataObservedAtUnixMs?: number;
  metadataSourceObjectId?: number;
  metadataSourceGeneration?: number;
  contentAdapterId?: string;
  contentSourceInstanceId?: number;
  contentObservedAtUnixMs?: number;
  contentSourceObjectId?: number;
  contentSourceGeneration?: number;
  metadataAssertionCount: number;
  competingMetadataCount: number;
  contentAssertionCount: number;
  competingContentCount: number;
  joinConflict: boolean;
  lastCommitSeq: number;
}

export interface SpaghettiEngineArtifactPage {
  contractVersion: number;
  atCommitSeq: number;
  sessionId: string;
  items: SpaghettiEngineArtifact[];
  /** Base64 text bytes returned by this page. */
  payloadBytes: number;
  payloadByteLimit: number;
  nextCursor?: string;
}

export interface SpaghettiEngineSourceSummary {
  sourceId: string;
  sourceInstanceId: number;
  adapterId: string;
  displayName: string;
  adapterVersion: string;
  adapterContractVersion: number;
  sourceSchemaVersions: string[];
  capabilities: SpaghettiEngineSourceCapability[];
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

export interface SpaghettiEngineSourceCapability {
  id: string;
  supportLevel: 'native' | 'derived' | 'estimated' | 'unsupported' | string;
  granularity: string;
  availability: 'live' | 'eventually_live' | 'completion_only' | 'backfill_only' | string;
  notes?: string;
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

/** Fixed-bucket, owner-lifetime latency distribution from the native engine. */
export interface SpaghettiEngineLatencyStats {
  samples: number;
  totalMs: number;
  meanMs: number;
  maxMs: number;
  p50UpperMs: number;
  p95UpperMs: number;
  p99UpperMs: number;
}

export interface SpaghettiEngineNamedLatencyStats {
  name: string;
  latency: SpaghettiEngineLatencyStats;
}

export interface SpaghettiEngineCheckpointPerformanceStats {
  attempts: number;
  completed: number;
  blocked: number;
  failures: number;
  lastLogFrames: number;
  lastCheckpointedFrames: number;
  lastRemainingFrames: number;
  blockedByReaderMs: number;
  latency: SpaghettiEngineLatencyStats;
}

export interface SpaghettiEngineWriterPerformanceStats {
  uptimeMs: number;
  commitAttempts: number;
  committed: number;
  failed: number;
  factsCommitted: number;
  changesPublished: number;
  sqliteRowsChanged: number;
  queueDepth: number;
  queueHighWatermark: number;
  checkpoint: SpaghettiEngineCheckpointPerformanceStats;
  timings: SpaghettiEngineNamedLatencyStats[];
}

/** Bounded owner-lifetime samples; repeated comparison queries remain visible. */
export interface SpaghettiEngineRuntimeUsageCompatibilityTelemetryStats {
  samples: number;
  readySamples: number;
  notReadySamples: number;
  equalSamples: number;
  differentSamples: number;
  incomparableSamples: number;
  equalBuckets: number;
  legacyHigherBuckets: number;
  v2HigherBuckets: number;
  incomparableBuckets: number;
  sampledAbsoluteDeltaTokens: number;
  maxAbsoluteDeltaTokens: number;
  firstAtCommitSeq?: number;
  lastAtCommitSeq?: number;
}

export interface SpaghettiEngineQueryPerformanceStats {
  uptimeMs: number;
  requestsEnqueued: number;
  requestsCompleted: number;
  queueRejections: number;
  queueDepth: number;
  queueHighWatermark: number;
  oldestActiveMs: number;
  runtimeUsageCompatibility: SpaghettiEngineRuntimeUsageCompatibilityTelemetryStats;
  timings: SpaghettiEngineNamedLatencyStats[];
}

export interface SpaghettiEngineSourcePipelineStats {
  readAttempts: number;
  readFailures: number;
  readRetries: number;
  readContinuations: number;
  recordsRead: number;
  payloadBytesRead: number;
  decodeAttempts: number;
  decodeFailures: number;
  decodeRetries: number;
  recordsDecoded: number;
  factsEmitted: number;
  recordsQuarantined: number;
  timings: SpaghettiEngineNamedLatencyStats[];
}

export interface SpaghettiEngineSourceDimensionPerformanceStats {
  adapterId: string;
  streamId: string;
  driverKind: string;
  pipeline: SpaghettiEngineSourcePipelineStats;
}

export interface SpaghettiEngineSourcePerformanceStats {
  uptimeMs: number;
  dimensionCapacity: number;
  dimensionOverflowAssignments: number;
  totals: SpaghettiEngineSourcePipelineStats;
  dimensions: SpaghettiEngineSourceDimensionPerformanceStats[];
}

export interface SpaghettiEngineStoragePerformanceStats {
  databaseFileBytes: number;
  walFileBytes: number;
  sharedMemoryFileBytes: number;
}

export interface SpaghettiEnginePerformanceStats {
  writer: SpaghettiEngineWriterPerformanceStats;
  queries: SpaghettiEngineQueryPerformanceStats;
  source: SpaghettiEngineSourcePerformanceStats;
  storage: SpaghettiEngineStoragePerformanceStats;
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
  /** Bounded owner-lifetime telemetry sampled by the sole native owner. */
  performance?: SpaghettiEnginePerformanceStats;
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

/** Query one response-revision usage page from the RFC 012C shadow projection. */
export interface SpaghettiEngineRuntimeUsageV2Options extends SpaghettiEngineHistoryPageOptions {
  /** Opaque project identity returned by {@link SpaghettiEngine.listHistoryProjects}. */
  projectId: string;
  /** Opaque session identity returned by {@link SpaghettiEngine.listHistorySessions}. */
  sessionId: string;
  /** Optional RFC 012A actor entity reference returned by a prior page. */
  actorRunRef?: string;
  /** Optional affiliation dimension. It must be paired with affiliationTargetRef. */
  affiliationDimension?: 'team' | 'workflow';
  /** RFC 012A team/workflow target entity reference paired with affiliationDimension. */
  affiliationTargetRef?: string;
}

export interface SpaghettiEngineRuntimeUsageV2ExternalEntityRef {
  externalEntityReferenceVersion: number;
  entityKey: string;
}

export interface SpaghettiEngineRuntimeUsageV2SemanticRevisionRef {
  semanticReferenceContractVersion: number;
  factRevisionId: string;
}

export interface SpaghettiEngineRuntimeUsageV2ValueProvenance {
  nativeField: string;
  normalizationContractVersion: number;
}

export type SpaghettiEngineRuntimeUsageV2Quality = 'exact' | 'native_claimed' | 'derived' | 'estimated' | 'unknown';
export type SpaghettiEngineRuntimeUsageV2Completeness = 'complete' | 'partial' | 'unknown';
export type SpaghettiEngineRuntimeUsageV2UnknownReason =
  | 'missing'
  | 'unsupported'
  | 'withheld'
  | 'not_yet_observed'
  | 'ambiguous'
  | 'malformed';
export type SpaghettiEngineRuntimeUsageV2Authority = 'native_response' | 'adapter_derived';

export interface SpaghettiEngineRuntimeUsageV2TokenValue {
  value?: number;
  quality: SpaghettiEngineRuntimeUsageV2Quality;
  authority: SpaghettiEngineRuntimeUsageV2Authority;
  completeness: SpaghettiEngineRuntimeUsageV2Completeness;
  unknownReason?: SpaghettiEngineRuntimeUsageV2UnknownReason;
  effectiveAt?: number;
  provenance: SpaghettiEngineRuntimeUsageV2ValueProvenance;
}

export interface SpaghettiEngineRuntimeUsageV2TextValue {
  value?: string;
  quality: SpaghettiEngineRuntimeUsageV2Quality;
  authority: SpaghettiEngineRuntimeUsageV2Authority;
  completeness: SpaghettiEngineRuntimeUsageV2Completeness;
  unknownReason?: SpaghettiEngineRuntimeUsageV2UnknownReason;
  effectiveAt?: number;
  provenance: SpaghettiEngineRuntimeUsageV2ValueProvenance;
}

export interface SpaghettiEngineRuntimeUsageV2Response {
  usageKey: string;
  semanticRevisionRef: SpaghettiEngineRuntimeUsageV2SemanticRevisionRef;
  sourceRecordRef: string;
  sessionRef: SpaghettiEngineRuntimeUsageV2ExternalEntityRef;
  actorRunRef: SpaghettiEngineRuntimeUsageV2ExternalEntityRef;
  responseKeyBase64: string;
  responseIdentity: 'native_message_id' | 'source_record_fallback';
  nativeMessageId?: string;
  requestId?: string;
  inputTokens: SpaghettiEngineRuntimeUsageV2TokenValue;
  outputTokens: SpaghettiEngineRuntimeUsageV2TokenValue;
  cacheCreationInputTokens: SpaghettiEngineRuntimeUsageV2TokenValue;
  cacheReadInputTokens: SpaghettiEngineRuntimeUsageV2TokenValue;
  model?: SpaghettiEngineRuntimeUsageV2TextValue;
  effort?: SpaghettiEngineRuntimeUsageV2TextValue;
  sourceTime?: string;
  sourceTimeQuality?: SpaghettiEngineTimestampQuality;
  observedAtUnixMs: number;
  sourceGeneration: number;
  lastCommitSeq: number;
}

export interface SpaghettiEngineRuntimeUsageV2Affiliation {
  affiliationRef: SpaghettiEngineRuntimeUsageV2ExternalEntityRef;
  semanticRevisionRef: SpaghettiEngineRuntimeUsageV2SemanticRevisionRef;
  dimension: 'team' | 'workflow';
  targetRef: SpaghettiEngineRuntimeUsageV2ExternalEntityRef;
  memberRef?: SpaghettiEngineRuntimeUsageV2ExternalEntityRef;
  nativeTargetId?: string;
  nativeMemberId?: string;
  state: 'present' | 'removed' | 'unknown';
  effectiveAt?: string;
  effectiveAtQuality?: SpaghettiEngineTimestampQuality;
  observedAtUnixMs: number;
  sourceGeneration: number;
  lastCommitSeq: number;
}

export interface SpaghettiEngineRuntimeUsageV2ActorContext {
  actorRunRef: SpaghettiEngineRuntimeUsageV2ExternalEntityRef;
  semanticRevisionRef: SpaghettiEngineRuntimeUsageV2SemanticRevisionRef;
  sessionRef: SpaghettiEngineRuntimeUsageV2ExternalEntityRef;
  role: 'root' | 'child';
  parentActorRunRef?: SpaghettiEngineRuntimeUsageV2ExternalEntityRef;
  nativeSessionId?: string;
  nativeActorId?: string;
  nativeActorType?: string;
  /** Current revisions, including explicit removed and unknown relations. */
  affiliations: SpaghettiEngineRuntimeUsageV2Affiliation[];
  observedAtUnixMs: number;
  sourceGeneration: number;
  lastCommitSeq: number;
}

export interface SpaghettiEngineRuntimeUsageV2BucketAggregate {
  knownTokens: number;
  knownResponseCount: number;
  exactResponseCount: number;
  nonExactResponseCount: number;
  unknownResponseCount: number;
  completeness: SpaghettiEngineRuntimeUsageV2Completeness;
}

export interface SpaghettiEngineRuntimeUsageV2Aggregate {
  responseCount: number;
  actorCount: number;
  inputTokens: SpaghettiEngineRuntimeUsageV2BucketAggregate;
  outputTokens: SpaghettiEngineRuntimeUsageV2BucketAggregate;
  cacheCreationInputTokens: SpaghettiEngineRuntimeUsageV2BucketAggregate;
  cacheReadInputTokens: SpaghettiEngineRuntimeUsageV2BucketAggregate;
}

export interface SpaghettiEngineRuntimeUsageV2ProjectionReadiness {
  projectionId: 'runtime.usage-v2';
  desiredVersion: number;
  completedVersion?: number;
  /** `untracked` is explicit legacy/direct-fixture state, never an alias for ready. */
  state: 'ready' | 'stale_safe' | 'pending' | 'unavailable' | 'untracked';
  lastCommitSeq?: number;
  updatedAtUnixMs?: number;
  detail?: string;
}

export interface SpaghettiEngineRuntimeUsageQuerySelectionValue {
  queryId: 'legacy.usage' | 'runtime.usage-v2';
  contractVersion: number;
}

export interface SpaghettiEngineRuntimeUsageQuerySelection {
  contractVersion: number;
  queryPackId: 'runtime.usage';
  /** Opaque common source identity once matching usage-v2 coverage exists. */
  sourceInstanceRef?: string;
  /** False means the immutable compatibility default legacy.usage@1 at epoch zero. */
  materialized: boolean;
  selected: SpaghettiEngineRuntimeUsageQuerySelectionValue;
  rollback: SpaghettiEngineRuntimeUsageQuerySelectionValue;
  selectionEpoch: number;
  lastCommitSeq?: number;
  updatedAtUnixMs?: number;
}

export type SpaghettiEngineRuntimeUsageTotalsQueryId = 'selected' | 'legacy.usage' | 'runtime.usage-v2';

export interface SpaghettiEngineRuntimeUsageTotalsOptions {
  /** One to 128 canonical scopes. Project-wide and session scopes may not overlap. */
  scopes: SpaghettiEngineUsageScopeOptions[];
  /** Defaults to `selected`; explicit values support compatibility and shadow comparison. */
  requestedQueryId?: SpaghettiEngineRuntimeUsageTotalsQueryId;
}

export interface SpaghettiEngineRuntimeUsageTotalsSelectionScope {
  /** Query-local opaque vector identity; not an RFC 012A sourceInstanceRef. */
  selectionScopeRef: string;
  adapterId: string;
  sessionCount: number;
  querySelection: SpaghettiEngineRuntimeUsageQuerySelection;
  projectionReadiness: SpaghettiEngineRuntimeUsageV2ProjectionReadiness;
  coverageStatus: 'complete' | 'partial' | 'unavailable' | 'not_materialized' | 'inconsistent';
  /** True only when the current v2 promotion guard is satisfied. */
  v2Eligible: boolean;
}

export interface SpaghettiEngineRuntimeUsageLegacyTotals {
  aggregate: SpaghettiEngineUsageAggregate;
  coverage: SpaghettiEngineUsageCoverage[];
  firstSourceTime?: string;
  lastSourceTime?: string;
  firstObservedAtUnixMs?: number;
  lastObservedAtUnixMs?: number;
  lastCommitSeq?: number;
}

export interface SpaghettiEngineRuntimeUsageTotals {
  contractVersion: number;
  atCommitSeq: number;
  requestedQueryId: SpaghettiEngineRuntimeUsageTotalsQueryId;
  status: 'resolved' | 'mixed_selection' | 'not_ready' | 'unsupported_selection';
  resolvedQuery?: SpaghettiEngineRuntimeUsageQuerySelectionValue;
  scopes: SpaghettiEngineUsageScopeOptions[];
  selectionVector: SpaghettiEngineRuntimeUsageTotalsSelectionScope[];
  /** Present exactly when the resolved query is legacy.usage. */
  legacy?: SpaghettiEngineRuntimeUsageLegacyTotals;
  /** Present exactly when the resolved query is runtime.usage-v2. */
  usageV2?: SpaghettiEngineRuntimeUsageV2Aggregate;
}

export interface SpaghettiEngineRuntimeUsageCompatibilityOptions {
  /** One to 128 canonical scopes. Project-wide and session scopes may not overlap. */
  scopes: SpaghettiEngineUsageScopeOptions[];
}

export interface SpaghettiEngineRuntimeUsageCompatibilityBucket {
  legacyExactTokens: number;
  legacyEstimatedTokens: number;
  legacyCombinedTokens: number;
  v2KnownTokens: number;
  v2UnknownResponseCount: number;
  v2Completeness: SpaghettiEngineRuntimeUsageV2Completeness;
  relation: 'equal' | 'legacy_higher' | 'v2_higher' | 'incomparable';
  /** Absent only when the v2 bucket is incomplete and therefore incomparable. */
  absoluteDeltaTokens?: number;
}

export interface SpaghettiEngineRuntimeUsageCompatibility {
  contractVersion: number;
  atCommitSeq: number;
  /** Opaque, request-order-independent identity for this scope set and commit. */
  comparisonRef: string;
  status: 'ready' | 'not_ready';
  comparisonStatus: 'equal' | 'different' | 'incomparable' | 'not_ready';
  scopes: SpaghettiEngineUsageScopeOptions[];
  selectionVector: SpaghettiEngineRuntimeUsageTotalsSelectionScope[];
  legacy: SpaghettiEngineUsageAggregate;
  usageV2?: SpaghettiEngineRuntimeUsageV2Aggregate;
  inputTokens?: SpaghettiEngineRuntimeUsageCompatibilityBucket;
  outputTokens?: SpaghettiEngineRuntimeUsageCompatibilityBucket;
  cacheCreationInputTokens?: SpaghettiEngineRuntimeUsageCompatibilityBucket;
  cacheReadInputTokens?: SpaghettiEngineRuntimeUsageCompatibilityBucket;
}

/**
 * Compare-and-set authorization for the source instance resolved through one
 * session. Copy every expected field from one `getRuntimeUsageV2()` page.
 */
export interface SpaghettiEngineRuntimeUsageQuerySelectionOptions {
  projectId: string;
  sessionId: string;
  targetQueryId: 'legacy.usage' | 'runtime.usage-v2';
  expectedMaterialized: boolean;
  expectedSelectedQueryId: 'legacy.usage' | 'runtime.usage-v2';
  expectedSelectedContractVersion: number;
  expectedSelectionEpoch: number;
  /** Bounded durable audit reason for this selection change. */
  reason: string;
}

export interface SpaghettiEngineRuntimeUsageQuerySelectionResult {
  contractVersion: number;
  atCommitSeq: number;
  projectId: string;
  sessionId: string;
  selection: SpaghettiEngineRuntimeUsageQuerySelection;
}

export interface SpaghettiEngineRuntimeUsageV2Page {
  contractVersion: number;
  atCommitSeq: number;
  /** `selected` is explicit; `not_materialized` means this session has no v2 projection yet. */
  projectionStatus: 'shadow' | 'selected' | 'not_materialized';
  /** Writer-owned readiness at the same atCommitSeq as rows and aggregates. */
  projectionReadiness: SpaghettiEngineRuntimeUsageV2ProjectionReadiness;
  /** Source-scoped migration selection from the same durable snapshot. */
  querySelection: SpaghettiEngineRuntimeUsageQuerySelection;
  projectId: string;
  sessionId: string;
  sessionRef?: SpaghettiEngineRuntimeUsageV2ExternalEntityRef;
  actorRunRef?: string;
  affiliationDimension?: 'team' | 'workflow';
  affiliationTargetRef?: string;
  aggregate: SpaghettiEngineRuntimeUsageV2Aggregate;
  items: SpaghettiEngineRuntimeUsageV2Response[];
  /** Actor contexts referenced by this page, not an unbounded session actor list. */
  actors: SpaghettiEngineRuntimeUsageV2ActorContext[];
  nextCursor?: string;
}

/** Page one normalized RFC 012A fact-family coverage set. */
export interface SpaghettiEngineFactFamilyCoverageOptions extends SpaghettiEngineHistoryPageOptions {
  /** Opaque project identity returned by {@link SpaghettiEngine.listHistoryProjects}. */
  projectId: string;
  /** Opaque session identity returned by {@link SpaghettiEngine.listHistorySessions}. */
  sessionId: string;
  /** Durable projection or coverage owner identifier. */
  ownerId: string;
  /** Common fact-family identifier, for example `runtime.usage-v2`. */
  family: string;
  /** Positive fact-family contract version. */
  familyVersion: number;
}

export type SpaghettiEngineFactFamilyCoverageCompleteness = 'complete' | 'partial' | 'unavailable';
export type SpaghettiEngineFactFamilyCoverageItemKind = 'point' | 'absence' | 'error';
export type SpaghettiEngineFactFamilyCoveragePositionKind =
  | 'append_cursor'
  | 'document_revision'
  | 'snapshot_revision'
  | 'database_watermark'
  | 'key_range_token';
export type SpaghettiEngineFactFamilyCoveragePointStatus =
  | 'complete_through'
  | 'exact_snapshot'
  | 'partial'
  | 'unavailable';
export type SpaghettiEngineFactFamilyCoverageAbsenceKind = 'absent' | 'deleted';

export interface SpaghettiEngineFactFamilyCoverageSetSummary {
  coverageSetContractVersion: number;
  coverageContractVersion: number;
  adapterId: string;
  /** Versioned opaque common reference; never a native source path. */
  sourceInstanceRef: string;
  supportReleaseId: string;
  /** Versioned opaque common reference to the source/scope declaration. */
  declarationRef: string;
  /** Versioned opaque common reference to the frozen membership revision. */
  membershipRevisionRef: string;
  completeness: SpaghettiEngineFactFamilyCoverageCompleteness;
  /** Versioned opaque digest of this complete normalized coverage set. */
  contentDigestRef: string;
  lastCommitSeq: number;
  updatedAtUnixMs: number;
}

export interface SpaghettiEngineFactFamilyCoverageItem {
  kind: SpaghettiEngineFactFamilyCoverageItemKind;
  /** Versioned opaque common stream reference, when the evidence is stream-scoped. */
  streamRef?: string;
  /** Versioned opaque common object reference, when the evidence is object-scoped. */
  objectRef?: string;
  generation?: number;
  positionKind?: SpaghettiEngineFactFamilyCoveragePositionKind;
  /** Versioned opaque common position reference. */
  positionRef?: string;
  monotonicOrder?: number;
  status?: SpaghettiEngineFactFamilyCoveragePointStatus;
  unavailableReason?: string;
  /** Versioned opaque common source-record reference. */
  sourceRecordRef?: string;
  /** Versioned opaque common semantic-revision reference. */
  semanticRevisionRef?: string;
  observedAtUnixMs?: number;
  absenceKind?: SpaghettiEngineFactFamilyCoverageAbsenceKind;
  errorCode?: string;
}

export interface SpaghettiEngineFactFamilyCoveragePage {
  contractVersion: number;
  /** Fixed durable watermark shared by metadata, items, and this page cursor. */
  atCommitSeq: number;
  status: 'materialized' | 'not_materialized';
  projectId: string;
  sessionId: string;
  ownerId: string;
  family: string;
  familyVersion: number;
  coverage?: SpaghettiEngineFactFamilyCoverageSetSummary;
  /** Deterministically ordered union of point, absence, and error evidence. */
  items: SpaghettiEngineFactFamilyCoverageItem[];
  /** Scope-bound cursor that expires when the durable commit watermark changes. */
  nextCursor?: string;
}

/**
 * Explicit replacement command authorized by one coverage snapshot. All
 * `expected*` fields must be copied from the same materialized coverage set.
 */
export interface SpaghettiEngineFactFamilyReplayOptions {
  /** Open adapter identifier registered by the native composition root. */
  adapterId: string;
  /** Configured native data roots understood by the selected adapter. */
  roots: string[];
  projectId: string;
  sessionId: string;
  ownerId: string;
  family: string;
  familyVersion: number;
  expectedSourceInstanceRef: string;
  expectedContentDigestRef: string;
  expectedCoverageLastCommitSeq: number;
  /** Bounded durable audit reason for this replacement command. */
  reason: string;
}

export interface SpaghettiEngineFactFamilyReplayResult {
  contractVersion: number;
  projectId: string;
  sessionId: string;
  ownerId: string;
  family: string;
  familyVersion: number;
  authorizedSourceInstanceRef: string;
  authorizedContentDigestRef: string;
  authorizedCoverageLastCommitSeq: number;
  outcome: SpaghettiEngineReconcileResult;
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
  /** Configured native data roots understood by the selected adapter. */
  roots: string[];
  /** Durable ingest reason. Defaults to `manual_reconcile`. */
  reason?: string;
}

export interface SpaghettiEngineObservationOptions {
  /** Configured native data roots understood by the selected adapter. */
  roots: string[];
  /** Durable reason prefix. Defaults to `native_watch`. */
  reason?: string;
}

export interface SpaghettiEngineAdapterReconcileOptions extends SpaghettiEngineReconcileOptions {
  /** Open adapter identifier, such as `claude-code`, `codex`, or `grok`. */
  adapterId: string;
}

export interface SpaghettiEngineAdapterObservationOptions extends SpaghettiEngineObservationOptions {
  /** Open adapter identifier, such as `claude-code`, `codex`, or `grok`. */
  adapterId: string;
}

/** One source in a configured startup unit. Alias, not a narrowing. */
export type SpaghettiEngineConfiguredObservationSourceOptions = SpaghettiEngineAdapterObservationOptions;

export interface SpaghettiEngineConfiguredObservationOptions {
  /** Complete source set planned as one startup unit before history scans begin. */
  sources: SpaghettiEngineConfiguredObservationSourceOptions[];
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
  incompleteTailRetries: number;
  dependencyAccessAttempts: number;
  dependencyAccessDenials: number;
  dependencyAccessAbandoned: number;
  dependencyObjectsAccessed: number;
  dependencyBytesRead: number;
  dependencyRowsRead: number;
  dependencyMaxDepth: number;
  dependencyTraceEntriesDropped: number;
  commits: number;
  lastCommitSeq?: number;
}

/**
 * Native owner for one store-free RFC 012D session attachment.
 *
 * Batches cross as one JSON string holding an array of `ObserverEvent`. The
 * typed element shape is generated from Rust by `ts-rs`; parse the string and
 * narrow on `type`.
 */
export interface NativeSessionObserver {
  /** Take up to `max` pending events now, and hint a reconciliation pass. */
  poll(max?: number): string;
  /** Wait up to `timeoutMs` for at least one event, then take up to `max`. */
  waitForEvents(timeoutMs: number, max?: number): Promise<string>;
  /** Current epoch, queue depth, and whether continuity still holds. */
  status(): NativeSessionObserverStatus;
  /** Idempotent; waits for every owned watch, read, and decode to stop. */
  close(): Promise<void>;
}

/** Health surface for one attachment. */
export interface NativeSessionObserverStatus {
  scopeEpoch: number;
  offeredThroughSequence: number;
  queuedSemantic: number;
  queuedControl: number;
  retainedBytes: number;
  /** False between continuity loss and the completion of its replacement. */
  epochValid: boolean;
  closed: boolean;
}

/** Async handle backed by one persistent Rust engine lifecycle. */
export interface SpaghettiEngine {
  readonly status: SpaghettiEngineStatus;
  health(signal?: AbortSignal): Promise<SpaghettiEngineHealth>;
  overview(signal?: AbortSignal): Promise<SpaghettiEngineOverview>;
  /** RFC 012B strict JSON transport; values are policy-WITHHELD. */
  getCatalogReadinessJson(requestJson: string, signal?: AbortSignal): Promise<string>;
  /** RFC 012B strict JSON transport; values are policy-WITHHELD. */
  listLibraryProjectsJson(requestJson: string, signal?: AbortSignal): Promise<string>;
  /** RFC 012B strict JSON transport; values are policy-WITHHELD. */
  listLibrarySessionsJson(requestJson: string, signal?: AbortSignal): Promise<string>;
  /** RFC 012B strict JSON transport; values are policy-WITHHELD. */
  resolveCatalogEntityJson(requestJson: string, signal?: AbortSignal): Promise<string>;
  /** RFC 012B engine-owned selected-session hydration command transport. */
  requestCatalogHydrationJson(requestJson: string, signal?: AbortSignal): Promise<string>;
  replayChanges(
    options?: SpaghettiEngineChangeReplayOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineChangeReplay>;
  waitForCommit(
    options: SpaghettiEngineCommitWaitOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineCommitWaitResult>;
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
  search(options: SpaghettiEngineSearchPageOptions, signal?: AbortSignal): Promise<SpaghettiEngineSearchPage>;
  getTimeline(options: SpaghettiEngineTimelinePageOptions, signal?: AbortSignal): Promise<SpaghettiEngineTimelinePage>;
  listDelegations(
    options: SpaghettiEngineDelegationPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineDelegationPage>;
  listWorkflows(
    options: SpaghettiEngineWorkflowPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineWorkflowPage>;
  getWorkflow(workflowId: string, signal?: AbortSignal): Promise<SpaghettiEngineWorkflowDetails>;
  listWorkflowMembers(
    options: SpaghettiEngineWorkflowMemberPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineWorkflowMemberPage>;
  listMemoryDocuments(
    options: SpaghettiEngineMemoryDocumentPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineMemoryDocumentPage>;
  listTaskCollections(
    options?: SpaghettiEngineTaskCollectionPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineTaskCollectionPage>;
  listTasks(options: SpaghettiEngineTaskPageOptions, signal?: AbortSignal): Promise<SpaghettiEngineTaskPage>;
  listPlans(options?: SpaghettiEngineCapabilityPageOptions, signal?: AbortSignal): Promise<SpaghettiEnginePlanPage>;
  listToolResults(
    options: SpaghettiEngineToolResultPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineToolResultPage>;
  listArtifacts(
    options: SpaghettiEngineArtifactPageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineArtifactPage>;
  listSources(options?: SpaghettiEngineHistoryPageOptions, signal?: AbortSignal): Promise<SpaghettiEngineSourcePage>;
  getStats(signal?: AbortSignal): Promise<SpaghettiEngineCanonicalStats>;
  getUsage(options: SpaghettiEngineUsageScopeOptions, signal?: AbortSignal): Promise<SpaghettiEngineUsageTotals>;
  getUsageActivity(
    options: SpaghettiEngineUsageActivityOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineUsageActivity>;
  getRuntimeUsageV2(
    options: SpaghettiEngineRuntimeUsageV2Options,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineRuntimeUsageV2Page>;
  getRuntimeUsageTotals(
    options: SpaghettiEngineRuntimeUsageTotalsOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineRuntimeUsageTotals>;
  getRuntimeUsageCompatibility(
    options: SpaghettiEngineRuntimeUsageCompatibilityOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineRuntimeUsageCompatibility>;
  selectRuntimeUsageQuery(
    options: SpaghettiEngineRuntimeUsageQuerySelectionOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineRuntimeUsageQuerySelectionResult>;
  getFactFamilyCoverage(
    options: SpaghettiEngineFactFamilyCoverageOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineFactFamilyCoveragePage>;
  replayFactFamily(
    options: SpaghettiEngineFactFamilyReplayOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineFactFamilyReplayResult>;
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
  /** Reconcile any registered adapter through the common source engine. */
  reconcileAdapter(
    options: SpaghettiEngineAdapterReconcileOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineReconcileResult>;
  /** Start one registered adapter's native observation supervisor. */
  startObservation(
    options: SpaghettiEngineAdapterObservationOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineStatus>;
  /** Start all configured sources behind one global catalog and watcher barrier. */
  startConfiguredObservation(
    options: SpaghettiEngineConfiguredObservationOptions,
    signal?: AbortSignal,
  ): Promise<SpaghettiEngineStatus>;
  /** Force one running adapter supervisor through reconciliation. */
  refreshObservation(adapterId: string, signal?: AbortSignal): Promise<SpaghettiEngineStatus>;
  /** Stop one adapter supervisor without disposing the engine. */
  stopObservation(adapterId: string, signal?: AbortSignal): Promise<SpaghettiEngineStatus>;
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
  /** Finish complete-only FTS after catalog-first queries are already admitted. */
  completeQueryBootstrap(): Promise<SpaghettiEngineStatus>;
  cancelPendingQueries(): number;
  dispose(): Promise<SpaghettiEngineStatus>;
}

export interface NativeAddon {
  /** Returns the semver of the loaded native addon. */
  nativeVersion(): string;
  /** Open the persistent RFC 011 engine shell off the JavaScript thread. */
  openSpaghettiEngine(options: SpaghettiEngineOpenOptions): Promise<SpaghettiEngine>;
  /** Attach a store-free observer to one native session tree. */
  observeSession(request: string | Record<string, unknown>): NativeSessionObserver;
}

/**
 * Why the native addon did not load, in terms a user can act on.
 *
 * The loader reports whether the package is missing, unsupported, or cannot
 * load on the current libc. RFC 011 has no production TypeScript fallback.
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
      `Native observation addon unavailable for ${target}. ` +
        `RFC 011 requires the Rust engine; no TypeScript production fallback is enabled. ` +
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
 * This is the low-level persistent lifecycle surface. Applications normally
 * use `createObservationService()`, which owns this handle and exposes the
 * canonical client-backed product API.
 */
export function openSpaghettiEngine(options: SpaghettiEngineOpenOptions): Promise<SpaghettiEngine> {
  const addon = loadNativeAddon();
  if (!addon) {
    const failure = nativeLoadFailure();
    if (failure) throw failure;
    throw new Error('Persistent SpaghettiEngine requires the native addon, but it could not be loaded.');
  }
  return addon.openSpaghettiEngine(options).then(withAbortSignalPreflight);
}

/**
 * Attach the native store-free observer for one session tree.
 *
 * Validation is synchronous: an unusable agent root, a locator outside the
 * adapter's declared source roots, or a session id that disagrees with the
 * locator throws here rather than surfacing as an error event later.
 */
export function openNativeSessionObserver(request: string | Record<string, unknown>): NativeSessionObserver {
  const addon = loadNativeAddon();
  if (!addon) {
    const failure = nativeLoadFailure();
    if (failure) throw failure;
    throw new Error('Session observation requires the native addon, but it could not be loaded.');
  }
  if (typeof addon.observeSession !== 'function') {
    throw new Error('The loaded native addon does not implement the RFC 012D session observer.');
  }
  return addon.observeSession(request);
}

/**
 * NAPI-RS observes abort events after a task is created, but an already
 * aborted signal has no future event to deliver. Keep that transport detail
 * out of every query method by rejecting it once at the SDK boundary.
 */
function withAbortSignalPreflight(engine: SpaghettiEngine): SpaghettiEngine {
  return new Proxy(engine, {
    get(target, property) {
      const value: unknown = Reflect.get(target, property, target);
      if (typeof value !== 'function') return value;
      return (...args: unknown[]) => {
        const aborted = args.find(isAbortedSignal);
        if (aborted) return Promise.reject(aborted.reason ?? abortError());
        return Reflect.apply(value, target, args);
      };
    },
  });
}

function isAbortedSignal(value: unknown): value is AbortSignal {
  if (typeof value !== 'object' || value === null) return false;
  const candidate = value as Partial<AbortSignal>;
  return candidate.aborted === true && typeof candidate.addEventListener === 'function';
}

function abortError(): Error {
  const error = new Error('The operation was aborted.');
  error.name = 'AbortError';
  return error;
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
 * Call after {@link loadNativeAddon}. Production hosts throw this failure at
 * startup; diagnostics can inspect it without opening an engine.
 */
export function nativeLoadFailure(): EngineUnavailableError | null {
  loadNativeAddon();
  return loadFailure;
}

/**
 * Compatibility diagnostic retained for callers from the pre-RFC 011 SDK.
 * Production ingest is always native; availability is reported separately.
 */
export function isNativeIngestEnabled(): boolean {
  return true;
}

/** Rust-only production engine status. */
export interface ActiveEngineInfo {
  /** The sole production engine. */
  engine: IngestEngine;
  /** Retained for response compatibility; always `rs`. */
  preference: IngestEngine;
  /** Whether the native addon (`@vibecook/spaghetti-sdk-native`) loaded. */
  nativeAvailable: boolean;
  /** Loaded native addon semver, or `null` when unavailable. */
  nativeVersion: string | null;
}

/**
 * Resolve production engine availability without inventing a fallback.
 */
export function resolveActiveEngine(): ActiveEngineInfo {
  const native = loadNativeAddon();
  const nativeAvailable = native !== null;
  return {
    engine: 'rs',
    preference: 'rs',
    nativeAvailable,
    nativeVersion: native?.nativeVersion() ?? null,
  };
}
