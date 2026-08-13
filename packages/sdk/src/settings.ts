/**
 * Spaghetti SDK settings — persisted user preferences.
 *
 * Backs `~/.spaghetti/config.json`. Pre-RFC 011 releases persisted an engine
 * choice here. That field remains readable for migration diagnostics, but the
 * production observation engine is now always Rust.
 */

import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';

export type IngestEngine = 'ts' | 'rs';

export interface SpaghettiSettings {
  /** @deprecated Retained only to read pre-RFC 011 configuration. */
  engine?: IngestEngine;
  /** Unknown keys from future versions are preserved. */
  [key: string]: unknown;
}

export function settingsPath(): string {
  return path.join(os.homedir(), '.spaghetti', 'config.json');
}

export function readSettings(): SpaghettiSettings {
  const p = settingsPath();
  if (!existsSync(p)) return {};
  try {
    return JSON.parse(readFileSync(p, 'utf-8')) as SpaghettiSettings;
  } catch {
    return {};
  }
}

export function writeSettings(settings: SpaghettiSettings): void {
  const p = settingsPath();
  const dir = path.dirname(p);
  if (!existsSync(dir)) mkdirSync(dir, { recursive: true });
  writeFileSync(p, JSON.stringify(settings, null, 2) + '\n', 'utf-8');
}

/**
 * Resolve the production engine. Legacy environment/config preferences are
 * intentionally ignored: accepting `ts` here would recreate a second
 * production ingestion authority.
 */
export function resolveEngine(): IngestEngine {
  return 'rs';
}

/**
 * Default DB path for a given engine.
 *
 * The `ts` spelling is retained for repository differential tooling. Shipped
 * SDK/CLI/playground code always requests `rs`.
 */
export function defaultDbPathForEngine(engine: IngestEngine): string {
  return path.join(os.homedir(), '.spaghetti', 'cache', `spaghetti-${engine}.db`);
}
