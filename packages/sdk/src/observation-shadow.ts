/**
 * Opt-in RFC 011 Claude observation shadow.
 *
 * The shadow owns a persistent Rust engine and an isolated database. It is
 * deliberately not a query facade for the production service: callers use
 * its typed snapshots to collect parity evidence before selecting Rust as the
 * sole production writer.
 */

import { lstatSync, readlinkSync, realpathSync } from 'node:fs';
import { basename, dirname, extname, join, resolve } from 'node:path';

import {
  openSpaghettiEngine,
  type SpaghettiEngine,
  type SpaghettiEngineHealth,
  type SpaghettiEngineOverview,
  type SpaghettiEngineStatus,
} from './native.js';

const OWNER_LOCK_SUFFIX = '.owner-lock.sqlite3';
const OWNER_METADATA_SUFFIX = '.owner.json';

export interface ClaudeObservationShadowOptions {
  /** Production compatibility database. The shadow is forbidden from opening it. */
  productionDbPath: string;
  /** Isolated RFC 011 database. Defaults beside the production database. */
  shadowDbPath?: string;
  /** Explicit Claude Code roots. No source root is inferred by this helper. */
  roots: string[];
  /** Persistent read-only workers used for shadow diagnostics. Defaults to one. */
  queryWorkers?: number;
  /** Diagnostic owner label written to the shadow owner sidecar. */
  ownerLabel?: string;
  /** Cancels startup observation; a partially opened engine is still disposed. */
  signal?: AbortSignal;
}

export interface ClaudeObservationShadowSnapshot {
  mode: 'shadow';
  databasePath: string;
  roots: string[];
  status: SpaghettiEngineStatus;
  health: SpaghettiEngineHealth;
  overview: SpaghettiEngineOverview;
}

export interface ClaudeLegacyHistoryCounts {
  /** Claude parent-session rows in the compatibility projection. */
  sessions: number;
  /** Claude parent transcript rows in the compatibility projection. */
  messages: number;
  /** Claude subagent transcript rows, excluding derived timeline rows. */
  subagentMessages: number;
}

export interface ClaudeObservationHistoryParity {
  atCommitSeq: number;
  exact: boolean;
  sessions: {
    legacy: number;
    canonical: number;
    delta: number;
    exact: boolean;
  };
  messages: {
    legacyParent: number;
    legacySubagent: number;
    legacyTotal: number;
    canonical: number;
    delta: number;
    exact: boolean;
  };
}

export interface ClaudeObservationShadow {
  readonly databasePath: string;
  readonly roots: readonly string[];
  readonly status: SpaghettiEngineStatus;
  snapshot(signal?: AbortSignal): Promise<ClaudeObservationShadowSnapshot>;
  compareHistory(legacy: ClaudeLegacyHistoryCounts, signal?: AbortSignal): Promise<ClaudeObservationHistoryParity>;
  refresh(signal?: AbortSignal): Promise<SpaghettiEngineStatus>;
  dispose(): Promise<SpaghettiEngineStatus>;
}

/** Derive a sibling database without changing or opening either path. */
export function defaultClaudeObservationShadowDbPath(productionDbPath: string): string {
  const absolute = resolveDatabaseCandidate(productionDbPath, 'production');
  const extension = extname(absolute);
  const stem = basename(absolute, extension);
  return join(dirname(absolute), `${stem}.observation-shadow${extension || '.db'}`);
}

/**
 * Compare like-for-like Claude history counts.
 *
 * The caller must scope legacy counts to Claude. Whole-store counts from a
 * multi-source production database are not a valid parity oracle.
 */
export function compareClaudeObservationHistory(
  overview: SpaghettiEngineOverview,
  legacy: ClaudeLegacyHistoryCounts,
): ClaudeObservationHistoryParity {
  assertCount('sessions', legacy.sessions);
  assertCount('messages', legacy.messages);
  assertCount('subagentMessages', legacy.subagentMessages);
  const legacyMessages = legacy.messages + legacy.subagentMessages;
  assertCount('total messages', legacyMessages);
  const sessionDelta = overview.canonicalSessions - legacy.sessions;
  const messageDelta = overview.canonicalMessages - legacyMessages;
  return {
    atCommitSeq: overview.commitSeq,
    exact: sessionDelta === 0 && messageDelta === 0,
    sessions: {
      legacy: legacy.sessions,
      canonical: overview.canonicalSessions,
      delta: sessionDelta,
      exact: sessionDelta === 0,
    },
    messages: {
      legacyParent: legacy.messages,
      legacySubagent: legacy.subagentMessages,
      legacyTotal: legacyMessages,
      canonical: overview.canonicalMessages,
      delta: messageDelta,
      exact: messageDelta === 0,
    },
  };
}

/** Open one isolated engine, register watchers, then complete the initial scan. */
export async function openClaudeObservationShadow(
  options: ClaudeObservationShadowOptions,
): Promise<ClaudeObservationShadow> {
  options.signal?.throwIfAborted();
  const productionPath = canonicalPotentialPath(options.productionDbPath, 'production');
  const requestedShadowPath = options.shadowDbPath ?? defaultClaudeObservationShadowDbPath(productionPath);
  const shadowPath = canonicalPotentialPath(requestedShadowPath, 'shadow');
  assertIsolatedDatabaseArtifacts(productionPath, shadowPath);
  const roots = normalizeRoots(options.roots);

  const engine = await openSpaghettiEngine({
    dbPath: shadowPath,
    queryWorkers: options.queryWorkers ?? 1,
    ownerLabel: options.ownerLabel ?? 'sdk-claude-observation-shadow',
  });
  try {
    await engine.startClaudeObservation({ roots, reason: 'shadow_observation' }, options.signal);
    return new NativeClaudeObservationShadow(engine, roots);
  } catch (error) {
    await engine.dispose();
    throw error;
  }
}

