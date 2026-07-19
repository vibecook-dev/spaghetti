import type { SqliteService } from '../io/index.js';
import type { TokenUsageSummary } from './summary-types.js';

export type TokenActivityQuality = 'exact' | 'estimated' | 'mixed' | 'unavailable';

export interface TokenActivityBucketData {
  sourceId: string;
  projectSlug: string;
  date: string;
  tokenUsage: TokenUsageSummary;
  exactTokens: number;
  estimatedTokens: number;
  messageCount: number;
  sessionCount: number;
}

interface DirtyActivityDay {
  source_id: string;
  project_slug: string;
  activity_day: string;
}

interface ActivityRow {
  source_id: string;
  project_slug: string;
  activity_day: string;
  input_tokens: number;
  output_tokens: number;
  cache_creation_tokens: number;
  cache_read_tokens: number;
  total_tokens: number;
  exact_tokens: number;
  estimated_tokens: number;
  message_count: number;
  session_count: number;
}

/**
 * Canonical token volume is source-aware. OpenAI/Codex cached input is a
 * subset of input_tokens, while Claude cache buckets are additive.
 */
export function normalizedTokenTotal(
  sourceId: string,
  usage: Pick<TokenUsageSummary, 'inputTokens' | 'outputTokens' | 'cacheCreationTokens' | 'cacheReadTokens'>,
): number {
  return sourceId === 'codex'
    ? usage.inputTokens + usage.outputTokens
    : usage.inputTokens + usage.outputTokens + usage.cacheCreationTokens + usage.cacheReadTokens;
}

const TOKEN_ROW_UNION_SQL = `
  SELECT source_id, project_slug, session_id, timestamp,
         COALESCE(input_tokens, 0) AS input_tokens,
         COALESCE(output_tokens, 0) AS output_tokens,
         COALESCE(cache_creation_tokens, 0) AS cache_creation_tokens,
         COALESCE(cache_read_tokens, 0) AS cache_read_tokens
    FROM messages
   WHERE timestamp IS NOT NULL AND length(timestamp) >= 10
  UNION ALL
  SELECT source_id, project_slug, session_id, timestamp,
         COALESCE(input_tokens, 0), COALESCE(output_tokens, 0),
         COALESCE(cache_creation_tokens, 0), COALESCE(cache_read_tokens, 0)
    FROM subagent_messages
   WHERE timestamp IS NOT NULL AND length(timestamp) >= 10
`;

function rollupSelect(whereSql = ''): string {
  return `
    WITH token_rows AS (${TOKEN_ROW_UNION_SQL}),
    normalized AS (
      SELECT r.*,
             CASE WHEN r.source_id = 'codex'
                  THEN r.input_tokens + r.output_tokens
                  ELSE r.input_tokens + r.output_tokens + r.cache_creation_tokens + r.cache_read_tokens
              END AS normalized_tokens,
             COALESCE(s.tokens_estimated, 0) AS tokens_estimated
        FROM token_rows r
        LEFT JOIN sessions s ON s.id = r.session_id AND s.source_id = r.source_id
       ${whereSql}
    )
    SELECT source_id, project_slug, substr(timestamp, 1, 10) AS activity_day,
           SUM(input_tokens) AS input_tokens,
           SUM(output_tokens) AS output_tokens,
           SUM(cache_creation_tokens) AS cache_creation_tokens,
           SUM(cache_read_tokens) AS cache_read_tokens,
           SUM(normalized_tokens) AS total_tokens,
           SUM(CASE WHEN tokens_estimated = 0 THEN normalized_tokens ELSE 0 END) AS exact_tokens,
           SUM(CASE WHEN tokens_estimated = 1 THEN normalized_tokens ELSE 0 END) AS estimated_tokens,
           COUNT(*) AS message_count,
           COUNT(DISTINCT source_id || char(0) || session_id) AS session_count
      FROM normalized
     GROUP BY source_id, project_slug, substr(timestamp, 1, 10)
  `;
}

function insertRollupSql(selectSql: string): string {
  return `
    INSERT INTO token_activity_daily
      (source_id, project_slug, activity_day, input_tokens, output_tokens,
       cache_creation_tokens, cache_read_tokens, total_tokens, exact_tokens,
       estimated_tokens, message_count, session_count)
    ${selectSql}
  `;
}

export function rebuildAllTokenActivity(db: SqliteService): number {
  return db.transaction(() => {
    db.run('DELETE FROM token_activity_daily');
    db.exec(insertRollupSql(rollupSelect()));
    const count = db.get<{ count: number }>('SELECT COUNT(*) AS count FROM token_activity_daily')?.count ?? 0;
    db.run('DELETE FROM token_activity_dirty');
    return count;
  });
}

