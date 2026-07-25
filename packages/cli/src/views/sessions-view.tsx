/**
 * SessionsView — Scrollable list of sessions for a project
 */

import React, { useMemo } from 'react';
import { Box, Text, useInput, useStdout } from 'ink';
import type { ProjectListItem, SessionListItem } from '@vibecook/spaghetti-sdk';
import { useViewNav } from './context.js';
import { useApi } from './shell.js';
import { useListNavigation } from './hooks.js';
import { formatTokenUsage, formatRelativeTime, formatNumber, formatDuration } from '../lib/format.js';
import { SessionTabView } from './session-tab-view.js';
import type { ViewEntry } from './types.js';

// ─── SessionCard ───────────────────────────────────────────────────────

interface SessionCardProps {
  session: SessionListItem;
  index: number;
  selected: boolean;
  cols: number;
}

function SessionCard({ session, index, selected, cols }: SessionCardProps): React.ReactElement {
  const s = session;
  const dot = ' \u00B7 ';
  const prefix = selected ? '\x1b[33m\u258E\x1b[0m' : ' '; // yellow ▎ or space

  // Line 1: #index branch ... short ID
  const num = selected ? `\x1b[1m\x1b[37m#${index + 1}\x1b[0m` : `\x1b[37m#${index + 1}\x1b[0m`;
  const branch = s.gitBranch ? (selected ? `\x1b[33m${s.gitBranch}\x1b[0m` : `\x1b[2m${s.gitBranch}\x1b[0m`) : '';
  const shortId = `\x1b[2m${s.sourceId}:${s.sessionId.slice(0, 8)}\x1b[0m`;

  // Right-align the short ID
  const leftVisLen = `  #${index + 1}  ${s.gitBranch || ''}`.length + 2;
  const rightLen = s.sourceId.length + 9;
  const gap = Math.max(1, cols - leftVisLen - rightLen - 2);

  // Line 2: first prompt
  const preview = s.title || s.firstPrompt || s.summary;
  const promptFlat = preview ? preview.replace(/\n+/g, ' ').replace(/\s+/g, ' ').trim() : '';
  const promptText = promptFlat ? `"${promptFlat}"` : '';
  const maxPromptLen = Math.max(cols - 6, 20);
  const truncatedPrompt =
    promptText.length > maxPromptLen ? promptText.slice(0, maxPromptLen - 1) + '\u2026' : promptText;

  // Line 3: stats
  const msgCount = selected
    ? `\x1b[37m${formatNumber(s.messageCount)}\x1b[0m`
    : `\x1b[2m${formatNumber(s.messageCount)}\x1b[0m`;
  const tokLabel = formatTokenUsage(s.tokenUsage, s.sourceId, s.tokensEstimated);
  const tokenCount = selected ? `\x1b[33m${tokLabel}\x1b[0m` : `\x1b[2m${tokLabel}\x1b[0m`;
  const duration = selected
    ? `\x1b[37m${formatDuration(s.lifespanMs)}\x1b[0m`
    : `\x1b[2m${formatDuration(s.lifespanMs)}\x1b[0m`;
  const timeStr = `\x1b[2m${formatRelativeTime(s.lastUpdate)}\x1b[0m`;

  return (
    <Box flexDirection="column">
      <Text>{`${prefix} ${num}  ${branch}${' '.repeat(gap)}${shortId}`}</Text>
      <Text>
        {prefix}{' '}
        <Text dimColor italic>
          {truncatedPrompt}
        </Text>
      </Text>
      <Text>{`${prefix} ${msgCount}\x1b[2m msgs${dot}${tokenCount}\x1b[2m tokens${dot}${duration}${dot}${timeStr}`}</Text>
      <Text> </Text>
    </Box>
  );
}

// ─── SessionsView ──────────────────────────────────────────────────────

export interface SessionsViewProps {
  project: ProjectListItem;
  /** Optional initial selection index (e.g. from search navigation) */
  initialIndex?: number;
}

export function SessionsView({ project, initialIndex }: SessionsViewProps): React.ReactElement {
  const nav = useViewNav();
  const api = useApi();
  const { stdout } = useStdout();
  const cols = stdout?.columns ?? 80;

  const sessions = useMemo(() => {
    return api.getSessionList(project);
  }, [api, project]);

  const { selectedIndex, scrollOffset, moveUp, moveDown } = useListNavigation({
    itemCount: sessions.length,
    itemHeight: 4,
    initialIndex: initialIndex != null ? Math.min(initialIndex, Math.max(0, sessions.length - 1)) : 0,
  });

  useInput(
    (input, key) => {
      if (key.upArrow) {
        moveUp();
      } else if (key.downArrow) {
        moveDown();
      } else if (key.return) {
        if (sessions.length === 0) return;
        const session = sessions[selectedIndex];
        const entry: ViewEntry = {
          type: 'session-tabs',
          component: () => <SessionTabView project={project} session={session} sessionIndex={selectedIndex} />,
          breadcrumb: `#${selectedIndex + 1}`,
        };
        (entry as any)._project = project;
        (entry as any)._session = session;
        (entry as any)._sessionIndex = selectedIndex;
        nav.push(entry);
      } else if (key.escape) {
        nav.pop();
      }
    },
    { isActive: !nav.searchMode },
  );

  if (sessions.length === 0) {
    return (
      <Box flexDirection="column" paddingLeft={2}>
        <Text dimColor>No sessions found.</Text>
      </Box>
    );
  }

  const termRows = stdout?.rows ?? 24;
  const viewportItems = Math.max(1, Math.floor((termRows - 6) / 4));
  const visibleSessions = sessions.slice(scrollOffset, scrollOffset + viewportItems);

  return (
    <Box flexDirection="column">
      {visibleSessions.map((s, i) => {
        const actualIndex = scrollOffset + i;
        return (
          <SessionCard
            key={`${s.sourceId}:${s.sessionId}`}
            session={s}
            index={actualIndex}
            selected={actualIndex === selectedIndex}
            cols={cols}
          />
        );
      })}
    </Box>
  );
}
