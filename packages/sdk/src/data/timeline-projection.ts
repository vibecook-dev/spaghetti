import type { SqliteService } from '../io/index.js';
import { extractToolResultsFromRawMessage, transformRawMessagesToTimeline } from '../react/chat/transform-messages.js';
import type { SessionMessage, ToolResultInfo } from '../react/chat/types.js';

/** Leaves room for every display part emitted by one raw assistant envelope. */
export const TIMELINE_INDEX_STRIDE = 100_000;

interface DirtyRow {
  session_id: string;
  source_id: string;
  project_slug: string;
}

interface RawRow {
  msg_index: number;
  data: string;
  timestamp: string | null;
}

interface StoredResultRow {
  tool_use_id: string;
}

interface TimelineToolRow {
  id: number;
  data: string;
}

export interface TimelineProjectionProgress {
  current: number;
  total: number;
  sessionId: string;
}

export function timelineSearchText(message: SessionMessage): string {
  if (message.content) return message.content;
  if (message.toolUse) {
    try {
      return `${message.toolUse.toolName} ${JSON.stringify(message.toolUse.input)}`;
    } catch {
      return message.toolUse.toolName;
    }
  }
  if (message.toolResult) return message.toolResult.content;
  return '';
}

function parseRawMessage(row: Pick<RawRow, 'data' | 'timestamp'>): Record<string, unknown> | null {
  try {
    const value = JSON.parse(row.data) as unknown;
    if (!value || typeof value !== 'object') return null;
    const record = value as Record<string, unknown>;
    if (!record.timestamp && row.timestamp) record.timestamp = row.timestamp;
    return record;
  } catch {
    return null;
  }
}

function timelineIndex(rawIndex: number, partIndex: number): number {
  if (partIndex >= TIMELINE_INDEX_STRIDE) {
    throw new Error(`Raw message ${rawIndex} emitted too many timeline parts`);
  }
  return rawIndex * TIMELINE_INDEX_STRIDE + partIndex;
}

function updateToolResult(db: SqliteService, sessionId: string, toolUseId: string): void {
  if (!toolUseId) return;
  const stored = db.get<{ result_data: string }>(
    `SELECT result_data
       FROM timeline_tool_results
      WHERE session_id = ? AND tool_use_id = ?
      ORDER BY raw_index DESC
      LIMIT 1`,
    sessionId,
    toolUseId,
  );
  const result = stored ? (JSON.parse(stored.result_data) as ToolResultInfo) : undefined;
  const tools = db.all<TimelineToolRow>(
    `SELECT id, data
       FROM timeline_messages
      WHERE session_id = ? AND display_type = 'tool_use' AND tool_use_id = ?`,
    sessionId,
    toolUseId,
  );
  for (const row of tools) {
    try {
      const message = JSON.parse(row.data) as SessionMessage;
      if (!message.toolUse) continue;
      message.toolUse = { ...message.toolUse, result };
      db.run('UPDATE timeline_messages SET data = ? WHERE id = ?', JSON.stringify(message), row.id);
    } catch {
      /* malformed derived rows are replaced on the next full projection */
    }
  }
}

/**
 * Project one canonical raw row at a stable raw-derived index.
 *
 * This is safe for append and upsert paths. Tool-result associations are kept
 * separately, so a result can update an earlier tool row without rebuilding
 * the rest of the transcript, and result-before-call ordering still works.
 */
