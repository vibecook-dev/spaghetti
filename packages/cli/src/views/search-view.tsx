/**
 * SearchView — Full-text search results across all projects
 */

import React, { useMemo, useCallback } from 'react';
import { Box, Text, useInput, useStdout } from 'ink';
import type { ProjectListItem, SearchResultSet, SearchResult, SessionListItem } from '@vibecook/spaghetti-sdk';
import { useViewNav } from './context.js';
import { useApi } from './shell.js';
import { useAsyncValue, useListNavigation } from './hooks.js';
import { formatRelativeTime } from '../lib/format.js';
import { SessionsView } from './sessions-view.js';
import { MessagesView } from './messages-view.js';
import type { ViewEntry } from './types.js';

// ─── ResultCard ──────────────────────────────────────────────────────

interface ResultCardProps {
  result: SearchResult;
  projectName: string;
  sessionIndex: number | null;
  sessionTime: string | null;
  selected: boolean;
  cols: number;
}

function ResultCard({
  result,
  projectName,
  sessionIndex,
  sessionTime,
  selected,
  cols,
}: ResultCardProps): React.ReactElement {
  const dot = ' \u00B7 ';

  // Left side: project · #session · time
  const prefix = selected ? '\x1b[36m\u258E\x1b[0m' : ' ';
  const name = selected ? `\x1b[1m\x1b[37m${projectName}\x1b[0m` : `\x1b[2m${projectName}\x1b[0m`;
  const sessionStr =
    sessionIndex !== null ? (selected ? `\x1b[2m#${sessionIndex}\x1b[0m` : `\x1b[2m#${sessionIndex}\x1b[0m`) : '';
  const timeStr = sessionTime ? `\x1b[2m${formatRelativeTime(sessionTime)}\x1b[0m` : '';

  const leftParts = [name, sessionStr, timeStr].filter(Boolean);
  const leftText = leftParts.join(dot);

  // Right side: role derived from segment type
  const role = result.type === 'message' ? '' : result.type;
  const roleText = selected ? `\x1b[36m${role}\x1b[0m` : `\x1b[2m${role}\x1b[0m`;

  // Snippet: truncate to ~2 lines
  const maxSnippetLen = Math.max((cols - 6) * 2, 40);
  const rawSnippet = result.snippet || '';
  const snippet = rawSnippet.length > maxSnippetLen ? rawSnippet.slice(0, maxSnippetLen - 1) + '\u2026' : rawSnippet;
  // Clean up newlines for display
  const cleanSnippet = snippet.replace(/\n/g, ' ');

  // Split snippet into lines that fit within the available width
  const lineWidth = Math.max(cols - 6, 20);
  const snippetLine1 = cleanSnippet.slice(0, lineWidth);
  const snippetLine2 = cleanSnippet.length > lineWidth ? cleanSnippet.slice(lineWidth, lineWidth * 2) : '';

  return (
    <Box flexDirection="column">
      <Box>
        <Text>
          {prefix} {leftText}
        </Text>
        <Box flexGrow={1} />
        <Text>{roleText}</Text>
      </Box>
      <Box>
        <Text>
          {prefix}{' '}
          <Text dimColor={!selected} italic>
            &quot;{snippetLine1}
          </Text>
        </Text>
      </Box>
      {snippetLine2 ? (
        <Box>
          <Text>
            {prefix}{' '}
            <Text dimColor={!selected} italic>
              {snippetLine2}&quot;
            </Text>
          </Text>
        </Box>
      ) : (
        <Box>
          <Text>
            {prefix}{' '}
            <Text dimColor={!selected} italic>
              {snippetLine1 ? '' : ''}&quot;
            </Text>
          </Text>
        </Box>
      )}
      <Text> </Text>
    </Box>
  );
}

// ─── SearchView ──────────────────────────────────────────────────────

export interface SearchViewProps {
  query: string;
}

