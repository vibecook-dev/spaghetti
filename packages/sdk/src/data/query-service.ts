/**
 * QueryService — Read-only query layer for the Phase 3 dedicated-table schema
 *
 * All methods return domain types directly. No segment abstraction.
 */

import type { SqliteService } from '../io/index.js';
import type { ProjectSummaryData, SessionSummaryData, TokenUsageSummary } from './summary-types.js';
import type { SearchQuery, SearchResultSet, StoreStats } from './segment-types.js';
import { initializeSchema } from './schema.js';
import { ensureTimelineProjection } from './timeline-projection.js';
import type { TimelineFacets, TimelinePage, TimelinePageRequest } from './timeline-query.js';

// ═══════════════════════════════════════════════════════════════════════════
// INTERFACE
// ═══════════════════════════════════════════════════════════════════════════

export interface QueryService {
  open(dbPath: string): void;
  close(): void;
  isOpen(): boolean;

  // Projects
  getProjectSlugs(): string[];
  /** Distinct agent sources present in the index. */
  getSourceIds(): string[];
  getProjectSummaries(options?: { sourceId?: string }): ProjectSummaryData[];
  getSessionSummaries(projectSlug: string, options?: { sourceId?: string }): SessionSummaryData[];
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
  ): Array<{ agentId: string; agentType: string; messageCount: number }>;
  getSubagentMessages(
    slug: string,
    sessionId: string,
    agentId: string,
    limit: number,
    offset: number,
    workflowId?: string,
  ): { messages: unknown[]; total: number; offset: number; hasMore: boolean };

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
  ): Array<{ agentId: string; agentType: string; messageCount: number }>;

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
  has_memory: number;
}

