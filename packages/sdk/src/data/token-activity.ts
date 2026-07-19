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

interface DirtySessionSummary {
  source_id: string;
  project_slug: string;
  session_id: string;
}

export const TOKEN_ACTIVITY_MATERIALIZATION = 'token-activity';
export const TOKEN_ACTIVITY_MATERIALIZATION_VERSION = 1;

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
         COALESCE(cache_read_tokens, 0) AS cache_read_tokens,
         1 AS parent_message
    FROM messages
   WHERE project_slug IS NOT NULL AND session_id IS NOT NULL
     AND timestamp IS NOT NULL AND length(timestamp) >= 10
  UNION ALL
  SELECT source_id, project_slug, session_id, timestamp,
         COALESCE(input_tokens, 0), COALESCE(output_tokens, 0),
         COALESCE(cache_creation_tokens, 0), COALESCE(cache_read_tokens, 0),
         0
    FROM subagent_messages
   WHERE project_slug IS NOT NULL AND session_id IS NOT NULL
     AND timestamp IS NOT NULL AND length(timestamp) >= 10
`;

const SUMMARY_TOKEN_ROW_UNION_SQL = `
  SELECT source_id, project_slug, session_id,
         COALESCE(input_tokens, 0) AS input_tokens,
         COALESCE(output_tokens, 0) AS output_tokens,
         COALESCE(cache_creation_tokens, 0) AS cache_creation_tokens,
         COALESCE(cache_read_tokens, 0) AS cache_read_tokens,
         1 AS parent_message
    FROM messages
   WHERE project_slug IS NOT NULL AND session_id IS NOT NULL
  UNION ALL
  SELECT source_id, project_slug, session_id,
         COALESCE(input_tokens, 0), COALESCE(output_tokens, 0),
         COALESCE(cache_creation_tokens, 0), COALESCE(cache_read_tokens, 0),
         0
    FROM subagent_messages
   WHERE project_slug IS NOT NULL AND session_id IS NOT NULL
`;

function sessionRollupSelect(whereSql = ''): string {
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
    SELECT source_id, project_slug, session_id, substr(timestamp, 1, 10) AS activity_day,
           SUM(input_tokens) AS input_tokens,
           SUM(output_tokens) AS output_tokens,
           SUM(cache_creation_tokens) AS cache_creation_tokens,
           SUM(cache_read_tokens) AS cache_read_tokens,
           SUM(normalized_tokens) AS total_tokens,
           SUM(CASE WHEN tokens_estimated = 0 THEN normalized_tokens ELSE 0 END) AS exact_tokens,
           SUM(CASE WHEN tokens_estimated = 1 THEN normalized_tokens ELSE 0 END) AS estimated_tokens,
           COUNT(*) AS message_count,
           SUM(parent_message) AS parent_message_count
      FROM normalized
     GROUP BY source_id, project_slug, session_id, substr(timestamp, 1, 10)
  `;
}

function insertSessionRollupSql(selectSql: string): string {
  return `
    INSERT INTO token_activity_session_daily
      (source_id, project_slug, session_id, activity_day, input_tokens, output_tokens,
       cache_creation_tokens, cache_read_tokens, total_tokens, exact_tokens,
       estimated_tokens, message_count, parent_message_count)
    ${selectSql}
  `;
}

function insertDailyFromSessionsSql(whereSql = ''): string {
  return `
    INSERT INTO token_activity_daily
      (source_id, project_slug, activity_day, input_tokens, output_tokens,
       cache_creation_tokens, cache_read_tokens, total_tokens, exact_tokens,
       estimated_tokens, message_count, parent_message_count, session_count)
    SELECT source_id, project_slug, activity_day,
           SUM(input_tokens), SUM(output_tokens), SUM(cache_creation_tokens), SUM(cache_read_tokens),
           SUM(total_tokens), SUM(exact_tokens), SUM(estimated_tokens),
           SUM(message_count), SUM(parent_message_count), COUNT(*)
      FROM token_activity_session_daily
     ${whereSql}
     GROUP BY source_id, project_slug, activity_day
  `;
}

