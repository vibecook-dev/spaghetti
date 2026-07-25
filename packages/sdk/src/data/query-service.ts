/**
 * QueryService — Read-only query layer for the Phase 3 dedicated-table schema
 *
 * All methods return domain types directly. No segment abstraction.
 */

import type { SqliteService } from '../io/index.js';
import type { ProjectSummaryData, SessionSummaryData, TokenUsageSummary } from './summary-types.js';
import type { SearchQuery, SearchResultSet, StoreStats } from './segment-types.js';
import { initializeSchema } from './schema.js';
import { ensureTimelineProjection, rebuildDirtyTimelineProjections } from './timeline-projection.js';
import {
  rebuildDirtySubagentProjections,
  ensureSearchableSubagentProjections,
  ensureSessionSubagentProjections,
  ensureSubagentTimelineProjection,
} from './subagent-projection.js';
import type {
  SubagentTimelinePage,
  SubagentTimelinePageRequest,
  TimelineFacets,
  TimelinePage,
  TimelinePageRequest,
} from './timeline-query.js';
import type { SubagentListItem } from '../api.js';
import {
  normalizedTokenTotal,
  readTokenActivity,
  ensureTokenActivityMaterialized,
  type TokenActivityBucketData,
} from './token-activity.js';

// ═══════════════════════════════════════════════════════════════════════════
// INTERFACE
// ═══════════════════════════════════════════════════════════════════════════

export interface QueryService {
  open(dbPath: string): void;
  close(): void;
  isOpen(): boolean;
  /** Finish rebuildable display indexes after native cold/warm ingest. */
  prepareTimelineProjections(
    onProgress?: (progress: { kind: 'session' | 'subagent'; current: number; total: number }) => void,
  ): { sessions: number; subagents: number };
  /** Reconcile crash-left dirty token buckets during boot; reads never call this. */
  prepareTokenActivity(): number;

  // Projects
  getProjectSlugs(): string[];
  /** Distinct agent sources present in the index. */
  getSourceIds(): string[];
  getProjectSummaries(options?: { sourceId?: string }): ProjectSummaryData[];
  getSessionSummaries(projectSlug: string, options?: { sourceId?: string }): SessionSummaryData[];
  getProjectTokenActivity(
    projectSlug: string,
    options: { sourceId?: string; from: string; to: string },
  ): TokenActivityBucketData[];
  /**
   * Distinct `project_slug` values present in `messages` but absent
   * from `projects`. Used by warm-start recovery to detect orphaned
   * rows left behind by older code paths that emitted messages
   * without their parent `projects`/`sessions` rows.
   */
  getOrphanedMessageProjectSlugs(): string[];

  // Messages (paginated)
  getSessionMessages(
    slug: string,
    sessionId: string,
    limit: number,
    offset: number,
    options?: { sourceId?: string },
  ): { messages: unknown[]; total: number; offset: number; hasMore: boolean };
  /** Full-session counts from the normalized display projection. */
  getSessionTimelineFacets(slug: string, sessionId: string, options?: { sourceId?: string }): TimelineFacets;
  /** Cursor-paginated normalized display messages, filtered in SQLite. */
  getSessionTimeline(slug: string, sessionId: string, request?: TimelinePageRequest): TimelinePage;

  // Subagents
  getSessionSubagents(
    slug: string,
    sessionId: string,
    options?: { sourceId?: string; includeNested?: boolean },
  ): SubagentListItem[];
  getSubagentMessages(
    slug: string,
    sessionId: string,
    agentId: string,
    limit: number,
    offset: number,
    workflowId?: string,
    options?: { sourceId?: string },
  ): { messages: unknown[]; total: number; offset: number; hasMore: boolean };
  getSubagentTimeline(
    slug: string,
    sessionId: string,
    agentId: string,
    request: SubagentTimelinePageRequest,
  ): SubagentTimelinePage;

  // Workflows (agent-orchestration runs)
  getSessionWorkflows(
    slug: string,
    sessionId: string,
  ): Array<{
    workflowId: string;
    name: string;
    status: string;
    agentCount: number;
    totalTokens: number;
    totalToolCalls: number;
    durationMs: number;
    subagentCount: number;
  }>;
  getWorkflowSubagents(
    slug: string,
    sessionId: string,
    workflowId: string,
    options?: { sourceId?: string },
  ): SubagentListItem[];

  // Details
  getProjectMemory(slug: string, options?: { sourceId?: string }): string | null;
  getSessionTodos(slug: string, sessionId: string): unknown[];
  getSessionPlan(slug: string, sessionId: string): unknown | null;
  getSessionTask(slug: string, sessionId: string): unknown | null;
  getToolResult(slug: string, sessionId: string, toolUseId: string): string | null;

  // Search
  search(query: SearchQuery): SearchResultSet;

  // Stats
  getStats(): StoreStats;
}

// ═══════════════════════════════════════════════════════════════════════════
// ROW TYPES (internal)
// ═══════════════════════════════════════════════════════════════════════════

interface CountRow {
  count: number;
}

interface ProjectSlugRow {
  slug: string;
}

interface ProjectSummaryRow {
  slug: string;
  source_id: string;
  original_path: string;
  session_count: number;
  message_count: number;
  input_tokens: number;
  output_tokens: number;
  cache_creation_tokens: number;
  cache_read_tokens: number;
  tokens_estimated: number;
  last_active_at: string;
  first_active_at: string;
  latest_git_branch: string | null;
  latest_prompt: string;
  has_memory: number;
}

interface SessionSummaryRow {
  id: string;
  source_id: string;
  project_slug: string;
  full_path: string;
  first_prompt: string;
  summary: string;
  title: string;
  git_branch: string;
  project_path: string;
  is_sidechain: number;
  created_at: string;
  modified_at: string;
  message_count: number;
  input_tokens: number;
  output_tokens: number;
  cache_creation_tokens: number;
  cache_read_tokens: number;
  tokens_estimated: number;
  todo_count: number;
  plan_slug: string | null;
  has_task: number;
}

interface MessageDataRow {
  data: string;
  /** Column timestamp (sidecars may fill this when raw JSON has none). */
  timestamp: string | null;
}

