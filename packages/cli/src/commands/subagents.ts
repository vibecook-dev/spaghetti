/**
 * Subagents command — list and view subagent messages
 */

import type { SpaghettiAPI } from '@vibecook/spaghetti-sdk';
import { theme } from '../lib/color.js';
import { formatNumber } from '../lib/format.js';
import { renderTable } from '../lib/table.js';
import type { Column } from '../lib/table.js';
import { resolveProject, resolveSession, suggestProjects } from '../lib/resolve.js';
import { noProjectMatch, noSessionMatch, UserError } from '../lib/error.js';
import { renderMessages } from '../lib/message-render.js';
import { outputWithPager } from '../lib/pager.js';
import { getTerminalWidth } from '../lib/terminal.js';

export interface SubagentsOptions {
  json?: boolean;
}

export async function subagentsCommand(
  api: SpaghettiAPI,
  projectInput: string | undefined,
  sessionInput: string | undefined,
  agentIndex: string | undefined,
  opts: SubagentsOptions,
): Promise<void> {
  const projects = api.getProjectList();

  // Resolve project
  const projStr = projectInput ?? '.';
  const project = resolveProject(projStr, projects);

  if (!project) {
    throw noProjectMatch(projStr, suggestProjects(projStr, projects));
  }

  // Resolve session
  const sessions = api.getSessionList(project);

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

  // Get subagents
  const subagents = api.getSessionSubagents(session.projectSlug, session.sessionId);

  // If agent index provided, show that agent's messages
  if (agentIndex !== undefined) {
    const idx = parseInt(agentIndex, 10);
    if (isNaN(idx) || idx < 1 || idx > subagents.length) {
      throw new UserError(`Invalid agent index: ${agentIndex}`, `  Valid range: 1-${subagents.length}`);
    }

    const agent = subagents[idx - 1]!;
    const msgPage = api.getSubagentMessages(session.projectSlug, session.sessionId, agent.agentId, 1000, 0);

    if (opts.json) {
      process.stdout.write(
        JSON.stringify(
          {
            project: project.folderName,
            sessionId: session.sessionId,
            agent,
            messages: msgPage,
          },
          null,
          2,
        ) + '\n',
      );
      return;
    }

    const width = getTerminalWidth();
    const lines: string[] = [];
    lines.push('');
    lines.push(
      `  ${theme.project(project.folderName)} ${theme.muted('›')} ${theme.accent('Subagent')} ${theme.muted(`#${idx}`)}`,
    );
    lines.push(`  ${theme.label('Agent ID:')} ${theme.muted(agent.agentId.slice(0, 12))}`);
    lines.push(`  ${theme.label('Type:')} ${agent.agentType}`);
    lines.push(`  ${theme.label('Messages:')} ${formatNumber(agent.messageCount)}`);
    lines.push('');
    lines.push(`  ${theme.muted('─'.repeat(Math.min(width, 60)))}`);
    lines.push('');

    const rendered = renderMessages(msgPage.messages, { width });
    const output = lines.join('\n') + rendered + '\n';
    outputWithPager(output);
    return;
  }

  // List mode: show table of subagents
  if (opts.json) {
    process.stdout.write(
      JSON.stringify(
        {
          project: project.folderName,
          sessionId: session.sessionId,
          subagents,
        },
        null,
        2,
      ) + '\n',
    );
    return;
  }

  const lines: string[] = [];
  lines.push('');
  lines.push(`  ${theme.project(project.folderName)} ${theme.muted('›')} ${theme.accent('Subagents')}`);
  if (session.gitBranch) {
    lines.push(`  ${theme.label('Branch:')} ${theme.accent(session.gitBranch)}`);
  }
  lines.push(`  ${theme.label('Session:')} ${theme.muted(session.sessionId.slice(0, 8))}`);
  lines.push('');

  if (subagents.length === 0) {
    lines.push(`  ${theme.muted('No subagents found.')}`);
    lines.push('');
    process.stdout.write(lines.join('\n') + '\n');
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
      key: 'agentId',
      label: 'Agent ID',
      format: (v: any) => theme.accent(String(v).slice(0, 12)),
    },
    {
      key: 'agentType',
      label: 'Type',
      format: (v: any) => String(v),
    },
    {
      key: 'messageCount',
      label: 'Messages',
      width: 10,
      align: 'right',
      format: (v: any) => formatNumber(Number(v)),
    },
  ];

  const rows = subagents.map((a: any, i: number) => ({
    ...a,
    _index: i + 1,
  }));

  const table = renderTable(rows, columns);

  lines.push(table);
  lines.push('');
  lines.push(
    `  ${theme.muted(`${subagents.length} subagent${subagents.length === 1 ? '' : 's'} · Use \`spaghetti sub ${project.folderName} ${sessionInput ?? '1'} <#>\` to view messages`)}`,
  );
  lines.push('');

  process.stdout.write(lines.join('\n') + '\n');
}
