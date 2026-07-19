/**
 * Memory command — view project MEMORY.md content
 */

import type { SpaghettiAPI } from '@vibecook/spaghetti-sdk';
import { theme } from '../lib/color.js';
import { resolveProject, suggestProjects } from '../lib/resolve.js';
import { noProjectMatch } from '../lib/error.js';
import { outputWithPager } from '../lib/pager.js';

export interface MemoryOptions {
  json?: boolean;
}

export async function memoryCommand(
  api: SpaghettiAPI,
  projectInput: string | undefined,
  opts: MemoryOptions,
): Promise<void> {
  const projects = api.getProjectList();

  // If no project specified and cwd doesn't match, list projects with memory
  const input = projectInput ?? '.';
  const project = resolveProject(input, projects);

  if (!project) {
    // If user didn't specify a project (fell through cwd auto-detect), list projects with memory
    if (!projectInput) {
      const withMemory = projects.filter((p: any) => p.hasMemory);

      if (opts.json) {
        process.stdout.write(
          JSON.stringify(
            withMemory.map((p: any) => ({ slug: p.slug, folderName: p.folderName, path: p.absolutePath })),
            null,
            2,
          ) + '\n',
        );
        return;
      }

      if (withMemory.length === 0) {
        process.stdout.write('\n  ' + theme.muted('No projects have MEMORY.md files.') + '\n\n');
        return;
      }

      const lines: string[] = [];
      lines.push('');
      lines.push(`  ${theme.heading('Projects with Memory')}`);
      lines.push('');
      for (const p of withMemory) {
        lines.push(`    ${theme.project(p.folderName)} ${theme.muted(p.absolutePath)}`);
      }
      lines.push('');
      lines.push(`  ${theme.muted('Use `spaghetti memory <project>` to view.')}`);
      lines.push('');
      process.stdout.write(lines.join('\n') + '\n');
      return;
    }

    // User specified a project that wasn't found
    throw noProjectMatch(input, suggestProjects(input, projects));
  }

  // Get memory content
  const memory = api.getProjectMemory(
    project,
    project.sourceIds.includes('claude-code') ? { sourceId: 'claude-code' } : undefined,
  );

  if (opts.json) {
    process.stdout.write(
      JSON.stringify(
        {
          project: project.folderName,
          slug: project.slug,
          path: project.absolutePath,
          memory: memory,
        },
        null,
        2,
      ) + '\n',
    );
    return;
  }

  if (!memory) {
    process.stdout.write(
      '\n  ' + theme.project(project.folderName) + '\n' + theme.muted('  No MEMORY.md found for this project.\n\n'),
    );
    return;
  }

  // Header + content
  const lines: string[] = [];
  lines.push('');
  lines.push(`  ${theme.project(project.folderName)} ${theme.muted('› Memory')}`);
  lines.push(`  ${theme.muted(project.absolutePath)}`);
  lines.push('');
  lines.push(memory);

  outputWithPager(lines.join('\n'));
}