interface TimelineDataRow {
  timeline_index: number;
  data: string;
}

interface FacetRow {
  name: string;
  count: number;
}

interface SubagentRow {
  source_id: string;
  agent_id: string;
  agent_type: string;
  message_count: number;
  workflow_id: string;
  spawn_tool_id: string | null;
  link_method: 'tool_result' | 'unlinked';
  id: number;
}

interface WorkflowRow {
  workflow_id: string;
  name: string;
  status: string;
  agent_count: number;
  total_tokens: number;
  total_tool_calls: number;
  duration_ms: number;
  subagent_count: number;
}

interface SubagentRawMessageRow {
  data: string;
  timestamp: string | null;
}

interface ToolAnchorRow {
  tool_use_id: string;
}

interface MemoryRow {
  content: string;
}

interface TodoRow {
  items: string;
}

interface PlanRow {
  content: string;
  title: string;
  slug: string;
  size: number;
}

interface TaskRow {
  session_id: string;
  has_highwatermark: number;
  highwatermark: number | null;
  lock_exists: number;
}

interface ToolResultRow {
  content: string;
}

interface SearchFtsRow {
  project_slug: string;
  session_id: string;
  msg_index: number;
  source_id: string;
  snippet: string;
  rank: number;
}

interface SubagentSearchFtsRow extends SearchFtsRow {
  agent_id: string;
  workflow_id: string;
  timeline_index: number;
  spawn_tool_id: string | null;
}

// ═══════════════════════════════════════════════════════════════════════════
// IMPLEMENTATION
// ═══════════════════════════════════════════════════════════════════════════

class QueryServiceImpl implements QueryService {
  private db: SqliteService;
  private opened = false;

  constructor(sqliteServiceFactory: () => SqliteService) {
    this.db = sqliteServiceFactory();
  }

  open(dbPath: string): void {
    // If the underlying SqliteService is already open (shared connection),
    // skip opening again to avoid "Database already open" errors.
    if (!this.db.isOpen()) {
      this.db.open({ path: dbPath });
    }
    initializeSchema(this.db);
    this.opened = true;
  }

  close(): void {
    if (this.opened) {
      this.db.close();
      this.opened = false;
    }
  }

  isOpen(): boolean {
    return this.opened;
  }

  prepareTimelineProjections(
    onProgress?: (progress: { kind: 'session' | 'subagent'; current: number; total: number }) => void,
  ): { sessions: number; subagents: number } {
    const sessions = rebuildDirtyTimelineProjections(this.db, (progress) =>
      onProgress?.({ kind: 'session', current: progress.current, total: progress.total }),
    );
    const subagents = rebuildDirtySubagentProjections(this.db, (progress) =>
      onProgress?.({ kind: 'subagent', current: progress.current, total: progress.total }),
    );
    return { sessions, subagents };
  }

  prepareTokenActivity(): number {
    return ensureTokenActivityMaterialized(this.db);
  }

  // ─────────────────────────────────────────────────────────────────────────
  // Projects
  // ─────────────────────────────────────────────────────────────────────────

  getProjectSlugs(): string[] {
    const rows = this.db.all<ProjectSlugRow>('SELECT slug FROM projects ORDER BY slug');
    return rows.map((r) => r.slug);
  }

  getOrphanedMessageProjectSlugs(): string[] {
    // Join on (slug, source_id): multi-source same-cwd projects share a slug
    // but not a source. Without source_id, a Grok/Codex project row would hide
    // Claude orphaned messages (and vice versa). Scoped to claude-code because
    // only Claude warm-start recovery re-materialises parent project rows.
    const rows = this.db.all<{ project_slug: string }>(
      `SELECT DISTINCT m.project_slug
         FROM messages m
         LEFT JOIN projects p
           ON m.project_slug = p.slug AND m.source_id = p.source_id
        WHERE p.slug IS NULL
          AND m.source_id = 'claude-code'`,
    );
    return rows.map((r) => r.project_slug);
  }

  /** Distinct agent sources present in the index. */
  getSourceIds(): string[] {
    const rows = this.db.all<{ source_id: string }>('SELECT DISTINCT source_id FROM sessions ORDER BY source_id');
    return rows.map((r) => r.source_id);
  }

  getProjectSummaries(options?: { sourceId?: string }): ProjectSummaryData[] {
    const where = options?.sourceId ? 'WHERE p.source_id = ?' : '';
    const params = options?.sourceId ? [options.sourceId] : [];
    const rows = this.db.all<ProjectSummaryRow>(
      `
      WITH ranked_sessions AS (
        SELECT project_slug, source_id, project_path, first_prompt, summary, ai_title, custom_title, git_branch,
               created_at, modified_at, tokens_estimated,
               ROW_NUMBER() OVER (
                 PARTITION BY project_slug, source_id
                 ORDER BY modified_at DESC, id DESC
               ) AS recent_rank
          FROM sessions
      ),
      session_stats AS (
        SELECT project_slug, source_id,
               COUNT(*) AS session_count,
               MAX(tokens_estimated) AS tokens_estimated,
               MAX(modified_at) AS last_active_at,
               MIN(created_at) AS first_active_at,
               MAX(CASE WHEN recent_rank = 1 THEN NULLIF(project_path, '') END) AS latest_project_path,
               MAX(CASE WHEN recent_rank = 1 THEN git_branch END) AS latest_git_branch,
               MAX(CASE WHEN recent_rank = 1 THEN COALESCE(
                 NULLIF(custom_title, ''), NULLIF(ai_title, ''),
                 NULLIF(first_prompt, ''), NULLIF(summary, ''), ''
               ) END) AS latest_prompt
          FROM ranked_sessions
         GROUP BY project_slug, source_id
      ),
      token_totals AS (
        SELECT source_id, project_slug,
               SUM(input_tokens) AS input_tokens,
               SUM(output_tokens) AS output_tokens,
               SUM(cache_creation_tokens) AS cache_creation_tokens,
               SUM(cache_read_tokens) AS cache_read_tokens,
               SUM(parent_message_count) AS parent_message_count
          FROM session_summary_totals
         GROUP BY source_id, project_slug
      )
      SELECT p.slug, p.source_id,
        COALESCE(ss.latest_project_path, p.original_path, '') AS original_path,
        COALESCE(ss.session_count, 0) as session_count,
        COALESCE(tt.parent_message_count, 0) as message_count,
        COALESCE(tt.input_tokens, 0) as input_tokens,
        COALESCE(tt.output_tokens, 0) as output_tokens,
        COALESCE(tt.cache_creation_tokens, 0) as cache_creation_tokens,
        COALESCE(tt.cache_read_tokens, 0) as cache_read_tokens,
        COALESCE(ss.tokens_estimated, 0) as tokens_estimated,
        COALESCE(ss.last_active_at, '1970-01-01') as last_active_at,
        COALESCE(ss.first_active_at, '1970-01-01') as first_active_at,
        COALESCE(ss.latest_git_branch, '') as latest_git_branch,
        COALESCE(ss.latest_prompt, '') as latest_prompt,
        CASE
          WHEN p.source_id = 'claude-code'
          THEN EXISTS(SELECT 1 FROM project_memories WHERE project_slug = p.slug)
          ELSE 0
        END as has_memory
      FROM projects p
      LEFT JOIN session_stats ss ON ss.source_id = p.source_id AND ss.project_slug = p.slug
      LEFT JOIN token_totals tt ON tt.source_id = p.source_id AND tt.project_slug = p.slug
      ${where}
    `,
      ...params,
    );

    return rows.map((row) => this.toProjectSummary(row));
  }

