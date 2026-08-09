/**
 * Native addon loader — `@vibecook/spaghetti-sdk-native`.
 *
 * The Rust ingest core (RFC 003) ships as a separate native addon. This
 * module loads it opportunistically: if the addon is missing or fails
 * to load (unsupported platform, broken install), the SDK falls back
 * to the pure-TypeScript ingest path.
 *
 * As of Phase 4 (cutover, 0.7.0) the native path is the **default** —
 * set `SPAG_NATIVE_INGEST=0` to force the TS path.
 */

import { createRequire } from 'node:module';

import { resolveEngine, type IngestEngine } from './settings.js';

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
 * Frozen in Phase 0, produced in Phase 2 — see {@link FrozenNativeIngestError}.
 */
export type NativeIngestErrorSeverity = 'record-skip' | 'project-fatal' | 'source';

/**
 * The agreed shape of a native ingest error (RFC 008 Phase 0, item 5).
 *
 * **Not yet produced.** Today's addon returns `{ slug, message }` — see
 * {@link NativeIngestStats}. This type exists so Phase 2 implements an approved
 * contract rather than inventing one mid-change, which is what the RFC means by
 * freezing the wire shape before touching parser behavior.
 *
 * The difference that matters is `slug` becoming optional. It is required
 * today, so a failure that happens *before* a project slug exists cannot be
 * expressed at all — which is precisely why those failures are currently
 * swallowed instead of reported (`claude/project_parser.rs` documents the
 * swallow). A required slug also invites inventing a fake one, which the RFC
 * explicitly forbids.
 *
 * `path` becomes mandatory in exchange: every surfaced error can name a file
 * even when it cannot name a project.
 */
export interface FrozenNativeIngestError {
  /** Absent for `source` severity — no project identity existed yet. */
  slug?: string;
  /** Always present. The file the error is about. */
  path: string;
  severity: NativeIngestErrorSeverity;
  message: string;
}

/**
 * The agreed error-reporting fields on ingest stats (RFC 008 Phase 0, item 5).
 *
 * **Not yet produced.** `errors` is capped for display while `errorCount` stays
 * uncapped, so a caller can say "12 of 4,000 failures" instead of silently
 * showing the first hundred as if they were all of them. `errorsTruncated`
 * makes that distinction checkable rather than inferred from a length.
 */
export interface FrozenNativeIngestErrorReport {
  /** First N errors, for display. */
  errors: FrozenNativeIngestError[];
  /** Uncapped total, however many were kept for display. */
  errorCount: number;
  /** True when `errors.length < errorCount`. */
  errorsTruncated: boolean;
}

export interface NativeIngestStats {
  durationMs: number;
  projectsProcessed: number;
  sessionsProcessed: number;
  messagesWritten: number;
  subagentsWritten: number;
  /**
   * Current shape, produced by the shipped addon. RFC 008 Phase 2 replaces this
   * with {@link FrozenNativeIngestErrorReport}; until then `slug` is required
   * and pre-identity failures have nowhere to go.
   */
  errors: Array<{ slug: string; message: string }>;
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

export interface NativeAddon {
  /** Returns the semver of the loaded native addon. */
  nativeVersion(): string;
  /**
   * Run a full ingest and resolve to the stats. Optionally receives a
   * progress callback invoked from the libuv worker thread (safe from
   * any thread — caller need not synchronise).
   */
  ingest(opts: NativeIngestOptions, onProgress?: NativeProgressCallback): Promise<NativeIngestStats>;
  /**
   * Write a batch of live-update rows to the SQLite DB at `dbPath`.
   * Wraps `writer::write_batch_with_tx` (RFC 005 Phase 4 C4.1) so the
   * live-ingest path shares the cold-start writer's transaction +
   * UPSERT semantics.
   *
   * Synchronous on the Rust side (the whole batch is one BEGIN
   * IMMEDIATE / COMMIT) — the TS caller wraps it in a Promise at the
   * call site if it wants to interop with the rest of the async
   * live-updates pipeline.
   *
   * Throws on any single-row failure (bad JSON, unknown category,
   * SQLite error); the whole batch is rolled back and the TS side
   * falls back to its own writer.
   */
  /**
   * Write a live batch. Optional `sourceId` defaults to `claude-code` on
   * the Rust side when omitted.
   */
  liveIngestBatch(dbPath: string, rows: NativeLiveRow[], sourceId?: string): NativeLiveBatchResult;
}

let cached: NativeAddon | null | undefined;

/**
 * Load the native addon, returning null if unavailable.
 *
 * Result is memoized — a missing addon won't be retried on subsequent calls.
 */
export function loadNativeAddon(): NativeAddon | null {
  if (cached !== undefined) return cached;

  try {
    const require = createRequire(import.meta.url);
    cached = require('@vibecook/spaghetti-sdk-native') as NativeAddon;
  } catch {
    cached = null;
  }

  return cached;
}

/**
 * Whether the native ingest path is enabled.
 *
 * Resolves via the shared `resolveEngine()` helper — honours (in order)
 * `SPAG_ENGINE=ts|rs`, legacy `SPAG_NATIVE_INGEST=0|1`, the persisted
 * engine setting in `~/.spaghetti/config.json`, and the default (`rs`).
 *
 * If the addon itself is missing or fails to load, the SDK falls back
 * to the TS path regardless of this setting. This helper only gates
 * the *preference*; actual resolution is
 * `isNativeIngestEnabled() && loadNativeAddon() !== null`.
 */
export function isNativeIngestEnabled(): boolean {
  return resolveEngine() === 'rs';
}

/** Effective ingest engine after native-addon fallback — see {@link resolveActiveEngine}. */
export interface ActiveEngineInfo {
  /**
   * The engine actually used at runtime. `'rs'` only when the `rs`
   * preference is set AND the native addon loads on this platform;
   * otherwise `'ts'` (either the preference was `ts`, or it was `rs`
   * but the addon is missing and the SDK fell back).
   */
  engine: IngestEngine;
  /** The configured preference alone (env → legacy env → config → default `rs`). */
  preference: IngestEngine;
  /** Whether the native addon (`@vibecook/spaghetti-sdk-native`) loaded. */
  nativeAvailable: boolean;
  /** Loaded native addon semver, or `null` when unavailable. */
  nativeVersion: string | null;
}

/**
 * Resolve the ingest engine that a service will *actually* run — the
 * single source of truth for the native-fallback rule mirrored in
 * `LifecycleOwner.initialize()` (`engine === 'rs' ? loadNativeAddon() : null`,
 * then native-or-TS). Consumers that need to *display* the active engine
 * (CLI badge, `spag engine`, doctor) should call this rather than
 * `resolveEngine()`, which only reports the preference and so reads `rs`
 * even when the addon is missing and the run silently falls back to `ts`.
 *
 * Pure + cheap: `loadNativeAddon()` is memoized, so this is safe to call
 * from a hot render path.
 */
export function resolveActiveEngine(): ActiveEngineInfo {
  const preference = resolveEngine();
  const native = loadNativeAddon();
  const nativeAvailable = native !== null;
  const engine: IngestEngine = preference === 'rs' && nativeAvailable ? 'rs' : 'ts';
  return {
    engine,
    preference,
    nativeAvailable,
    nativeVersion: native?.nativeVersion() ?? null,
  };
}
