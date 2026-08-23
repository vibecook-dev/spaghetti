/**
 * SpaghettiAPI — Public interface for consuming Claude Code agent data
 */

import type {
  SearchQuery,
  SearchResultSet,
  StoreStats,
  InitProgress,
  SegmentChangeBatch,
} from './data/segment-types.js';
import type { TokenUsageSummary } from './data/summary-types.js';
import type { SessionMessage, TeamDirectory } from './types/index.js';
import type {
  SubagentTimelinePage,
  SubagentTimelinePageRequest,
  TimelineFacets,
  TimelinePage,
  TimelinePageRequest,
} from './data/timeline-query.js';

// ═══════════════════════════════════════════════════════════════════════════
// RESPONSE TYPES
// ═══════════════════════════════════════════════════════════════════════════

/** Optional agent scope for source-aware reads. */
export interface SourceFilter {
  sourceId?: string;
}

/** Subagent list scope; nested workflow agents are opt-in for compatibility. */
export interface SubagentFilter extends SourceFilter {
  includeNested?: boolean;
}

/** One source-owned database row contributing to an aggregated workspace. */
export interface ProjectMember {
  sourceId: string;
  slug: string;
}

/** Stable handle for project-scoped reads. */
export interface ProjectLocator {
  /** Stable workspace identity; path-based when an authoritative path exists. */
  projectId: string;
  /** Exact source/slug rows that belong to this workspace. */
  members: ProjectMember[];
}

/** Legacy slug reads remain supported, but locators avoid lossy-slug collisions. */
export type ProjectReference = string | ProjectLocator;

export interface ProjectListItem extends ProjectLocator {
  /** Representative legacy slug. Use `projectId` for UI identity. */
  slug: string;
  /** Every agent product contributing indexed content to this workspace. */
  sourceIds: string[];
  folderName: string;
  absolutePath: string;
  sessionCount: number;
  messageCount: number;
  tokenUsage: TokenUsageSummary;
  /**
   * True when tokenUsage is not fully exact: some bucket was derived or
   * estimated, or some response asserted no bucket at all and the total is a
   * floor rather than a measurement. UI should show "~" / "est".
   */
  tokensEstimated: boolean;
  lastActiveAt: string;
  firstActiveAt: string;
  latestGitBranch: string;
  /** Prompt/summary of the most recently active member session. */
  latestPrompt: string;
  hasMemory: boolean;
}

export interface TokenActivityQuery extends SourceFilter {
  /** Inclusive YYYY-MM-DD bounds. */
  from: string;
  to: string;
}

export interface TokenActivityDay {
  date: string;
  tokenUsage: TokenUsageSummary;
  quality: 'exact' | 'estimated' | 'mixed' | 'unavailable';
  exactTokens: number;
  estimatedTokens: number;
  messageCount: number;
  sessionCount: number;
  sourceIds: string[];
}

export interface TokenActivityResult {
  from: string;
  to: string;
  days: TokenActivityDay[];
}

export interface SessionListItem {
  sessionId: string;
  /** Agent product this session came from (e.g. 'claude-code'). */
  sourceId: string;
  /** Source-owned project slug for subsequent session reads. */
  projectSlug: string;
  /**
   * Stable RFC 012 external identity when this row came from the canonical
   * observation owner. Legacy/in-memory readers may not have one.
   */
  externalRef?: string;
  /** True once transcript-backed fields on this row are readable. */
  decoded: boolean;
  startTime: string;
  lastUpdate: string;
  lifespanMs: number;
  tokenUsage: TokenUsageSummary;
  /**
   * True when tokenUsage is a local estimate, not agent-emitted usage.
   */
  tokensEstimated: boolean;
  messageCount: number;
  fullPath: string;
  /** User-pinned title, or the agent's latest generated title. */
  title: string;
  summary: string;
  firstPrompt: string;
  gitBranch: string;
  todoCount: number;
  planSlug: string | null;
  hasTask: boolean;
  isSidechain: boolean;
}

export interface MessagePage {
  messages: SessionMessage[];
  total: number;
  offset: number;
  hasMore: boolean;
}

export interface SubagentListItem {
  sourceId: string;
  agentId: string;
  agentType: string;
  messageCount: number;
  workflowId: string;
  /** Parent Task/Agent tool-use id when an explicit result link was found. */
  spawnToolId: string | null;
  linkMethod: 'tool_result' | 'ordinal' | 'unlinked';
  /**
   * Absolute path of the git worktree this agent ran in, when it was spawned
   * with worktree isolation. Absent for the majority of agents, which work
   * directly in the project root.
   */
  worktreePath?: string;
}

export interface WorkflowListItem {
  workflowId: string;
  name: string;
  status: string;
  agentCount: number;
  totalTokens: number;
  totalToolCalls: number;
  durationMs: number;
  subagentCount: number;
}

export interface SubagentMessagePage {
  messages: SessionMessage[];
  total: number;
  offset: number;
  hasMore: boolean;
}

// ═══════════════════════════════════════════════════════════════════════════
// SPAGHETTI API INTERFACE
// ═══════════════════════════════════════════════════════════════════════════

export interface SpaghettiAPI {
  /** Initialize the Rust observation owner. */
  initialize(): Promise<void>;

  /** Shut down watchers and close the database */
  shutdown(): void;

