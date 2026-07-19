/**
 * StatsView — Usage statistics dashboard
 *
 * Scrollable if content exceeds viewport. Esc pops back.
 */

import React, { useState, useMemo } from 'react';
import { Box, Text, useInput, useStdout } from 'ink';
import { useViewNav } from './context.js';
import { useApi } from './shell.js';
import { sourceReportsPerMessageTokens } from '@vibecook/spaghetti-sdk';
import { formatTokens, formatNumber, formatBytes, formatBar, totalTokens } from '../lib/format.js';

export function StatsView(): React.ReactElement {
  const nav = useViewNav();
  const api = useApi();
  const { stdout } = useStdout();
  const termRows = stdout?.rows ?? 24;

  // Gather data
  const { stats, totalTok, topProjects } = useMemo(() => {
    const s = api.getStats();
    const p = api.getProjectList();
    let sessions = 0;
    let messages = 0;
    let inputTokens = 0;
    let outputTokens = 0;
    let cacheRead = 0;
    let cacheWrite = 0;
    for (const proj of p) {
      sessions += proj.sessionCount;
      messages += proj.messageCount;
      // Codex (and any source without per-message tokens) contributes 0s —
      // skip so totals aren't diluted by silent zeros.
      if (!proj.sourceIds.some(sourceReportsPerMessageTokens)) continue;
      inputTokens += proj.tokenUsage.inputTokens;
      outputTokens += proj.tokenUsage.outputTokens;
      cacheRead += proj.tokenUsage.cacheReadTokens;
      cacheWrite += proj.tokenUsage.cacheCreationTokens;
    }
    const total = inputTokens + outputTokens + cacheRead + cacheWrite;

    // Top projects by total tokens, descending (token-reporting sources only)
    const ranked = [...p]
      .filter((proj) => proj.sourceIds.some(sourceReportsPerMessageTokens))
      .map((proj) => ({ name: proj.folderName, tokens: totalTokens(proj.tokenUsage) }))
      .sort((a, b) => b.tokens - a.tokens)
      .slice(0, 5);

    return {
      stats: {
        projectCount: p.length,
        sessions,
        messages,
        dbSize: s.dbSizeBytes,
        inputTokens,
        outputTokens,
        cacheRead,
        cacheWrite,
      },
      projects: p,
      totalTok: total,
      topProjects: ranked,
    };
  }, [api]);

  // Build lines
  const lines: React.ReactElement[] = [];
  const col1 = 18;
  const col2 = 16;

  // Overview section
  lines.push(
    <Text key="h-overview" bold>
      {' '}
      Overview
    </Text>,
  );
  lines.push(
    <Text key="overview-1">
      {'    '}
      {'Projects'.padEnd(col1)}
      {formatNumber(stats.projectCount).padEnd(col2)}
      {'Sessions'.padEnd(col1)}
      {formatNumber(stats.sessions)}
    </Text>,
  );
  lines.push(
    <Text key="overview-2">
      {'    '}
      {'Messages'.padEnd(col1)}
      {formatNumber(stats.messages).padEnd(col2)}
      {'DB size'.padEnd(col1)}
      {formatBytes(stats.dbSize)}
    </Text>,
  );
  lines.push(<Text key="sp1"> </Text>);

  // Token Usage section
  lines.push(
    <Text key="h-tokens" bold>
      {' '}
      Token Usage
    </Text>,
  );
  lines.push(
    <Text key="tok-1">
      {'    '}
      {'Input'.padEnd(col1)}
      {formatTokens(stats.inputTokens).padEnd(col2)}
      {'Cache read'.padEnd(col1)}
      {formatTokens(stats.cacheRead)}
    </Text>,
  );
  lines.push(
    <Text key="tok-2">
      {'    '}
      {'Output'.padEnd(col1)}
      {formatTokens(stats.outputTokens).padEnd(col2)}
      {'Cache write'.padEnd(col1)}
      {formatTokens(stats.cacheWrite)}
    </Text>,
  );
  lines.push(
    <Text key="tok-sep">
      {'    '}
      {''.padEnd(col1)}
      {''.padEnd(col2)}
      {'───────────────────'}
    </Text>,
  );
  lines.push(
    <Text key="tok-total">
      {'    '}
      {''.padEnd(col1)}
      {''.padEnd(col2)}
      {'Total'.padEnd(col1)}
      {formatTokens(totalTok)}
    </Text>,
  );
  lines.push(<Text key="sp2"> </Text>);

  // Top Projects section
  if (topProjects.length > 0) {
    const maxTokenVal = topProjects[0].tokens;
    lines.push(
      <Text key="h-top" bold>
        {' '}
        Top Projects
      </Text>,
    );
    for (const proj of topProjects) {
      const bar = formatBar(proj.tokens, maxTokenVal, 20);
      lines.push(
        <Text key={`proj-${proj.name}`}>
          {'    '}
          {proj.name.padEnd(col1)}
          {bar} {formatTokens(proj.tokens)} tokens
        </Text>,
      );
    }
    lines.push(<Text key="sp3"> </Text>);
  }

  // Scrolling
  const [scrollOffset, setScrollOffset] = useState(0);
  const viewportHeight = Math.max(termRows - 6, 5);
  const maxScroll = Math.max(0, lines.length - viewportHeight);

  useInput(
    (_input, key) => {
      if (nav.searchMode) return;
      if (key.upArrow) {
        setScrollOffset((prev) => Math.max(0, prev - 1));
      } else if (key.downArrow) {
        setScrollOffset((prev) => Math.min(maxScroll, prev + 1));
      } else if (key.escape) {
        nav.pop();
      }
    },
    { isActive: !nav.searchMode },
  );

  const visibleLines = lines.slice(scrollOffset, scrollOffset + viewportHeight);

  return <Box flexDirection="column">{visibleLines}</Box>;
}