/** Refresh only affected project-days; a cold/broad change uses one full scan. */
export function rebuildDirtyTokenActivity(
  db: SqliteService,
  scope?: { projectSlug: string; sourceId?: string },
): number {
  const scopeSql = scope ? ` WHERE project_slug = ?${scope.sourceId ? ' AND source_id = ?' : ''}` : '';
  const scopeParams = scope ? (scope.sourceId ? [scope.projectSlug, scope.sourceId] : [scope.projectSlug]) : [];
  const dirtyCount =
    db.get<{ count: number }>(`SELECT COUNT(*) AS count FROM token_activity_dirty${scopeSql}`, ...scopeParams)?.count ??
    0;
  if (dirtyCount === 0) {
    // Bulk importers may deliberately suppress per-row dirty triggers and
    // leave the materialized table empty for one final single-pass rebuild.
    // Also makes recovery self-healing if derived rows were cleared manually.
    const existingCount = db.get<{ count: number }>('SELECT COUNT(*) AS count FROM token_activity_daily')?.count ?? 0;
    if (existingCount > 0) return 0;
    const hasTokenRows =
      db.get<{ found: number }>(
        `SELECT EXISTS(
           SELECT 1 FROM messages WHERE timestamp IS NOT NULL AND length(timestamp) >= 10
           UNION ALL
           SELECT 1 FROM subagent_messages WHERE timestamp IS NOT NULL AND length(timestamp) >= 10
         ) AS found`,
      )?.found ?? 0;
    return hasTokenRows ? rebuildAllTokenActivity(db) : 0;
  }
  const existingCount = db.get<{ count: number }>('SELECT COUNT(*) AS count FROM token_activity_daily')?.count ?? 0;
  if (existingCount === 0 || dirtyCount > 256) return rebuildAllTokenActivity(db);

  const dirty = db.all<DirtyActivityDay>(
    `SELECT source_id, project_slug, activity_day FROM token_activity_dirty${scopeSql} ORDER BY activity_day`,
    ...scopeParams,
  );
  return db.transaction(() => {
    for (const day of dirty) {
      db.run(
        'DELETE FROM token_activity_daily WHERE source_id = ? AND project_slug = ? AND activity_day = ?',
        day.source_id,
        day.project_slug,
        day.activity_day,
      );
      db.run(
        insertRollupSql(
          rollupSelect(`WHERE r.source_id = ? AND r.project_slug = ? AND substr(r.timestamp, 1, 10) = ?`),
        ),
        day.source_id,
        day.project_slug,
        day.activity_day,
      );
      db.run(
        'DELETE FROM token_activity_dirty WHERE source_id = ? AND project_slug = ? AND activity_day = ?',
        day.source_id,
        day.project_slug,
        day.activity_day,
      );
    }
    return dirty.length;
  });
}

export function readTokenActivity(
  db: SqliteService,
  projectSlug: string,
  options: { sourceId?: string; from: string; to: string },
): TokenActivityBucketData[] {
  rebuildDirtyTokenActivity(db, { projectSlug, ...(options.sourceId ? { sourceId: options.sourceId } : {}) });
  const sourceClause = options.sourceId ? ' AND source_id = ?' : '';
  const params = options.sourceId
    ? [projectSlug, options.from, options.to, options.sourceId]
    : [projectSlug, options.from, options.to];
  return db
    .all<ActivityRow>(
      `SELECT source_id, project_slug, activity_day, input_tokens, output_tokens,
              cache_creation_tokens, cache_read_tokens, total_tokens, exact_tokens,
              estimated_tokens, message_count, session_count
         FROM token_activity_daily
        WHERE project_slug = ? AND activity_day >= ? AND activity_day <= ?${sourceClause}
        ORDER BY activity_day, source_id`,
      ...params,
    )
    .map((row) => ({
      sourceId: row.source_id,
      projectSlug: row.project_slug,
      date: row.activity_day,
      tokenUsage: {
        inputTokens: row.input_tokens,
        outputTokens: row.output_tokens,
        cacheCreationTokens: row.cache_creation_tokens,
        cacheReadTokens: row.cache_read_tokens,
        totalTokens: row.total_tokens,
      },
      exactTokens: row.exact_tokens,
      estimatedTokens: row.estimated_tokens,
      messageCount: row.message_count,
      sessionCount: row.session_count,
    }));
}
