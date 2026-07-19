/**
 * ProjectTabView — Tab container showing Sessions + Memory for a project
 *
 * Wraps SessionsView (tab 0) and inline memory content (tab 1).
 * Left/Right arrows switch tabs. Esc pops back to the project list.
 */

import React, { useState, useMemo } from 'react';
import { Box, Text, useInput, useStdout } from 'ink';
import type { ProjectListItem } from '@vibecook/spaghetti-sdk';
import { useViewNav } from './context.js';
import { useApi } from './shell.js';
import { HRule } from './chrome.js';
import { TabBar } from './tab-bar.js';
import { SessionsView } from './sessions-view.js';

// ─── Constants ────────────────────────────────────────────────────────

const TABS = ['Sessions', 'Memory'] as const;

// ─── Simple Markdown Rendering ────────────────────────────────────────

function renderMarkdownLines(content: string): React.ReactElement[] {
  return content.split('\n').map((line, i) => {
    const trimmed = line.trimStart();
    if (trimmed.startsWith('#')) {
      const text = trimmed.replace(/^#+\s*/, '');
      return (
        <Text key={i} bold>
          {' '}
          {text}
        </Text>
      );
    }
    return (
      <Text key={i} dimColor={trimmed === ''}>
        {' '}
        {line}
      </Text>
    );
  });
}

// ─── MemoryPanel ──────────────────────────────────────────────────────

interface MemoryPanelProps {
  project: ProjectListItem;
}

function MemoryPanel({ project }: MemoryPanelProps): React.ReactElement {
  const api = useApi();
  const nav = useViewNav();
  const { stdout } = useStdout();
  const termRows = stdout?.rows ?? 24;

  const content = useMemo(
    () =>
      api.getProjectMemory(
        project,
        project.sourceIds.includes('claude-code') ? { sourceId: 'claude-code' } : undefined,
      ),
    [api, project],
  );
  const lines = useMemo(() => (content ? renderMarkdownLines(content) : []), [content]);

  const [scrollOffset, setScrollOffset] = useState(0);
  const viewportHeight = Math.max(termRows - 8, 5); // extra room for tab bar + header
  const maxScroll = Math.max(0, lines.length - viewportHeight);

  useInput(
    (_input, key) => {
      if (key.upArrow) {
        setScrollOffset((prev) => Math.max(0, prev - 1));
      } else if (key.downArrow) {
        setScrollOffset((prev) => Math.min(maxScroll, prev + 1));
      }
    },
    { isActive: !nav.searchMode },
  );

  if (!content) {
    return (
      <Box flexDirection="column" paddingLeft={2}>
        <Text dimColor>No MEMORY.md found for this project.</Text>
      </Box>
    );
  }

  const visibleLines = lines.slice(scrollOffset, scrollOffset + viewportHeight);

  return <Box flexDirection="column">{visibleLines}</Box>;
}

// ─── ProjectTabView ───────────────────────────────────────────────────

export interface ProjectTabViewProps {
  project: ProjectListItem;
}

export function ProjectTabView({ project }: ProjectTabViewProps): React.ReactElement {
  const nav = useViewNav();
  const [activeTab, setActiveTab] = useState(0);

  // Tab switching with left/right arrows
  // Esc: only handled here when Memory tab is active (tab 1).
  // When Sessions tab is active (tab 0), SessionsView's own Esc handler
  // calls nav.pop() — handling it here too would cause a double-pop.
  useInput(
    (_input, key) => {
      if (key.leftArrow) {
        setActiveTab((prev) => Math.max(0, prev - 1));
      } else if (key.rightArrow) {
        setActiveTab((prev) => Math.min(TABS.length - 1, prev + 1));
      } else if (key.escape && activeTab !== 0) {
        nav.pop();
      }
    },
    { isActive: !nav.searchMode },
  );

  return (
    <Box flexDirection="column">
      <TabBar tabs={[...TABS]} activeIndex={activeTab} onTabChange={setActiveTab} breadcrumb={project.folderName} />
      <HRule />
      {activeTab === 0 ? <SessionsView project={project} /> : <MemoryPanel project={project} />}
    </Box>
  );
}
