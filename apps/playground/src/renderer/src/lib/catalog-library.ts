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
import type { SpaghettiCatalogProject } from '@vibecook/spaghetti-sdk/observation';

export interface LibraryProject extends ProjectListItem {
  /** False while this row is catalog-only: messages and tokens are unknown. */
  decoded: boolean;
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
      });
      continue;
    }
    if (!current.members.some((existing) => memberKey(existing) === memberKey(member))) {
      current.members.push(member);
      current.members.sort((a, b) => memberKey(a).localeCompare(memberKey(b)));
      current.sourceIds = [...new Set(current.members.map((entry) => entry.sourceId))].sort();
    }
    current.sessionCount += project.sessionCount;
    if (lastActiveAt > current.lastActiveAt) {
      current.lastActiveAt = lastActiveAt;
      current.slug = project.nativeProjectKey;
    }
  }
  return [...groups.values()].sort(
    (a, b) => b.lastActiveAt.localeCompare(a.lastActiveAt) || a.projectId.localeCompare(b.projectId),
  );
}
