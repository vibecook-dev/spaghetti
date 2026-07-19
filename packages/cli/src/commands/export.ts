/**
 * Export command — export project data to JSON or Markdown
 */

import { writeFileSync } from 'node:fs';
import type { SpaghettiAPI, SessionListItem, SessionMessage } from '@vibecook/spaghetti-sdk';
import { theme } from '../lib/color.js';
import { formatDuration, formatTokens, totalTokens } from '../lib/format.js';
import { resolveProject, suggestProjects } from '../lib/resolve.js';
import { noProjectMatch, noSessionMatch } from '../lib/error.js';

export interface ExportOptions {
  session?: string;
  format?: string;
  output?: string;
  includeTools?: boolean;
  json?: boolean;
}

interface ExportedSession {
  sessionId: string;
  startTime: string;
  lastUpdate: string;
  lifespanMs: number;
  messageCount: number;
  gitBranch: string;
  summary: string;
  tokenUsage: {
    inputTokens: number;
    outputTokens: number;
    cacheCreationTokens: number;
    cacheReadTokens: number;
  };
  messages: SessionMessage[];
}

interface ExportedProject {
  name: string;
  slug: string;
  path: string;
  sessionCount: number;
  messageCount: number;
  exportedAt: string;
  sessions: ExportedSession[];
}

function extractTextContent(msg: SessionMessage, includeTools: boolean): string {
  if (msg.type === 'user') {
    const payload = msg.message;
    if (typeof payload.content === 'string') return payload.content;
    if (Array.isArray(payload.content)) {
      return payload.content
        .map((block: any) => {
          if (block.type === 'text') return block.text;
          if (block.type === 'tool_result' && includeTools) {
            const c = block.content;
            if (typeof c === 'string') return `[Tool Result]\n${c}`;
            return '[Tool Result]';
          }
          return '';
        })
        .filter(Boolean)
        .join('\n');
    }
    return '';
  }

  if (msg.type === 'assistant') {
    const payload = msg.message;
    const blocks = payload.content || [];
    return blocks
      .map((block: any) => {
        if (block.type === 'text') return block.text;
        if (block.type === 'tool_use' && includeTools) {
          return `[Tool: ${block.name}]\n${JSON.stringify(block.input, null, 2)}`;
        }
        if (block.type === 'thinking') return '';
        return '';
      })
      .filter(Boolean)
      .join('\n\n');
  }

  return '';
}

function exportSessionAsMarkdown(session: SessionListItem, messages: SessionMessage[], includeTools: boolean): string {
  const lines: string[] = [];

  lines.push(`## Session: ${session.sessionId.slice(0, 8)}`);
  lines.push('');
  if (session.summary) {
    lines.push(`> ${session.summary}`);
    lines.push('');
  }
  lines.push(`- **Branch:** ${session.gitBranch || 'N/A'}`);
  lines.push(`- **Duration:** ${formatDuration(session.lifespanMs)}`);
  lines.push(`- **Messages:** ${session.messageCount}`);
  lines.push(`- **Tokens:** ${formatTokens(totalTokens(session.tokenUsage))}`);
  lines.push(`- **Started:** ${session.startTime}`);
  lines.push(`- **Last update:** ${session.lastUpdate}`);
  lines.push('');
  lines.push('---');
  lines.push('');

  for (const msg of messages) {
    if (msg.type === 'user') {
      lines.push('### You');
      lines.push('');
      lines.push(extractTextContent(msg, includeTools));
      lines.push('');
    } else if (msg.type === 'assistant') {
      const agent =
        session.sourceId === 'codex'
          ? 'Codex'
          : session.sourceId === 'claude-code'
            ? 'Claude'
            : session.sourceId || 'Assistant';
      lines.push(`### ${agent}`);
      lines.push('');
      lines.push(extractTextContent(msg, includeTools));
      lines.push('');
    }
    // Skip system, progress, and other meta messages in markdown export
  }

  return lines.join('\n');
}

export async function exportCommand(
  api: SpaghettiAPI,
  projectInput: string | undefined,
  opts: ExportOptions,
): Promise<void> {
  const projects = api.getProjectList();

  // Resolve project
  const projStr = projectInput ?? '.';
  const project = resolveProject(projStr, projects);

  if (!project) {
    throw noProjectMatch(projStr, suggestProjects(projStr, projects));
  }

  const fmt = opts.format ?? (opts.json ? 'json' : 'json');
  const includeTools = opts.includeTools ?? false;

  // Get sessions to export
  let sessions = api.getSessionList(project);

  if (opts.session) {
    // Filter to a single session
    const { resolveSession } = await import('../lib/resolve.js');
    const session = resolveSession(opts.session, sessions);
    if (!session) {
      throw noSessionMatch(opts.session, project.folderName);
    }
    sessions = [session];
  }

  // Show progress for large exports
  if (!opts.output) {
    process.stderr.write(theme.muted(`Exporting ${sessions.length} session${sessions.length === 1 ? '' : 's'}...\n`));
  }

  if (fmt === 'markdown' || fmt === 'md') {
    // Markdown export
    const parts: string[] = [];
    parts.push(`# ${project.folderName}`);
    parts.push('');
    parts.push(`Exported: ${new Date().toISOString()}`);
    parts.push(`Path: ${project.absolutePath}`);
    parts.push(`Sessions: ${sessions.length}`);
    parts.push('');

    for (const session of sessions) {
      const page = api.getSessionMessages(session.projectSlug, session.sessionId, 100000, 0, {
        sourceId: session.sourceId,
      });
      parts.push(exportSessionAsMarkdown(session, page.messages, includeTools));
      parts.push('');
    }

    const content = parts.join('\n');
    outputResult(content, opts.output);
  } else {
    // JSON export
    const exportedSessions: ExportedSession[] = [];
    for (const session of sessions) {
      const page = api.getSessionMessages(session.projectSlug, session.sessionId, 100000, 0, {
        sourceId: session.sourceId,
      });

      let messages = page.messages;
      if (!includeTools) {
        // Strip tool result content to reduce size
        messages = messages.map((m: any) => {
          if (m.type === 'user' && Array.isArray(m.message.content)) {
            return {
              ...m,
              message: {
                ...m.message,
                content: m.message.content.map((block: any) => {
                  if (block.type === 'tool_result') {
                    return { ...block, content: '[stripped]' };
                  }
                  return block;
                }),
              },
            } as SessionMessage;
          }
          return m;
        });
      }

      exportedSessions.push({
        sessionId: session.sessionId,
        startTime: session.startTime,
        lastUpdate: session.lastUpdate,
        lifespanMs: session.lifespanMs,
        messageCount: session.messageCount,
        gitBranch: session.gitBranch,
        summary: session.summary,
        tokenUsage: session.tokenUsage,
        messages,
      });
    }

    const data: ExportedProject = {
      name: project.folderName,
      slug: project.slug,
      path: project.absolutePath,
      sessionCount: sessions.length,
      messageCount: sessions.reduce((sum: number, s: any) => sum + s.messageCount, 0),
      exportedAt: new Date().toISOString(),
      sessions: exportedSessions,
    };

    const content = JSON.stringify(data, null, 2);
    outputResult(content, opts.output);
  }
}

function outputResult(content: string, outputPath: string | undefined): void {
  if (outputPath) {
    writeFileSync(outputPath, content, 'utf-8');
    process.stderr.write(
      theme.success(`\n  Exported to ${outputPath}`) +
        '\n' +
        theme.muted(`  ${(content.length / 1024).toFixed(1)} KB written\n\n`),
    );
  } else {
    process.stdout.write(content + '\n');
  }
}
