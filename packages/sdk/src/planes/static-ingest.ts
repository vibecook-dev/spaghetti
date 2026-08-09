/**
 * StaticIngest — Plane 1 façade (cold / warm / full rebuild).
 *
 * Implementation remains on LifecycleOwner (AgentDataServiceImpl).
 * This module names the boundary and maps AgentSource + DurableStore
 * into the options LifecycleOwner already understands.
 *
 * See `docs/TWO-PLANE-INGEST-ARCHITECTURE.md`.
 */

import type { FileService } from '../io/file-service.js';
import type { ClaudeCodeParser } from '../sources/claude-code/parser/claude-code-parser.js';
import type { AgentDataServiceOptions } from '../data/agent-data-service.js';
import type { AgentSource } from '../sources/types.js';
import type { DurableStore } from '../store/durable-store.js';
import type { IngestEngine } from '../settings.js';

/**
 * Dependencies StaticIngest needs from the factory. LifecycleOwner
 * still owns the actual cold/warm/native algorithms.
 */
export interface StaticIngestDeps {
  source: AgentSource;
  store: DurableStore;
  fileService: FileService;
  parser: ClaudeCodeParser;
  engine?: IngestEngine;
  dbPath?: string;
}

/**
 * Map plane deps into LifecycleOwner constructor options.
 */
export function toLifecycleOptions(
  deps: Pick<StaticIngestDeps, 'source' | 'engine' | 'dbPath'> & { safeBulk?: boolean },
): AgentDataServiceOptions {
  const options: AgentDataServiceOptions = {
    rootDir: deps.source.rootDir,
  };
  if (deps.dbPath !== undefined) options.dbPath = deps.dbPath;
  if (deps.engine !== undefined) options.engine = deps.engine;
  if (deps.safeBulk !== undefined) options.safeBulk = deps.safeBulk;
  return options;
}
