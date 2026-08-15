import type { ProjectListItem, SegmentChange } from '@vibecook/spaghetti-sdk';

/**
 * Apply the activity information carried by the change feed without
 * re-querying every project in the database.
 *
 * Counts are deliberately left unchanged: an upsert event does not tell the
 * renderer whether a canonical row was inserted or replaced. Exact counts
 * are refreshed by explicit lifecycle operations (startup/rebuild), while
 * the selected project's session lane performs its own scoped live read.
 */
export function applyProjectLiveActivity(
  projects: readonly ProjectListItem[],
  changes: readonly SegmentChange[],
  receivedAt: number,
): ProjectListItem[] {
  if (changes.length === 0) return projects as ProjectListItem[];

  const touchedMembers = new Set(
    changes.flatMap((change) =>
      change.sourceId && change.projectSlug ? [memberKey(change.sourceId, change.projectSlug)] : [],
    ),
  );
  if (touchedMembers.size === 0) return projects as ProjectListItem[];

  const activityAt = new Date(receivedAt).toISOString();
  let changed = false;
  const next = projects.map((project) => {
    const touched = project.members.some((member) => touchedMembers.has(memberKey(member.sourceId, member.slug)));
    if (!touched || project.lastActiveAt >= activityAt) return project;
    changed = true;
    return { ...project, lastActiveAt: activityAt };
  });
  return changed ? next : (projects as ProjectListItem[]);
}

function memberKey(sourceId: string, projectSlug: string): string {
  return JSON.stringify([sourceId, projectSlug]);
}