class NativeClaudeObservationShadow implements ClaudeObservationShadow {
  readonly databasePath: string;
  readonly roots: readonly string[];
  private disposePromise: Promise<SpaghettiEngineStatus> | null = null;

  constructor(
    private readonly engine: SpaghettiEngine,
    roots: string[],
  ) {
    this.databasePath = engine.status.databasePath;
    this.roots = Object.freeze([...roots]);
  }

  get status(): SpaghettiEngineStatus {
    return this.engine.status;
  }

  async snapshot(signal?: AbortSignal): Promise<ClaudeObservationShadowSnapshot> {
    const [health, overview] = await Promise.all([this.engine.health(signal), this.engine.overview(signal)]);
    return {
      mode: 'shadow',
      databasePath: this.databasePath,
      roots: [...this.roots],
      status: health.status,
      health,
      overview,
    };
  }

  async compareHistory(
    legacy: ClaudeLegacyHistoryCounts,
    signal?: AbortSignal,
  ): Promise<ClaudeObservationHistoryParity> {
    return compareClaudeObservationHistory(await this.engine.overview(signal), legacy);
  }

  refresh(signal?: AbortSignal): Promise<SpaghettiEngineStatus> {
    return this.engine.refreshClaudeObservation(signal);
  }

  async dispose(): Promise<SpaghettiEngineStatus> {
    if (!this.disposePromise) this.disposePromise = this.engine.dispose();
    return await this.disposePromise;
  }
}

function normalizeRoots(roots: string[]): string[] {
  if (!Array.isArray(roots) || roots.length === 0) {
    throw new Error('Claude observation shadow requires at least one explicit source root.');
  }
  const normalized = roots.map((root) => canonicalPotentialPath(root, 'source root'));
  return [...new Set(normalized)];
}

function resolveDatabaseCandidate(value: string, label: string): string {
  if (typeof value !== 'string' || value.trim() === '' || value === ':memory:') {
    throw new Error(`Claude observation ${label} must be a non-empty file-backed path.`);
  }
  return resolve(value);
}

/** Resolve symlinked ancestors even when the leaf does not exist yet. */
function canonicalPotentialPath(value: string, label: string): string {
  const absolute = resolveDatabaseCandidate(value, label);
  return canonicalPotentialAbsolutePath(absolute, label, new Set());
}

function canonicalPotentialAbsolutePath(absolute: string, label: string, seenLinks: Set<string>): string {
  let existing = absolute;
  const missing: string[] = [];
  while (true) {
    const entry = lstatSync(existing, { throwIfNoEntry: false });
    if (entry?.isSymbolicLink()) {
      const linkIdentity = pathIdentity(existing);
      if (seenLinks.has(linkIdentity)) {
        throw new Error(`Claude observation ${label} contains a symbolic-link cycle: ${absolute}`);
      }
      seenLinks.add(linkIdentity);
      const target = resolve(dirname(existing), readlinkSync(existing));
      return resolve(canonicalPotentialAbsolutePath(target, label, seenLinks), ...missing);
    }
    if (entry) {
      if (missing.length > 0 && !entry.isDirectory()) {
        throw new Error(`Claude observation ${label} has a non-directory ancestor: ${existing}`);
      }
      return resolve(realpathSync.native(existing), ...missing);
    }
    const parent = dirname(existing);
    if (parent === existing) {
      throw new Error(`Claude observation ${label} could not be resolved: ${absolute}`);
    }
    missing.unshift(basename(existing));
    existing = parent;
  }
}

function artifactFamily(databasePath: string): Set<string> {
  return new Set(
    [
      databasePath,
      `${databasePath}-wal`,
      `${databasePath}-shm`,
      `${databasePath}-journal`,
      `${databasePath}${OWNER_LOCK_SUFFIX}`,
      `${databasePath}${OWNER_METADATA_SUFFIX}`,
    ].map(pathIdentity),
  );
}

function assertIsolatedDatabaseArtifacts(productionPath: string, shadowPath: string): void {
  const productionArtifacts = artifactFamily(productionPath);
  const shadowArtifacts = artifactFamily(shadowPath);
  for (const artifact of shadowArtifacts) {
    if (productionArtifacts.has(artifact)) {
      throw new Error(
        `Claude observation shadow database must be isolated from the production database: ${shadowPath}`,
      );
    }
  }
}

function pathIdentity(value: string): string {
  // Windows paths are case-insensitive. Default macOS volumes are too; being
  // conservative on a case-sensitive macOS volume only rejects a risky alias.
  return process.platform === 'win32' || process.platform === 'darwin' ? value.toLocaleLowerCase('en-US') : value;
}

function assertCount(label: string, value: number): void {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new Error(`Claude legacy ${label} count must be a non-negative safe integer.`);
  }
}
