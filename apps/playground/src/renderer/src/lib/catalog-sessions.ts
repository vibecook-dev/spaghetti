/** Catalog-first session rows and their eventual decoded replacements. */

import type { SessionListItem } from '@vibecook/spaghetti-sdk';
import type {
  SpaghettiCatalogSession,
  SpaghettiCatalogSessionPage,
  SpaghettiCatalogSessionPageOptions,
} from '@vibecook/spaghetti-sdk/observation';
import type { CatalogProjectTarget } from './catalog-library.js';

const CATALOG_PAGE_LIMIT = 500;

export interface LibrarySession extends SessionListItem {
  /** Canonical catalog identity, including when no native session ID exists. */
  externalRef?: string;
  catalogSessionId?: string;
  catalogState?: SpaghettiCatalogSession['catalogState'];
  degraded?: boolean;
  degradedReason?: string;
}

interface CatalogSessionSeed {
  session: SpaghettiCatalogSession;
  project: CatalogProjectTarget;
}

type CatalogSessionPageReader = (options: SpaghettiCatalogSessionPageOptions) => Promise<SpaghettiCatalogSessionPage>;

function durationBetween(start: string, end: string): number {
  const startMs = Date.parse(start);
  const endMs = Date.parse(end);
  return Number.isFinite(startMs) && Number.isFinite(endMs) && endMs >= startMs ? endMs - startMs : 0;
}

function sessionAliases(session: LibrarySession): string[] {
  return [
    ...(session.externalRef ? [`external:${session.externalRef}`] : []),
    `native:${JSON.stringify([session.sourceId, session.sessionId])}`,
  ];
}

function sessionIndex(sessions: readonly LibrarySession[]): Map<string, LibrarySession> {
  const index = new Map<string, LibrarySession>();
  for (const session of sessions) {
    for (const alias of sessionAliases(session)) index.set(alias, session);
  }
  return index;
}

function findSession(index: ReadonlyMap<string, LibrarySession>, session: LibrarySession): LibrarySession | undefined {
  for (const alias of sessionAliases(session)) {
    const found = index.get(alias);
    if (found) return found;
  }
  return undefined;
}

function sortSessions(sessions: LibrarySession[]): LibrarySession[] {
  return sessions.sort(
    (a, b) =>
      b.lastUpdate.localeCompare(a.lastUpdate) ||
      (a.externalRef ?? `${a.sourceId}:${a.sessionId}`).localeCompare(b.externalRef ?? `${b.sourceId}:${b.sessionId}`),
  );
}

function catalogSession(seed: CatalogSessionSeed): LibrarySession {
  const { session, project } = seed;
  const startTime = session.nativeCreatedAt ?? '';
  const lastUpdate = session.nativeUpdatedAt ?? startTime;
  return {
    // A catalog session without a provable native ID is deliberately not
    // clickable. The canonical ID only supplies stable React/list identity.
    sessionId: session.nativeSessionId ?? session.sessionId,
    sourceId: session.adapterId,
    projectSlug: project.nativeProjectKey,
    externalRef: session.externalRef,
    decoded: false,
    startTime,
    lastUpdate,
    lifespanMs: durationBetween(startTime, lastUpdate),
    tokenUsage: {
      inputTokens: 0,
      outputTokens: 0,
      cacheCreationTokens: 0,
      cacheReadTokens: 0,
      totalTokens: 0,
    },
    tokensEstimated: false,
    messageCount: 0,
    fullPath: '',
    title: session.title ?? '',
    summary: '',
    firstPrompt: '',
    gitBranch: '',
    todoCount: 0,
    planSlug: null,
    hasTask: false,
    isSidechain: false,
    catalogSessionId: session.sessionId,
    catalogState: session.catalogState,
    degraded: session.degraded,
    ...(session.degradedReason ? { degradedReason: session.degradedReason } : {}),
  };
}

/** Page every source-owned catalog project grouped into one playground row. */
export async function allCatalogSessionSeeds(
  projects: readonly CatalogProjectTarget[],
  readPage: CatalogSessionPageReader,
): Promise<LibrarySession[]> {
  const uniqueProjects = [...new Map(projects.map((project) => [project.projectId, project])).values()];
  const groups = await Promise.all(
    uniqueProjects.map(async (project): Promise<LibrarySession[]> => {
      const seeds: CatalogSessionSeed[] = [];
      const seenCursors = new Set<string>();
      let cursor: string | undefined;
      do {
        const page = await readPage({ projectId: project.projectId, cursor, limit: CATALOG_PAGE_LIMIT });
        seeds.push(...page.sessions.map((session) => ({ session, project })));
        cursor = page.cursor;
        if (cursor && seenCursors.has(cursor)) {
          throw new Error(`Catalog session cursor repeated for project '${project.projectId}'.`);
        }
        if (cursor) seenCursors.add(cursor);
      } while (cursor);
      return seeds.map(catalogSession);
    }),
  );
  return sortSessions(groups.flat());
}

/** Merge a fresh catalog snapshot without erasing decoded rows that won the race. */
export function mergeCatalogSessions(
  current: readonly LibrarySession[],
  catalog: readonly LibrarySession[],
): LibrarySession[] {
  const decoded = current.filter((session) => session.decoded);
  const decodedIndex = sessionIndex(decoded);
  const used = new Set<LibrarySession>();
  const merged = catalog.map((session) => {
    const replacement = findSession(decodedIndex, session);
    if (!replacement) return session;
    used.add(replacement);
    return { ...session, ...replacement };
  });
  for (const session of decoded) {
    if (!used.has(session)) merged.push(session);
  }
  return sortSessions(merged);
}

/** Replace matching catalog rows while preserving still-undecoded discoveries. */
export function mergeDecodedSessions(
  current: readonly LibrarySession[],
  decoded: readonly SessionListItem[],
): LibrarySession[] {
  const catalog = current.filter((session) => !session.decoded);
  const catalogIndex = sessionIndex(catalog);
  const used = new Set<LibrarySession>();
  const merged: LibrarySession[] = decoded.map((session) => {
    const replacement = findSession(catalogIndex, session);
    if (!replacement) return session;
    used.add(replacement);
    return { ...replacement, ...session };
  });
  for (const session of catalog) {
    if (!used.has(session)) merged.push(session);
  }
  return sortSessions(merged);
}
