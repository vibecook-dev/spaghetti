/**
 * MenuView — Home screen with welcome panel and 3-item menu
 *
 * Menu items: Projects, Stats, Help
 * Each pushes the corresponding view onto the navigation stack.
 */

import React from 'react';
import { Box, Text, useInput } from 'ink';
import { sourceDisplayName, sourceDisplayRoot, sourceReportsPerMessageTokens } from '@vibecook/spaghetti-sdk';
import { useViewNav } from './context.js';
import { useApi } from './shell.js';
import { useAsyncValue, useListNavigation, useTerminalSize } from './hooks.js';
import { WelcomePanel } from './welcome-panel.js';
import type { WelcomePanelStats } from './welcome-panel.js';
import { ProjectsView } from './projects-view.js';
import { StatsView } from './stats-view.js';
import { HelpView } from './help-view.js';
import { DoctorView } from './doctor-view.js';
import { formatTokens, formatBytes, totalTokens } from '../lib/format.js';
import { defaultClaudeHome, probePluginLeftovers } from '../lib/plugin-leftovers.js';
import type { ViewEntry } from './types.js';

// ─── Menu Item Rendering ──────────────────────────────────────────────

interface MenuItemProps {
  name: string;
  description: string;
  rightStat: string;
  selected: boolean;
  cols: number;
}

function MenuItem({ name, description, rightStat, selected, cols }: MenuItemProps): React.ReactElement {
  const prefix = selected ? '\u258E' : ' ';
  const prefixColor = selected ? 'cyan' : undefined;

  // Calculate gap to right-align the stat
  const fixedChars = 4; // prefix + space + two trailing spaces
  const gap = Math.max(2, cols - fixedChars - name.length - rightStat.length);

  return (
    <Box flexDirection="column">
      <Box>
        <Text color={prefixColor}>{prefix}</Text>
        <Text> </Text>
        <Text bold={selected} color={selected ? 'white' : undefined}>
          {name}
        </Text>
        <Text>{' '.repeat(gap)}</Text>
        <Text dimColor>{rightStat}</Text>
      </Box>
      <Box>
        <Text color={prefixColor}>{prefix}</Text>
        <Text> </Text>
        <Text dimColor>{description}</Text>
      </Box>
      <Text> </Text>
    </Box>
  );
}

// ─── MenuView ─────────────────────────────────────────────────────────

export function MenuView(): React.ReactElement {
  const nav = useViewNav();
  const api = useApi();
  const { cols } = useTerminalSize();

  // Load aggregate data for stats display through the async canonical client.
  const query = useAsyncValue(
    async () => {
      const t0 = performance.now();
      const [projects, storeStats, sourceIds] = await Promise.all([
        api.getProjectList(),
        api.getStats(),
        api.getSourceIds(),
      ]);
      const elapsed = Math.round(performance.now() - t0);

      let sessions = 0;
      let messages = 0;
      let tokens = 0;
      let anyTokenSource = false;
      for (const p of projects) {
        sessions += p.sessionCount;
        messages += p.messageCount;
        if (p.sourceIds.some(sourceReportsPerMessageTokens)) {
          anyTokenSource = true;
          tokens += totalTokens(p.tokenUsage);
        }
      }

      const stats: WelcomePanelStats = {
        projects: projects.length,
        sessions,
        messages,
        tokens: anyTokenSource || projects.length === 0 ? formatTokens(tokens) : '—',
      };

      return {
        panelStats: stats,
        dataSize: formatBytes(storeStats.dbSizeBytes),
        queryMs: elapsed,
        projectCount: projects.length,
        tokenStr: anyTokenSource || projects.length === 0 ? formatTokens(tokens) : '—',
        sourceIds,
      };
    },
    [api],
    {
      panelStats: { projects: 0, sessions: 0, messages: 0, tokens: '—' },
      dataSize: '—',
      queryMs: 0,
      projectCount: 0,
      tokenStr: '—',
      sourceIds: [] as string[],
    },
  );
  const { panelStats, dataSize, queryMs, projectCount, tokenStr, sourceIds } = query.value;
  const loadError = query.error?.message ?? null;

  // This is a read-only diagnostic for retired plugin state, unrelated to
  // source observation or projection ownership.
  const leftovers = probePluginLeftovers({ claudeHome: defaultClaudeHome() });
  const pluginStatStr = leftovers.clean
    ? 'no leftovers'
    : leftovers.unknowns.length > 0
      ? 'state unknown'
      : 'leftovers found';

  // Agent-aware labels (RFC 006 multi-source): reflect whichever agents are
  // actually indexed rather than assuming Claude Code.
  const agentIds = sourceIds.length > 0 ? sourceIds : ['claude-code'];
  const projectsDescription =
    agentIds.length > 1
      ? `Browse project conversations across ${agentIds.map(sourceDisplayName).join(' + ')}`
      : `Browse ${sourceDisplayName(agentIds[0])} project conversations`;
  const dataPath = agentIds.map(sourceDisplayRoot).join(' + ');

  // Menu items
  const menuItems = [
    {
      name: 'Projects',
      description: projectsDescription,
      rightStat: `${projectCount} projects`,
    },
    { name: 'Stats', description: 'Usage statistics, token counts, top projects', rightStat: `${tokenStr} tokens` },
    { name: 'Help', description: 'Navigation, commands, and keyboard shortcuts', rightStat: '? keybindings' },
    {
      name: 'Doctor',
      description: 'Health check for spaghetti and its data paths',
      rightStat: pluginStatStr,
    },
  ];

  const { selectedIndex, moveUp, moveDown } = useListNavigation({
    itemCount: menuItems.length,
    itemHeight: 3,
  });

  useInput(
    (input, key) => {
      if (key.upArrow) {
        moveUp();
      } else if (key.downArrow) {
        moveDown();
      } else if (key.return) {
        if (selectedIndex === 0) {
          const entry: ViewEntry = {
            type: 'projects',
            component: ProjectsView,
            breadcrumb: 'Projects',
            hints: agentIds.length > 1 ? '↑↓ navigate  ←→ switch agent  ⏎ open  Esc back' : undefined,
          };
          nav.push(entry);
        } else if (selectedIndex === 1) {
          const entry: ViewEntry = {
            type: 'stats',
            component: StatsView,
            breadcrumb: 'Stats',
          };
          nav.push(entry);
        } else if (selectedIndex === 2) {
          const entry: ViewEntry = {
            type: 'help',
            component: HelpView,
            breadcrumb: 'Help',
          };
          nav.push(entry);
        } else if (selectedIndex === 3) {
          const entry: ViewEntry = {
            type: 'doctor',
            component: DoctorView,
            breadcrumb: 'Doctor',
            hints: 'r refresh  Esc back  q quit',
          };
          nav.push(entry);
        }
      }
    },
    { isActive: !nav.searchMode },
  );

  return (
    <Box flexDirection="column">
      <WelcomePanel stats={panelStats} dataPath={dataPath} dataSize={dataSize} initMs={queryMs} />
      {loadError ? (
        <Box flexDirection="column" marginBottom={1}>
          <Text color="red"> ✗ {loadError}</Text>
          <Text dimColor> Delete ~/.spaghetti/cache/spaghetti-rs.db and re-run spag to rebuild.</Text>
        </Box>
      ) : null}
      {menuItems.map((item, i) => (
        <MenuItem
          key={item.name}
          name={item.name}
          description={item.description}
          rightStat={item.rightStat}
          selected={i === selectedIndex}
          cols={cols}
        />
      ))}
    </Box>
  );
}
