/**
 * Plan command — view session plan content
 */

import type { SpaghettiAPI } from '@vibecook/spaghetti-sdk';
import { theme } from '../lib/color.js';
import { resolveProject, resolveSession, suggestProjects } from '../lib/resolve.js';
import { UserError, noProjectMatch, noSessionMatch } from '../lib/error.js';
import { outputWithPager } from '../lib/pager.js';

export interface PlanOptions {
  json?: boolean;
}

interface PlanData {
  content?: string;
  [key: string]: unknown;
}

export async function planCommand(
  api: SpaghettiAPI,
  projectInput: string | undefined,
  sessionInput: string | undefined,
  opts: PlanOptions,
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

  // Get plan
  const plan = api.getSessionPlan(session.projectSlug, session.sessionId) as PlanData | null;

  // JSON output
  if (opts.json) {
    process.stdout.write(
      JSON.stringify(
        {
          project: project.folderName,
          sessionId: session.sessionId,
          plan,
        },
        null,
        2,
      ) + '\n',
    );
    return;
  }

  if (!plan) {
    process.stdout.write(
      '\n  ' + theme.project(project.folderName) + '\n' + theme.muted('  No plan found for this session.\n\n'),
    );
    return;
  }

  // Render plan content
  const content = typeof plan === 'string' ? plan : (plan.content ?? JSON.stringify(plan, null, 2));

  const lines: string[] = [];
  lines.push('');
  lines.push(`  ${theme.project(project.folderName)} ${theme.muted('›')} ${theme.accent('Plan')}`);
  if (session.gitBranch) {
    lines.push(`  ${theme.label('Branch:')} ${theme.accent(session.gitBranch)}`);
  }
  lines.push(`  ${theme.label('Session:')} ${theme.muted(session.sessionId.slice(0, 8))}`);
  lines.push('');
  lines.push(content);

  outputWithPager(lines.join('\n'));
}
