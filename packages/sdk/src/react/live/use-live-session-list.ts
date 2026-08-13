import { useCallback, useMemo } from 'react';
import type { ProjectListItem, ProjectReference, SessionListItem } from '../../api.js';
import type { ChangeTopic } from '../../live/change-events.js';
import { useSpaghettiClient } from '../context.js';
import { changeBatchMatchesTopic } from './change-filter.js';
import { useAsyncSnapshot } from './use-async-snapshot.js';

const EMPTY_PROJECTS: ProjectListItem[] = [];
const EMPTY_SESSIONS: SessionListItem[] = [];

export function useLiveSessionList(project: ProjectReference): SessionListItem[];
export function useLiveSessionList(): ProjectListItem[];
export function useLiveSessionList(project?: ProjectReference): SessionListItem[] | ProjectListItem[] {
  const client = useSpaghettiClient();
  const projectKey = projectReferenceKey(project);
  const stableProject = useMemo(() => project, [projectKey]);
  const topics = useMemo<ChangeTopic[]>(() => {
    if (stableProject === undefined) return [{ kind: 'session' }];
    if (typeof stableProject === 'string') return [{ kind: 'session', slug: stableProject }];
    const slugs = [...new Set(stableProject.members.map((member) => member.slug))];
    if (slugs.length === 0) return [{ kind: 'session' }];
    return slugs.map((slug) => ({
      kind: 'session' as const,
      slug,
    }));
  }, [stableProject]);
  const load = useCallback(
    async (): Promise<SessionListItem[] | ProjectListItem[]> =>
      stableProject === undefined ? await client.getProjectList() : await client.getSessionList(stableProject),
    [client, stableProject],
  );
  const subscribe = useCallback(
    (invalidate: () => void) =>
      client.onChange((batch) => {
        if (topics.some((topic) => changeBatchMatchesTopic(batch, topic))) invalidate();
      }),
    [client, topics],
  );
  return useAsyncSnapshot(stableProject === undefined ? EMPTY_PROJECTS : EMPTY_SESSIONS, load, subscribe).value;
}

function projectReferenceKey(project: ProjectReference | undefined): string {
  if (project === undefined) return '';
  if (typeof project === 'string') return `slug:${project}`;
  return JSON.stringify([project.projectId, project.members.map((member) => [member.sourceId, member.slug])]);
}
