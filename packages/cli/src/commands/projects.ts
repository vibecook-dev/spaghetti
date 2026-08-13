/**
 * Projects command — list all projects with usage stats
 */

import type { ProjectListItem } from '@vibecook/spaghetti-sdk';
import type { ObservationService } from '@vibecook/spaghetti-sdk/observation';
import { theme } from '../lib/color.js';
import { formatTokens, formatTokenUsage, formatRelativeTime, formatNumber, totalTokens } from '../lib/format.js';
import { sourceReportsPerMessageTokens } from '@vibecook/spaghetti-sdk';
import { renderTable } from '../lib/table.js';
import type { Column } from '../lib/table.js';

export interface ProjectsOptions {
  sort?: string;
  limit?: number;
  json?: boolean;
}

type SortKey = 'active' | 'sessions' | 'messages' | 'tokens' | 'name';

function sortProjects(projects: ProjectListItem[], key: SortKey): ProjectListItem[] {
  const sorted = [...projects];
  switch (key) {
    case 'active':
      return sorted.sort(
        (a: ProjectListItem, b: ProjectListItem) =>
          new Date(b.lastActiveAt).getTime() - new Date(a.lastActiveAt).getTime(),
      );
    case 'sessions':
      return sorted.sort((a: ProjectListItem, b: ProjectListItem) => b.sessionCount - a.sessionCount);
    case 'messages':
      return sorted.sort((a: ProjectListItem, b: ProjectListItem) => b.messageCount - a.messageCount);
    case 'tokens':
      return sorted.sort(
        (a: ProjectListItem, b: ProjectListItem) => totalTokens(b.tokenUsage) - totalTokens(a.tokenUsage),
      );
    case 'name':
      return sorted.sort((a: ProjectListItem, b: ProjectListItem) => a.folderName.localeCompare(b.folderName));
    default:
      return sorted;
  }
}

export async function projectsCommand(api: ObservationService, opts: ProjectsOptions): Promise<void> {
  let projects = await api.getProjectList();

  // Sort
  const sortKey = (opts.sort ?? 'active') as SortKey;
  projects = sortProjects(projects, sortKey);

  // Limit
  if (opts.limit && opts.limit > 0) {
    projects = projects.slice(0, opts.limit);
  }

  // JSON output
  if (opts.json) {
    process.stdout.write(JSON.stringify(projects, null, 2) + '\n');
    return;
  }

  if (projects.length === 0) {
    process.stdout.write(theme.muted('\n  No projects found.\n\n'));
    return;
  }

  const columns: Column[] = [
    {
      key: '_index',
      label: '#',
      width: 4,
      align: 'right',
      format: (v: any) => theme.muted(String(v)),
    },
    {
      key: 'folderName',
      label: 'Project',
      format: (v: any) => theme.project(String(v)),
    },
    {
      key: '_agents',
      label: 'Agents',
      width: 20,
      format: (v: any) => theme.agent(String(v)),
    },
    {
      key: 'sessionCount',
      label: 'Sessions',
      width: 10,
      align: 'right',
      format: (v: any) => formatNumber(Number(v)),
    },
    {
      key: 'messageCount',
      label: 'Messages',
      width: 10,
      align: 'right',
      format: (v: any) => formatNumber(Number(v)),
    },
    {
      key: '_tokens',
      label: 'Tokens',
      width: 10,
      align: 'right',
      format: (v: any) => theme.tokens(String(v)),
    },
    {
      key: 'lastActiveAt',
      label: 'Last Active',
      width: 12,
      align: 'right',
      format: (v: any) => theme.time(formatRelativeTime(String(v))),
    },
  ];

  // Add index + pre-formatted tokens (Codex shows "—")
  const rows = projects.map((p: ProjectListItem, i: number) => ({
    ...p,
    _index: i + 1,
    _agents: p.sourceIds.join(', '),
    _tokens: formatTokenUsage(p.tokenUsage, undefined, p.tokensEstimated),
  }));

  const table = renderTable(rows, columns);

  // Summary footer — only sum tokens from sources that report them
  let totalSessions = 0;
  let totalMessages = 0;
  let totalTok = 0;
  let anyTokenSource = false;
  for (const p of projects) {
    totalSessions += p.sessionCount;
    totalMessages += p.messageCount;
    if (p.sourceIds.some(sourceReportsPerMessageTokens)) {
      anyTokenSource = true;
      totalTok += totalTokens(p.tokenUsage);
    }
  }

  const tokFooter = anyTokenSource ? `${formatTokens(totalTok)} tokens` : 'tokens n/a';
  const footer = theme.muted(
    `  ${projects.length} projects \u00b7 ${formatNumber(totalSessions)} sessions \u00b7 ${formatNumber(totalMessages)} messages \u00b7 ${tokFooter}`,
  );

  process.stdout.write('\n' + table + '\n\n' + footer + '\n\n');
}
