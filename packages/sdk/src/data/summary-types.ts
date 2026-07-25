/**
 * Summary Types — Aggregated token usage and summary data
 */

export interface TokenUsageSummary {
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  /** Source-normalized total; cached Codex input is not double-counted. */
  totalTokens: number;
}

export interface SessionSummaryData {
  sessionId: string;
  /** Which agent product this session came from (e.g. 'claude-code'). */
  sourceId: string;
  projectSlug: string;
  startTime: string;
  lastUpdate: string;
  lifespanMs: number;
  tokenUsage: TokenUsageSummary;
  /**
   * True when tokenUsage was filled by a local estimate (e.g. tiktoken on
   * Codex text) rather than agent-emitted usage events. UI should show "~".
   */
  tokensEstimated: boolean;
  messageCount: number;
  fullPath: string;
  /** User-pinned title, or Claude's latest generated title when unpinned. */
  title: string;
  summary: string;
  firstPrompt: string;
  gitBranch: string;
  todoCount: number;
  planSlug: string | null;
  hasTask: boolean;
  isSidechain: boolean;
}

export interface ProjectSummaryData {
  slug: string;
  /** Which agent product this project came from (e.g. 'claude-code'). */
  sourceId: string;
  folderName: string;
  absolutePath: string;
  sessionCount: number;
  messageCount: number;
  tokenUsage: TokenUsageSummary;
  /**
   * True when any session under this project has estimated tokens (and no
   * fully official-only mix — currently true if MAX(tokens_estimated)=1).
   */
  tokensEstimated: boolean;
  lastActiveAt: string;
  firstActiveAt: string;
  latestGitBranch: string;
  /** Prompt/summary of the most recently active session, for project-list preview. */
  latestPrompt: string;
  hasMemory: boolean;
}
