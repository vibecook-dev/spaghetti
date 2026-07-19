/**
 * useLiveSessionList — session / project list hook (RFC 005 C3.5).
 *
 * Two modes:
 *
 *  - project locator/slug provided → prewarms + subscribes to each member
 *    slug, snapshot = `api.getSessionList(project)`. Good for a
 *    project-detail sidebar, including multi-agent workspaces.
 *  - project omitted → prewarms + subscribes to the session firehose
 *    (`{ kind: 'session' }`), snapshot = `api.getProjectList()`. The
 *    public SDK does not expose a cross-project session accessor,
 *    so the firehose variant returns the project list (per RFC 005
 *    §8; the design calls this "session/project list"). Any session
 *    Change bumps the list because project summaries roll up session
 *    counts / last-active timestamps.
 *
 * Snapshot stability follows the same pattern as
 * `useLiveSessionMessages`: ref-held cache keyed on a local counter
 * bumped by the subscribe callback.
 */

import { useCallback, useEffect, useMemo, useRef, useSyncExternalStore } from 'react';
import type { ProjectListItem, ProjectReference, SessionListItem } from '../../api.js';
import type { ChangeTopic } from '../../live/change-events.js';
import { useSpaghettiAPI } from '../context.js';

type ListSnapshot = {
  /** Input key the snapshot was computed against. */
  key: string;
  /** API identity the snapshot was read from — a provider swap must invalidate. */
  api: unknown;
  seq: number;
  items: SessionListItem[] | ProjectListItem[];
};

// Overloads so a project reference narrows to `SessionListItem[]` and the
// bare call narrows to `ProjectListItem[]`. Consumers pick the shape
// they need at the call site.
export function useLiveSessionList(project: ProjectReference): SessionListItem[];
export function useLiveSessionList(): ProjectListItem[];
export function useLiveSessionList(project?: ProjectReference): SessionListItem[] | ProjectListItem[] {
  const api = useSpaghettiAPI();

  const cacheRef = useRef<ListSnapshot | null>(null);
  const localSeqRef = useRef(0);

  const key = typeof project === 'string' ? project : project ? project.projectId : '';
  const subscriptionSlugs = useMemo(
    () =>
      typeof project === 'string'
        ? [project]
        : project
          ? [...new Set(project.members.map((member) => member.slug))]
          : [],
    [project],
  );

  // Memoize the topic so useEffect / useCallback deps stay honest —
  // this replaces the previous `eslint-disable-next-line
  // react-hooks/exhaustive-deps` escape hatches.
  const topics = useMemo<ChangeTopic[]>(
    () =>
      subscriptionSlugs.length > 0
        ? subscriptionSlugs.map((slug) => ({ kind: 'session' as const, slug }))
        : [{ kind: 'session' }],
    [subscriptionSlugs],
  );

  useEffect(() => {
    const disposers = topics.map((topic) => api.live?.prewarm(topic));
    return () => {
      for (const dispose of disposers) dispose?.();
    };
  }, [api, topics]);

  const subscribe = useCallback(
    (onStoreChange: () => void): (() => void) => {
      const disposers = topics.map((topic) =>
        api.live?.onChange(topic, () => {
          localSeqRef.current += 1;
          onStoreChange();
        }),
      );
      return () => {
        for (const dispose of disposers) dispose?.();
      };
    },
    [api, topics],
  );

  const getSnapshot = useCallback((): ListSnapshot => {
    const cached = cacheRef.current;
    if (cached && cached.key === key && cached.api === api && cached.seq === localSeqRef.current) {
      return cached;
    }
    const items: SessionListItem[] | ProjectListItem[] =
      project !== undefined ? api.getSessionList(project) : api.getProjectList();
    const next: ListSnapshot = { key, api, seq: localSeqRef.current, items };
    cacheRef.current = next;
    return next;
  }, [api, key, project]);

  const snapshot = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
  return snapshot.items;
}