export function SearchView({ query }: SearchViewProps): React.ReactElement {
  const nav = useViewNav();
  const api = useApi();
  const { stdout } = useStdout();
  const cols = stdout?.columns ?? 80;

  const loaded = useAsyncValue(
    async () => {
      const [resultSet, projects] = await Promise.all([api.search({ text: query }), api.getProjectList()]);
      const sessionsByProject = new Map<string, SessionListItem[]>();
      for (const project of projects) {
        if (
          resultSet.results.some((result) =>
            project.members.some(
              (member) =>
                member.slug === result.projectSlug && (!result.sourceId || member.sourceId === result.sourceId),
            ),
          )
        ) {
          sessionsByProject.set(project.projectId, await api.getSessionList(project));
        }
      }
      return { resultSet, projects, sessionsByProject };
    },
    [api, query],
    {
      resultSet: { results: [], total: 0, hasMore: false } as SearchResultSet,
      projects: [] as ProjectListItem[],
      sessionsByProject: new Map<string, SessionListItem[]>(),
    },
  );
  const { resultSet, projects, sessionsByProject } = loaded.value;

  const results = resultSet.results;

  // Build an exact source/slug lookup; slugs alone are lossy path encodings.
  const projectsByMember = useMemo(() => {
    const map = new Map<string, string>();
    for (const p of projects) {
      for (const member of p.members) {
        map.set(`${member.sourceId}/${member.slug}`, p.folderName);
      }
    }
    return map;
  }, [projects]);

  // Build session index + time lookup, scoped per project source when known.
  const sessionInfo = useMemo(() => {
    const map = new Map<string, { index: number; time: string; sourceId: string }>();
    const projectMembers = new Map<string, { slug: string; sourceId: string }>();
    for (const r of results) {
      if (r.projectSlug && r.sourceId) {
        projectMembers.set(`${r.sourceId}/${r.projectSlug}`, { slug: r.projectSlug, sourceId: r.sourceId });
      }
    }
    for (const { slug, sourceId } of projectMembers.values()) {
      const project = projects.find((candidate) =>
        candidate.members.some((member) => member.sourceId === sourceId && member.slug === slug),
      );
      if (!project) continue;
      const sessions = (sessionsByProject.get(project.projectId) ?? []).filter(
        (session) => session.sourceId === sourceId,
      );
      for (let i = 0; i < sessions.length; i++) {
        const key = `${slug}/${sessions[i].sourceId}/${sessions[i].sessionId}`;
        if (!map.has(key)) {
          map.set(key, {
            index: i,
            time: sessions[i].lastUpdate,
            sourceId: sessions[i].sourceId,
          });
        }
      }
    }
    return map;
  }, [projects, results, sessionsByProject]);

  const { selectedIndex, scrollOffset, moveUp, moveDown } = useListNavigation({
    itemCount: results.length,
    itemHeight: 4,
  });

  // Navigate to a search result: pop SearchView, push Sessions + Messages
  const navigateToResult = useCallback(
    (result: SearchResult) => {
      const slug = result.projectSlug;
      const sessionId = result.sessionId;
      if (!slug) {
        // No project info — just pop back
        nav.pop();
        return;
      }

      // Find the project — prefer the source that owns this session when known.
      const project = projects.find((candidate) =>
        candidate.members.some(
          (member) => member.slug === slug && (!result.sourceId || member.sourceId === result.sourceId),
        ),
      );
      if (!project) {
        nav.pop();
        return;
      }

      if (!sessionId) {
        // No session info — navigate to project's sessions list
        const sessionsEntry: ViewEntry = {
          type: 'sessions',
          component: () => <SessionsView project={project} />,
          breadcrumb: project.folderName,
        };
        (sessionsEntry as any)._project = project;
        nav.popAndPush(sessionsEntry);
        return;
      }

      const sessions = sessionsByProject.get(project.projectId) ?? [];
      const sessIdx = sessions.findIndex(
        (s) => s.sessionId === sessionId && (!result.sourceId || s.sourceId === result.sourceId),
      );
      if (sessIdx < 0) {
        // Session not found — navigate to project level
        const sessionsEntry: ViewEntry = {
          type: 'sessions',
          component: () => <SessionsView project={project} />,
          breadcrumb: project.folderName,
        };
        (sessionsEntry as any)._project = project;
        nav.popAndPush(sessionsEntry);
        return;
      }

      const session = sessions[sessIdx];

      // Build SessionsView entry (pre-selected to the right session)
      const sessionsEntry: ViewEntry = {
        type: 'sessions',
        component: () => <SessionsView project={project} initialIndex={sessIdx} />,
        breadcrumb: project.folderName,
      };
      (sessionsEntry as any)._project = project;

      // Build MessagesView entry (scrolled to end — the search result is likely recent)
      const messagesEntry: ViewEntry = {
        type: 'messages',
        component: () => <MessagesView project={project} session={session} sessionIndex={sessIdx} />,
        breadcrumb: `#${sessIdx + 1}`,
      };
      (messagesEntry as any)._project = project;
      (messagesEntry as any)._session = session;
      (messagesEntry as any)._sessionIndex = sessIdx;

      // Multi-push: pop search, push sessions + messages
      nav.popAndPush(sessionsEntry, messagesEntry);
    },
    [nav, projects, sessionInfo, sessionsByProject],
  );

  // Key handling
  useInput(
    (input, key) => {
      if (key.upArrow) {
        moveUp();
      } else if (key.downArrow) {
        moveDown();
      } else if (key.return) {
        if (results.length === 0) return;
        const result = results[selectedIndex];
        if (result) {
          navigateToResult(result);
        }
      } else if (key.escape) {
        nav.pop();
      }
    },
    { isActive: !nav.searchMode },
  );

  if (loaded.loading) return <Text dimColor> Loading canonical search results…</Text>;
  if (loaded.error) return <Text color="red"> {loaded.error.message}</Text>;

  // Empty state
  if (results.length === 0) {
    return (
      <Box flexDirection="column" paddingLeft={2}>
        <Text> </Text>
        <Text dimColor>No results found for &quot;{query}&quot;</Text>
      </Box>
    );
  }

  // Calculate visible range
  const termRows = stdout?.rows ?? 24;
  const viewportItems = Math.max(1, Math.floor((termRows - 6) / 4));
  const visibleResults = results.slice(scrollOffset, scrollOffset + viewportItems);

  return (
    <Box flexDirection="column">
      {visibleResults.map((r, i) => {
        const actualIndex = scrollOffset + i;
        const memberKey = r.projectSlug && r.sourceId ? `${r.sourceId}/${r.projectSlug}` : '';
        const projName = projectsByMember.get(memberKey) || r.projectSlug || 'unknown';
        const sessKey =
          r.projectSlug && r.sessionId ? `${r.projectSlug}/${r.sourceId ?? 'claude-code'}/${r.sessionId}` : null;
        const info = sessKey ? sessionInfo.get(sessKey) : null;

        return (
          <ResultCard
            key={r.key}
            result={r}
            projectName={projName}
            sessionIndex={info?.index ?? null}
            sessionTime={info?.time ?? null}
            selected={actualIndex === selectedIndex}
            cols={cols}
          />
        );
      })}
    </Box>
  );
}
