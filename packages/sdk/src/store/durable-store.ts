/**
 * DurableStore — Plane 1+2 shared SQLite surface.
 *
 * Owns the single shared connection used by query (read), ingest (write),
 * and AgentDataStore (read cache + change emit). Construct via
 * {@link createDurableStore} so factories never open a second connection.
 *
 * See `docs/TWO-PLANE-INGEST-ARCHITECTURE.md`.
 */

import type { ErrorSink } from '../io/error-sink.js';
import type { SqliteService } from '../io/sqlite-service.js';
import { createAgentDataStore, type AgentDataStore } from '../data/agent-data-store.js';
import { createIngestService, type IngestService } from '../data/ingest-service.js';
import { createQueryService, type QueryService } from '../data/query-service.js';
import type { LegacyNativeAddon as NativeAddon } from '../legacy-native.js';
import type { IngestEngine } from '../settings.js';

export interface DurableStore {
  /** Read queries over the index. */
  readonly query: QueryService;
  /** Write sink (ProjectParseSink + live writeBatch). */
  readonly ingest: IngestService;
  /** High-level reads, config/analytics cache, subscriber emit. */
  readonly data: AgentDataStore;
  /** Shared SQLite connection owner. */
  readonly sqlite: SqliteService;
}

export interface CreateDurableStoreOptions {
  /** Already-created SqliteService instance (shared for the process lifetime of this service). */
  sqlite: SqliteService;
  errorSink?: ErrorSink;
  /** Ingest engine pin for native live-batch routing. */
  engine?: IngestEngine;
  /** Loaded native addon, or null when unavailable / TS engine. */
  native?: NativeAddon | null;
}

/**
 * Wire QueryService + IngestService + AgentDataStore on one SqliteService.
 */
export function createDurableStore(options: CreateDurableStoreOptions): DurableStore {
  const { sqlite, errorSink, engine, native } = options;
  const sqliteFactory = () => sqlite;
  const query = createQueryService(sqliteFactory);
  const ingest = createIngestService(sqliteFactory, {
    engine: engine ?? 'ts',
    native: native ?? null,
  });
  const data = createAgentDataStore(query, { errorSink });
  return { query, ingest, data, sqlite };
}
