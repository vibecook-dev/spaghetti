/**
 * recover-legacy-data.ts — Data recovery script for @vibecook/spaghetti-sdk
 *
 * Recovers ~40,308 messages from two legacy databases that were deleted from disk.
 *
 * CRITICAL: Source DBs are the ONLY copies. They are opened READ-ONLY.
 *
 * ⚠️  DANGER: this manual dev script WRITES the recovered rows into your REAL
 *     Spaghetti cache DB at ~/.spaghetti/cache/spaghetti.db. Set SPAGHETTI_DB_PATH
 *     to a throwaway path to target a scratch DB instead of your real data.
 *
 * Run with: npx tsx scripts/recover-legacy-data.ts
 */

import { DatabaseSync } from 'node:sqlite';
import { decode } from '@msgpack/msgpack';
import { join } from 'path';
import { homedir } from 'os';
import { createSqliteService } from '../packages/sdk/src/io/sqlite-service.js';
import { createIngestService } from '../packages/sdk/src/data/ingest-service.js';
import type {
  SessionsIndex,
  SessionIndexEntry,
  SessionMessage,
  SubagentTranscript,
  PersistedToolResult,
  TodoFile,
  TaskEntry,
  PlanFile,
} from '../packages/sdk/src/types/index.js';
import type { FileHistorySession } from '../packages/sdk/src/types/file-history-data.js';

// ═══════════════════════════════════════════════════════════════════════════
// PATHS
// ═══════════════════════════════════════════════════════════════════════════

const SOURCE_A_PATH = join(homedir(), '.claude-on-the-go', 'cache', 'agent-claude-code-segments.db');
const SOURCE_B_PATH = join(homedir(), '.claude-on-the-go', 'cache', 'messages.db');
// SPAGHETTI_DB_PATH overrides the real cache DB — see the danger note above.
const TARGET_DB_PATH =
  process.env.SPAGHETTI_DB_PATH ?? join(homedir(), '.spaghetti', 'cache', 'spaghetti.db');

// ═══════════════════════════════════════════════════════════════════════════
// KEY PARSING
// ═══════════════════════════════════════════════════════════════════════════

const UUID_RE = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/;

const MESSAGE_KEY_RE = /^message:(.+)\/([0-9a-f-]{36})\/(\d{6})$/;
const SESSION_KEY_RE = /^session:(.+)\/([0-9a-f-]{36})$/;
const SUBAGENT_KEY_RE = /^subagent:(.+)\/([0-9a-f-]{36})\/(.+)$/;
const TOOL_RESULT_KEY_RE = /^tool_result:(.+)\/([0-9a-f-]{36})\/(.+)$/;
const TODO_KEY_RE = /^todo:([0-9a-f-]{36})\/(.+)$/;
const TASK_KEY_RE = /^task:([0-9a-f-]{36})$/;
const FILE_HISTORY_KEY_RE = /^file_history:([0-9a-f-]{36})$/;
const PROJECT_KEY_RE = /^project:(.+)$/;
const PROJECT_MEMORY_KEY_RE = /^project_memory:(.+)$/;
const PLAN_KEY_RE = /^plan:(.+)$/;

// ═══════════════════════════════════════════════════════════════════════════
// HELPERS
// ═══════════════════════════════════════════════════════════════════════════