  getProjectTokenActivity(
    projectSlug: string,
    options: { sourceId?: string; from: string; to: string },
  ): TokenActivityBucketData[] {
    return readTokenActivity(this.db, projectSlug, options);
  }

  getSessionSummaries(projectSlug: string, options?: { sourceId?: string }): SessionSummaryData[] {
    const sourceClause = options?.sourceId ? ' AND s.source_id = ?' : '';
    const params: unknown[] = options?.sourceId ? [projectSlug, options.sourceId] : [projectSlug];
    const rows = this.db.all<SessionSummaryRow>(
      `
      SELECT
        s.id,
        s.source_id,
        s.project_slug,
        s.full_path,
        COALESCE(s.first_prompt, '') as first_prompt,
        COALESCE(s.summary, '') as summary,
        COALESCE(NULLIF(s.custom_title, ''), NULLIF(s.ai_title, ''), '') as title,
        COALESCE(s.git_branch, '') as git_branch,
        COALESCE(s.project_path, '') as project_path,
        COALESCE(s.is_sidechain, 0) as is_sidechain,
        COALESCE(s.created_at, '1970-01-01') as created_at,
        COALESCE(s.modified_at, '1970-01-01') as modified_at,
        COALESCE(tt.parent_message_count, 0) as message_count,
        COALESCE(tt.input_tokens, 0) as input_tokens,
        COALESCE(tt.output_tokens, 0) as output_tokens,
        COALESCE(tt.cache_creation_tokens, 0) as cache_creation_tokens,
        COALESCE(tt.cache_read_tokens, 0) as cache_read_tokens,
        COALESCE(s.tokens_estimated, 0) as tokens_estimated,
        COALESCE((SELECT COUNT(*) FROM todos WHERE session_id = s.id), 0) as todo_count,
        s.plan_slug,
        COALESCE(s.has_task, 0) as has_task
      FROM sessions s
      LEFT JOIN session_summary_totals tt
        ON tt.source_id = s.source_id AND tt.project_slug = s.project_slug AND tt.session_id = s.id
      WHERE s.project_slug = ?${sourceClause}
    `,
      ...params,
    );

    return rows.map((row) => this.toSessionSummary(row));
  }

  // ─────────────────────────────────────────────────────────────────────────
  // Messages
  // ─────────────────────────────────────────────────────────────────────────

  getSessionMessages(
    slug: string,
    sessionId: string,
    limit: number,
    offset: number,
    options?: { sourceId?: string },
  ): { messages: unknown[]; total: number; offset: number; hasMore: boolean } {
    const sourceClause = options?.sourceId ? ' AND source_id = ?' : '';
    const baseParams: unknown[] = options?.sourceId ? [slug, sessionId, options.sourceId] : [slug, sessionId];

    const countRow = this.db.get<CountRow>(
      `SELECT COUNT(*) as count FROM messages WHERE project_slug = ? AND session_id = ?${sourceClause}`,
      ...baseParams,
    );
    const total = countRow?.count ?? 0;

    const rows = this.db.all<MessageDataRow>(
      `SELECT data, timestamp FROM messages WHERE project_slug = ? AND session_id = ?${sourceClause} ORDER BY msg_index LIMIT ? OFFSET ?`,
      ...baseParams,
      limit,
      offset,
    );

    const messages = rows
      .map((r) => {
        try {
          const msg = JSON.parse(r.data) as unknown;
          // Prefer the typed column when present (Grok events.jsonl join, etc.).
          if (msg && typeof msg === 'object' && r.timestamp) {
            const rec = msg as Record<string, unknown>;
            if (!rec.timestamp || rec.timestamp === '') {
              rec.timestamp = r.timestamp;
            }
          }
          return msg;
        } catch {
          return null;
        }
      })
      .filter((m) => m !== null);

    return {
      messages,
      total,
      offset,
      hasMore: offset + rows.length < total,
    };
  }

