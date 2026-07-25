/**
 * ProjectsView — Scrollable list of projects
 */

import React, { useMemo, useRef } from 'react';
import { Box, Text, useInput } from 'ink';
import type { ProjectListItem } from '@vibecook/spaghetti-sdk';
import { useViewNav } from './context.js';
import { useApi } from './shell.js';
import { useListNavigation, useTerminalSize } from './hooks.js';
import { formatTokenUsage, formatRelativeTime, formatNumber } from '../lib/format.js';
import { ProjectTabView } from './project-tab-view.js';
import type { ViewEntry } from './types.js';

function projectKey(p: ProjectListItem): string {
  return p.projectId;
}

// ─── ProjectCard ───────────────────────────────────────────────────────

interface ProjectCardProps {
  project: ProjectListItem;
  firstPrompt: string;
  selected: boolean;
  cols: number;
}

function ProjectCard({ project, firstPrompt, selected, cols }: ProjectCardProps): React.ReactElement {
  const p = project;
  const maxWidth = cols - 2; // leave 2 chars margin

  // Truncate helper
  const trunc = (s: string, max: number) => (s.length > max ? s.slice(0, max - 1) + '\u2026' : s);

  // Prefix: ▎ (selected) or space
  const prefix = selected ? '\u258E' : ' ';
  const prefixColor = selected ? 'cyan' : undefined;

  // Line 1: name + branch
  const branchStr = p.latestGitBranch || '';
  const nameMaxLen = maxWidth - 4 - branchStr.length; // "▎ name  branch"
  const displayName = trunc(p.folderName, Math.max(nameMaxLen, 10));

  // Line 2: first prompt (collapse newlines, then truncate)
  const promptFlat = firstPrompt ? firstPrompt.replace(/\n+/g, ' ').replace(/\s+/g, ' ').trim() : '';
  const promptText = promptFlat ? `"${promptFlat}"` : '';
  const truncatedPrompt = trunc(promptText, maxWidth - 4);

  // Line 3: stats (tokens: "—" when the agent has no per-message counts)
  const tok = formatTokenUsage(p.tokenUsage, undefined, p.tokensEstimated);
  const stats = `${formatNumber(p.sessionCount)} sessions \u00B7 ${formatNumber(p.messageCount)} msgs \u00B7 ${tok} tokens \u00B7 ${formatRelativeTime(p.lastActiveAt)}`;
  const truncatedStats = trunc(stats, maxWidth - 4);

  return (
    <Box flexDirection="column" width={cols}>
      <Box width={cols}>
        <Text>
          <Text color={prefixColor}>{prefix}</Text>
          <Text> </Text>
          <Text bold={selected} color="white">
            {displayName}
          </Text>
          <Text> </Text>
          <Text color={selected ? 'cyan' : undefined} dimColor={!selected}>
            {branchStr}
          </Text>
        </Text>
      </Box>
      <Box width={cols}>
        <Text>
          <Text color={prefixColor}>{prefix}</Text>
          <Text> </Text>
          <Text dimColor italic>
            {truncatedPrompt}
          </Text>
        </Text>
      </Box>
      <Box width={cols}>
        <Text>
          <Text color={prefixColor}>{prefix}</Text>
          <Text> </Text>
          {selected ? <Text>{truncatedStats}</Text> : <Text dimColor>{truncatedStats}</Text>}
        </Text>
      </Box>
      <Text> </Text>
    </Box>
  );
}

// ─── ProjectsView ──────────────────────────────────────────────────────

export function ProjectsView(): React.ReactElement {
  const nav = useViewNav();
  const api = useApi();
  const { cols, rows } = useTerminalSize();

  // Load every project across sources, most-recent first.
  const allProjects = useMemo(() => {
    const list = api.getProjectList();
    list.sort((a, b) => new Date(b.lastActiveAt).getTime() - new Date(a.lastActiveAt).getTime());
    return list;
  }, [api]);

  const projects = allProjects;

  // Cache first prompts across renders and tab switches so a project is queried
  // at most once. Populated lazily for only the visible slice below — the old
  // code ran one blocking getSessionList() per project on every mount (N+1).
  const firstPromptCache = useRef<Map<string, string>>(new Map());

  // Viewport = terminal rows - header/footer chrome - the agent tab bar (1 line).
  const chromeLines = 4;
  const viewportHeight = Math.max(5, rows - chromeLines);
  const visibleItems = Math.max(1, Math.floor(viewportHeight / 4));

  const { selectedIndex, scrollOffset, moveUp, moveDown } = useListNavigation({
    itemCount: projects.length,
    itemHeight: 4,
    viewportHeight,
  });

  // First prompts for just the on-screen projects. Recomputed as the user
  // scrolls or switches tabs, but each project's session list is fetched only
  // once (cached), so scrolling stays cheap.
  const firstPrompts = useMemo(() => {
    const cache = firstPromptCache.current;
    const slice = projects.slice(scrollOffset, scrollOffset + visibleItems);
    for (const p of slice) {
      const key = projectKey(p);
      if (!cache.has(key)) {
        const sess = api.getSessionList(p);
        cache.set(key, sess.length > 0 ? sess[0].title || sess[0].firstPrompt || sess[0].summary || '' : '');
      }
    }
    // Return a fresh snapshot so newly-cached prompts trigger a re-render.
    return new Map(cache);
  }, [api, projects, scrollOffset, visibleItems]);

  // Key handling
  useInput(
    (input, key) => {
      if (key.upArrow) {
        moveUp();
      } else if (key.downArrow) {
        moveDown();
      } else if (key.return) {
        if (projects.length === 0) return;
        const project = projects[selectedIndex];
        if (!project) return;
        const entry: ViewEntry = {
          type: 'project-tabs',
          component: () => <ProjectTabView project={project} />,
          breadcrumb: project.folderName,
        };
        (entry as any)._project = project;
        nav.push(entry);
      } else if (key.escape) {
        nav.pop();
      }
    },
    { isActive: !nav.searchMode },
  );

  if (allProjects.length === 0) {
    return (
      <Box flexDirection="column" paddingLeft={2}>
        <Text dimColor>No projects found.</Text>
      </Box>
    );
  }

  // Visible slice — visibleItems is computed above (shared with firstPrompts).
  const visibleProjects = projects.slice(scrollOffset, scrollOffset + visibleItems);

  return (
    <Box flexDirection="column">
      {visibleProjects.map((p, i) => {
        const actualIndex = scrollOffset + i;
        return (
          <ProjectCard
            key={projectKey(p)}
            project={p}
            firstPrompt={firstPrompts.get(projectKey(p)) || ''}
            selected={actualIndex === selectedIndex}
            cols={cols}
          />
        );
      })}
    </Box>
  );
}
