/**
 * @vibecook/spaghetti-sdk — Local-first library for browsing multi-agent
 * history (Claude Code, Codex, Grok, …) from on-disk agent data.
 */

// Types
export * from './types/index.js';

// I/O services
export * from './io/index.js';

// Parsers
export * from './parser/index.js';

// Data layer (segments, search, summaries)
export * from './data/segment-types.js';
export * from './data/summary-types.js';
export * from './data/timeline-query.js';
export { createSearchIndexer, type SearchIndexer, type SearchIndexEntry } from './data/search-indexer.js';
export { createSegmentStore, type SegmentStore } from './data/segment-store.js';
// Public data-service interface + options. The concrete impl
// (`AgentDataServiceImpl` / `LifecycleOwner`) is intentionally NOT
// re-exported — consumers should construct services through
// `createSpaghettiService(...)`. The shim file at
// `./data/agent-data-service.js` keeps existing internal imports
// working but the public barrel only exposes the interface.
export {
  type AgentDataService,
  type ClaudeCodeAgentDataService,
  type AgentDataServiceOptions,
} from './data/agent-data-service.js';

// Schema
export { SCHEMA_VERSION, initializeSchema } from './data/schema.js';

// Query & Ingest services.
// NOTE: these factories (and the io/* ones above) are composition
// internals kept exported for the ingest-diff parity harness and manual
// wiring. Constructing them directly bypasses the shared-SQLite-connection
// invariant — application consumers should go through
// `createSpaghettiService(...)`. Slated for removal from the public
// barrel in the CLI-redesign export tightening.
export { createQueryService, type QueryService } from './data/query-service.js';
export { createIngestService, type IngestService } from './data/ingest-service.js';

// Workers
export {
  type WorkerPool,
  type WorkerPoolOptions,
  type WorkerToMainMessage,
  type MainToWorkerMessage,
  createWorkerPool,
  isWorkerThreadsAvailable,
} from './workers/index.js';

// API
export * from './api.js';

// Factory
export { createSpaghettiService, type SpaghettiServiceOptions } from './create.js';
export { createSpaghettiAppService } from './app-service.js';

// Agent sources (two-plane architecture — Claude Code, Codex, Grok)
export type {
  AgentSource,
  AgentSourceId,
  AgentSourcePaths,
  ClaudeCodePaths,
  ExtractedMessage,
  MessageExtractor,
} from './sources/types.js';
export {
  createClaudeCodeSource,
  defaultClaudeDir,
  defaultSpaghettiStateDir,
  listActiveSessionsFromDir,
  isProcessAlive,
  type ListActiveSessionsOptions,
  type ClaudeCodeSourceOptions,
  createCodexSource,
  defaultCodexDir,
  parseCodexTokenCount,
  type CodexSourceOptions,
  type CodexTokenUsage,
  createGrokSource,
  defaultGrokDir,
  type GrokSourceOptions,
  sourceReportsPerMessageTokens,
  sourceDisplayName,
  sourceDisplayRoot,
  adaptMessageForDisplay,
  adaptMessagesForDisplay,
} from './sources/index.js';

// Durable store + plane façades (composition helpers; prefer createSpaghettiService)
export { createDurableStore, type DurableStore, type CreateDurableStoreOptions } from './store/durable-store.js';
export {
  toLifecycleOptions,
  type StaticIngestDeps,
  createLiveDiskIngest,
  type LiveDiskIngest,
  type LiveDiskIngestOptions,
} from './planes/index.js';

// Native addon bridge (RFC 003)
export {
  loadNativeAddon,
  nativeLoadFailure,
  EngineUnavailableError,
  detectLibc,
  isNativeIngestEnabled,
  resolveActiveEngine,
  type NativeAddon,
  type ActiveEngineInfo,
  // Ingest error reporting (RFC 008 Phase 2). Exported so a consumer can read
  // `path` and `severity` off a failure directly, rather than casting its way
  // into the stats object.
  type NativeIngestStats,
  type NativeIngestError,
  type NativeIngestErrorReport,
  type NativeIngestErrorSeverity,
} from './native.js';

export { reportIngestErrors, summarizeIngestErrors } from './data/ingest-error-report.js';

// Settings (engine selection, etc.)
export {
  type IngestEngine,
  type SpaghettiSettings,
  readSettings,
  writeSettings,
  resolveEngine,
  defaultDbPathForEngine,
  settingsPath,
} from './settings.js';

// Live updates (RFC 005 Phase 3) — public surface + event types (Plane 2).
export type { SpaghettiLive } from './live/spaghetti-live.js';
export type { Change, ChangeType, ChangeTopic, SubscribeOptions, Dispose } from './live/change-events.js';
export {
  isSessionMessageAdded,
  isSessionCreated,
  isSessionRewritten,
  isSubagentUpdated,
  isToolResultAdded,
  isFileHistoryAdded,
  isTodoUpdated,
  isTaskUpdated,
  isPlanUpserted,
  isSettingsChanged,
} from './live/change-events.js';

// Scoped single-session transcript tail — for consumers that know the session
// a priori (agent runtimes holding --session-id) and don't need the full plane.
export { watchSessionTranscript } from './live/session-tail.js';
export type {
  SessionTranscriptTail,
  SessionTranscriptEvent,
  WatchSessionTranscriptOptions,
} from './live/session-tail.js';
