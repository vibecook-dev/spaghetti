/**
 * Todos command — view todo items for a session
 */

import type { SpaghettiAPI } from '@vibecook/spaghetti-sdk';
import { theme } from '../lib/color.js';
import { resolveProject, resolveSession, suggestProjects } from '../lib/resolve.js';
import { UserError, noProjectMatch, noSessionMatch } from '../lib/error.js';

export interface TodosOptions {
  json?: boolean;
}

interface TodoItem {
  content?: string;
  status?: string;
  id?: string;
  [key: string]: unknown;
}

function renderTodoStatus(status: string): string {
  switch (status) {
    case 'completed':
      return theme.success('[x]');
    case 'in_progress':
      return theme.warning('[~]');
    case 'pending':
    default:
      return '[ ]';
  }
}

function renderTodoContent(todo: TodoItem): string {
  const status = todo.status ?? 'pending';
  const content = todo.content ?? String(todo.id ?? '');
  const checkbox = renderTodoStatus(status);

  if (status === 'completed') {
    return `  ${checkbox} ${theme.muted(content)}`;
  }
  if (status === 'in_progress') {
    return `  ${checkbox} ${theme.warning(content)}`;
  }
  return `  ${checkbox} ${content}`;
}

export async function todosCommand(
  api: SpaghettiAPI,
  projectInput: string | undefined,
  sessionInput: string | undefined,
  opts: TodosOptions,
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

  // Get todos
  const todos = api.getSessionTodos(session.projectSlug, session.sessionId) as TodoItem[];

  // JSON output
  if (opts.json) {
    process.stdout.write(
      JSON.stringify(
        {
          project: project.folderName,
          sessionId: session.sessionId,
          todos,
        },
        null,
        2,
      ) + '\n',
    );
    return;
  }

  const lines: string[] = [];

  // Header
  lines.push('');
  lines.push(`  ${theme.project(project.folderName)} ${theme.muted('›')} ${theme.accent('Todos')}`);
  if (session.gitBranch) {
    lines.push(`  ${theme.label('Branch:')} ${theme.accent(session.gitBranch)}`);
  }
  lines.push(`  ${theme.label('Session:')} ${theme.muted(session.sessionId.slice(0, 8))}`);
  lines.push('');

  if (todos.length === 0) {
    lines.push(`  ${theme.muted('No todos found.')}`);
    lines.push('');
    process.stdout.write(lines.join('\n') + '\n');
    return;
  }

  for (const todo of todos) {
    lines.push(renderTodoContent(todo));
  }

  // Summary
  const completed = todos.filter((t) => t.status === 'completed').length;
  const inProgress = todos.filter((t) => t.status === 'in_progress').length;
  const pending = todos.filter((t) => t.status === 'pending' || !t.status).length;

  lines.push('');
  const parts: string[] = [];
  if (completed > 0) parts.push(theme.success(`${completed} done`));
  if (inProgress > 0) parts.push(theme.warning(`${inProgress} in progress`));
  if (pending > 0) parts.push(`${pending} pending`);
  lines.push(`  ${theme.muted(parts.join(' · '))}`);
  lines.push('');

  process.stdout.write(lines.join('\n') + '\n');
}