function insertSessionSummarySql(whereSql = ''): string {
  return `
    INSERT INTO session_summary_totals
      (source_id, project_slug, session_id, input_tokens, output_tokens,
       cache_creation_tokens, cache_read_tokens, parent_message_count)
    WITH token_rows AS (${SUMMARY_TOKEN_ROW_UNION_SQL})
    SELECT source_id, project_slug, session_id,
           SUM(input_tokens), SUM(output_tokens),
           SUM(cache_creation_tokens), SUM(cache_read_tokens),
           SUM(parent_message)
      FROM token_rows r
     ${whereSql}
     GROUP BY source_id, project_slug, session_id
  `;
}

function markSourceMaterialized(db: SqliteService, sourceId: string): void {
  db.run(
    `INSERT INTO source_materializations(source_id, projection, version, completed_at)
     VALUES (?, ?, ?, ?)
     ON CONFLICT(source_id, projection) DO UPDATE SET
       version = excluded.version,
       completed_at = excluded.completed_at`,
    sourceId,
    TOKEN_ACTIVITY_MATERIALIZATION,
    TOKEN_ACTIVITY_MATERIALIZATION_VERSION,
    Date.now(),
  );
}

export function invalidateTokenActivityMaterialization(db: SqliteService, sourceId: string): void {
  db.run(
    'DELETE FROM source_materializations WHERE source_id = ? AND projection = ?',
    sourceId,
    TOKEN_ACTIVITY_MATERIALIZATION,
  );
}

export function isTokenActivityMaterialized(db: SqliteService, sourceId: string): boolean {
  return !!db.get(
    `SELECT 1 FROM source_materializations
      WHERE source_id = ? AND projection = ? AND version = ?`,
    sourceId,
    TOKEN_ACTIVITY_MATERIALIZATION,
    TOKEN_ACTIVITY_MATERIALIZATION_VERSION,
  );
}

/** Full cold-rebuild fallback. Production native ingestion rebuilds per source. */
export function rebuildAllTokenActivity(db: SqliteService): number {
  return db.transaction(() => {
    db.run('DELETE FROM token_activity_daily');
    db.run('DELETE FROM token_activity_session_daily');
    db.run('DELETE FROM session_summary_totals');
    db.exec(insertSessionRollupSql(sessionRollupSelect()));
    db.exec(insertDailyFromSessionsSql());
    db.exec(insertSessionSummarySql());
    db.run('DELETE FROM token_activity_dirty');
    db.run('DELETE FROM session_summary_dirty');
    const sources = db.all<{ source_id: string }>(
      `SELECT source_id FROM source_files
       UNION SELECT source_id FROM projects
       UNION SELECT source_id FROM sessions`,
    );
    for (const { source_id } of sources) markSourceMaterialized(db, source_id);
    return db.get<{ count: number }>('SELECT COUNT(*) AS count FROM token_activity_daily')?.count ?? 0;
  });
}

/** Materialize one source during boot, before the SDK reports ready. */
export function rebuildSourceTokenActivity(db: SqliteService, sourceId: string): number {
  return db.transaction(() => {
    db.run('DELETE FROM token_activity_daily WHERE source_id = ?', sourceId);
    db.run('DELETE FROM token_activity_session_daily WHERE source_id = ?', sourceId);
    db.run('DELETE FROM session_summary_totals WHERE source_id = ?', sourceId);
    db.run(insertSessionRollupSql(sessionRollupSelect('WHERE r.source_id = ?')), sourceId);
    db.run(insertDailyFromSessionsSql('WHERE source_id = ?'), sourceId);
    db.run(insertSessionSummarySql('WHERE r.source_id = ?'), sourceId);
    db.run('DELETE FROM token_activity_dirty WHERE source_id = ?', sourceId);
    db.run('DELETE FROM session_summary_dirty WHERE source_id = ?', sourceId);
    markSourceMaterialized(db, sourceId);
    return (
      db.get<{ count: number }>('SELECT COUNT(*) AS count FROM token_activity_daily WHERE source_id = ?', sourceId)
        ?.count ?? 0
    );
  });
}

