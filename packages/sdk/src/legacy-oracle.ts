/**
 * Test-only TypeScript ingestion/query oracle.
 *
 * This module is intentionally absent from package exports and production
 * entry graphs. It exists only for migration differentials and legacy unit
 * fixtures while the accepted Rust results are recorded. Do not import it
 * from applications.
 */

export * from './index.js';
export * from './legacy-native.js';
export * from './io/index.js';
export * from './parser/index.js';
export { createSearchIndexer, type SearchIndexer, type SearchIndexEntry } from './data/search-indexer.js';
export { createSegmentStore, type SegmentStore } from './data/segment-store.js';
export {
  type AgentDataService,
  type ClaudeCodeAgentDataService,
  type AgentDataServiceOptions,
} from './data/agent-data-service.js';
export { SCHEMA_VERSION, initializeSchema } from './data/schema.js';
export { createQueryService, type QueryService } from './data/query-service.js';
export { createIngestService, type IngestService } from './data/ingest-service.js';
export {
  type WorkerPool,
  type WorkerPoolOptions,
  type WorkerToMainMessage,
  type MainToWorkerMessage,
  createWorkerPool,
  isWorkerThreadsAvailable,
} from './workers/index.js';
export { createSpaghettiService, type SpaghettiServiceOptions } from './create.js';
export { createSpaghettiAppService } from './app-service.js';
export * from './sources/index.js';
export { createDurableStore, type DurableStore, type CreateDurableStoreOptions } from './store/durable-store.js';
export {
  toLifecycleOptions,
  type StaticIngestDeps,
  createLiveDiskIngest,
  type LiveDiskIngest,
  type LiveDiskIngestOptions,
} from './planes/index.js';
export { reportIngestErrors, summarizeIngestErrors } from './data/ingest-error-report.js';
export * from './observation-shadow.js';
export type { SpaghettiLive } from './live/spaghetti-live.js';
export { watchSessionTranscript } from './live/session-tail.js';
export type {
  SessionTranscriptTail,
  SessionTranscriptEvent,
  WatchSessionTranscriptOptions,
} from './live/session-tail.js';
