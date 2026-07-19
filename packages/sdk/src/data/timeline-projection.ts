import type { SqliteService } from '../io/index.js';
import { transformRawMessagesToTimeline } from '../react/chat/transform-messages.js';
import type { SessionMessage } from '../react/chat/types.js';

interface DirtyRow {
  session_id: string;
  source_id: string;
  project_slug: string;
}

interface RawRow {
  data: string;
  timestamp: string | null;
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

/**
 * Rebuild a dirty session's materialized display timeline before querying it.
 * Raw `messages` remain canonical; this projection is safe to delete/rebuild.
 */
export function ensureTimelineProjection(db: SqliteService, sessionId: string): void {
  const dirty = db.get<DirtyRow>(
    'SELECT session_id, source_id, project_slug FROM timeline_dirty_sessions WHERE session_id = ?',
    sessionId,
  );
  if (!dirty) return;

  const rows = db.all<RawRow>(
    `SELECT data, timestamp
       FROM messages
      WHERE session_id = ? AND source_id = ? AND project_slug = ?
      ORDER BY msg_index`,
    sessionId,
    dirty.source_id,
    dirty.project_slug,
  );

  const rawMessages = rows
    .map((row) => {
      try {
        const value = JSON.parse(row.data) as unknown;
        if (value && typeof value === 'object' && row.timestamp) {
          const record = value as Record<string, unknown>;
          if (!record.timestamp) record.timestamp = row.timestamp;
        }
        return value as Record<string, unknown>;
      } catch {
        return null;
      }
    })
    .filter((value): value is Record<string, unknown> => value !== null);

  // Sidechain user rows are internal agent prompts and have never been shown
  // by the transcript. Excluding them here makes DB totals equal visible rows.
  const timeline = transformRawMessagesToTimeline(rawMessages, { sourceId: dirty.source_id }).filter(
    (message) => !(message.type === 'user' && message.isSidechain),
  );

  db.exec('BEGIN IMMEDIATE');
  try {
    db.run('DELETE FROM timeline_messages WHERE session_id = ?', sessionId);
    const insert = db.prepare(
      `INSERT INTO timeline_messages
         (source_id, project_slug, session_id, timeline_index, display_type, tool_name, tool_use_id, search_text, data)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    );
    for (let index = 0; index < timeline.length; index++) {
      const message = timeline[index];
      insert.run(
        dirty.source_id,
        dirty.project_slug,
        sessionId,
        index,
        message.type,
        message.toolUse?.toolName ?? null,
        message.toolUse?.toolId ?? message.toolResult?.toolId ?? null,
        timelineSearchText(message),
        JSON.stringify(message),
      );
    }
    db.run('DELETE FROM timeline_dirty_sessions WHERE session_id = ?', sessionId);
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
