/**
 * Search command — full-text search across all segments
 */

import type { SpaghettiAPI } from '@vibecook/spaghetti-sdk';
import { theme } from '../lib/color.js';
import { resolveProject, suggestProjects } from '../lib/resolve.js';
import { noProjectMatch } from '../lib/error.js';
import { resolveLimit, resolveOffset } from '../lib/limit.js';
import { outputWithPager } from '../lib/pager.js';
import { getTerminalWidth } from '../lib/terminal.js';
import cliTruncate from 'cli-truncate';

export interface SearchOptions {
  project?: string;
  limit?: number;
  offset?: number;
  json?: boolean;
}

export async function searchCommand(api: SpaghettiAPI, query: string, opts: SearchOptions): Promise<void> {
  // Resolve project scope if provided
  let projectMembers: Array<{ sourceId: string; slug: string }> | undefined;

  if (opts.project) {
    const projects = api.getProjectList();
    const project = resolveProject(opts.project, projects);

    if (!project) {
      throw noProjectMatch(opts.project, suggestProjects(opts.project, projects));
    }

    projectMembers = project.members;
  }

  const limit = resolveLimit(opts.limit, 20);
  const offset = resolveOffset(opts.offset);

  // Execute search
  const results = api.search({
    text: query,
    projectMembers,
    limit,
    offset,
  });

  // JSON output
  if (opts.json) {
    process.stdout.write(JSON.stringify(results, null, 2) + '\n');
    return;
  }

  if (results.results.length === 0) {
    process.stdout.write('\n  ' + theme.muted(`No results for "${query}"`) + '\n\n');
    return;
  }

  const width = getTerminalWidth();
  const lines: string[] = [];

  lines.push('');
  lines.push(`  ${theme.heading('Search:')} ${theme.accent(query)}` + theme.muted(` (${results.total} results)`));
  lines.push('');

  // Group results by project
  const grouped = new Map<string, typeof results.results>();
  for (const r of results.results) {
    const key = r.projectSlug ?? 'unknown';
    const group = grouped.get(key);
    if (group) {
      group.push(r);
    } else {
      grouped.set(key, [r]);
    }
  }

  for (const [slug, group] of grouped) {
    lines.push(`  ${theme.project(slug)} ${theme.muted(`(${group.length} matches)`)}`);
    lines.push('');

    for (const result of group) {
      const snippet = highlightQuery(result.snippet, query, width - 6);
      const sessionRef = result.sessionId ? theme.muted(` [${result.sessionId.slice(0, 8)}]`) : '';
      const typeLabel = theme.muted(`[${result.type}]`);
      lines.push(`    ${typeLabel}${sessionRef}`);
      lines.push(`    ${snippet}`);
      lines.push('');
    }
  }

  // Pagination footer
  const footerParts: string[] = [];
  footerParts.push(`${results.results.length}/${results.total} results`);
  if (results.hasMore) {
    footerParts.push(`use --offset ${offset + limit} for more`);
  }
  lines.push(theme.muted(`  ${footerParts.join(' \u00b7 ')}`));
  lines.push('');

  const output = lines.join('\n');
  outputWithPager(output);
}

/**
 * Highlight the query term in a snippet string.
 */
function highlightQuery(snippet: string, query: string, maxWidth: number): string {
  // Clean up the snippet
  const clean = snippet.replace(/\n/g, ' ').replace(/\s+/g, ' ').trim();
  const truncated = cliTruncate(clean, maxWidth);

  // Case-insensitive highlight
  const lower = truncated.toLowerCase();
  const queryLower = query.toLowerCase();
  const idx = lower.indexOf(queryLower);

  if (idx === -1) return truncated;

  const before = truncated.slice(0, idx);
  const match = truncated.slice(idx, idx + query.length);
  const after = truncated.slice(idx + query.length);

  return before + theme.warning(match) + after;
}
