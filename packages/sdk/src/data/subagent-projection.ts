import type { SqliteService } from '../io/index.js';
import { transformRawMessagesToTimeline } from '../react/chat/transform-messages.js';
import { timelineSearchText } from './timeline-projection.js';

export interface SubagentThreadRef {
  sourceId: string;
  projectSlug: string;
  sessionId: string;
  workflowId: string;
  agentId: string;
}

interface DirtyThreadRow {
  source_id: string;
  project_slug: string;
  session_id: string;
  workflow_id: string;
  agent_id: string;
}

interface RawRow {
  data: string;
  timestamp: string | null;
}

export function subagentThreadKey(ref: SubagentThreadRef): string {
  return JSON.stringify([ref.sourceId, ref.sessionId, ref.workflowId, ref.agentId]);
}

/** Materialize one dirty subagent transcript into normalized display rows. */
export function ensureSubagentTimelineProjection(db: SqliteService, ref: SubagentThreadRef): void {
  const dirty = db.get<DirtyThreadRow>(
    `SELECT source_id, project_slug, session_id, workflow_id, agent_id
       FROM subagent_dirty_threads
      WHERE source_id = ? AND session_id = ? AND workflow_id = ? AND agent_id = ?`,
    ref.sourceId,
    ref.sessionId,
    ref.workflowId,
    ref.agentId,
  );
  if (!dirty) return;

  const rows = db.all<RawRow>(
    `SELECT data, timestamp
       FROM subagent_messages
      WHERE source_id = ? AND session_id = ? AND workflow_id = ? AND agent_id = ?
      ORDER BY msg_index`,
    ref.sourceId,
    ref.sessionId,
    ref.workflowId,
    ref.agentId,
  );
  const rawMessages = rows
    .map((row) => {
      try {
        const value = JSON.parse(row.data) as Record<string, unknown>;
        if (!value.timestamp && row.timestamp) value.timestamp = row.timestamp;
        return value;
      } catch {
        return null;
      }
    })
    .filter((value): value is Record<string, unknown> => value !== null);

  // Sidechain user rows are injected agent prompts. Tool results are already
  // merged into their tool calls by the shared display transform.
  const timeline = transformRawMessagesToTimeline(rawMessages, { sourceId: ref.sourceId }).filter(
    (message) => !(message.type === 'user' && message.isSidechain),
  );

  db.exec('BEGIN IMMEDIATE');
  try {
    db.run(
      `DELETE FROM subagent_timeline_messages
        WHERE source_id = ? AND session_id = ? AND workflow_id = ? AND agent_id = ?`,
      ref.sourceId,
      ref.sessionId,
      ref.workflowId,
      ref.agentId,
    );
    const insert = db.prepare(
      `INSERT INTO subagent_timeline_messages
         (source_id, project_slug, session_id, workflow_id, agent_id, timeline_index,
          display_type, tool_name, tool_use_id, search_text, data)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    );
    const branchKey = subagentThreadKey(ref);
    for (let index = 0; index < timeline.length; index++) {
      const message = timeline[index];
      message.agentId = ref.agentId;
      message.isSidechain = true;
      message.branchKey = branchKey;
      insert.run(
        ref.sourceId,
        ref.projectSlug,
        ref.sessionId,
        ref.workflowId,
        ref.agentId,
        index,
        message.type,
        message.toolUse?.toolName ?? null,
        message.toolUse?.toolId ?? message.toolResult?.toolId ?? null,
        timelineSearchText(message),
        JSON.stringify(message),
      );
    }
    db.run(
      `DELETE FROM subagent_dirty_threads
        WHERE source_id = ? AND session_id = ? AND workflow_id = ? AND agent_id = ?`,
      ref.sourceId,
      ref.sessionId,
      ref.workflowId,
      ref.agentId,
    );
    db.exec('COMMIT');
  } catch (error) {
    try {
      db.exec('ROLLBACK');
    } catch {
      /* transaction may already have rolled back */
    }
    throw error;
  }
}

export function ensureSessionSubagentProjections(db: SqliteService, sessionId: string, sourceId?: string): void {
  const rows = db.all<DirtyThreadRow>(
    `SELECT source_id, project_slug, session_id, workflow_id, agent_id
       FROM subagent_dirty_threads
      WHERE session_id = ?${sourceId ? ' AND source_id = ?' : ''}`,
    ...(sourceId ? [sessionId, sourceId] : [sessionId]),
  );
  for (const row of rows) {
    ensureSubagentTimelineProjection(db, {
      sourceId: row.source_id,
      projectSlug: row.project_slug,
      sessionId: row.session_id,
      workflowId: row.workflow_id,
      agentId: row.agent_id,
    });
  }
}

/** Materialize native-ingested branch projections before the service is ready. */
export function rebuildDirtySubagentProjections(
  db: SqliteService,
  onProgress?: (progress: { current: number; total: number }) => void,
): number {
  const rows = db.all<DirtyThreadRow>(
    `SELECT source_id, project_slug, session_id, workflow_id, agent_id
       FROM subagent_dirty_threads
      ORDER BY session_id, workflow_id, agent_id`,
  );
  for (let index = 0; index < rows.length; index++) {
    const row = rows[index]!;
    ensureSubagentTimelineProjection(db, {
      sourceId: row.source_id,
      projectSlug: row.project_slug,
      sessionId: row.session_id,
      workflowId: row.workflow_id,
      agentId: row.agent_id,
    });
    onProgress?.({ current: index + 1, total: rows.length });
  }
  return rows.length;
}

/** Materialize dirty threads relevant to a global/project/session search. */
export function ensureSearchableSubagentProjections(
  db: SqliteService,
  scope: {
    sourceId?: string;
    sessionId?: string;
    projectMembers?: Array<{ sourceId: string; slug: string }>;
    projectSlug?: string;
  },
): void {
  const clauses: string[] = [];
  const params: unknown[] = [];
  if (scope.sourceId) {
    clauses.push('source_id = ?');
    params.push(scope.sourceId);
  }
  if (scope.sessionId) {
    clauses.push('session_id = ?');
    params.push(scope.sessionId);
  }
  if (scope.projectMembers?.length) {
    clauses.push(`(${scope.projectMembers.map(() => '(source_id = ? AND project_slug = ?)').join(' OR ')})`);
    for (const member of scope.projectMembers) params.push(member.sourceId, member.slug);
  } else if (scope.projectSlug) {
    clauses.push('project_slug = ?');
    params.push(scope.projectSlug);
  }
  const rows = db.all<DirtyThreadRow>(
    `SELECT source_id, project_slug, session_id, workflow_id, agent_id
       FROM subagent_dirty_threads${clauses.length ? ` WHERE ${clauses.join(' AND ')}` : ''}`,
    ...params,
  );
  for (const row of rows) {
    ensureSubagentTimelineProjection(db, {
      sourceId: row.source_id,
      projectSlug: row.project_slug,
      sessionId: row.session_id,
      workflowId: row.workflow_id,
      agentId: row.agent_id,
    });
  }
}
