/**
 * Repository-only binding for the retired RFC 003 bulk/live writer.
 *
 * Default and published native builds do not expose these functions. The
 * explicit `legacy-oracle` Cargo feature exists solely for differential tests.
 * Production code must use `openSpaghettiEngine()` and the observation host.
 */

import {
  loadNativeAddon,
  type NativeAddon,
  type NativeIngestOptions,
  type NativeIngestStats,
  type NativeLiveBatchResult,
  type NativeLiveRow,
  type NativeProgressCallback,
} from './native.js';

export interface LegacyNativeAddon extends NativeAddon {
  ingest(options: NativeIngestOptions, onProgress?: NativeProgressCallback): Promise<NativeIngestStats>;
  liveIngestBatch(dbPath: string, rows: NativeLiveRow[], sourceId?: string): NativeLiveBatchResult;
}

/** Return the old writer only when the addon was built with its test feature. */
export function loadLegacyNativeAddon(): LegacyNativeAddon | null {
  const addon = loadNativeAddon() as Partial<LegacyNativeAddon> | null;
  return addon && typeof addon.ingest === 'function' && typeof addon.liveIngestBatch === 'function'
    ? (addon as LegacyNativeAddon)
    : null;
}
