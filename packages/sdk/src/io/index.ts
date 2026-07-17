/**
 * I/O Module — Core I/O primitives
 */

export {
  type FileEvent,
  type FileChange,
  type FileStats,
  type DirectoryWatchOptions,
  type FileWatchOptions,
  type ScanOptions,
  type ReadOptions,
  type ReadBytesOptions,
  type JsonlReadResult,
  type IncrementalReadResult,
  type FileService,
  FileServiceImpl,
  getFileService,
  createFileService,
} from './file-service.js';

export {
  type JsonlLineCallback,
  type StreamingJsonlOptions,
  type StreamingJsonlResult,
  readJsonlStreaming,
} from './streaming-jsonl-reader.js';

export {
  type HookEventWatcherOptions,
  type HookEventWatcher,
  createHookEventWatcher,
  getDefaultHookEventsPath,
} from './hook-event-watcher.js';

export {
  type SqliteConfig,
  type RunResult,
  type PreparedStatement,
  type TableInfo,
  type SqliteService,
  SqliteServiceImpl,
  createSqliteService,
} from './sqlite-service.js';

export {
  type CacheHealthResult,
  wipeSqliteCacheFiles,
  ensureSqliteCacheHealthy,
  isSqliteCorruptError,
} from './sqlite-health.js';

export { type ChannelRegistryOptions, type ChannelRegistry, createChannelRegistry } from './channel-registry.js';

export { type ChannelClientOptions, type ChannelClient, createChannelClient } from './channel-client.js';

export { type ChannelManagerOptions, type ChannelManager, createChannelManager } from './channel-manager.js';

export {
  type ErrorSink,
  type ErrorContext,
  createConsoleErrorSink,
  createNoopErrorSink,
  errorSinkFromCallback,
} from './error-sink.js';
