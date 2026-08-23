/**
 * The library list, rendered from catalog rows before history converges.
 *
 * `getProjectList` groups source-owned projects into workspaces: keyed by the
 * authoritative path when one exists, by source and slug otherwise. This
 * mirrors that grouping exactly, so a catalog row and its decoded counterpart
 * carry the same `projectId` and the decoded row replaces it in place —
 * no reshuffle, no lost selection, no duplicate.
 *
 * Counts that need decoding are not invented here. `sessionCount` is the
 * number of discovered sessions — the decoded row later reports only the
 * transcript-backed ones, so this can shrink — and messages and tokens stay
 * unknown until they are real.
 */
import type { ProjectListItem, ProjectMember } from '@vibecook/spaghetti-sdk';
import type {
  SpaghettiCatalogPageOptions,
  SpaghettiCatalogProject,
  SpaghettiCatalogProjectPage,
} from '@vibecook/spaghetti-sdk/observation';

const CATALOG_PAGE_LIMIT = 500;

/** The source-owned catalog project needed to page its discovered sessions. */
export interface CatalogProjectTarget {
  projectId: string;
  externalRef: string;
  adapterId: string;
  nativeProjectKey: string;
}

export interface LibraryProject extends ProjectListItem {
  /** False while this row is catalog-only: messages and tokens are unknown. */
  decoded: boolean;
  /** Every source-owned catalog project grouped into this workspace row. */
  catalogProjects: CatalogProjectTarget[];
}

function normalizePath(value: string): string {
  const normalized = value.trim().replace(/\\/g, '/');
  return normalized.length > 1 ? normalized.replace(/\/+$/, '') : normalized;
}

/** An authoritative path, or '' when the native key is not one. */
function pathLikeNativeKey(value: string): string {
  return value.startsWith('/') || /^[A-Za-z]:[\\/]/.test(value) ? value : '';
}

function memberKey(member: ProjectMember): string {
  return JSON.stringify([member.sourceId, member.slug]);
}

function catalogTarget(project: SpaghettiCatalogProject): CatalogProjectTarget {
  return {
    projectId: project.projectId,
    externalRef: project.externalRef,
    adapterId: project.adapterId,
    nativeProjectKey: project.nativeProjectKey,
  };
}

function sortLibrary(projects: LibraryProject[]): LibraryProject[] {
  return projects.sort(
    (a, b) => b.lastActiveAt.localeCompare(a.lastActiveAt) || a.projectId.localeCompare(b.projectId),
  );
}

function workspacePath(project: SpaghettiCatalogProject): string {
  return project.displayPath ?? pathLikeNativeKey(project.nativeProjectKey);
}

function folderName(project: SpaghettiCatalogProject, absolutePath: string): string {
  if (absolutePath) {
    const base = absolutePath.split('/').filter(Boolean).pop();
    if (base) return base;
  }
  return project.displayName ?? project.nativeProjectKey;
}

/** The workspace key `getProjectList` would give this project once decoded. */
export function catalogWorkspaceKey(project: SpaghettiCatalogProject): string {
  const absolutePath = workspacePath(project);
  if (absolutePath) return `path:${normalizePath(absolutePath)}`;
  return `member:${JSON.stringify({ sourceId: project.adapterId, slug: project.nativeProjectKey })}`;
}

/** Read a complete snapshot-consistent project catalog, not just page one. */
export async function allCatalogProjects(
  readPage: (options: SpaghettiCatalogPageOptions) => Promise<SpaghettiCatalogProjectPage>,
): Promise<SpaghettiCatalogProject[]> {
  const projects: SpaghettiCatalogProject[] = [];
  const seenCursors = new Set<string>();
  let cursor: string | undefined;
  do {
    const page = await readPage({ cursor, limit: CATALOG_PAGE_LIMIT });
    projects.push(...page.projects);
    cursor = page.cursor;
    if (cursor && seenCursors.has(cursor))
      throw new Error('Catalog project cursor repeated; refusing an infinite read.');
    if (cursor) seenCursors.add(cursor);
  } while (cursor);
  return projects;
}

