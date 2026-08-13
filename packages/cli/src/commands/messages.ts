/**
 * Messages command — read messages from a session
 */

import type { ObservationService } from '@vibecook/spaghetti-sdk/observation';
import { theme } from '../lib/color.js';
import { formatTokenUsage, formatRelativeTime, formatDuration, formatNumber } from '../lib/format.js';
import { resolveProject, resolveSession, suggestProjects } from '../lib/resolve.js';
import { UserError, noProjectMatch, noSessionMatch } from '../lib/error.js';
import { resolveLimit, resolveOffset, resolveOptionalCount } from '../lib/limit.js';
import { renderMessages, filterDisplayableMessages } from '../lib/message-render.js';
import { outputWithPager } from '../lib/pager.js';
import { getTerminalWidth } from '../lib/terminal.js';

export interface MessagesOptions {
  limit?: number;
  offset?: number;
  last?: number;
  compact?: boolean;
  noTools?: boolean;
  noThinking?: boolean;
  raw?: boolean;
  json?: boolean;
}

/**
 * Whether messages exist beyond the ones displayed — drives the footer's
 * "more available" hint. Exported for testing.
 *
 * For the head view (default / --offset), more exist when the raw pages had
 * more (`rawHasMore`) or over-fetched filtered messages were trimmed
 * (`trimmedMore`). For the `--last` tail view, "more" means OLDER messages —
 * either ones we never fetched (`offset > 0`) or ones trimmed off the front
 * (`trimmedMore`). The old `displayed >= limit` check wrongly fired whenever
 * the last page was fully shown, even with nothing older to see.
 */
export function hasMoreToShow(opts: {
  isLast: boolean;
  offset: number;
  trimmedMore: boolean;
  rawHasMore: boolean;
}): boolean {
  return opts.isLast ? opts.offset > 0 || opts.trimmedMore : opts.rawHasMore || opts.trimmedMore;
}