  getSessionTimelineFacets(slug: string, sessionId: string, options?: { sourceId?: string }): TimelineFacets {
    ensureTimelineProjection(this.db, sessionId);
    ensureSessionSubagentProjections(this.db, sessionId, options?.sourceId);
    const sourceClause = options?.sourceId ? ' AND source_id = ?' : '';
    const params: unknown[] = options?.sourceId ? [slug, sessionId, options.sourceId] : [slug, sessionId];

    const messageRows = this.db.all<FacetRow>(
      `SELECT display_type AS name, COUNT(*) AS count
         FROM timeline_messages
        WHERE project_slug = ? AND session_id = ?${sourceClause}
        GROUP BY display_type`,
      ...params,
    );
    const toolRows = this.db.all<FacetRow>(
      `SELECT tool_name AS name, COUNT(*) AS count
         FROM timeline_messages
        WHERE project_slug = ? AND session_id = ?${sourceClause} AND tool_name IS NOT NULL
        GROUP BY tool_name
        ORDER BY count DESC, tool_name`,
      ...params,
    );
    const agentMessageRows = this.db.all<FacetRow>(
      `SELECT display_type AS name, COUNT(*) AS count
         FROM subagent_timeline_messages
        WHERE project_slug = ? AND session_id = ?${sourceClause}
        GROUP BY display_type`,
      ...params,
    );
    const agentToolRows = this.db.all<FacetRow>(
      `SELECT tool_name AS name, COUNT(*) AS count
         FROM subagent_timeline_messages
        WHERE project_slug = ? AND session_id = ?${sourceClause} AND tool_name IS NOT NULL
        GROUP BY tool_name`,
      ...params,
    );
    const messageCounts: Record<string, number> = {};
    const toolCounts: Record<string, number> = {};
    for (const row of [...messageRows, ...agentMessageRows]) {
      messageCounts[row.name] = (messageCounts[row.name] ?? 0) + row.count;
    }
    for (const row of [...toolRows, ...agentToolRows]) {
      toolCounts[row.name] = (toolCounts[row.name] ?? 0) + row.count;
    }
    return {
      total: [...messageRows, ...agentMessageRows].reduce((sum, row) => sum + row.count, 0),
      messageCounts,
      toolCounts,
    };
  }

  getSessionTimeline(slug: string, sessionId: string, request: TimelinePageRequest = {}): TimelinePage {
    ensureTimelineProjection(this.db, sessionId);
    const { sql: filterSql, params: filterParams } = buildTimelineFilter(request);
    const sourceSql = request.sourceId ? ' AND source_id = ?' : '';
    const scopeParams: unknown[] = request.sourceId ? [slug, sessionId, request.sourceId] : [slug, sessionId];
    let where = `project_slug = ? AND session_id = ?${sourceSql}${filterSql}`;
    let baseParams: unknown[] = [...scopeParams, ...filterParams];

    // A matching branch still needs its Task/Agent row in the parent page;
    // otherwise a DB-level solo/search filter could produce branch counts but
    // no control through which the user can reveal those messages.
    if (filterSql) {
      ensureSessionSubagentProjections(this.db, sessionId, request.sourceId);
      const branchRows = this.db.all<{
        source_id: string;
        workflow_id: string;
        agent_id: string;
      }>(
        `SELECT DISTINCT source_id, workflow_id, agent_id
           FROM subagent_timeline_messages
          WHERE project_slug = ? AND session_id = ?${sourceSql}${filterSql}`,
        ...scopeParams,
        ...filterParams,
      );
      const matchingBranches = new Set(
        branchRows.map((row) => subagentIdentity(row.source_id, row.workflow_id, row.agent_id)),
      );
      const anchorIds = this.listSubagents(slug, sessionId, undefined, { sourceId: request.sourceId })
        .filter(
          (thread) =>
            thread.spawnToolId &&
            matchingBranches.has(subagentIdentity(thread.sourceId, thread.workflowId, thread.agentId)),
        )
        .map((thread) => thread.spawnToolId!);
      if (anchorIds.length) {
        where = `project_slug = ? AND session_id = ?${sourceSql} AND ((1 = 1${filterSql}) OR tool_use_id IN (${placeholders(anchorIds)}))`;
        baseParams = [...scopeParams, ...filterParams, ...anchorIds];
      }
    }
    const total =
      this.db.get<CountRow>(`SELECT COUNT(*) AS count FROM timeline_messages WHERE ${where}`, ...baseParams)?.count ??
      0;

    const limit = Math.max(1, Math.min(500, Math.floor(request.limit ?? 30)));
    const cursorSql = request.before == null ? '' : ' AND timeline_index < ?';
    const cursorParams = request.before == null ? [] : [request.before];
    const rows = this.db.all<TimelineDataRow>(
      `SELECT timeline_index, data
         FROM timeline_messages
        WHERE ${where}${cursorSql}
        ORDER BY timeline_index DESC
        LIMIT ?`,
      ...baseParams,
      ...cursorParams,
      limit + 1,
    );
    const hasMore = rows.length > limit;
    const pageRows = rows.slice(0, limit);
    const messages = pageRows
      .map((row) => {
        try {
          const message = JSON.parse(row.data) as TimelinePage['messages'][number];
          // Source UUIDs are domain references, not row identities: Codex may
          // omit them, checkpoints reuse their referenced messageId, and some
          // system events repeat UUIDs. The normalized row index is unique.
          message.timelineId = `${sessionId}:${row.timeline_index}`;
          return message;
        } catch {
          return null;
        }
      })
      .filter((message): message is TimelinePage['messages'][number] => message !== null)
      .reverse();

    return {
      messages,
      total,
      nextCursor: hasMore ? pageRows[pageRows.length - 1]?.timeline_index : undefined,
      hasMore,
    };
  }

  // ─────────────────────────────────────────────────────────────────────────
  // Subagents
  // ─────────────────────────────────────────────────────────────────────────

  getSessionSubagents(
    slug: string,
    sessionId: string,
    options?: { sourceId?: string; includeNested?: boolean },
  ): SubagentListItem[] {
    return this.listSubagents(slug, sessionId, options?.includeNested ? undefined : '', options);
  }

  getSessionWorkflows(
    slug: string,
    sessionId: string,
  ): Array<{
    workflowId: string;
    name: string;
    status: string;
    agentCount: number;
    totalTokens: number;
    totalToolCalls: number;
    durationMs: number;
    subagentCount: number;
  }> {
    const rows = this.db.all<WorkflowRow>(
      'SELECT workflow_id, name, status, agent_count, total_tokens, total_tool_calls, duration_ms, subagent_count FROM workflows WHERE project_slug = ? AND session_id = ? ORDER BY workflow_id',
      slug,
      sessionId,
    );
    return rows.map((r) => ({
      workflowId: r.workflow_id,
      name: r.name,
      status: r.status,
      agentCount: r.agent_count,
      totalTokens: r.total_tokens,
      totalToolCalls: r.total_tool_calls,
      durationMs: r.duration_ms,
      subagentCount: r.subagent_count,
    }));
  }

