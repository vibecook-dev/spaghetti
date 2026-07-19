/**
 * Smart resolution — fuzzy matching for projects and sessions
 */

import type { ProjectListItem, SessionListItem } from '@vibecook/spaghetti-sdk';
import path from 'node:path';

/**
 * Resolve a user-provided string to a project.
 *
 * Resolution order:
 * 1. "." → match project whose absolutePath matches or contains cwd
 * 2. Numeric string → 1-indexed from the projects array (already sorted by lastActiveAt)
 * 3. Exact folderName match (case-insensitive)
 * 4. Prefix match on folderName (if only one match)
 * 5. Substring match on folderName (if only one match)
 * 6. Exact slug match
 */
export function resolveProject(input: string, projects: ProjectListItem[]): ProjectListItem | null {
  if (projects.length === 0) return null;

  // 1. "." → match project whose absolutePath matches or contains cwd.
  // Multiple agents can share a cwd (same slug, different sourceId) — pick
  // the most recently active among matches (list is usually already sorted
  // that way; re-sort to be safe).
  if (input === '.') {
    const cwd = process.cwd();
    const byRecency = (a: ProjectListItem, b: ProjectListItem) =>
      new Date(b.lastActiveAt).getTime() - new Date(a.lastActiveAt).getTime();

    const exact = projects.filter((p) => p.absolutePath === cwd).sort(byRecency);
    if (exact.length > 0) return exact[0]!;

    const containing = projects.filter((p) => cwd.startsWith(p.absolutePath + path.sep)).sort(byRecency);
    if (containing.length > 0) return containing[0]!;

    const inside = projects.filter((p) => p.absolutePath.startsWith(cwd + path.sep)).sort(byRecency);
    if (inside.length > 0) return inside[0]!;

    return null;
  }

  // 2. Numeric string → 1-indexed
  const num = Number(input);
  if (!Number.isNaN(num) && Number.isInteger(num) && num >= 1 && num <= projects.length) {
    return projects[num - 1]!;
  }

  const lower = input.toLowerCase();

  // 3. Exact folderName match (case-insensitive)
  const exactNames = projects.filter((p) => p.folderName.toLowerCase() === lower);
  if (exactNames.length === 1) return exactNames[0]!;

  // 4. Prefix match on folderName (if only one match)
  const prefixMatches = projects.filter((p) => p.folderName.toLowerCase().startsWith(lower));
  if (prefixMatches.length === 1) return prefixMatches[0]!;

  // 5. Substring match on folderName (if only one match)
  const substringMatches = projects.filter((p) => p.folderName.toLowerCase().includes(lower));
  if (substringMatches.length === 1) return substringMatches[0]!;

  // 6. Exact slug match
  const slugMatches = projects.filter((p) => p.slug === input || p.projectId === input);
  if (slugMatches.length === 1) return slugMatches[0]!;

  return null;
}

/**
 * Resolve a user-provided string to a session.
 *
 * Resolution order:
 * 1. "latest" or "last" → first session (already sorted by lastUpdate desc)
 * 2. Numeric string → 1-indexed
 * 3. Full UUID match on sessionId
 * 4. Partial UUID prefix match (first 6+ chars)
 */
export function resolveSession(input: string, sessions: SessionListItem[]): SessionListItem | null {
  if (sessions.length === 0) return null;

  // 1. "latest" or "last"
  const lower = input.toLowerCase();
  if (lower === 'latest' || lower === 'last') {
    return sessions[0]!;
  }

  // 2. Numeric string → 1-indexed
  const num = Number(input);
  if (!Number.isNaN(num) && Number.isInteger(num) && num >= 1 && num <= sessions.length) {
    return sessions[num - 1]!;
  }

  // 3. Full UUID match
  const fullMatch = sessions.find((s) => s.sessionId === input);
  if (fullMatch) return fullMatch;

  // 4. Partial UUID prefix match (require at least 6 chars)
  if (input.length >= 6) {
    const prefixMatches = sessions.filter((s) => s.sessionId.startsWith(input));
    if (prefixMatches.length === 1) return prefixMatches[0]!;
  }

  return null;
}

/**
 * Return top 3 projects whose folderName is closest to input (substring/prefix match).
 */
export function suggestProjects(input: string, projects: ProjectListItem[]): ProjectListItem[] {
  const lower = input.toLowerCase();

  // Score: exact=100, prefix=50, substring=25, otherwise 0
  const scored = projects
    .map((p) => {
      const name = p.folderName.toLowerCase();
      let score = 0;
      if (name === lower) score = 100;
      else if (name.startsWith(lower)) score = 50;
      else if (name.includes(lower)) score = 25;
      else if (lower.includes(name)) score = 10;
      return { project: p, score };
    })
    .filter((s) => s.score > 0)
    .sort((a, b) => b.score - a.score);

  return scored.slice(0, 3).map((s) => s.project);
}

/**
 * Return top 3 sessions closest to input.
 */
export function suggestSessions(input: string, sessions: SessionListItem[]): SessionListItem[] {
  // Try partial UUID prefix matching
  const prefixMatches = sessions.filter((s) => s.sessionId.startsWith(input));
  if (prefixMatches.length > 0) return prefixMatches.slice(0, 3);

  // Try matching against summary or firstPrompt
  const lower = input.toLowerCase();
  const textMatches = sessions.filter((s) => {
    const summary = (s.summary || '').toLowerCase();
    const prompt = (s.firstPrompt || '').toLowerCase();
    return summary.includes(lower) || prompt.includes(lower);
  });

  return textMatches.slice(0, 3);
}