interface SessionSummaryRow {
  id: string;
  source_id: string;
  project_slug: string;
  full_path: string;
  first_prompt: string;
  summary: string;
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
  agent_id: string;
  agent_type: string;
  message_count: number;
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

interface SubagentMessagesRow {
  messages: string;
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
      SELECT p.slug, p.source_id, p.original_path,
        (SELECT COUNT(*) FROM sessions WHERE project_slug = p.slug AND source_id = p.source_id) as session_count,
        COALESCE((SELECT SUM(mc.cnt) FROM (SELECT COUNT(*) as cnt FROM messages WHERE project_slug = p.slug AND source_id = p.source_id GROUP BY session_id) mc), 0) as message_count,
        COALESCE((SELECT SUM(input_tokens) FROM messages WHERE project_slug = p.slug AND source_id = p.source_id), 0) as input_tokens,
        COALESCE((SELECT SUM(output_tokens) FROM messages WHERE project_slug = p.slug AND source_id = p.source_id), 0) as output_tokens,
        COALESCE((SELECT SUM(cache_creation_tokens) FROM messages WHERE project_slug = p.slug AND source_id = p.source_id), 0) as cache_creation_tokens,
        COALESCE((SELECT SUM(cache_read_tokens) FROM messages WHERE project_slug = p.slug AND source_id = p.source_id), 0) as cache_read_tokens,
        COALESCE((SELECT MAX(tokens_estimated) FROM sessions WHERE project_slug = p.slug AND source_id = p.source_id), 0) as tokens_estimated,
        COALESCE((SELECT MAX(modified_at) FROM sessions WHERE project_slug = p.slug AND source_id = p.source_id), '1970-01-01') as last_active_at,
        COALESCE((SELECT MIN(created_at) FROM sessions WHERE project_slug = p.slug AND source_id = p.source_id), '1970-01-01') as first_active_at,
        (SELECT git_branch FROM sessions WHERE project_slug = p.slug AND source_id = p.source_id ORDER BY modified_at DESC LIMIT 1) as latest_git_branch,
        CASE
          WHEN p.source_id = 'claude-code'
          THEN EXISTS(SELECT 1 FROM project_memories WHERE project_slug = p.slug)
          ELSE 0
        END as has_memory
      FROM projects p
      ${where}
    `,
      ...params,
    );

    return rows.map((row) => this.toProjectSummary(row));
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
        COALESCE(s.git_branch, '') as git_branch,
        COALESCE(s.project_path, '') as project_path,
        COALESCE(s.is_sidechain, 0) as is_sidechain,
        COALESCE(s.created_at, '1970-01-01') as created_at,
        COALESCE(s.modified_at, '1970-01-01') as modified_at,
        COALESCE((SELECT COUNT(*) FROM messages WHERE session_id = s.id AND project_slug = s.project_slug AND source_id = s.source_id), 0) as message_count,
        COALESCE((SELECT SUM(input_tokens) FROM messages WHERE session_id = s.id AND project_slug = s.project_slug AND source_id = s.source_id), 0) as input_tokens,
        COALESCE((SELECT SUM(output_tokens) FROM messages WHERE session_id = s.id AND project_slug = s.project_slug AND source_id = s.source_id), 0) as output_tokens,
        COALESCE((SELECT SUM(cache_creation_tokens) FROM messages WHERE session_id = s.id AND project_slug = s.project_slug AND source_id = s.source_id), 0) as cache_creation_tokens,
        COALESCE((SELECT SUM(cache_read_tokens) FROM messages WHERE session_id = s.id AND project_slug = s.project_slug AND source_id = s.source_id), 0) as cache_read_tokens,
        COALESCE(s.tokens_estimated, 0) as tokens_estimated,
        COALESCE((SELECT COUNT(*) FROM todos WHERE session_id = s.id), 0) as todo_count,
        s.plan_slug,
        COALESCE(s.has_task, 0) as has_task
      FROM sessions s
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
    const messageCounts = Object.fromEntries(messageRows.map((row) => [row.name, row.count]));
    const toolCounts = Object.fromEntries(toolRows.map((row) => [row.name, row.count]));
    return {
      total: messageRows.reduce((sum, row) => sum + row.count, 0),
      messageCounts,
      toolCounts,
    };
  }

  getSessionTimeline(slug: string, sessionId: string, request: TimelinePageRequest = {}): TimelinePage {
    ensureTimelineProjection(this.db, sessionId);
    const { sql: filterSql, params: filterParams } = buildTimelineFilter(request);
    const sourceSql = request.sourceId ? ' AND source_id = ?' : '';
    const baseParams: unknown[] = request.sourceId
      ? [slug, sessionId, request.sourceId, ...filterParams]
      : [slug, sessionId, ...filterParams];
    const where = `project_slug = ? AND session_id = ?${sourceSql}${filterSql}`;
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
  ): Array<{ agentId: string; agentType: string; messageCount: number }> {
    // Top-level subagents only (workflow_id ''); workflow-nested ones are
    // surfaced under their run via getWorkflowSubagents.
    const rows = this.db.all<SubagentRow>(
      "SELECT agent_id, agent_type, message_count FROM subagents WHERE project_slug = ? AND session_id = ? AND workflow_id = '' ORDER BY agent_id",
      slug,
      sessionId,
    );
    return rows.map((r) => ({
      agentId: r.agent_id,
      agentType: r.agent_type,
      messageCount: r.message_count,
    }));
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
  ): Array<{ agentId: string; agentType: string; messageCount: number }> {
    const rows = this.db.all<SubagentRow>(
      'SELECT agent_id, agent_type, message_count FROM subagents WHERE project_slug = ? AND session_id = ? AND workflow_id = ? ORDER BY agent_id',
      slug,
      sessionId,
      workflowId,
    );
    return rows.map((r) => ({
      agentId: r.agent_id,
      agentType: r.agent_type,
      messageCount: r.message_count,
    }));
  }

  getSubagentMessages(
    slug: string,
    sessionId: string,
    agentId: string,
    limit: number,
    offset: number,
    workflowId?: string,
  ): { messages: unknown[]; total: number; offset: number; hasMore: boolean } {
    // The subagents key is (project_slug, session_id, workflow_id, agent_id):
    // the same agent_id can exist top-level (workflow_id = '') AND under a
    // workflow. With an explicit workflowId we match exactly; without one,
    // prefer the top-level transcript deterministically instead of letting
    // SQLite pick an arbitrary row.
    const row =
      workflowId !== undefined
        ? this.db.get<SubagentMessagesRow>(
            'SELECT messages FROM subagents WHERE project_slug = ? AND session_id = ? AND agent_id = ? AND workflow_id = ?',
            slug,
            sessionId,
            agentId,
            workflowId,
          )
        : this.db.get<SubagentMessagesRow>(
            `SELECT messages FROM subagents WHERE project_slug = ? AND session_id = ? AND agent_id = ?
             ORDER BY CASE WHEN workflow_id = '' THEN 0 ELSE 1 END, workflow_id LIMIT 1`,
            slug,
            sessionId,
            agentId,
          );

    if (!row) {
      return { messages: [], total: 0, offset, hasMore: false };
    }

    let allMessages: unknown[];
    try {
      allMessages = JSON.parse(row.messages) as unknown[];
    } catch {
      allMessages = [];
    }

    const total = allMessages.length;
    const paged = allMessages.slice(offset, offset + limit);

    return {
      messages: paged,
      total,
      offset,
      hasMore: offset + paged.length < total,
    };
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

    // Build the FTS5 MATCH expression
    const matchExpr = escapeFts5(query.text);

    // Build WHERE clauses for additional filters (applied as JOIN conditions)
    const whereParts: string[] = [];
    const whereParams: unknown[] = [];

    if (query.projectSlug) {
      whereParts.push('m.project_slug = ?');
      whereParams.push(query.projectSlug);
    }
    if (query.sessionId) {
      whereParts.push('m.session_id = ?');
      whereParams.push(query.sessionId);
    }
    if (query.type) {
      whereParts.push('m.msg_type = ?');
      whereParams.push(query.type);
    }
    if (query.sourceId) {
      whereParts.push('m.source_id = ?');
      whereParams.push(query.sourceId);
    }

    const whereClause = whereParts.length > 0 ? `AND ${whereParts.join(' AND ')}` : '';

    // Count query
    const countRow = this.db.get<CountRow>(
      `SELECT COUNT(*) as count
       FROM search_fts
       JOIN messages m ON m.id = search_fts.rowid
       WHERE search_fts MATCH ? ${whereClause}`,
      matchExpr,
      ...whereParams,
    );
    const total = countRow?.count ?? 0;

    // Result query
    const rows = this.db.all<SearchFtsRow>(
      `SELECT m.project_slug, m.session_id, m.msg_index, m.source_id,
              snippet(search_fts, 0, '<b>', '</b>', '...', 64) as snippet,
              rank
       FROM search_fts
       JOIN messages m ON m.id = search_fts.rowid
       WHERE search_fts MATCH ? ${whereClause}
       ORDER BY rank
       LIMIT ? OFFSET ?`,
      matchExpr,
      ...whereParams,
      limit,
      offset,
    );

    return {
      results: rows.map((row) => ({
        key: `message:${row.project_slug}/${row.session_id}/${row.msg_index}`,
        type: 'message' as const,
        snippet: row.snippet,
        rank: row.rank,
        projectSlug: row.project_slug || undefined,
        sessionId: row.session_id || undefined,
        sourceId: row.source_id || undefined,
      })),
      total,
      hasMore: offset + rows.length < total,
    };
  }

  // ─────────────────────────────────────────────────────────────────────────
  // Stats
  // ─────────────────────────────────────────────────────────────────────────

  getStats(): StoreStats {
    const tables = ['projects', 'sessions', 'messages', 'subagents', 'tool_results', 'todos', 'tasks', 'plans'];
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
    const searchIndexed = ftsRow?.count ?? 0;

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

  private toProjectSummary(row: ProjectSummaryRow): ProjectSummaryData {
    const originalPath = row.original_path ?? '';
    const parts = originalPath.split(/[\\/]/);
    const folderName = parts[parts.length - 1] || row.slug;

    const tokenUsage: TokenUsageSummary = {
      inputTokens: row.input_tokens,
      outputTokens: row.output_tokens,
      cacheCreationTokens: row.cache_creation_tokens,
      cacheReadTokens: row.cache_read_tokens,
    };

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
    };

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