  /**
   * Reconcile every configured native adapter through the sole Rust owner.
   */
  rebuildIndex(): Promise<{ durationMs: number }>;

  /** Whether the service is ready to accept queries */
  isReady(): boolean;

  /** Get all projects sorted by last active date */
  /** Distinct agent sources present in the index (e.g. ['claude-code']). */
  getSourceIds(): Promise<string[]>;
  /**
   * List unique workspaces, optionally scoped to those touched by one agent.
   * Project metrics are aggregated across the contributing agents.
   */
  getProjectList(options?: SourceFilter): Promise<ProjectListItem[]>;

  /** Source-normalized daily token activity for one aggregated workspace. */
  getProjectTokenActivity(project: ProjectReference, query: TokenActivityQuery): Promise<TokenActivityResult>;

  /**
   * Get all sessions for a project sorted by last update. Sessions retain
   * their individual `sourceId`; use the optional filter for an agent-only
   * session list.
   */
  getSessionList(project: ProjectReference, options?: SourceFilter): Promise<SessionListItem[]>;

  /**
   * Get paginated messages for a session.
   * Pass `{ sourceId }` to scope by agent (defense in depth; session ids are
   * usually globally unique already).
   */
  getSessionMessages(
    projectSlug: string,
    sessionId: string,
    limit?: number,
    offset?: number,
    options?: SourceFilter,
  ): Promise<MessagePage>;

  /** Full-session normalized display counts, independent of loaded pages. */
  getSessionTimelineFacets(projectSlug: string, sessionId: string, options?: SourceFilter): Promise<TimelineFacets>;

  /** Database-filtered normalized display messages, newest page first. */
  getSessionTimeline(projectSlug: string, sessionId: string, request?: TimelinePageRequest): Promise<TimelinePage>;

  /**
   * Get project MEMORY.md content.
   * Memory is Claude-only today; with `{ sourceId }` other than `claude-code`,
   * returns null so a Codex project does not surface Claude's MEMORY.md.
   */
  getProjectMemory(project: ProjectReference, options?: SourceFilter): Promise<string | null>;

  /** Get todos for a session */
  getSessionTodos(projectSlug: string, sessionId: string): Promise<unknown[]>;

  /** Get plan for a session */
  getSessionPlan(projectSlug: string, sessionId: string): Promise<unknown | null>;

  /** Get task for a session */
  getSessionTask(projectSlug: string, sessionId: string): Promise<unknown | null>;

  /** Get a persisted tool result */
  getToolResult(projectSlug: string, sessionId: string, toolUseId: string): Promise<string | null>;

  /** Get top-level subagent list for a session (excludes workflow-nested) */
  getSessionSubagents(projectSlug: string, sessionId: string, options?: SubagentFilter): Promise<SubagentListItem[]>;

  /** Get agent-orchestration workflow runs for a session */
  getSessionWorkflows(projectSlug: string, sessionId: string): Promise<WorkflowListItem[]>;

  /** Get the subagents that ran under a specific workflow */
  getWorkflowSubagents(
    projectSlug: string,
    sessionId: string,
    workflowId: string,
    options?: SourceFilter,
  ): Promise<SubagentListItem[]>;

  /**
   * Get paginated subagent messages. Pass `workflowId` to disambiguate when
   * the same agentId ran both top-level and under a workflow (`''` = the
   * top-level transcript); without it the top-level transcript wins.
   */
  getSubagentMessages(
    projectSlug: string,
    sessionId: string,
    agentId: string,
    limit?: number,
    offset?: number,
    workflowId?: string,
    options?: SourceFilter,
  ): Promise<SubagentMessagePage>;

  /** Normalized, filterable display rows for one inline agent branch. */
  getSubagentTimeline(
    projectSlug: string,
    sessionId: string,
    agentId: string,
    request: SubagentTimelinePageRequest,
  ): Promise<SubagentTimelinePage>;

  /** Full-text search across all segments */
  search(query: SearchQuery): Promise<SearchResultSet>;

  /** Get store statistics */
  getStats(): Promise<StoreStats>;

  /**
   * Agent teams parsed from `~/.claude/teams/` (experimental agent-teams
   * feature). Empty array when the feature is unused. `config` is null
   * for orphaned team dirs that only have inboxes on disk.
   */
  getTeams(): Promise<TeamDirectory[]>;

  /** Subscribe to init progress events */
  onProgress(cb: (progress: InitProgress) => void): () => void;

  /** Subscribe to ready event */
  onReady(cb: (info: { durationMs: number }) => void): () => void;

  /** Subscribe to data change events */
  onChange(cb: (batch: SegmentChangeBatch) => void): () => void;

  /**
   * Awaitable teardown. Stops the live disk pipeline, drains
   * in-flight writes, disposes subscribers, and closes SQLite. Prefer
   * this to `shutdown()` when the caller can `await` — `shutdown()` is
   * fire-and-forget.
   *
   * Declared as a plain method because the SDK's tsconfig targets
   * ES2022 and `Symbol.asyncDispose` (ES2024) isn't available in the
   * type lib. Once the target bumps, this becomes
   * `[Symbol.asyncDispose](): Promise<void>` and consumers can use
   * `await using`.
   */
  dispose(): Promise<void>;
}
