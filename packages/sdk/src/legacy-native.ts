/**
 * Repository-only binding for the retired RFC 003 bulk/live writer.
 *
 * Default and published native builds do not expose these functions. The
 * explicit `legacy-oracle` Cargo feature exists solely for differential tests,
 * and `index.d.ts` is generated without it — so unlike every other type that
 * crosses this boundary, the shapes below have no generated counterpart to
 * import and are declared here, next to their only consumer.
 */

import { loadNativeAddon, type NativeAddon } from './native.js';

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

export type LegacyNativeAddon = NativeAddon & {
  ingest(options: NativeIngestOptions, onProgress?: NativeProgressCallback): Promise<NativeIngestStats>;
  liveIngestBatch(dbPath: string, rows: NativeLiveRow[], sourceId?: string): NativeLiveBatchResult;
};

/** Return the old writer only when the addon was built with its test feature. */
export function loadLegacyNativeAddon(): LegacyNativeAddon | null {
  const addon = loadNativeAddon() as Partial<LegacyNativeAddon> | null;
  return addon && typeof addon.ingest === 'function' && typeof addon.liveIngestBatch === 'function'
    ? (addon as LegacyNativeAddon)
    : null;
}
