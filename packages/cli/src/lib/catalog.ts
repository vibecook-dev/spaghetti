/**
 * Catalog and readiness formatting shared by `spag projects`, `spag sessions`,
 * and `spag doctor`.
 *
 * The catalog is listable long before history, usage, and search converge, so
 * these commands render two different things honestly: what exists (always
 * available) and what has been decoded (still arriving).
 */

import type {
  ObservationService,
  SpaghettiCatalogProject,
  SpaghettiCatalogSession,
  SpaghettiReadiness,
  SpaghettiReadinessField,
  SpaghettiReadinessState,
} from '@vibecook/spaghetti-sdk/observation';
import path from 'node:path';

import { theme } from './color.js';

const PAGE_LIMIT = 500;

/** Read every catalog project, following the snapshot-consistent cursor. */
export async function allCatalogProjects(api: ObservationService): Promise<SpaghettiCatalogProject[]> {
  const projects: SpaghettiCatalogProject[] = [];
  let cursor: string | undefined;
  do {
    const page = await api.listCatalogProjects({ cursor, limit: PAGE_LIMIT });
    projects.push(...page.projects);
    cursor = page.cursor;
  } while (cursor);
  return projects;
}

/** Read every catalog session, optionally within one project. */
export async function allCatalogSessions(
  api: ObservationService,
  projectId?: string,
): Promise<SpaghettiCatalogSession[]> {
  const sessions: SpaghettiCatalogSession[] = [];
  let cursor: string | undefined;
  do {
    const page = await api.listCatalogSessions({ projectId, cursor, limit: PAGE_LIMIT });
    sessions.push(...page.sessions);
    cursor = page.cursor;
  } while (cursor);
  return sessions;
}

/** Short per-row label for how much of an entity is available. */
export function catalogStateLabel(state: SpaghettiCatalogProject['catalogState']): string {
  switch (state) {
    case 'searchable':
      return theme.success('searchable');
    case 'hydrated':
      return theme.success('ready');
    case 'transcript_backed':
      return theme.warning('indexing');
    case 'discovered':
      return theme.muted('discovered');
  }
}

function stateColor(state: SpaghettiReadinessState, text: string): string {
  switch (state) {
    case 'ready':
      return theme.success(text);
    case 'indexing':
    case 'pending':
      return theme.warning(text);
    case 'degraded':
    case 'unavailable':
      return theme.error(text);
  }
}

/** `catalog: ready · history: indexing (12 of 40 transcripts decoded)` */
export function readinessField(label: string, field: SpaghettiReadinessField): string {
  const detail = field.detail ? theme.muted(` (${field.detail})`) : '';
  return `${theme.muted(label)}: ${stateColor(field.state, field.state)}${detail}`;
}

/** One line naming everything that has not converged yet. */
export function indexingNotice(readiness: SpaghettiReadiness): string | undefined {
  const pending = (
    [
      ['history', readiness.history],
      ['usage', readiness.usage],
      ['search', readiness.search],
    ] as const
  ).filter(([, field]) => field.state === 'indexing' || field.state === 'pending');
  if (pending.length === 0) return undefined;
  return theme.muted(`indexing in the background: ${pending.map(([name]) => name).join(', ')}`);
}

/**
 * Whether decoded statistics are worth fetching. While history is converging
 * the per-project usage fan-out would be both slow and incomplete, so these
 * commands show catalog facts alone and say so.
 */
export function decodedStatsAvailable(readiness: SpaghettiReadiness): boolean {
  return readiness.history.state === 'ready';
}

/** Every readiness field in a stable order, for `spag doctor`. */
export function readinessFields(readiness: SpaghettiReadiness): Array<[string, SpaghettiReadinessField]> {
  return [
    ['catalog', readiness.catalog],
    ['history', readiness.history],
    ['usage', readiness.usage],
    ['capabilities', readiness.capabilities],
    ['artifacts', readiness.artifacts],
    ['search', readiness.search],
  ];
}

/** Display label for one catalog project. */
export function catalogProjectName(project: SpaghettiCatalogProject): string {
  const fromPath = project.displayPath?.split(/[\\/]/).filter(Boolean).pop();
  return project.displayName ?? fromPath ?? project.nativeProjectKey;
}

/**
 * Resolve a user-supplied project reference against catalog rows, in the same
 * order `spag` has always used: cwd, then 1-based index, then exact, prefix,
 * and substring name matches, then the native key.
 *
 * Working from catalog rows is what lets `spag sessions` resolve a project
 * during background ingestion, before any of its history is decoded.
 */
export function resolveCatalogProject(
  input: string,
  projects: SpaghettiCatalogProject[],
): SpaghettiCatalogProject | null {
  if (projects.length === 0) return null;

  if (input === '.') {
    const cwd = process.cwd();
    const byRecency = (a: SpaghettiCatalogProject, b: SpaghettiCatalogProject) =>
      (b.latestActivityAt ?? '').localeCompare(a.latestActivityAt ?? '');
    const withPath = projects.filter((project): project is SpaghettiCatalogProject & { displayPath: string } =>
      Boolean(project.displayPath),
    );
    const exact = withPath.filter((project) => project.displayPath === cwd).sort(byRecency);
    if (exact.length > 0) return exact[0]!;
    const containing = withPath.filter((project) => cwd.startsWith(project.displayPath + path.sep)).sort(byRecency);
    if (containing.length > 0) return containing[0]!;
    const inside = withPath.filter((project) => project.displayPath.startsWith(cwd + path.sep)).sort(byRecency);
    return inside[0] ?? null;
  }

  const index = Number(input);
  if (Number.isInteger(index) && index >= 1 && index <= projects.length) return projects[index - 1]!;

  const lower = input.toLowerCase();
  const names = projects.map((project) => [project, catalogProjectName(project).toLowerCase()] as const);
  for (const match of [
    names.filter(([, name]) => name === lower),
    names.filter(([, name]) => name.startsWith(lower)),
    names.filter(([, name]) => name.includes(lower)),
  ]) {
    if (match.length === 1) return match[0]![0];
  }
  return projects.find((project) => project.nativeProjectKey.toLowerCase() === lower) ?? null;
}

/** Closest catalog projects, for a "did you mean" hint. */
export function suggestCatalogProjects(
  input: string,
  projects: SpaghettiCatalogProject[],
  limit = 5,
): Array<{ folderName: string; sessionCount: number }> {
  const lower = input.toLowerCase();
  return projects
    .map((project) => ({ folderName: catalogProjectName(project), sessionCount: project.sessionCount }))
    .filter(({ folderName }) => folderName.toLowerCase().includes(lower) || lower.includes(folderName.toLowerCase()))
    .slice(0, limit);
}
