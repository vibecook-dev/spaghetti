/**
 * Sessions command — list the session catalog for one project.
 *
 * Both the project resolution and the row set come from the catalog, so this
 * works during background ingestion. Decoded per-session statistics are merged
 * in only once history has converged; until then a row shows what the native
 * surface claims and how far decoding has got.
 */

import type { SessionListItem } from '@vibecook/spaghetti-sdk';
import type { ObservationService, SpaghettiCatalogSession } from '@vibecook/spaghetti-sdk/observation';
import { theme } from '../lib/color.js';
import { formatTokens, formatTokenUsage, formatDuration, formatNumber, totalTokens } from '../lib/format.js';
import { sourceReportsPerMessageTokens } from '@vibecook/spaghetti-sdk';
import {
  allCatalogProjects,
  allCatalogSessions,
  catalogProjectName,
  catalogStateLabel,
  decodedStatsAvailable,
  indexingNotice,
  readinessField,
  resolveCatalogProject,
  suggestCatalogProjects,
} from '../lib/catalog.js';
import { renderTable } from '../lib/table.js';
import type { Column } from '../lib/table.js';
import { noProjectMatch } from '../lib/error.js';
import { resolveLimit } from '../lib/limit.js';

export interface SessionsOptions {
  sort?: string;
  limit?: number;
  all?: boolean;
  since?: string;
  json?: boolean;
}

type SortKey = 'recent' | 'tokens' | 'messages' | 'duration';

/** One display row: a catalog session plus decoded stats when they exist. */
interface SessionRow {
  catalog: SpaghettiCatalogSession;
  stats?: SessionListItem;
}

/** Decoded tokens for a row, or zero when history has not reached it. */
function statTokens(stats: SessionListItem | undefined): number {
  return stats ? totalTokens(stats.tokenUsage) : 0;
}

/** Best available activity time: native first, then decoded. */
function activityAt(row: SessionRow): string {
  return row.catalog.nativeUpdatedAt ?? row.catalog.nativeCreatedAt ?? row.stats?.lastUpdate ?? '';
}

/** Messages the row can actually prove, preferring what the agent claims. */
function messageCount(row: SessionRow): number | null {
  if (row.stats) return row.stats.messageCount;
  if (row.catalog.decodedMessageCount > 0) return row.catalog.decodedMessageCount;
  return row.catalog.nativeMessageCount ?? null;
}

function sortRows(rows: SessionRow[], key: SortKey): SessionRow[] {
  const sorted = [...rows];
  switch (key) {
    case 'recent':
      return sorted.sort((a, b) => activityAt(b).localeCompare(activityAt(a)));
    case 'tokens':
      return sorted.sort((a, b) => statTokens(b.stats) - statTokens(a.stats));
    case 'messages':
      return sorted.sort((a, b) => (messageCount(b) ?? 0) - (messageCount(a) ?? 0));
    case 'duration':
      return sorted.sort((a, b) => (b.stats?.lifespanMs ?? 0) - (a.stats?.lifespanMs ?? 0));
    default:
      return sorted;
  }
}

function parseSince(since: string): Date | null {
  const lower = since.toLowerCase().trim();

  if (lower === 'today') {
    const d = new Date();
    d.setHours(0, 0, 0, 0);
    return d;
  }

  if (lower === 'yesterday') {
    const d = new Date();
    d.setDate(d.getDate() - 1);
    d.setHours(0, 0, 0, 0);
    return d;
  }

  // "N days ago" / "N hours ago"
  const agoMatch = lower.match(/^(\d+)\s*(day|days|hour|hours|h|d)\s*ago$/);
  if (agoMatch) {
    const n = parseInt(agoMatch[1]!, 10);
    const unit = agoMatch[2]!;
    const d = new Date();
    if (unit.startsWith('h')) {
      d.setHours(d.getHours() - n);
    } else {
      d.setDate(d.getDate() - n);
    }
    return d;
  }

  // "this week"
  if (lower === 'this week') {
    const d = new Date();
    const dayOfWeek = d.getDay();
    d.setDate(d.getDate() - dayOfWeek);
    d.setHours(0, 0, 0, 0);
    return d;
  }

  // Try ISO date parse
  const parsed = new Date(since);
  if (!isNaN(parsed.getTime())) return parsed;

  return null;
}