/** Refresh affected project-days after a live transaction; never escalates globally. */
export function rebuildDirtyTokenActivity(
  db: SqliteService,
  scope?: { projectSlug?: string; sourceId?: string },
): number {
  const conditions: string[] = [];
  const scopeParams: unknown[] = [];
  if (scope?.projectSlug) {
    conditions.push('project_slug = ?');
    scopeParams.push(scope.projectSlug);
  }
  if (scope?.sourceId) {
    conditions.push('source_id = ?');
    scopeParams.push(scope.sourceId);
  }
  const scopeSql = conditions.length > 0 ? ` WHERE ${conditions.join(' AND ')}` : '';
  const dirty = db.all<DirtyActivityDay>(
    `SELECT source_id, project_slug, activity_day FROM token_activity_dirty${scopeSql} ORDER BY activity_day`,
    ...scopeParams,
  );
  const dirtySessions = db.all<DirtySessionSummary>(
    `SELECT source_id, project_slug, session_id FROM session_summary_dirty${scopeSql} ORDER BY session_id`,
    ...scopeParams,
  );
  if (dirty.length === 0 && dirtySessions.length === 0) return 0;
  return db.transaction(() => {
    for (const session of dirtySessions) {
      db.run(
        'DELETE FROM session_summary_totals WHERE source_id = ? AND session_id = ?',
        session.source_id,
        session.session_id,
      );
      db.run(
        insertSessionSummarySql('WHERE r.source_id = ? AND r.project_slug = ? AND r.session_id = ?'),
        session.source_id,
        session.project_slug,
        session.session_id,
      );
      db.run(
        'DELETE FROM session_summary_dirty WHERE source_id = ? AND session_id = ?',
        session.source_id,
        session.session_id,
      );
    }
    for (const day of dirty) {
      db.run(
        'DELETE FROM token_activity_session_daily WHERE source_id = ? AND project_slug = ? AND activity_day = ?',
        day.source_id,
        day.project_slug,
        day.activity_day,
      );
      db.run(
        insertSessionRollupSql(
          sessionRollupSelect('WHERE r.source_id = ? AND r.project_slug = ? AND substr(r.timestamp, 1, 10) = ?'),
        ),
        day.source_id,
        day.project_slug,
        day.activity_day,
      );
      db.run(
        'DELETE FROM token_activity_daily WHERE source_id = ? AND project_slug = ? AND activity_day = ?',
        day.source_id,
        day.project_slug,
        day.activity_day,
      );
      db.run(
        insertDailyFromSessionsSql('WHERE source_id = ? AND project_slug = ? AND activity_day = ?'),
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
    return dirty.length + dirtySessions.length;
  });
}

/** Boot-time recovery for interrupted bulk materialization, then bounded dirty reconciliation. */
export function ensureTokenActivityMaterialized(db: SqliteService): number {
  const sources = db.all<{ source_id: string }>(
    `SELECT source_id FROM source_files
     UNION SELECT source_id FROM projects
     UNION SELECT source_id FROM sessions
     UNION SELECT source_id FROM messages
     UNION SELECT source_id FROM subagent_messages`,
  );
  let rebuilt = 0;
  for (const { source_id } of sources) {
    if (!isTokenActivityMaterialized(db, source_id)) {
      rebuildSourceTokenActivity(db, source_id);
      rebuilt++;
    }
  }
  return rebuilt + rebuildDirtyTokenActivity(db);
}

export function readTokenActivity(
  db: SqliteService,
  projectSlug: string,
  options: { sourceId?: string; from: string; to: string },
): TokenActivityBucketData[] {
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