export function projectTimelineRawMessage(
  db: SqliteService,
  input: {
    sourceId: string;
    projectSlug: string;
    sessionId: string;
    rawIndex: number;
    rawMessage: Record<string, unknown>;
    replaceExisting?: boolean;
  },
): void {
  const { sourceId, projectSlug, sessionId, rawIndex, rawMessage, replaceExisting = true } = input;
  const affectedToolIds = new Set<string>();
  if (replaceExisting) {
    for (const row of db.all<StoredResultRow>(
      'SELECT tool_use_id FROM timeline_tool_results WHERE session_id = ? AND raw_index = ?',
      sessionId,
      rawIndex,
    )) {
      affectedToolIds.add(row.tool_use_id);
    }
    db.run('DELETE FROM timeline_tool_results WHERE session_id = ? AND raw_index = ?', sessionId, rawIndex);
    db.run('DELETE FROM timeline_messages WHERE session_id = ? AND raw_index = ?', sessionId, rawIndex);
  }

  const messages = transformRawMessagesToTimeline([rawMessage], { sourceId }).filter(
    (message) => !(message.type === 'user' && message.isSidechain),
  );
  for (let partIndex = 0; partIndex < messages.length; partIndex++) {
    const message = messages[partIndex]!;
    db.run(
      `INSERT INTO timeline_messages
         (source_id, project_slug, session_id, raw_index, timeline_index,
          display_type, tool_name, tool_use_id, search_text, data)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      sourceId,
      projectSlug,
      sessionId,
      rawIndex,
      timelineIndex(rawIndex, partIndex),
      message.type,
      message.toolUse?.toolName ?? null,
      message.toolUse?.toolId ?? message.toolResult?.toolId ?? null,
      timelineSearchText(message),
      JSON.stringify(message),
    );
    if (message.toolUse?.toolId) affectedToolIds.add(message.toolUse.toolId);
  }

  for (const result of extractToolResultsFromRawMessage(rawMessage, { sourceId })) {
    if (!result.toolId) continue;
    db.run(
      `INSERT INTO timeline_tool_results(session_id, raw_index, tool_use_id, result_data)
       VALUES (?, ?, ?, ?)
       ON CONFLICT(session_id, raw_index, tool_use_id)
       DO UPDATE SET result_data = excluded.result_data`,
      sessionId,
      rawIndex,
      result.toolId,
      JSON.stringify(result),
    );
    affectedToolIds.add(result.toolId);
  }

  for (const toolUseId of affectedToolIds) updateToolResult(db, sessionId, toolUseId);
  db.run('DELETE FROM timeline_dirty_sessions WHERE session_id = ?', sessionId);
}

/** Atomically rebuild one dirty session; used for native cold ingest and rewrites. */
export function rebuildTimelineProjection(db: SqliteService, dirty: DirtyRow): void {
  const rows = db.all<RawRow>(
    `SELECT msg_index, data, timestamp
       FROM messages
      WHERE session_id = ? AND source_id = ? AND project_slug = ?
      ORDER BY msg_index`,
    dirty.session_id,
    dirty.source_id,
    dirty.project_slug,
  );

  db.exec('BEGIN IMMEDIATE');
  try {
    db.run('DELETE FROM timeline_tool_results WHERE session_id = ?', dirty.session_id);
    db.run('DELETE FROM timeline_messages WHERE session_id = ?', dirty.session_id);
    for (const row of rows) {
      const rawMessage = parseRawMessage(row);
      if (!rawMessage) continue;
      projectTimelineRawMessage(db, {
        sourceId: dirty.source_id,
        projectSlug: dirty.project_slug,
        sessionId: dirty.session_id,
        rawIndex: row.msg_index,
        rawMessage,
        replaceExisting: false,
      });
    }
    db.run('DELETE FROM timeline_dirty_sessions WHERE session_id = ?', dirty.session_id);
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

/** Rebuild all native-ingested or rewrite-invalidated projections before ready. */
export function rebuildDirtyTimelineProjections(
  db: SqliteService,
  onProgress?: (progress: TimelineProjectionProgress) => void,
): number {
  const rows = db.all<DirtyRow>(
    'SELECT session_id, source_id, project_slug FROM timeline_dirty_sessions ORDER BY session_id',
  );
  for (let index = 0; index < rows.length; index++) {
    const row = rows[index]!;
    rebuildTimelineProjection(db, row);
    onProgress?.({ current: index + 1, total: rows.length, sessionId: row.session_id });
  }
  return rows.length;
}

/** Read-time safety net for externally mutated or interrupted caches. */
export function ensureTimelineProjection(db: SqliteService, sessionId: string): void {
  const dirty = db.get<DirtyRow>(
    'SELECT session_id, source_id, project_slug FROM timeline_dirty_sessions WHERE session_id = ?',
    sessionId,
  );
  if (dirty) rebuildTimelineProjection(db, dirty);
}
