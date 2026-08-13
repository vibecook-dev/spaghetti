/**
 * TypeScript interfaces for ~/.claude/cache/
 */

export interface ChangelogFile {
  content: string;
  size: number;
}

/**
 * Cached issue records owned by Claude Code. The upstream record shape is not
 * stable, so retain each record without claiming fields the observed corpus
 * cannot establish.
 */
export type MyClosedIssuesFile = unknown[];

export interface CacheDirectory {
  changelog?: ChangelogFile;
  myClosedIssues?: MyClosedIssuesFile;
}
