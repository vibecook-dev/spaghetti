/**
 * Projects command — list the project catalog.
 *
 * The row set comes from the catalog, so this answers seconds after the engine
 * opens and keeps working while history, usage, and search converge in the
 * background. Decoded statistics are merged in only once history has actually
 * converged; until then the table shows what exists and says what is missing.
 */

import type { ProjectListItem } from '@vibecook/spaghetti-sdk';
import type { ObservationService, SpaghettiCatalogProject } from '@vibecook/spaghetti-sdk/observation';
import { theme } from '../lib/color.js';
import { formatTokens, formatTokenUsage, formatRelativeTime, formatNumber, totalTokens } from '../lib/format.js';
import { sourceReportsPerMessageTokens } from '@vibecook/spaghetti-sdk';
import {
  allCatalogProjects,
  catalogStateLabel,
  decodedStatsAvailable,
  indexingNotice,
  readinessField,
} from '../lib/catalog.js';
import { renderTable } from '../lib/table.js';
import type { Column } from '../lib/table.js';

export interface ProjectsOptions {
  sort?: string;
  limit?: number;
  json?: boolean;
}

type SortKey = 'active' | 'sessions' | 'messages' | 'tokens' | 'name';

/** One display row: a catalog project plus decoded stats when they exist. */
interface ProjectRow {
  catalog: SpaghettiCatalogProject;
  stats?: ProjectListItem;
}

/** Decoded tokens for a row, or zero when history has not reached it. */
function statTokens(stats: ProjectListItem | undefined): number {
  return stats ? totalTokens(stats.tokenUsage) : 0;
}

function displayName(row: ProjectRow): string {
  return row.stats?.folderName ?? row.catalog.displayPath?.split('/').pop() ?? row.catalog.nativeProjectKey;
}

function sortRows(rows: ProjectRow[], key: SortKey): ProjectRow[] {
  const sorted = [...rows];
  switch (key) {
    case 'active':
      return sorted.sort((a, b) => (b.catalog.latestActivityAt ?? '').localeCompare(a.catalog.latestActivityAt ?? ''));
    case 'sessions':
      return sorted.sort((a, b) => b.catalog.sessionCount - a.catalog.sessionCount);
    case 'messages':
      return sorted.sort((a, b) => (b.stats?.messageCount ?? 0) - (a.stats?.messageCount ?? 0));
    case 'tokens':
      return sorted.sort((a, b) => statTokens(b.stats) - statTokens(a.stats));
    case 'name':
      return sorted.sort((a, b) => displayName(a).localeCompare(displayName(b)));
    default:
      return sorted;
  }
}

/**
 * Index decoded project statistics by `<adapter>:<native key>`. One display
 * project may merge several native members, so every member is indexed.
 */
function indexStats(projects: ProjectListItem[]): Map<string, ProjectListItem> {
  const byMember = new Map<string, ProjectListItem>();
  for (const project of projects) {
    for (const member of project.members) {
      byMember.set(`${member.sourceId}:${member.slug}`, project);
    }
  }
  return byMember;
}

export async function projectsCommand(api: ObservationService, opts: ProjectsOptions): Promise<void> {
  const [readiness, catalog] = await Promise.all([api.getReadiness(), allCatalogProjects(api)]);
  const stats = decodedStatsAvailable(readiness)
    ? indexStats(await api.getProjectList())
    : new Map<string, ProjectListItem>();

  let rows: ProjectRow[] = catalog.map((project) => ({
    catalog: project,
    stats: stats.get(`${project.adapterId}:${project.nativeProjectKey}`),
  }));

  rows = sortRows(rows, (opts.sort ?? 'active') as SortKey);
  if (opts.limit && opts.limit > 0) rows = rows.slice(0, opts.limit);

  if (opts.json) {
    process.stdout.write(
      JSON.stringify(
        {
          readiness,
          atCommitSeq: readiness.atCommitSeq,
          projects: rows.map((row) => ({ ...row.catalog, stats: row.stats ?? null })),
        },
        null,
        2,
      ) + '\n',
    );
    return;
  }

  if (rows.length === 0) {
    const reason = readiness.catalog.state === 'pending' ? 'No source has been scanned yet.' : 'No projects found.';
    process.stdout.write(theme.muted(`\n  ${reason}\n\n`));
    return;
  }

  const columns: Column[] = [
    { key: '_index', label: '#', width: 4, align: 'right', format: (v: any) => theme.muted(String(v)) },
    { key: '_name', label: 'Project', format: (v: any) => theme.project(String(v)) },
    { key: '_agents', label: 'Agents', width: 14, format: (v: any) => theme.agent(String(v)) },
    {
      key: '_sessions',
      label: 'Sessions',
      width: 9,
      align: 'right',
      format: (v: any) => formatNumber(Number(v)),
    },
    { key: '_state', label: 'State', width: 12, format: (v: any) => String(v) },
    {
      key: '_messages',
      label: 'Messages',
      width: 9,
      align: 'right',
      format: (v: any) => (v === null ? theme.muted('—') : formatNumber(Number(v))),
    },
    { key: '_tokens', label: 'Tokens', width: 10, align: 'right', format: (v: any) => theme.tokens(String(v)) },
    {
      key: '_lastActive',
      label: 'Last Active',
      width: 12,
      align: 'right',
      format: (v: any) => (v ? theme.time(formatRelativeTime(String(v))) : theme.muted('—')),
    },
  ];

  const tableRows = rows.map((row, index) => ({
    _index: index + 1,
    _name: displayName(row),
    _agents: row.catalog.adapterId,
    _sessions: row.catalog.sessionCount,
    _state: row.catalog.degraded ? theme.error('degraded') : catalogStateLabel(row.catalog.catalogState),
    _messages: row.stats?.messageCount ?? null,
    _tokens: row.stats ? formatTokenUsage(row.stats.tokenUsage, undefined, row.stats.tokensEstimated) : '—',
    _lastActive: row.catalog.latestActivityAt ?? '',
  }));

  const table = renderTable(tableRows, columns);

  let totalSessions = 0;
  let totalMessages = 0;
  let totalTok = 0;
  let anyTokenSource = false;
  for (const row of rows) {
    totalSessions += row.catalog.sessionCount;
    totalMessages += row.stats?.messageCount ?? 0;
    if (row.stats?.sourceIds.some(sourceReportsPerMessageTokens)) {
      anyTokenSource = true;
      totalTok += totalTokens(row.stats.tokenUsage);
    }
  }

  const tokFooter = anyTokenSource ? `${formatTokens(totalTok)} tokens` : 'tokens n/a';
  const lines = [
    theme.muted(
      `  ${rows.length} projects · ${formatNumber(totalSessions)} sessions · ${formatNumber(totalMessages)} messages · ${tokFooter}`,
    ),
    `  ${readinessField('catalog', readiness.catalog)}`,
  ];
  const notice = indexingNotice(readiness);
  if (notice) lines.push(`  ${notice}`);

  process.stdout.write('\n' + table + '\n\n' + lines.join('\n') + '\n\n');
}
