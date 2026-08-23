/**
 * Stats command — usage statistics overview
 */

import type { ObservationService } from '@vibecook/spaghetti-sdk/observation';
import { theme } from '../lib/color.js';
import { formatTokens, formatBytes, formatNumber, formatBar, totalTokens } from '../lib/format.js';

export interface StatsOptions {
  json?: boolean;
}

export async function statsCommand(api: ObservationService, opts: StatsOptions): Promise<void> {
  const [storeStats, projects] = await Promise.all([api.getStats(), api.getProjectList()]);

  // Aggregate totals. These are response-level: one agent response
  // contributes once no matter how many native rows revised its counters.
  let totalSessions = 0;
  let totalMessages = 0;
  let totalInput = 0;
  let totalOutput = 0;
  let totalCacheCreation = 0;
  let totalCacheRead = 0;
  let qualifiedProjects = 0;

  for (const p of projects) {
    totalSessions += p.sessionCount;
    totalMessages += p.messageCount;
    totalInput += p.tokenUsage.inputTokens;
    totalOutput += p.tokenUsage.outputTokens;
    totalCacheCreation += p.tokenUsage.cacheCreationTokens;
    totalCacheRead += p.tokenUsage.cacheReadTokens;
    if (p.tokensEstimated) qualifiedProjects += 1;
  }
  const tokenQuality =
    qualifiedProjects === 0 ? 'exact' : qualifiedProjects === projects.length ? 'estimated' : 'mixed';

  // JSON output
  if (opts.json) {
    const data = {
      overview: {
        projects: projects.length,
        sessions: totalSessions,
        messages: totalMessages,
      },
      store: {
        dbSizeBytes: storeStats.dbSizeBytes,
        searchIndexed: storeStats.searchIndexed,
        totalFingerprints: storeStats.totalFingerprints,
        totalSegments: storeStats.totalSegments,
        segmentsByType: storeStats.segmentsByType,
      },
      tokens: {
        input: totalInput,
        output: totalOutput,
        cacheCreation: totalCacheCreation,
        cacheRead: totalCacheRead,
        total: totalInput + totalOutput + totalCacheCreation + totalCacheRead,
        /** `exact` only when every bucket of every response was exact. */
        quality: tokenQuality,
        projectsWithQualifiedTokens: qualifiedProjects,
      },
      topProjects: projects
        .map((p: any) => ({
          name: p.folderName,
          tokens: totalTokens(p.tokenUsage),
          sessions: p.sessionCount,
          messages: p.messageCount,
        }))
        .sort((a: any, b: any) => b.tokens - a.tokens)
        .slice(0, 5),
    };
    process.stdout.write(JSON.stringify(data, null, 2) + '\n');
    return;
  }

  const lines: string[] = [];

  // Header
  lines.push('');
  lines.push(`  ${theme.heading('Spaghetti Stats')}`);
  lines.push('');

  // Overview section
  lines.push(`  ${theme.label('Overview')}`);
  lines.push(`    Projects:     ${theme.value(formatNumber(projects.length))}`);
  lines.push(`    Sessions:     ${theme.value(formatNumber(totalSessions))}`);
  lines.push(`    Messages:     ${theme.value(formatNumber(totalMessages))}`);
  lines.push('');

  // Store section
  lines.push(`  ${theme.label('Data Store')}`);
  lines.push(`    DB size:      ${theme.value(formatBytes(storeStats.dbSizeBytes))}`);
  lines.push(`    Search index: ${theme.value(formatNumber(storeStats.searchIndexed))} entries`);
  lines.push(`    Source files:  ${theme.value(formatNumber(storeStats.totalFingerprints))} tracked`);
  lines.push('');

  // Token usage section
  const allTokens = totalInput + totalOutput + totalCacheCreation + totalCacheRead;
  lines.push(`  ${theme.label('Token Usage')}`);
  lines.push(`    Input:          ${theme.tokens(formatTokens(totalInput))}`);
  lines.push(`    Output:         ${theme.tokens(formatTokens(totalOutput))}`);
  lines.push(`    Cache creation: ${theme.tokens(formatTokens(totalCacheCreation))}`);
  lines.push(`    Cache read:     ${theme.tokens(formatTokens(totalCacheRead))}`);
  lines.push(`    ${theme.muted('─'.repeat(30))}`);
  lines.push(`    Total:          ${theme.tokens(formatTokens(allTokens))}`);
  lines.push(
    `    Quality:        ${theme.value(tokenQuality)}${
      qualifiedProjects > 0
        ? theme.muted(
            ` (${formatNumber(qualifiedProjects)} of ${formatNumber(projects.length)} projects not fully exact)`,
          )
        : ''
    }`,
  );
  lines.push('');

  // Top projects by tokens
  const sorted = projects
    .map((p: any) => ({ name: p.folderName, tokens: totalTokens(p.tokenUsage) }))
    .sort((a: any, b: any) => b.tokens - a.tokens)
    .slice(0, 5);

  if (sorted.length > 0) {
    const maxTokens = sorted[0]!.tokens;
    lines.push(`  ${theme.label('Top Projects by Tokens')}`);
    for (const item of sorted) {
      const bar = formatBar(item.tokens, maxTokens, 20);
      const nameStr = item.name.padEnd(20);
      lines.push(`    ${nameStr} ${bar} ${theme.tokens(formatTokens(item.tokens))}`);
    }
    lines.push('');
  }

  process.stdout.write(lines.join('\n') + '\n');
}