  getWorkflowSubagents(
    slug: string,
    sessionId: string,
    workflowId: string,
    options?: { sourceId?: string },
  ): SubagentListItem[] {
    return this.listSubagents(slug, sessionId, workflowId, options);
  }

  getSubagentMessages(
    slug: string,
    sessionId: string,
    agentId: string,
    limit: number,
    offset: number,
    workflowId?: string,
    options?: { sourceId?: string },
  ): { messages: unknown[]; total: number; offset: number; hasMore: boolean } {
    const sourceId = options?.sourceId ?? this.resolveSubagentSource(slug, sessionId, agentId, workflowId);
    if (!sourceId) return { messages: [], total: 0, offset, hasMore: false };
    const resolvedWorkflowId = this.resolveSubagentWorkflow(sourceId, sessionId, agentId, workflowId);
    if (resolvedWorkflowId == null) return { messages: [], total: 0, offset, hasMore: false };
    const params = [sourceId, sessionId, resolvedWorkflowId, agentId];
    const total =
      this.db.get<CountRow>(
        `SELECT COUNT(*) AS count FROM subagent_messages
          WHERE source_id = ? AND session_id = ? AND workflow_id = ? AND agent_id = ?`,
        ...params,
      )?.count ?? 0;
    const rows = this.db.all<SubagentRawMessageRow>(
      `SELECT data, timestamp FROM subagent_messages
        WHERE source_id = ? AND session_id = ? AND workflow_id = ? AND agent_id = ?
        ORDER BY msg_index LIMIT ? OFFSET ?`,
      ...params,
      limit,
      offset,
    );
    const messages = rows
      .map((row) => {
        try {
          const message = JSON.parse(row.data) as Record<string, unknown>;
          if (!message.timestamp && row.timestamp) message.timestamp = row.timestamp;
          return message;
        } catch {
          return null;
        }
      })
      .filter((message): message is Record<string, unknown> => message !== null);

    return {
      messages,
      total,
      offset,
      hasMore: offset + rows.length < total,
    };
  }

  getSubagentTimeline(
    slug: string,
    sessionId: string,
    agentId: string,
    request: SubagentTimelinePageRequest,
  ): SubagentTimelinePage {
    const workflowId = this.resolveSubagentWorkflow(request.sourceId, sessionId, agentId, request.workflowId);
    if (workflowId == null) return { messages: [], total: 0, offset: request.offset ?? 0, hasMore: false };
    ensureSubagentTimelineProjection(this.db, {
      sourceId: request.sourceId,
      projectSlug: slug,
      sessionId,
      workflowId,
      agentId,
    });
    const { sql: filterSql, params: filterParams } = buildTimelineFilter(request);
    const params: unknown[] = [request.sourceId, slug, sessionId, workflowId, agentId, ...filterParams];
    const where = `source_id = ? AND project_slug = ? AND session_id = ? AND workflow_id = ? AND agent_id = ?${filterSql}`;
    const total =
      this.db.get<CountRow>(`SELECT COUNT(*) AS count FROM subagent_timeline_messages WHERE ${where}`, ...params)
        ?.count ?? 0;
    const limit = Math.max(1, Math.min(500, Math.floor(request.limit ?? 80)));
    const offset = Math.max(0, Math.floor(request.offset ?? 0));
    const rows = this.db.all<TimelineDataRow>(
      `SELECT timeline_index, data FROM subagent_timeline_messages
        WHERE ${where} ORDER BY timeline_index LIMIT ? OFFSET ?`,
      ...params,
      limit,
      offset,
    );
    const thread = this.listSubagents(slug, sessionId, workflowId, { sourceId: request.sourceId }).find(
      (candidate) => candidate.agentId === agentId,
    );
    const messages = rows
      .map((row) => {
        try {
          const message = JSON.parse(row.data) as SubagentTimelinePage['messages'][number];
          message.timelineId = `${sessionId}:${workflowId}:${agentId}:${row.timeline_index}`;
          message.branchToolId = thread?.spawnToolId ?? undefined;
          return message;
        } catch {
          return null;
        }
      })
      .filter((message): message is SubagentTimelinePage['messages'][number] => message !== null);
    return { messages, total, offset, hasMore: offset + rows.length < total };
  }

  // ─────────────────────────────────────────────────────────────────────────
  // Details
  // ─────────────────────────────────────────────────────────────────────────

  getProjectMemory(slug: string, options?: { sourceId?: string }): string | null {
    // project_memories is Claude-only today (no source_id column). Never
    // surface Claude's MEMORY.md under a non-Claude project that happens to
    // share the same slug.
    if (options?.sourceId && options.sourceId !== 'claude-code') {
      return null;
    }
    const row = this.db.get<MemoryRow>('SELECT content FROM project_memories WHERE project_slug = ?', slug);
    return row?.content ?? null;
  }

  getSessionTodos(slug: string, sessionId: string): unknown[] {
    // slug unused in todos table — match by session_id
    void slug;
    const rows = this.db.all<TodoRow>('SELECT items FROM todos WHERE session_id = ?', sessionId);
    const result: unknown[] = [];
    for (const row of rows) {
      try {
        const parsed = JSON.parse(row.items);
        result.push(parsed);
      } catch {
        // skip bad todo JSON
      }
    }
    return result;
  }

  getSessionPlan(slug: string, sessionId: string): unknown | null {
    // Look up the session's plan_slug, then fetch the plan
    void slug;
    const sessionRow = this.db.get<{ plan_slug: string | null }>(
      'SELECT plan_slug FROM sessions WHERE id = ?',
      sessionId,
    );
    if (!sessionRow?.plan_slug) return null;

    const planRow = this.db.get<PlanRow>(
      'SELECT slug, title, content, size FROM plans WHERE slug = ?',
      sessionRow.plan_slug,
    );
    if (!planRow) return null;
    return { slug: planRow.slug, title: planRow.title, content: planRow.content, size: planRow.size };
  }