export async function messagesCommand(
  api: ObservationService,
  projectInput: string | undefined,
  sessionInput: string | undefined,
  opts: MessagesOptions,
): Promise<void> {
  const projects = await api.getProjectList();

  // Resolve project
  const projStr = projectInput ?? '.';
  const project = resolveProject(projStr, projects);

  if (!project) {
    throw noProjectMatch(projStr, suggestProjects(projStr, projects));
  }

  // Resolve the session across every agent that worked in this project.
  const sessions = await api.getSessionList(project);

  if (sessions.length === 0) {
    throw new UserError(
      `No sessions found for "${project.folderName}"`,
      `  Run \`spaghetti projects\` to verify the project has sessions.`,
    );
  }

  const sesStr = sessionInput ?? '1'; // default to latest
  const session = resolveSession(sesStr, sessions);

  if (!session) {
    throw noSessionMatch(sesStr, project.folderName);
  }
  const sourceScope = { sourceId: session.sourceId };

  // Calculate offset and limit (guard NaN from commander's parseInt coercer)
  const requestedLimit = resolveLimit(opts.limit, 50);
  const lastN = resolveOptionalCount(opts.last);
  let offset = resolveOffset(opts.offset);

  if (lastN) {
    // Show last N messages: over-fetch from the end to account for internal types
    const total = session.messageCount;
    const overfetch = lastN * 3;
    offset = Math.max(total - overfetch, 0);
  }

  // The effective display limit: --last N overrides --limit
  const displayLimit = lastN ?? requestedLimit;

  // JSON/raw: fetch with exact limit, no filtering (raw stays source-native)
  if (opts.json) {
    const page = await api.getSessionMessages(
      session.projectSlug,
      session.sessionId,
      displayLimit,
      offset,
      sourceScope,
    );
    process.stdout.write(JSON.stringify(page, null, 2) + '\n');
    return;
  }

  if (opts.raw) {
    const page = await api.getSessionMessages(
      session.projectSlug,
      session.sessionId,
      displayLimit,
      offset,
      sourceScope,
    );
    for (const msg of page.messages) {
      process.stdout.write(JSON.stringify(msg) + '\n');
    }
    return;
  }

  // Over-fetch to account for internal message types (progress, file-history-snapshot,
  // saved_hook_context, queue-operation, last-prompt) that get filtered during rendering.
  // Fetch limit*3 initially, filter, and retry up to 2 times if still short.
  const OVER_FETCH_MULTIPLIER = 3;
  const MAX_RETRIES = 2;

  const page = await api.getSessionMessages(
    session.projectSlug,
    session.sessionId,
    displayLimit * OVER_FETCH_MULTIPLIER,
    offset,
    sourceScope,
  );
  let displayMessages = filterDisplayableMessages(page.messages);
  let totalRaw = page.total;
  let lastHasMore = page.hasMore;
  let fetchOffset = offset + page.messages.length;

  for (let retry = 0; retry < MAX_RETRIES && displayMessages.length < displayLimit && lastHasMore; retry++) {
    const morePage = await api.getSessionMessages(
      session.projectSlug,
      session.sessionId,
      displayLimit * OVER_FETCH_MULTIPLIER,
      fetchOffset,
      sourceScope,
    );
    displayMessages = displayMessages.concat(filterDisplayableMessages(morePage.messages));
    totalRaw = morePage.total;
    lastHasMore = morePage.hasMore;
    fetchOffset += morePage.messages.length;
  }

  // Trim to the requested limit: for --last, take the tail; otherwise take the head.
  // Keep the pre-trim count so the footer can tell whether messages exist beyond
  // what's shown (older ones dropped for --last, extra fetched ones for the head).
  const preTrimCount = displayMessages.length;
  if (lastN) {
    displayMessages = displayMessages.slice(-displayLimit);
  } else {
    displayMessages = displayMessages.slice(0, displayLimit);
  }
  const trimmedMore = preTrimCount > displayMessages.length;

  const width = getTerminalWidth();
  const divider = theme.muted('\u2500'.repeat(Math.min(width, 60)));

  // Session header
  const headerLines: string[] = [];
  headerLines.push('');
  headerLines.push(`  ${theme.project(project.folderName)} ${theme.muted('\u203a')} ${theme.accent('Session')}`);
  headerLines.push('');

  const metaParts: string[] = [];
  if (session.gitBranch) {
    metaParts.push(`${theme.label('Branch:')} ${theme.accent(session.gitBranch)}`);
  }
  metaParts.push(`${theme.label('Messages:')} ${formatNumber(session.messageCount)}`);
  metaParts.push(
    `${theme.label('Tokens:')} ${theme.tokens(formatTokenUsage(session.tokenUsage, session.sourceId, session.tokensEstimated))}`,
  );
  metaParts.push(`${theme.label('Duration:')} ${formatDuration(session.lifespanMs)}`);
  metaParts.push(`${theme.label('Last active:')} ${formatRelativeTime(session.lastUpdate)}`);

  for (const part of metaParts) {
    headerLines.push(`  ${part}`);
  }

  if (session.summary) {
    headerLines.push('');
    headerLines.push(`  ${theme.label('Summary:')} ${session.summary}`);
  }

  headerLines.push('');
  headerLines.push(`  ${divider}`);
  headerLines.push('');

  // Render messages (displayMessages is already filtered, renderMessages will be a no-op filter)
  const rendered = renderMessages(displayMessages, {
    compact: opts.compact,
    noTools: opts.noTools,
    noThinking: opts.noThinking,
    width,
    sourceId: session.sourceId,
  });

  // Pagination footer — reflect filtered count
  const footerParts: string[] = [];
  if (offset > 0) {
    footerParts.push(`offset ${offset}`);
  }
  footerParts.push(`${displayMessages.length}/${totalRaw} messages`);
  if (hasMoreToShow({ isLast: lastN !== undefined, offset, trimmedMore, rawHasMore: lastHasMore })) {
    footerParts.push('more available (use --offset or --last)');
  }
  const footer = theme.muted(`  ${footerParts.join(' \u00b7 ')}`);

  const output = headerLines.join('\n') + rendered + '\n\n' + footer + '\n';

  outputWithPager(output);
}