export async function sessionsCommand(
  api: ObservationService,
  projectInput: string | undefined,
  opts: SessionsOptions,
): Promise<void> {
  const [readiness, catalogProjects] = await Promise.all([api.getReadiness(), allCatalogProjects(api)]);

  const input = projectInput ?? '.';
  const project = resolveCatalogProject(input, catalogProjects);
  if (!project) {
    throw noProjectMatch(input, suggestCatalogProjects(input, catalogProjects));
  }

  const catalogSessions = await allCatalogSessions(api, project.projectId);

  // Decoded statistics need a resolved history project, which only exists once
  // that project's transcripts have been decoded.
  const stats = new Map<string, SessionListItem>();
  if (decodedStatsAvailable(readiness)) {
    const decoded = await api
      .getSessionList({
        projectId: project.projectId,
        members: [{ sourceId: project.adapterId, slug: project.nativeProjectKey }],
      })
      .catch(() => [] as SessionListItem[]);
    for (const session of decoded) stats.set(session.sessionId, session);
  }

  let rows: SessionRow[] = catalogSessions.map((session) => ({
    catalog: session,
    stats: session.nativeSessionId ? stats.get(session.nativeSessionId) : undefined,
  }));
  const totalSessions = rows.length;

  if (opts.since) {
    const sinceDate = parseSince(opts.since);
    if (sinceDate) {
      const sinceMs = sinceDate.getTime();
      rows = rows.filter((row) => {
        const at = activityAt(row);
        return at !== '' && new Date(at).getTime() >= sinceMs;
      });
    } else {
      process.stderr.write(theme.warning(`\n  Could not parse time: "${opts.since}"\n\n`));
    }
  }

  rows = sortRows(rows, (opts.sort ?? 'recent') as SortKey);
  if (!opts.all) rows = rows.slice(0, resolveLimit(opts.limit, 20));

  if (opts.json) {
    process.stdout.write(
      JSON.stringify(
        {
          readiness,
          atCommitSeq: readiness.atCommitSeq,
          project,
          sessions: rows.map((row) => ({ ...row.catalog, stats: row.stats ?? null })),
        },
        null,
        2,
      ) + '\n',
    );
    return;
  }

  const projectName = catalogProjectName(project);
  if (rows.length === 0) {
    process.stdout.write('\n  ' + theme.project(projectName) + '\n' + theme.muted('  No sessions found.\n\n'));
    return;
  }

  const header = `  ${theme.project(projectName)} ${theme.agent(project.adapterId)} ${theme.muted(`(${totalSessions} sessions)`)}`;

  const columns: Column[] = [
    { key: '_index', label: '#', width: 4, align: 'right', format: (v: any) => theme.muted(String(v)) },
    { key: '_agent', label: 'Agent', width: 8, format: (v: any) => theme.agent(String(v)) },
    { key: '_state', label: 'State', width: 12, format: (v: any) => String(v) },
    {
      key: '_branch',
      label: 'Branch',
      format: (v: any) => (v ? theme.accent(String(v)) : theme.muted('-')),
    },
    {
      key: '_messages',
      label: 'Msgs',
      width: 6,
      align: 'right',
      format: (v: any) => (v === null ? theme.muted('—') : formatNumber(Number(v))),
    },
    { key: '_tokens', label: 'Tokens', width: 9, align: 'right', format: (v: any) => theme.tokens(String(v)) },
    {
      key: '_duration',
      label: 'Duration',
      width: 10,
      align: 'right',
      format: (v: any) => (v === null ? theme.muted('—') : theme.muted(formatDuration(Number(v)))),
    },
    { key: '_summary', label: 'Summary', format: (v: any) => String(v || '') },
  ];

  const tableRows = rows.map((row, index) => ({
    _index: index + 1,
    _agent: row.catalog.adapterId,
    _state: row.catalog.degraded ? theme.error('degraded') : catalogStateLabel(row.catalog.catalogState),
    _branch: row.stats?.gitBranch ?? '',
    _messages: messageCount(row),
    _tokens: row.stats ? formatTokenUsage(row.stats.tokenUsage, row.stats.sourceId, row.stats.tokensEstimated) : '—',
    _duration: row.stats?.lifespanMs ?? null,
    _summary: row.catalog.title ?? row.stats?.title ?? row.stats?.summary ?? '',
  }));

  const table = renderTable(tableRows, columns);

  let totalTok = 0;
  let totalMsgs = 0;
  const tokensKnown = sourceReportsPerMessageTokens(project.adapterId);
  for (const row of rows) {
    if (tokensKnown && row.stats) totalTok += totalTokens(row.stats.tokenUsage);
    totalMsgs += messageCount(row) ?? 0;
  }

  const showing = rows.length < totalSessions ? `showing ${rows.length}/${totalSessions}` : `${rows.length} sessions`;
  const tokFooter = tokensKnown ? `${formatTokens(totalTok)} tokens` : 'tokens n/a';
  const lines = [
    theme.muted(`  ${showing} · ${formatNumber(totalMsgs)} messages · ${tokFooter}`),
    `  ${readinessField('catalog', readiness.catalog)}`,
  ];
  const notice = indexingNotice(readiness);
  if (notice) lines.push(`  ${notice}`);

  process.stdout.write('\n' + header + '\n\n' + table + '\n\n' + lines.join('\n') + '\n\n');
}