/** Group catalog rows into the library list, most recently active first. */
export function catalogLibrary(projects: SpaghettiCatalogProject[]): LibraryProject[] {
  const groups = new Map<string, LibraryProject>();
  for (const project of projects) {
    const key = catalogWorkspaceKey(project);
    const member: ProjectMember = { sourceId: project.adapterId, slug: project.nativeProjectKey };
    const lastActiveAt = project.latestActivityAt ?? '';
    const current = groups.get(key);
    if (!current) {
      const absolutePath = workspacePath(project);
      groups.set(key, {
        projectId: key,
        members: [member],
        slug: project.nativeProjectKey,
        sourceIds: [project.adapterId],
        folderName: folderName(project, absolutePath),
        absolutePath,
        // Every discovered session, which is what `spag projects` reports and
        // what the catalog can know before a transcript is decoded.
        sessionCount: project.sessionCount,
        messageCount: 0,
        tokenUsage: { inputTokens: 0, outputTokens: 0, cacheCreationTokens: 0, cacheReadTokens: 0, totalTokens: 0 },
        tokensEstimated: false,
        lastActiveAt,
        firstActiveAt: '',
        latestGitBranch: '',
        latestPrompt: '',
        hasMemory: false,
        decoded: false,
        catalogProjects: [catalogTarget(project)],
      });
      continue;
    }
    if (!current.members.some((existing) => memberKey(existing) === memberKey(member))) {
      current.members.push(member);
      current.members.sort((a, b) => memberKey(a).localeCompare(memberKey(b)));
      current.sourceIds = [...new Set(current.members.map((entry) => entry.sourceId))].sort();
    }
    if (!current.catalogProjects.some((entry) => entry.projectId === project.projectId)) {
      current.catalogProjects.push(catalogTarget(project));
      current.catalogProjects.sort((a, b) => a.projectId.localeCompare(b.projectId));
    }
    current.sessionCount += project.sessionCount;
    if (lastActiveAt > current.lastActiveAt) {
      current.lastActiveAt = lastActiveAt;
      current.slug = project.nativeProjectKey;
    }
  }
  return sortLibrary([...groups.values()]);
}

/**
 * Add a fresh catalog snapshot without overwriting decoded statistics that may
 * have arrived first. This makes the two independent startup requests safe in
 * either completion order.
 */
export function mergeCatalogProjects(
  current: readonly LibraryProject[],
  catalog: readonly LibraryProject[],
): LibraryProject[] {
  const currentById = new Map(current.map((project) => [project.projectId, project]));
  const catalogIds = new Set(catalog.map((project) => project.projectId));
  const merged = catalog.map((project) => {
    const existing = currentById.get(project.projectId);
    return existing?.decoded
      ? { ...project, ...existing, catalogProjects: project.catalogProjects }
      : { ...project, catalogProjects: [...project.catalogProjects] };
  });

  // Preserve decoded-only rows when a source is degraded, and the development
  // gallery which deliberately has no catalog identity.
  for (const existing of current) {
    if (existing.decoded && !catalogIds.has(existing.projectId)) merged.push(existing);
  }
  return sortLibrary(merged);
}

/**
 * Replace catalog-only rows with their decoded counterparts while retaining
 * catalog identities for session paging. Catalog rows with no decoded history
 * remain visible.
 */
export function mergeDecodedProjects(
  current: readonly LibraryProject[],
  decoded: readonly ProjectListItem[],
  preserveProjectIds: ReadonlySet<string> = new Set(),
): LibraryProject[] {
  const currentById = new Map(current.map((project) => [project.projectId, project]));
  const decodedIds = new Set(decoded.map((project) => project.projectId));
  const merged: LibraryProject[] = decoded.map((project) => ({
    ...project,
    decoded: true,
    catalogProjects: currentById.get(project.projectId)?.catalogProjects ?? [],
  }));

  for (const existing of current) {
    if (decodedIds.has(existing.projectId)) continue;
    if (!existing.decoded || preserveProjectIds.has(existing.projectId)) merged.push(existing);
  }
  return sortLibrary(merged);
}
