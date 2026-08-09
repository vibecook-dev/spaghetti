/**
 * Per-source ingest-contract marker (RFC 008 Phase 0, item 4).
 *
 * Records which version of the Rust bulk-ingest contract last completed for a
 * given source, so a warm run can tell "unchanged since a build that was
 * correct" from "unchanged since a build that predates a fix". Historical
 * builds can leave rows no fingerprint diff reveals — a parent-less sidecar
 * created after a rolled-back project, for instance — and those never surface
 * from an ordinary file change.
 *
 * Stored in `source_materializations` under a per-source projection key. A
 * global key would be wrong: repairing Claude Code must not re-ingest Codex.
 *
 * ── Who writes it, and why that is asymmetric ──────────────────────────────
 *
 * The marker describes *Rust ingest* completion — hence the key name — so only
 * the Rust orchestrator publishes it, success-last, after the writer joins
 * cleanly with no per-record errors. The TypeScript engine never publishes it.
 *
 * That asymmetry is intentional and safe in both directions:
 *
 * - TS-then-Rust: Rust finds no marker, reads that as "older", and does one
 *   full re-ingest. Extra work, correct result.
 * - Rust-then-TS: the TS engine has its own warm logic and ignores the marker
 *   entirely.
 *
 * What is *not* optional is invalidation. `clearSourceData` below drops the
 * marker inside the same transaction as the rows, in both engines — a TS clear
 * that left a Rust marker behind would tell the next Rust warm start that a
 * wiped source was fully materialised.
 *
 * `source_materializations` is deliberately absent from the ingest-diff table
 * inventory, because a cross-engine diff would flag this asymmetry as a
 * divergence when it is the design.
 */

import type { SqliteService } from '../io/sqlite-service.js';

/** Projection key. Per-source by construction — the table's PK is (source_id, projection). */
export const RUST_INGEST_CONTRACT = 'rust-ingest-contract';

/**
 * Bump when a Rust ingest fix leaves previously-written rows wrong in a way
 * fingerprints cannot detect. A bump forces one full clear-and-reingest for
 * each source whose stored version is older.
 */
export const RUST_INGEST_CONTRACT_VERSION = 1;

/**
 * True when this source completed under the current contract version.
 *
 * Read-only, and deliberately strict: an older *or* absent version both read as
 * "not current", so the safe answer on unknown state is to do the work again.
 */
export function isSourceContractCurrent(db: SqliteService, sourceId: string): boolean {
  return !!db.get(
    `SELECT 1 FROM source_materializations
      WHERE source_id = ? AND projection = ? AND version = ?`,
    sourceId,
    RUST_INGEST_CONTRACT,
    RUST_INGEST_CONTRACT_VERSION,
  );
}

/**
 * Publish the current contract version for one source.
 *
 * Success-last: call this only after everything the contract covers has
 * committed. Calling it early is the one way to make a failed repair look
 * complete, which the next warm run would then skip.
 *
 * Unused in Phase 0 — see the module note.
 */
export function markSourceContractCurrent(db: SqliteService, sourceId: string): void {
  db.run(
    `INSERT INTO source_materializations(source_id, projection, version, completed_at)
     VALUES (?, ?, ?, ?)
     ON CONFLICT(source_id, projection) DO UPDATE SET
       version = excluded.version,
       completed_at = excluded.completed_at`,
    sourceId,
    RUST_INGEST_CONTRACT,
    RUST_INGEST_CONTRACT_VERSION,
    Date.now(),
  );
}

/**
 * Drop the marker for one source.
 *
 * Phase 1 runs this inside the atomic source clear, so a clear that rolls back
 * cannot leave a source looking repaired.
 *
 * Unused in Phase 0 — see the module note.
 */
export function invalidateSourceContract(db: SqliteService, sourceId: string): void {
  db.run('DELETE FROM source_materializations WHERE source_id = ? AND projection = ?', sourceId, RUST_INGEST_CONTRACT);
}