  getSessionTask(slug: string, sessionId: string): unknown | null {
    void slug;
    const row = this.db.get<TaskRow>(
      'SELECT session_id, has_highwatermark, highwatermark, lock_exists FROM tasks WHERE session_id = ?',
      sessionId,
    );
    if (!row) return null;
    return {
      taskId: row.session_id,
      hasHighwatermark: !!row.has_highwatermark,
      highwatermark: row.highwatermark,
      lockExists: !!row.lock_exists,
    };
  }

  getToolResult(slug: string, sessionId: string, toolUseId: string): string | null {
    const row = this.db.get<ToolResultRow>(
      'SELECT content FROM tool_results WHERE project_slug = ? AND session_id = ? AND tool_use_id = ?',
      slug,
      sessionId,
      toolUseId,
    );
    return row?.content ?? null;
  }

  // ─────────────────────────────────────────────────────────────────────────
  // Search
  // ─────────────────────────────────────────────────────────────────────────

  search(query: SearchQuery): SearchResultSet {
    const limit = query.limit ?? 50;
    const offset = query.offset ?? 0;
    ensureSearchableSubagentProjections(this.db, query);

    // Build the FTS5 MATCH expression
    const matchExpr = escapeFts5(query.text);

    // Build WHERE clauses for main-session messages.
    const whereParts: string[] = [];
    const whereParams: unknown[] = [];

    if (query.projectMembers && query.projectMembers.length > 0) {
      whereParts.push(`(${query.projectMembers.map(() => '(m.source_id = ? AND m.project_slug = ?)').join(' OR ')})`);
      for (const member of query.projectMembers) {
        whereParams.push(member.sourceId, member.slug);
      }
    } else if (query.projectSlug) {
      whereParts.push('m.project_slug = ?');
      whereParams.push(query.projectSlug);
    }
    if (query.sessionId) {
      whereParts.push('m.session_id = ?');
      whereParams.push(query.sessionId);
    }
    if (query.type && query.type !== 'message' && query.type !== 'subagent') {
      whereParts.push('m.msg_type = ?');
      whereParams.push(query.type);
    }
    if (query.sourceId) {
      whereParts.push('m.source_id = ?');
      whereParams.push(query.sourceId);
    }

    const whereClause = whereParts.length > 0 ? `AND ${whereParts.join(' AND ')}` : '';
    const includeMain = query.type !== 'subagent';
    const includeSubagents = !query.type || query.type === 'subagent';

    // Count query
    const mainTotal = includeMain
      ? (this.db.get<CountRow>(
          `SELECT COUNT(*) as count
           FROM search_fts
           JOIN messages m ON m.id = search_fts.rowid
           WHERE search_fts MATCH ? ${whereClause}`,
          matchExpr,
          ...whereParams,
        )?.count ?? 0)
      : 0;

    // Result query
    const rows = includeMain
      ? this.db.all<SearchFtsRow>(
          `SELECT m.project_slug, m.session_id, m.msg_index, m.source_id,
                  snippet(search_fts, 0, '<b>', '</b>', '...', 64) as snippet,
                  rank
           FROM search_fts
           JOIN messages m ON m.id = search_fts.rowid
           WHERE search_fts MATCH ? ${whereClause}
           ORDER BY rank
           LIMIT ?`,
          matchExpr,
          ...whereParams,
          limit + offset,
        )
      : [];

    const agentWhereParts: string[] = [];
    const agentWhereParams: unknown[] = [];
    if (query.projectMembers?.length) {
      agentWhereParts.push(
        `(${query.projectMembers.map(() => '(a.source_id = ? AND a.project_slug = ?)').join(' OR ')})`,
      );
      for (const member of query.projectMembers) agentWhereParams.push(member.sourceId, member.slug);
    } else if (query.projectSlug) {
      agentWhereParts.push('a.project_slug = ?');
      agentWhereParams.push(query.projectSlug);
    }
    if (query.sessionId) {
      agentWhereParts.push('a.session_id = ?');
      agentWhereParams.push(query.sessionId);
    }
    if (query.sourceId) {
      agentWhereParts.push('a.source_id = ?');
      agentWhereParams.push(query.sourceId);
    }
    const agentWhere = agentWhereParts.length ? `AND ${agentWhereParts.join(' AND ')}` : '';
    const agentTotal = includeSubagents
      ? (this.db.get<CountRow>(
          `SELECT COUNT(*) AS count
             FROM subagent_search_fts
             JOIN subagent_timeline_messages a ON a.id = subagent_search_fts.rowid
            WHERE subagent_search_fts MATCH ? ${agentWhere}`,
          matchExpr,
          ...agentWhereParams,
        )?.count ?? 0)
      : 0;
    const agentRows = includeSubagents
      ? this.db.all<SubagentSearchFtsRow>(
          `SELECT a.project_slug, a.session_id, a.timeline_index AS msg_index, a.timeline_index,
                  a.source_id, a.agent_id, a.workflow_id, s.spawn_tool_id,
                  snippet(subagent_search_fts, 0, '<b>', '</b>', '...', 64) AS snippet,
                  rank
             FROM subagent_search_fts
             JOIN subagent_timeline_messages a ON a.id = subagent_search_fts.rowid
             JOIN subagents s ON s.source_id = a.source_id AND s.project_slug = a.project_slug
               AND s.session_id = a.session_id AND s.workflow_id = a.workflow_id AND s.agent_id = a.agent_id
            WHERE subagent_search_fts MATCH ? ${agentWhere}
            ORDER BY rank
            LIMIT ?`,
          matchExpr,
          ...agentWhereParams,
          limit + offset,
        )
      : [];

    const merged = [
      ...rows.map((row) => ({
        key: `message:${row.project_slug}/${row.session_id}/${row.msg_index}`,
        type: 'message' as const,
        snippet: row.snippet,
        rank: row.rank,
        projectSlug: row.project_slug || undefined,
        sessionId: row.session_id || undefined,
        sourceId: row.source_id || undefined,
      })),
      ...agentRows.map((row) => ({
        key: `subagent:${row.project_slug}/${row.session_id}/${row.workflow_id}/${row.agent_id}/${row.timeline_index}`,
        type: 'subagent' as const,
        snippet: row.snippet,
        rank: row.rank,
        projectSlug: row.project_slug || undefined,
        sessionId: row.session_id || undefined,
        sourceId: row.source_id || undefined,
        agentId: row.agent_id,
        workflowId: row.workflow_id,
        spawnToolId: row.spawn_tool_id || undefined,
        agentTimelineIndex: row.timeline_index,
      })),
    ]
      .sort((a, b) => a.rank - b.rank)
      .slice(offset, offset + limit);
    const total = mainTotal + agentTotal;

    return {
      results: merged,
      total,
      hasMore: offset + merged.length < total,
    };
  }

