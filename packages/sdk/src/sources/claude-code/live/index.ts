/**
 * Claude Code live disk pipeline (Plane 2).
 *
 * Product-owned: multi-scope watcher for `~/.claude` (projects, todos, tasks,
 * file-history, plans, settings). Generic agents implement {@link LiveWatch}
 * under their own package (CodexLiveWatch, GrokLiveWatch).
 *
 * Shared infrastructure stays in `live/`:
 * - LiveWatch interface, watcher, change-events, SpaghettiLive (api.live)
 * - ParsedRow write-batch types
 */

export {
  createClaudeCodeLiveUpdates,
  coalescePath,
  TASK_COALESCE_FILENAME,
  type ClaudeCodeLiveUpdates,
  type ClaudeCodeLiveUpdatesOptions,
  type ClaudeCodeLiveUpdatesDeps,
  // Deprecated aliases
  createClaudeCodeLiveUpdates as createLiveUpdates,
  type ClaudeCodeLiveUpdates as LiveUpdates,
  type ClaudeCodeLiveUpdatesOptions as LiveUpdatesOptions,
  type ClaudeCodeLiveUpdatesDeps as LiveUpdatesDeps,
} from './live-updates.js';

export {
  createClaudeCodeLiveDiskIngest,
  type ClaudeCodeLiveDiskIngest,
  type ClaudeCodeLiveDiskIngestOptions,
  // Deprecated aliases
  createClaudeCodeLiveDiskIngest as createLiveDiskIngest,
  type ClaudeCodeLiveDiskIngest as LiveDiskIngest,
  type ClaudeCodeLiveDiskIngestOptions as LiveDiskIngestOptions,
} from './disk-ingest.js';

export {
  createIncrementalParser,
  type IncrementalParser,
  type IncrementalParseResult,
  type ParseFileDeltaParams,
  type ParsedRow,
  type ParsedRowCategory,
} from './incremental-parser.js';

export { createCheckpointStore, type Checkpoint, type CheckpointStore } from './checkpoints.js';
export { createCoalescingQueue, type CoalescingQueue, type QueuedReason } from './coalescing-queue.js';
export { createScopeAttacher, topicToScopes, type ScopeAttacher, type WatchScopeKey } from './scope-attacher.js';
export { createSettingsHandler, type SettingsHandler } from './settings-handler.js';
export { watchSessionTranscript } from './session-tail.js';
export type { SessionTranscriptTail, SessionTranscriptEvent, WatchSessionTranscriptOptions } from './session-tail.js';