function cwdToSlug(cwd: string): string {
  return cwd.replace(/\//g, '-');
}

interface SegmentRow {
  key: string;
  data: Buffer;
}

interface SourceBMessageRow {
  session_id: string;
  idx: number;
  raw_json: string;
}

interface CountResult {
  count: number;
}

// ═══════════════════════════════════════════════════════════════════════════
// STATS TRACKING
// ═══════════════════════════════════════════════════════════════════════════

const stats = {
  sourceA: {
    projects: { recovered: 0, failed: 0 },
    projectMemories: { recovered: 0, failed: 0 },
    plans: { recovered: 0, failed: 0 },
    sessions: { recovered: 0, failed: 0 },
    messages: { recovered: 0, failed: 0 },
    subagents: { recovered: 0, failed: 0 },
    toolResults: { recovered: 0, failed: 0 },
    todos: { recovered: 0, failed: 0 },
    tasks: { recovered: 0, failed: 0 },
    fileHistory: { recovered: 0, failed: 0 },
  },
  sourceB: {
    sessions: { recovered: 0, failed: 0 },
    messages: { recovered: 0, failed: 0 },
    projects: { recovered: 0, failed: 0 },
  },
};

const errors: string[] = [];

function logError(step: string, key: string, err: unknown): void {
  const msg = err instanceof Error ? err.message : String(err);
  const line = `[${step}] key=${key}: ${msg}`;
  errors.push(line);
  console.error(`  ERROR: ${line}`);
}

// ═══════════════════════════════════════════════════════════════════════════
// MAIN
// ═══════════════════════════════════════════════════════════════════════════

async function main() {
  console.log('');
  console.log('Spaghetti Legacy Data Recovery');
  console.log('═══════════════════════════════════════════════════════════════');
  console.log(`Source A: ${SOURCE_A_PATH}`);
  console.log(`Source B: ${SOURCE_B_PATH}`);
  console.log(`Target:   ${TARGET_DB_PATH}`);
  console.log('');

  // ─────────────────────────────────────────────────────────────────────────
  // Open source databases (READ-ONLY)
  // ─────────────────────────────────────────────────────────────────────────

  console.log('Opening source databases (read-only)...');
  const sourceA = new DatabaseSync(SOURCE_A_PATH, { readOnly: true });
  const sourceB = new DatabaseSync(SOURCE_B_PATH, { readOnly: true });
  console.log('  Source A opened.');
  console.log('  Source B opened.');

  // ─────────────────────────────────────────────────────────────────────────
  // Open target database via spaghetti services
  // ─────────────────────────────────────────────────────────────────────────

  console.log('Opening target database...');
  const sharedSqlite = createSqliteService();
  const sqliteFactory = () => sharedSqlite;
  const ingestService = createIngestService(sqliteFactory);
  ingestService.open(TARGET_DB_PATH);
  console.log('  Target opened and schema initialized.');

  // Get pre-recovery counts
  const db = sharedSqlite.getDb();
  const preProjects = (db.prepare('SELECT COUNT(*) as count FROM projects').get() as CountResult).count;
  const preSessions = (db.prepare('SELECT COUNT(*) as count FROM sessions').get() as CountResult).count;
  const preMessages = (db.prepare('SELECT COUNT(*) as count FROM messages').get() as CountResult).count;
  let preFts = 0;
  try {
    preFts = (db.prepare("SELECT COUNT(*) as count FROM search_fts").get() as CountResult).count;
  } catch {
    preFts = 0;
  }
  console.log(`  Pre-recovery: ${preProjects} projects, ${preSessions} sessions, ${preMessages} messages, ${preFts} FTS entries`);
  console.log('');

  // ═══════════════════════════════════════════════════════════════════════════
  // STEP 1: Projects (Source A)
  // ═══════════════════════════════════════════════════════════════════════════

  console.log('Step 1: Recovering projects from Source A...');
  {
    const rows = sourceA.prepare("SELECT key, data FROM segments WHERE type='project' ORDER BY key").all() as SegmentRow[];
    ingestService.beginTransaction();
    for (const row of rows) {
      try {
        const decoded = decode(row.data) as Record<string, unknown>;
        const match = PROJECT_KEY_RE.exec(row.key);
        if (!match) throw new Error(`Cannot parse project key: ${row.key}`);
        const slug = match[1];
        const originalPath = (decoded.originalPath as string) || '';
        const sessionsIndex = (decoded.sessionsIndex as SessionsIndex) || { version: 1, entries: [] };
        ingestService.onProject(slug, originalPath, sessionsIndex);
        stats.sourceA.projects.recovered++;
        console.log(`  Recovered project: ${slug}`);
      } catch (err) {
        stats.sourceA.projects.failed++;
        logError('projects', row.key, err);
      }
    }
    ingestService.commitTransaction();
    console.log(`  Done: ${stats.sourceA.projects.recovered} projects recovered, ${stats.sourceA.projects.failed} failed`);
    console.log('');
  }

  // ═══════════════════════════════════════════════════════════════════════════
  // STEP 2: Project memories (Source A)
  // ═══════════════════════════════════════════════════════════════════════════

  console.log('Step 2: Recovering project memories from Source A...');
  {
    const rows = sourceA.prepare("SELECT key, data FROM segments WHERE type='project_memory' ORDER BY key").all() as SegmentRow[];
    ingestService.beginTransaction();
    for (const row of rows) {
      try {
        const decoded = decode(row.data) as Record<string, unknown>;
        const match = PROJECT_MEMORY_KEY_RE.exec(row.key);
        if (!match) throw new Error(`Cannot parse project_memory key: ${row.key}`);
        const slug = match[1];
        const content = (decoded.content as string) || '';
        ingestService.onProjectMemory(slug, content);
        stats.sourceA.projectMemories.recovered++;
        console.log(`  Recovered project memory: ${slug}`);
      } catch (err) {
        stats.sourceA.projectMemories.failed++;
        logError('project_memories', row.key, err);
      }
    }
    ingestService.commitTransaction();
    console.log(`  Done: ${stats.sourceA.projectMemories.recovered} project memories recovered`);
    console.log('');
  }

  // ═══════════════════════════════════════════════════════════════════════════
  // STEP 3: Plans (Source A)
  // ═══════════════════════════════════════════════════════════════════════════

  console.log('Step 3: Recovering plans from Source A...');
  {
    const rows = sourceA.prepare("SELECT key, data FROM segments WHERE type='plan' ORDER BY key").all() as SegmentRow[];
    ingestService.beginTransaction();
    for (const row of rows) {
      try {
        const decoded = decode(row.data) as Record<string, unknown>;
        const match = PLAN_KEY_RE.exec(row.key);
        if (!match) throw new Error(`Cannot parse plan key: ${row.key}`);
        const slug = match[1];
        const plan: PlanFile = {
          title: (decoded.title as string) || '',
          content: (decoded.content as string) || '',
          size: (decoded.size as number) || 0,
        };
        ingestService.onPlan(slug, plan);
        stats.sourceA.plans.recovered++;
        console.log(`  Recovered plan: ${slug}`);
      } catch (err) {
        stats.sourceA.plans.failed++;
        logError('plans', row.key, err);
      }
    }
    ingestService.commitTransaction();
    console.log(`  Done: ${stats.sourceA.plans.recovered} plans recovered`);
    console.log('');
  }

  // ═══════════════════════════════════════════════════════════════════════════
  // STEP 4: Sessions (Source A)
  // ═══════════════════════════════════════════════════════════════════════════

  console.log('Step 4: Recovering sessions from Source A...');
  {
    const rows = sourceA.prepare("SELECT key, data FROM segments WHERE type='session' ORDER BY key").all() as SegmentRow[];
    ingestService.beginTransaction();
    for (const row of rows) {
      try {
        const decoded = decode(row.data) as Record<string, unknown>;
        const match = SESSION_KEY_RE.exec(row.key);
        if (!match) throw new Error(`Cannot parse session key: ${row.key}`);
        const slug = match[1];
        const indexEntry = decoded.indexEntry as SessionIndexEntry;
        if (!indexEntry) throw new Error('No indexEntry in decoded session data');
        // Ensure sessionId is set
        if (!indexEntry.sessionId) {
          indexEntry.sessionId = (decoded.sessionId as string) || match[2];
        }
        ingestService.onSession(slug, indexEntry);
        stats.sourceA.sessions.recovered++;
      } catch (err) {
        stats.sourceA.sessions.failed++;
        logError('sessions', row.key, err);
      }
    }
    ingestService.commitTransaction();
    console.log(`  Done: ${stats.sourceA.sessions.recovered} sessions recovered, ${stats.sourceA.sessions.failed} failed`);
    console.log('');
  }

  // ═══════════════════════════════════════════════════════════════════════════
  // STEP 5: Messages from Source A (THE BIG ONE — 37,741 segments)
  // ═══════════════════════════════════════════════════════════════════════════

  console.log('Step 5: Recovering messages from Source A (37,741 expected)...');
  {
    const rows = sourceA.prepare("SELECT key, data FROM segments WHERE type='message' ORDER BY key").all() as SegmentRow[];
    console.log(`  Found ${rows.length} message segments`);

    ingestService.beginTransaction();
    for (let i = 0; i < rows.length; i++) {
      const row = rows[i];
      try {
        const match = MESSAGE_KEY_RE.exec(row.key);
        if (!match) throw new Error(`Cannot parse message key: ${row.key}`);
        const slug = match[1];
        const sessionId = match[2];
        const index = parseInt(match[3], 10);

        const message = decode(row.data) as SessionMessage;
        ingestService.onMessage(slug, sessionId, message, index, 0);
        stats.sourceA.messages.recovered++;
      } catch (err) {
        stats.sourceA.messages.failed++;
        logError('messages-A', row.key, err);
      }

      // Transaction batching every 1000
      if ((i + 1) % 1000 === 0) {
        ingestService.commitTransaction();
        ingestService.beginTransaction();
      }

      // Progress reporting every 5000
      if ((i + 1) % 5000 === 0) {
        console.log(`  ${i + 1}/${rows.length} messages...`);
      }
    }
    ingestService.commitTransaction();
    console.log(`  Done: ${stats.sourceA.messages.recovered} messages recovered, ${stats.sourceA.messages.failed} failed`);
    console.log('');
  }

  // ═══════════════════════════════════════════════════════════════════════════
  // STEP 6: Messages from Source B (2,567 rows with raw_json)
  // ═══════════════════════════════════════════════════════════════════════════

  console.log('Step 6: Recovering messages from Source B (2,567 expected)...');
  {
    // First, create sessions and projects for Source B sessions
    const distinctSessions = sourceB.prepare(
      "SELECT DISTINCT session_id FROM messages WHERE raw_json IS NOT NULL"
    ).all() as Array<{ session_id: string }>;

    console.log(`  Found ${distinctSessions.length} distinct sessions in Source B`);

    // Track created projects for Source B
    const sourceBProjects = new Set<string>();

    ingestService.beginTransaction();

    // For each session, look up a sample message to get cwd and create project/session
    for (const { session_id } of distinctSessions) {
      try {
        // Get first message to extract cwd
        const firstMsg = sourceB.prepare(
          "SELECT raw_json FROM messages WHERE session_id = ? AND raw_json IS NOT NULL ORDER BY idx LIMIT 1"
        ).get(session_id) as { raw_json: string } | undefined;

        let slug = '-unknown';
        if (firstMsg) {
          const parsed = JSON.parse(firstMsg.raw_json);
          if (parsed.cwd) {
            slug = cwdToSlug(parsed.cwd);
          }
        }

        // Create project if not seen
        if (!sourceBProjects.has(slug)) {
          sourceBProjects.add(slug);
          ingestService.onProject(slug, slug.replace(/-/g, '/').replace(/^\//, '/'), { version: 1, entries: [] });
          stats.sourceB.projects.recovered++;
        }

        // Create minimal session entry
        const minimalEntry: SessionIndexEntry = {
          sessionId: session_id,
          fullPath: '',
          fileMtime: 0,
          firstPrompt: '',
          summary: '',
          messageCount: 0,
          created: '',
          modified: '',
          gitBranch: '',
          projectPath: '',
          isSidechain: false,
        };
        ingestService.onSession(slug, minimalEntry);
        stats.sourceB.sessions.recovered++;
      } catch (err) {
        stats.sourceB.sessions.failed++;
        logError('sessions-B', session_id, err);
      }
    }
    ingestService.commitTransaction();

    // Now process all messages from Source B
    const msgRows = sourceB.prepare(
      "SELECT session_id, idx, raw_json FROM messages WHERE raw_json IS NOT NULL ORDER BY session_id, idx"
    ).all() as SourceBMessageRow[];

    console.log(`  Found ${msgRows.length} messages with raw_json`);

    // Build a session → slug lookup
    const sessionSlugMap = new Map<string, string>();
    for (const { session_id } of distinctSessions) {
      try {
        const firstMsg = sourceB.prepare(
          "SELECT raw_json FROM messages WHERE session_id = ? AND raw_json IS NOT NULL ORDER BY idx LIMIT 1"
        ).get(session_id) as { raw_json: string } | undefined;
        if (firstMsg) {
          const parsed = JSON.parse(firstMsg.raw_json);
          sessionSlugMap.set(session_id, parsed.cwd ? cwdToSlug(parsed.cwd) : '-unknown');
        } else {
          sessionSlugMap.set(session_id, '-unknown');
        }
      } catch {
        sessionSlugMap.set(session_id, '-unknown');
      }
    }

    ingestService.beginTransaction();
    for (let i = 0; i < msgRows.length; i++) {
      const row = msgRows[i];
      try {
        const message = JSON.parse(row.raw_json) as SessionMessage;
        const slug = sessionSlugMap.get(row.session_id) || '-unknown';
        ingestService.onMessage(slug, row.session_id, message, row.idx, 0);
        stats.sourceB.messages.recovered++;
      } catch (err) {
        stats.sourceB.messages.failed++;
        logError('messages-B', `${row.session_id}/${row.idx}`, err);
      }

      if ((i + 1) % 1000 === 0) {
        ingestService.commitTransaction();
        ingestService.beginTransaction();
      }
    }
    ingestService.commitTransaction();
    console.log(`  Done: ${stats.sourceB.messages.recovered} messages recovered, ${stats.sourceB.messages.failed} failed`);
    console.log('');
  }

  // ═══════════════════════════════════════════════════════════════════════════
  // STEP 7: Subagents (Source A)
  // ═══════════════════════════════════════════════════════════════════════════

  console.log('Step 7: Recovering subagents from Source A...');
  {
    const rows = sourceA.prepare("SELECT key, data FROM segments WHERE type='subagent' ORDER BY key").all() as SegmentRow[];
    ingestService.beginTransaction();
    for (let i = 0; i < rows.length; i++) {
      const row = rows[i];
      try {
        const match = SUBAGENT_KEY_RE.exec(row.key);
        if (!match) throw new Error(`Cannot parse subagent key: ${row.key}`);
        const slug = match[1];
        const sessionId = match[2];

        const decoded = decode(row.data) as SubagentTranscript;
        ingestService.onSubagent(slug, sessionId, decoded);
        stats.sourceA.subagents.recovered++;
      } catch (err) {
        stats.sourceA.subagents.failed++;
        logError('subagents', row.key, err);
      }

      if ((i + 1) % 200 === 0) {
        ingestService.commitTransaction();
        ingestService.beginTransaction();
      }
    }
    ingestService.commitTransaction();
    console.log(`  Done: ${stats.sourceA.subagents.recovered} subagents recovered, ${stats.sourceA.subagents.failed} failed`);
    console.log('');
  }

  // ═══════════════════════════════════════════════════════════════════════════
  // STEP 8: Tool results (Source A)
  // ═══════════════════════════════════════════════════════════════════════════

  console.log('Step 8: Recovering tool results from Source A...');
  {
    const rows = sourceA.prepare("SELECT key, data FROM segments WHERE type='tool_result' ORDER BY key").all() as SegmentRow[];
    ingestService.beginTransaction();
    for (let i = 0; i < rows.length; i++) {
      const row = rows[i];
      try {
        const match = TOOL_RESULT_KEY_RE.exec(row.key);
        if (!match) throw new Error(`Cannot parse tool_result key: ${row.key}`);
        const slug = match[1];
        const sessionId = match[2];

        const decoded = decode(row.data) as PersistedToolResult;
        ingestService.onToolResult(slug, sessionId, decoded);
        stats.sourceA.toolResults.recovered++;
      } catch (err) {
        stats.sourceA.toolResults.failed++;
        logError('tool_results', row.key, err);
      }

      if ((i + 1) % 200 === 0) {
        ingestService.commitTransaction();
        ingestService.beginTransaction();
      }
    }
    ingestService.commitTransaction();
    console.log(`  Done: ${stats.sourceA.toolResults.recovered} tool results recovered, ${stats.sourceA.toolResults.failed} failed`);
    console.log('');
  }

  // ═══════════════════════════════════════════════════════════════════════════
  // STEP 9: Todos (Source A)
  // ═══════════════════════════════════════════════════════════════════════════

  console.log('Step 9: Recovering todos from Source A...');
  {
    const rows = sourceA.prepare("SELECT key, data FROM segments WHERE type='todo' ORDER BY key").all() as SegmentRow[];
    ingestService.beginTransaction();
    for (const row of rows) {
      try {
        const match = TODO_KEY_RE.exec(row.key);
        if (!match) throw new Error(`Cannot parse todo key: ${row.key}`);
        const sessionId = match[1];

        const decoded = decode(row.data) as TodoFile;
        ingestService.onTodo(sessionId, decoded);
        stats.sourceA.todos.recovered++;
      } catch (err) {
        stats.sourceA.todos.failed++;
        logError('todos', row.key, err);
      }
    }
    ingestService.commitTransaction();
    console.log(`  Done: ${stats.sourceA.todos.recovered} todos recovered, ${stats.sourceA.todos.failed} failed`);
    console.log('');
  }

  // ═══════════════════════════════════════════════════════════════════════════
  // STEP 9b: Tasks (Source A)
  // ═══════════════════════════════════════════════════════════════════════════

  console.log('Step 9b: Recovering tasks from Source A...');
  {
    const rows = sourceA.prepare("SELECT key, data FROM segments WHERE type='task' ORDER BY key").all() as SegmentRow[];
    ingestService.beginTransaction();
    for (const row of rows) {
      try {
        const match = TASK_KEY_RE.exec(row.key);
        if (!match) throw new Error(`Cannot parse task key: ${row.key}`);
        const sessionId = match[1];

        const decoded = decode(row.data) as TaskEntry;
        ingestService.onTask(sessionId, decoded);
        stats.sourceA.tasks.recovered++;
      } catch (err) {
        stats.sourceA.tasks.failed++;
        logError('tasks', row.key, err);
      }
    }
    ingestService.commitTransaction();
    console.log(`  Done: ${stats.sourceA.tasks.recovered} tasks recovered, ${stats.sourceA.tasks.failed} failed`);
    console.log('');
  }

  // ═══════════════════════════════════════════════════════════════════════════
  // STEP 9c: File history (Source A)
  // ═══════════════════════════════════════════════════════════════════════════

  console.log('Step 9c: Recovering file history from Source A...');
  {
    const rows = sourceA.prepare("SELECT key, data FROM segments WHERE type='file_history' ORDER BY key").all() as SegmentRow[];
    ingestService.beginTransaction();
    for (const row of rows) {
      try {
        const match = FILE_HISTORY_KEY_RE.exec(row.key);
        if (!match) throw new Error(`Cannot parse file_history key: ${row.key}`);
        const sessionId = match[1];

        const decoded = decode(row.data) as FileHistorySession;
        ingestService.onFileHistory(sessionId, decoded);
        stats.sourceA.fileHistory.recovered++;
      } catch (err) {
        stats.sourceA.fileHistory.failed++;
        logError('file_history', row.key, err);
      }
    }
    ingestService.commitTransaction();
    console.log(`  Done: ${stats.sourceA.fileHistory.recovered} file history entries recovered, ${stats.sourceA.fileHistory.failed} failed`);
    console.log('');
  }

  // ═══════════════════════════════════════════════════════════════════════════
  // STEP 10: Mark recovery in fingerprints
  // ═══════════════════════════════════════════════════════════════════════════

  console.log('Step 10: Recording recovery fingerprints...');
  ingestService.upsertFingerprint({
    path: 'recovery://agent-claude-code-segments.db/2026-03-21',
    mtimeMs: Date.now(),
    size: 0,
  });
  ingestService.upsertFingerprint({
    path: 'recovery://messages.db/2026-03-21',
    mtimeMs: Date.now(),
    size: 0,
  });
  console.log('  Fingerprints recorded.');
  console.log('');

  // ═══════════════════════════════════════════════════════════════════════════
  // VERIFICATION
  // ═══════════════════════════════════════════════════════════════════════════

  console.log('Verifying...');
  const postProjects = (db.prepare('SELECT COUNT(*) as count FROM projects').get() as CountResult).count;
  const postSessions = (db.prepare('SELECT COUNT(*) as count FROM sessions').get() as CountResult).count;
  const postMessages = (db.prepare('SELECT COUNT(*) as count FROM messages').get() as CountResult).count;
  let postFts = 0;
  try {
    postFts = (db.prepare("SELECT COUNT(*) as count FROM search_fts('*')").get() as CountResult).count;
  } catch {
    // FTS count may fail if the index is empty; fall back to a different query
    try {
      postFts = (db.prepare("SELECT COUNT(*) as count FROM search_fts").get() as CountResult).count;
    } catch {
      postFts = -1;
    }
  }

  // ═══════════════════════════════════════════════════════════════════════════
  // SUMMARY REPORT
  // ═══════════════════════════════════════════════════════════════════════════

  console.log('');
  console.log('Recovery Complete');
  console.log('═══════════════════════════════════════════════════════════════');
  console.log('Source A (agent-claude-code-segments.db):');
  console.log(`  Projects:         ${stats.sourceA.projects.recovered} recovered (${stats.sourceA.projects.failed} failed)`);
  console.log(`  Project memories: ${stats.sourceA.projectMemories.recovered} recovered (${stats.sourceA.projectMemories.failed} failed)`);
  console.log(`  Plans:            ${stats.sourceA.plans.recovered} recovered (${stats.sourceA.plans.failed} failed)`);
  console.log(`  Sessions:         ${stats.sourceA.sessions.recovered} recovered (${stats.sourceA.sessions.failed} failed)`);
  console.log(`  Messages:         ${stats.sourceA.messages.recovered} recovered (${stats.sourceA.messages.failed} failed)`);
  console.log(`  Subagents:        ${stats.sourceA.subagents.recovered} recovered (${stats.sourceA.subagents.failed} failed)`);
  console.log(`  Tool results:     ${stats.sourceA.toolResults.recovered} recovered (${stats.sourceA.toolResults.failed} failed)`);
  console.log(`  Todos:            ${stats.sourceA.todos.recovered} recovered (${stats.sourceA.todos.failed} failed)`);
  console.log(`  Tasks:            ${stats.sourceA.tasks.recovered} recovered (${stats.sourceA.tasks.failed} failed)`);
  console.log(`  File history:     ${stats.sourceA.fileHistory.recovered} recovered (${stats.sourceA.fileHistory.failed} failed)`);
  console.log('');
  console.log('Source B (messages.db):');
  console.log(`  Projects:         ${stats.sourceB.projects.recovered} recovered`);
  console.log(`  Sessions:         ${stats.sourceB.sessions.recovered} recovered (${stats.sourceB.sessions.failed} failed)`);
  console.log(`  Messages:         ${stats.sourceB.messages.recovered} recovered (${stats.sourceB.messages.failed} failed)`);
  console.log('');
  console.log(`Database totals (before -> after):`);
  console.log(`  Projects:         ${preProjects} -> ${postProjects}`);
  console.log(`  Sessions:         ${preSessions} -> ${postSessions}`);
  console.log(`  Messages:         ${preMessages} -> ${postMessages}`);
  console.log(`  Search index:     ${preFts} -> ${postFts}`);
  console.log('═══════════════════════════════════════════════════════════════');

  if (errors.length > 0) {
    console.log('');
    console.log(`Total errors: ${errors.length}`);
    console.log('First 20 errors:');
    for (const e of errors.slice(0, 20)) {
      console.log(`  ${e}`);
    }
  }

  // ─────────────────────────────────────────────────────────────────────────
  // Cleanup
  // ─────────────────────────────────────────────────────────────────────────

  ingestService.close();
  sourceA.close();
  sourceB.close();
  console.log('');
  console.log('All databases closed. Recovery complete.');
}

main().catch((err) => {
  console.error('FATAL:', err);
  process.exit(1);
});