  // ─────────────────────────────────────────────────────────────────────────
  // Stats
  // ─────────────────────────────────────────────────────────────────────────

  getStats(): StoreStats {
    const tables = [
      'projects',
      'sessions',
      'messages',
      'subagents',
      'subagent_messages',
      'subagent_timeline_messages',
      'tool_results',
      'todos',
      'tasks',
      'plans',
    ];
    const segmentsByType: Record<string, number> = {};
    let totalSegments = 0;

    for (const table of tables) {
      const row = this.db.get<CountRow>(`SELECT COUNT(*) as count FROM ${table}`);
      const count = row?.count ?? 0;
      segmentsByType[table] = count;
      totalSegments += count;
    }

    const fpRow = this.db.get<CountRow>('SELECT COUNT(*) as count FROM source_files');
    const totalFingerprints = fpRow?.count ?? 0;

    const ftsRow = this.db.get<CountRow>('SELECT COUNT(*) as count FROM search_fts');
    const subagentFtsRow = this.db.get<CountRow>('SELECT COUNT(*) as count FROM subagent_search_fts');
    const searchIndexed = (ftsRow?.count ?? 0) + (subagentFtsRow?.count ?? 0);

    const dbSizeBytes = this.db.getFileSize();

    return {
      totalSegments,
      segmentsByType,
      totalFingerprints,
      dbSizeBytes,
      searchIndexed,
    };
  }

  // ─────────────────────────────────────────────────────────────────────────
  // Private helpers
  // ─────────────────────────────────────────────────────────────────────────

  private resolveSubagentSource(slug: string, sessionId: string, agentId: string, workflowId?: string): string | null {
    const row = this.db.get<{ source_id: string }>(
      `SELECT source_id FROM subagents
        WHERE project_slug = ? AND session_id = ? AND agent_id = ?${workflowId === undefined ? '' : ' AND workflow_id = ?'}
        ORDER BY CASE WHEN workflow_id = '' THEN 0 ELSE 1 END, workflow_id LIMIT 1`,
      ...(workflowId === undefined ? [slug, sessionId, agentId] : [slug, sessionId, agentId, workflowId]),
    );
    return row?.source_id ?? null;
  }

  private resolveSubagentWorkflow(
    sourceId: string,
    sessionId: string,
    agentId: string,
    workflowId?: string,
  ): string | null {
    if (workflowId !== undefined) return workflowId;
    const row = this.db.get<{ workflow_id: string }>(
      `SELECT workflow_id FROM subagents
        WHERE source_id = ? AND session_id = ? AND agent_id = ?
        ORDER BY CASE WHEN workflow_id = '' THEN 0 ELSE 1 END, workflow_id LIMIT 1`,
      sourceId,
      sessionId,
      agentId,
    );
    return row?.workflow_id ?? null;
  }

  private listSubagents(
    slug: string,
    sessionId: string,
    workflowId: string | undefined,
    options?: { sourceId?: string },
  ): SubagentListItem[] {
    ensureTimelineProjection(this.db, sessionId);
    const workflowSql = workflowId === undefined ? '' : ' AND s.workflow_id = ?';
    const rowSourceSql = options?.sourceId ? ' AND s.source_id = ?' : '';
    const anchorSourceSql = options?.sourceId ? ' AND source_id = ?' : '';
    const rowParams: unknown[] = [slug, sessionId];
    if (workflowId !== undefined) rowParams.push(workflowId);
    if (options?.sourceId) rowParams.push(options.sourceId);
    const rows = this.db.all<SubagentRow>(
      `SELECT s.id, s.source_id, s.agent_id, s.agent_type, s.message_count,
              s.workflow_id, s.spawn_tool_id, s.link_method
         FROM subagents s
        WHERE s.project_slug = ? AND s.session_id = ?${workflowSql}${rowSourceSql}
        ORDER BY s.id`,
      ...rowParams,
    );
    const anchors = this.db.all<ToolAnchorRow>(
      `SELECT tool_use_id FROM timeline_messages
        WHERE project_slug = ? AND session_id = ?${anchorSourceSql}
          AND tool_name IN ('Task', 'Agent') AND tool_use_id IS NOT NULL
        ORDER BY timeline_index`,
      ...(options?.sourceId ? [slug, sessionId, options.sourceId] : [slug, sessionId]),
    );
    const explicitLinks = this.resolveSubagentSpawnToolIds(sessionId, options?.sourceId);
    const used = new Set(rows.map((row) => row.spawn_tool_id).filter((value): value is string => Boolean(value)));
    let ordinal = 0;
    return rows.map((row) => {
      let spawnToolId = row.spawn_tool_id ?? explicitLinks.get(row.agent_id) ?? null;
      let linkMethod: SubagentListItem['linkMethod'] = row.link_method;
      if (!row.spawn_tool_id && spawnToolId) linkMethod = 'tool_result';
      while (!spawnToolId && ordinal < anchors.length) {
        const candidate = anchors[ordinal++]!.tool_use_id;
        if (!used.has(candidate)) {
          spawnToolId = candidate;
          used.add(candidate);
          linkMethod = 'ordinal';
        }
      }
      return {
        sourceId: row.source_id,
        agentId: row.agent_id,
        agentType: row.agent_type,
        messageCount: row.message_count,
        workflowId: row.workflow_id,
        spawnToolId,
        linkMethod,
      };
    });
  }

  /** Resolve native-ingested sidecars using the agent id returned by Task/Agent. */
  private resolveSubagentSpawnToolIds(sessionId: string, sourceId?: string): Map<string, string> {
    const rows = this.db.all<{ data: string }>(
      `SELECT data FROM messages WHERE session_id = ?${sourceId ? ' AND source_id = ?' : ''} ORDER BY msg_index`,
      ...(sourceId ? [sessionId, sourceId] : [sessionId]),
    );
    const result = new Map<string, string>();
    const agentIds = this.db
      .all<{
        agent_id: string;
      }>(
        `SELECT DISTINCT agent_id FROM subagents WHERE session_id = ?${sourceId ? ' AND source_id = ?' : ''}`,
        ...(sourceId ? [sessionId, sourceId] : [sessionId]),
      )
      .map((row) => row.agent_id);
    for (const row of rows) {
      try {
        const raw = JSON.parse(row.data) as Record<string, unknown>;
        const message = raw.message as Record<string, unknown> | undefined;
        if (!Array.isArray(message?.content)) continue;
        for (const value of message.content) {
          const block = value as Record<string, unknown>;
          if (block.type !== 'tool_result' || typeof block.tool_use_id !== 'string') continue;
          const content = typeof block.content === 'string' ? block.content : JSON.stringify(block.content ?? '');
          for (const agentId of agentIds) {
            if (!result.has(agentId) && content.includes(agentId)) result.set(agentId, block.tool_use_id);
          }
        }
      } catch {
        /* malformed raw row */
      }
    }
    return result;
  }

  private toProjectSummary(row: ProjectSummaryRow): ProjectSummaryData {
    const originalPath = row.original_path ?? '';
    const parts = originalPath.split(/[\\/]/);
    const folderName = parts[parts.length - 1] || row.slug;

    const tokenUsage: TokenUsageSummary = {
      inputTokens: row.input_tokens,
      outputTokens: row.output_tokens,
      cacheCreationTokens: row.cache_creation_tokens,
      cacheReadTokens: row.cache_read_tokens,
      totalTokens: 0,
    };
    tokenUsage.totalTokens = normalizedTokenTotal(row.source_id, tokenUsage);

    return {
      slug: row.slug,
      sourceId: row.source_id,
      folderName,
      absolutePath: originalPath,
      sessionCount: row.session_count,
      messageCount: row.message_count,
      tokenUsage,
      tokensEstimated: !!row.tokens_estimated,
      lastActiveAt: row.last_active_at,
      firstActiveAt: row.first_active_at,
      latestGitBranch: row.latest_git_branch ?? '',
      latestPrompt: row.latest_prompt ?? '',
      hasMemory: !!row.has_memory,
    };
  }

  private toSessionSummary(row: SessionSummaryRow): SessionSummaryData {
    const createdAt = row.created_at || '1970-01-01';
    const modifiedAt = row.modified_at || '1970-01-01';

    let lifespanMs = 0;
    try {
      const start = new Date(createdAt).getTime();
      const end = new Date(modifiedAt).getTime();
      if (!isNaN(start) && !isNaN(end)) {
        lifespanMs = Math.max(0, end - start);
      }
    } catch {
      // ignore date parsing errors
    }

    const tokenUsage: TokenUsageSummary = {
      inputTokens: row.input_tokens,
      outputTokens: row.output_tokens,
      cacheCreationTokens: row.cache_creation_tokens,
      cacheReadTokens: row.cache_read_tokens,
      totalTokens: 0,
    };
    tokenUsage.totalTokens = normalizedTokenTotal(row.source_id, tokenUsage);

    return {
      sessionId: row.id,
      sourceId: row.source_id,
      projectSlug: row.project_slug,
      startTime: createdAt,
      lastUpdate: modifiedAt,
      lifespanMs,
      tokenUsage,
      tokensEstimated: !!row.tokens_estimated,
      messageCount: row.message_count,
      fullPath: row.full_path ?? '',
      summary: row.summary ?? '',
      title: row.title ?? '',
      firstPrompt: row.first_prompt ?? '',
      gitBranch: row.git_branch ?? '',
      todoCount: row.todo_count,
      planSlug: row.plan_slug ?? null,
      hasTask: !!row.has_task,
      isSidechain: !!row.is_sidechain,
    };
  }
}

function placeholders(values: readonly unknown[]): string {
  return values.map(() => '?').join(', ');
}

function subagentIdentity(sourceId: string, workflowId: string, agentId: string): string {
  return JSON.stringify([sourceId, workflowId, agentId]);
}

function buildTimelineFilter(request: TimelinePageRequest): { sql: string; params: unknown[] } {
  const clauses: string[] = [];
  const params: unknown[] = [];
  const includes: string[] = [];

  if (request.includeTypes?.length) {
    includes.push(`display_type IN (${placeholders(request.includeTypes)})`);
    params.push(...request.includeTypes);
  }
  if (request.includeTools?.length) {
    includes.push(`tool_name IN (${placeholders(request.includeTools)})`);
    params.push(...request.includeTools);
  }
  if (includes.length) clauses.push(`(${includes.join(' OR ')})`);

  if (request.excludeTypes?.length) {
    clauses.push(`display_type NOT IN (${placeholders(request.excludeTypes)})`);
    params.push(...request.excludeTypes);
  }
  if (request.excludeTools?.length) {
    clauses.push(`(tool_name IS NULL OR tool_name NOT IN (${placeholders(request.excludeTools)}))`);
    params.push(...request.excludeTools);
  }
  const search = request.search?.trim();
  if (search) {
    clauses.push('instr(lower(search_text), lower(?)) > 0');
    params.push(search);
  }
  return { sql: clauses.length ? ` AND ${clauses.join(' AND ')}` : '', params };
}

function escapeFts5(text: string): string {
  return `"${text.replace(/"/g, '""')}"`;
}

// ═══════════════════════════════════════════════════════════════════════════
// FACTORY
// ═══════════════════════════════════════════════════════════════════════════

export function createQueryService(sqliteServiceFactory: () => SqliteService): QueryService {
  return new QueryServiceImpl(sqliteServiceFactory);
}
