#!/usr/bin/env -S tsx
/**
 * ingest-diff.ts — correctness gate for the Rust ingest port.
 *
 * Two modes, picked via `--mode=`:
 *
 *   - `cold` (default) — runs the TS ingest (via
 *     `@vibecook/spaghetti-sdk` → `createSpaghettiService`) and the Rust
 *     ingest (via `@vibecook/spaghetti-sdk-native`) against the same
 *     fixture directory, dumps every row of every table from both
 *     resulting SQLite databases, and asserts the two dumps are
 *     semantically identical.
 *
 *   - `live-batch` (RFC 005 C4.3 parity) — generates a synthetic session
 *     JSONL incrementally (configurable line count + step), parses it
 *     into `ParsedRow[]` once, then exercises both write paths:
 *       * TS:   `IngestService.writeBatch(rows)` directly.
 *       * Rust: `native.liveIngestBatch(dbPath, rows)`.
 *     The two DBs are then diffed via the same per-table walker the
 *     `cold` mode uses. This proves the live-update fast path produces
 *     identical SQLite state across engines.
 *
 * The TS SDK writes first (and closes its connection cleanly) before the
 * Rust addon ever opens the Rust DB — they use different files and are
 * sequenced, so there is no WAL contention.
 *
 * Expected differences that the harness deliberately normalises away:
 *   - `updated_at` columns: both paths call `Date.now()` at write time and
 *     will always differ → ignored.
 *   - `file_mtime` / nested `fileMtime`: rounded to whole ms — Node and Rust
 *     f64 stats of the same path can differ by sub-ms noise (esp. on CI where
 *     git checkout mtimes are not the fixture generator's pinned utimes).
 *   - `source_files.mtime_ms`: set from fs.statSync and is ignored as per
 *     RFC 003. In addition, the Rust ingest (Phase 1 commit 1.7) does not
 *     write to `source_files` at all — the TS `saveAllFingerprints()`
 *     path has no Rust equivalent yet. The whole table is skipped.
 *   - JSON-valued columns — `projects.sessions_index`, `subagents.messages`,
 *     `todos.items`, `file_history.data`, and `messages.data` — are parsed
 *     as JSON before compare, because TS re-stringifies via `JSON.stringify`
 *     while Rust passes the raw JSONL line (for `messages.data`) or
 *     re-serialises via `serde_json` (for the others). The on-disk bytes
 *     therefore differ by whitespace / key order but the semantic value
 *     matches.
 *   - `search_fts` content-synced virtual table: FTS auxiliary tables
 *     (`search_fts_*`) are not diffed — they are a function of `messages`
 *     and derive from trigger output. We sanity-check the row count
 *     matches `messages` on both sides and leave it at that.
 *
 * Run examples:
 *   tsx scripts/ingest-diff.ts                              # cold mode, default Claude fixture
 *   tsx scripts/ingest-diff.ts --source=grok                # Grok RS↔TS cold parity
 *   tsx scripts/ingest-diff.ts --mode=live-batch            # 200 lines, 25-row chunks
 *   tsx scripts/ingest-diff.ts --mode=live-batch --lines=500 --chunk=50
 *
 * Exit codes:
 *   0 — zero diffs.
 *   1 — at least one semantic diff, first ~10 printed.
 *   2 — harness error (fixture missing, DB open failed, etc.).
 */

import { createRequire } from 'node:module';
import { createHash } from 'node:crypto';
import { existsSync, rmSync, mkdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

import { DatabaseSync } from 'node:sqlite';

import {
  createSpaghettiService,
  createIngestService,
  createSqliteService,
  initializeSchema,
  createClaudeCodeSource,
  createCodexSource,
  createGrokSource,
} from '../packages/sdk/dist/index.js';

// `@vibecook/spaghetti-sdk-native` is a workspace dep of the SDK — not of
// the repo root — so under pnpm's isolated layout it is only resolvable
// from the SDK's own node_modules. Anchor the require there (the SDK dist
// entry we import from above) instead of at this script's path.
const require = createRequire(new URL('../packages/sdk/dist/index.js', import.meta.url));

// ─── CLI ────────────────────────────────────────────────────────────────────

const { values } = parseArgs({
  options: {
    fixture: { type: 'string' },
    'ts-db': { type: 'string' },
    'rust-db': { type: 'string' },
    mode: { type: 'string' },
    source: { type: 'string' },
    'snapshot-json': { type: 'string' },
    lines: { type: 'string' },
    chunk: { type: 'string' },
  },
});

type Mode = 'cold' | 'live-batch';
const mode: Mode = (values.mode as Mode | undefined) ?? 'cold';
if (mode !== 'cold' && mode !== 'live-batch') {
  console.error(`unknown --mode=${values.mode!}; expected 'cold' or 'live-batch'`);
  process.exit(2);
}

/** Agent product for cold mode. Live-batch stays Claude-shaped. */
type ColdSource = 'claude' | 'codex' | 'grok';
const coldSource: ColdSource = (values.source as ColdSource | undefined) ?? 'claude';
if (coldSource !== 'claude' && coldSource !== 'codex' && coldSource !== 'grok') {
  console.error(`unknown --source=${values.source!}; expected 'claude' | 'codex' | 'grok'`);
  process.exit(2);
}

/** NAPI `sourceId` stamped on native rows (omit for Claude default). */
const nativeSourceId: string | undefined =
  coldSource === 'claude' ? undefined : coldSource === 'codex' ? 'codex' : 'grok';

// `fileURLToPath`, not `new URL(...).pathname`: on Windows the latter
// yields `/D:/repo/scripts`, which `path.resolve` then reads as a
// drive-relative path and expands to `D:\D:\repo` — the fixture lookup
// below can never hit.
const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const defaultFixtureBySource: Record<ColdSource, string> = {
  claude: path.join(repoRoot, 'crates/spaghetti-napi/fixtures/small/.claude'),
  codex: path.join(repoRoot, 'crates/spaghetti-napi/fixtures/small-codex/.codex'),
  grok: path.join(repoRoot, 'crates/spaghetti-napi/fixtures/small-grok/.grok'),
};
const defaultFixture = defaultFixtureBySource[coldSource];
const fixtureRootDir = path.resolve(values.fixture ?? defaultFixture);
const tsDbPath = path.resolve(values['ts-db'] ?? path.join(tmpdir(), 'ingest-diff-ts.db'));
const rustDbPath = path.resolve(values['rust-db'] ?? path.join(tmpdir(), 'ingest-diff-rust.db'));

const liveBatchLines = Number.parseInt(values.lines ?? '200', 10);
const liveBatchChunk = Number.parseInt(values.chunk ?? '25', 10);
if (!Number.isFinite(liveBatchLines) || liveBatchLines <= 0) {
  console.error(`--lines must be a positive integer, got: ${values.lines}`);
  process.exit(2);
}
if (!Number.isFinite(liveBatchChunk) || liveBatchChunk <= 0) {
  console.error(`--chunk must be a positive integer, got: ${values.chunk}`);
  process.exit(2);
}

if (mode === 'cold' && !existsSync(fixtureRootDir)) {
  console.error(`fixture not found: ${fixtureRootDir}`);
  if (coldSource === 'grok') {
    console.error(
      'regenerate with: node scripts/generate-grok-fixture.mjs --out crates/spaghetti-napi/fixtures/small-grok',
    );
  } else if (coldSource === 'codex') {
    console.error(
      'regenerate with: node scripts/generate-codex-fixture.mjs --out crates/spaghetti-napi/fixtures/small-codex',
    );
  } else {
    console.error(
      'regenerate with: node scripts/generate-ingest-fixture.mjs --out crates/spaghetti-napi/fixtures/small',
    );
  }
  process.exit(2);
}

for (const p of [tsDbPath, rustDbPath]) {
  if (existsSync(p)) rmSync(p, { force: true });
  // Better-sqlite3 also creates -wal / -shm side files.
  for (const suffix of ['-wal', '-shm', '-journal']) {
    if (existsSync(p + suffix)) rmSync(p + suffix, { force: true });
  }
}

// ─── Run the TS ingest ──────────────────────────────────────────────────────
//
// Worker threads are on when the SDK decides the project count warrants it.
// In this small fixture we have 3 projects, below the SDK's threshold of 4,
// so the sequential path runs — which is exactly what we want for a clean
// row-by-row diff (no worker-thread JSON round-trip).
//
// We deliberately run the TS ingest in the main Node process rather than
// shelling out: the SDK exports `createSpaghettiService`, `initialize()`
// blocks until the DB is closed cleanly, and we need the DB closed before
// the Rust addon opens its own (separate) file.

function makeAgentSource() {
  switch (coldSource) {
    case 'grok':
      return createGrokSource({ rootDir: fixtureRootDir });
    case 'codex':
      return createCodexSource({ rootDir: fixtureRootDir });
    case 'claude':
    default:
      return createClaudeCodeSource({ rootDir: fixtureRootDir });
  }
}

async function runTsIngest(): Promise<{ durationMs: number }> {
  const start = Date.now();

  // Quieten the SDK's progress emitter — useful in the CLI but noisy in CI.
  // Consumers can opt in with VERBOSE=1.
  const verbose = process.env.VERBOSE === '1';

  // Pin engine=ts so this side of the diff never silently becomes Rust-vs-Rust
  // (SDK default is rs when the native addon is present).
  const svc = createSpaghettiService({
    source: makeAgentSource(),
    dbPath: tsDbPath,
    engine: 'ts',
  });

  if (verbose) {
    // Progress events are emitted on the underlying data service, which is
    // wrapped by AppService. AppService re-emits 'progress' so we tap it.
    (svc as unknown as { on: (ev: string, cb: (p: unknown) => void) => void }).on('progress', (p) => {
      console.log('[ts]', p);
    });
  }

  await svc.initialize();
  svc.shutdown();

  return { durationMs: Date.now() - start };
}

// ─── Run the Rust ingest ────────────────────────────────────────────────────

interface NativeAddon {
  ingest(opts: {
    agentDir: string;
    dbPath: string;
    mode: 'cold' | 'warm';
    progressIntervalMs?: number;
    parallelism?: number;
    sourceId?: string;
  }): Promise<{
    durationMs: number;
    projectsProcessed: number;
    sessionsProcessed: number;
    messagesWritten: number;
    subagentsWritten: number;
    errors: Array<{ slug: string; message: string }>;
  }>;
  /**
   * RFC 005 C4.3: live-update fast path. Takes a fully-prepared
   * `LiveRow[]` batch (the same shape `IngestServiceImpl.writeBatch`
   * derives via `parsedRowToNativeLiveRow`) and writes it to `dbPath`
   * inside one transaction. The script's live-batch mode mirrors the
   * exact wire shape so both engines see byte-identical input.
   */
  liveIngestBatch(
    dbPath: string,
    rows: Array<{
      category: string;
      slug?: string;
      sessionId?: string;
      payloadJson: string;
    }>,
  ): unknown;
  nativeVersion(): string;
}

async function runRustIngest(): Promise<{ durationMs: number; stats: Awaited<ReturnType<NativeAddon['ingest']>> }> {
  const native = require('@vibecook/spaghetti-sdk-native') as NativeAddon;
  const start = Date.now();
  const stats = await native.ingest({
    agentDir: fixtureRootDir,
    dbPath: rustDbPath,
    mode: 'cold',
    // Claude defaults to source_id='claude-code' inside the addon when omitted.
    ...(nativeSourceId ? { sourceId: nativeSourceId } : {}),
  });
  return { durationMs: Date.now() - start, stats };
}

// ─── Live-batch fixture (RFC 005 C4.3 parity) ──────────────────────────────
//
// Synthesises N session-message ParsedRows (one per JSONL line) plus a
// single project + sessions_index seed so the message rows have a parent.
// Both writers consume the same array; we slice it into chunks of
// `liveBatchChunk` rows to mirror the live pipeline's drain windows.

const LIVE_SLUG = 'live-batch-slug';
const LIVE_SESSION_ID = 'live-batch-session';
const LIVE_PROJECT_PATH = '/tmp/live-batch-project';

interface LiveBatchRow {
  category: string;
  slug?: string;
  sessionId?: string;
  msgIndex?: number;
  byteOffset?: number;
  message?: Record<string, unknown>;
}

/**
 * Build the ParsedRow[] for the live-batch fixture. Two row types:
 *   - One `session_index` row to seed the project + session index.
 *   - N `message` rows with monotonically increasing msgIndex.
 *
 * Returns the array typed loosely as `LiveBatchRow[]`; the TS writer
 * accepts it through its discriminated-union narrowing on `category`.
 */
function buildLiveBatchRows(lines: number): LiveBatchRow[] {
  const rows: LiveBatchRow[] = [];
  // Seed the project. session_index rows reuse `onProject` underneath.
  // The payload must match the typed `SessionsIndex` shape — the Rust
  // deserializer requires `version` (and `entries`, not `sessions`);
  // the TS writer is lenient and would accept malformed values that
  // Rust correctly rejects, wedging the whole batch.
  rows.push({
    category: 'session_index',
    slug: LIVE_SLUG,
    // Carrier fields — read by the writer's session_index handler.
    // Match the inline handler signature.
    ...({
      originalPath: LIVE_PROJECT_PATH,
      // Carry every SessionIndexEntry field, like real on-disk indexes do.
      // The Rust writer round-trips entries through its typed struct and
      // materializes `#[serde(default)]` fields; a sparse entry here would
      // diff against the TS writer's verbatim pass-through.
      sessionsIndex: {
        version: 1,
        entries: [
          {
            sessionId: LIVE_SESSION_ID,
            fullPath: '/tmp/x.jsonl',
            fileMtime: 0,
            firstPrompt: '',
            summary: '',
            messageCount: 0,
            created: '',
            modified: '',
            gitBranch: '',
            projectPath: '',
            isSidechain: false,
          },
        ],
      },
    } as unknown as Record<string, unknown>),
  });
  for (let i = 0; i < lines; i++) {
    rows.push({
      category: 'message',
      slug: LIVE_SLUG,
      sessionId: LIVE_SESSION_ID,
      msgIndex: i,
      byteOffset: i * 200, // synthetic
      message: {
        type: i % 2 === 0 ? 'user' : 'assistant',
        uuid: `uuid-${i}`,
        timestamp: new Date(2026, 0, 1, 0, 0, i).toISOString(),
        sessionId: LIVE_SESSION_ID,
        message:
          i % 2 === 0
            ? { role: 'user', content: `prompt ${i}` }
            : {
                role: 'assistant',
                content: [{ type: 'text', text: `response ${i}` }],
                usage: {
                  input_tokens: 10,
                  output_tokens: 20,
                  cache_creation_input_tokens: 0,
                  cache_read_input_tokens: 0,
                },
              },
      },
    });
  }
  return rows;
}

interface LiveBatchResult {
  durationMs: number;
  batchCount: number;
}

/**
 * Open a fresh SQLite DB at `dbPath`, build an `IngestService` pinned
 * to `engine`, then writeBatch the rows in chunks of size
 * `liveBatchChunk`. Closes the DB cleanly before returning so the
 * compare phase can re-open it read-only.
 */
async function runLiveBatch(engine: 'ts' | 'rs', dbPath: string): Promise<LiveBatchResult> {
  const sqlite = createSqliteService();
  sqlite.open({ path: dbPath });
  initializeSchema(sqlite);
  // Sanity rows for the messages_fts triggers — none of the
  // session_index handler's writes need the projects row up front
  // (the row IS the projects row), so we go straight to writeBatch.

  const native = engine === 'rs' ? (require('@vibecook/spaghetti-sdk-native') as NativeAddon) : null;
  const ingest = createIngestService(() => sqlite, {
    engine,
    // The SDK type declares `native` as the loaded NativeAddon
    // shape; structural typing accepts the same object the script
    // already requires for the cold-mode rust path.
    native: native as unknown as never,
  });
  ingest.open(dbPath);

  const rows = buildLiveBatchRows(liveBatchLines);
  // Use `unknown` to bridge the loose harness shape onto the SDK's
  // ParsedRow union — the writer's switch branches on `category` so
  // structurally-compatible rows hit the right handler.
  const start = Date.now();
  let batchCount = 0;
  for (let i = 0; i < rows.length; i += liveBatchChunk) {
    const chunk = rows.slice(i, i + liveBatchChunk) as unknown as Parameters<typeof ingest.writeBatch>[0];
    await ingest.writeBatch(chunk);
    batchCount += 1;
  }
  ingest.close();
  return { durationMs: Date.now() - start, batchCount };
}

async function runTsLiveBatch(): Promise<LiveBatchResult> {
  return runLiveBatch('ts', tsDbPath);
}

async function runRustLiveBatch(): Promise<LiveBatchResult> {
  return runLiveBatch('rs', rustDbPath);
}

// ─── Table inventory ────────────────────────────────────────────────────────
//
// Tables to diff, in the order we care about them. Each table declaration
// says which columns to parse-as-JSON and which to ignore entirely.
// `source_files` is deliberately absent: the Rust ingest doesn't write it
// in Phase 1, and the TS ingest writes it from `saveAllFingerprints()` —
// there would be nothing to meaningfully diff.

interface TableSpec {
  name: string;
  /** `ORDER BY` clause that gives a deterministic row ordering for diffing. */
  orderBy: string;
  /** Optional `WHERE` clause to exclude engine-local rows from the diff. */
  where?: string;
  /** Columns whose values are JSON strings; parse them before comparing. */
  jsonColumns?: string[];
  /** Columns to skip (e.g. updated_at, mtimeMs that both sides set to now()). */
  ignoreColumns?: string[];
}

const RFC011_SCHEMA_TABLES = [
  'source_instances',
  'source_streams',
  'source_objects',
  'ingest_commits',
  'projection_versions',
  'source_record_errors',
  'change_log',
  'fact_records',
  'canonical_sessions',
  'canonical_messages',
  'canonical_runs',
  'run_evidence',
  'observed_run_states',
  'delegation_assertions',
  'canonical_delegations',
  'usage_contributions',
  'usage_totals',
] as const;

const TABLE_SPECS: TableSpec[] = [
  {
    name: 'schema_meta',
    orderBy: 'key',
    // TS-engine-local / lifecycle markers that native cold ingest never writes:
    // - heal_msg_index_v1: Claude TS one-shot heal
    // - *_extract_version: stamped by lifecycle attachShared after native returns
    where: "key NOT IN ('heal_msg_index_v1', 'grok_extract_version', 'codex_extract_version')",
  },
  {
    name: 'source_instances',
    orderBy: 'source_instance_id',
  },
  {
    name: 'source_streams',
    orderBy: 'source_stream_id',
  },
  {
    name: 'source_objects',
    orderBy: 'source_object_id',
  },
  {
    name: 'ingest_commits',
    orderBy: 'commit_seq',
  },
  {
    name: 'projection_versions',
    orderBy: 'projection_id, scope_key',
  },
  {
    name: 'source_record_errors',
    orderBy: 'source_object_id, generation, cursor_start, cursor_end',
  },
  {
    name: 'change_log',
    orderBy: 'commit_seq, ordinal',
  },
  {
    name: 'fact_records',
    orderBy: 'fact_id',
  },
  {
    name: 'canonical_sessions',
    orderBy: 'session_key',
  },
  {
    name: 'canonical_messages',
    orderBy: 'message_key',
  },
  {
    name: 'canonical_runs',
    orderBy: 'run_key',
  },
  {
    name: 'run_evidence',
    orderBy: 'fact_id',
  },
  {
    name: 'observed_run_states',
    orderBy: 'run_key',
  },
  {
    name: 'delegation_assertions',
    orderBy: 'fact_id',
  },
  {
    name: 'canonical_delegations',
    orderBy: 'child_run_key',
  },
  {
    name: 'usage_contributions',
    orderBy: 'fact_id',
  },
  {
    name: 'usage_totals',
    orderBy: 'session_key',
  },
  {
    name: 'projects',
    // Composite PK is (source_id, slug) — order by both for multi-source stability.
    orderBy: 'source_id, slug',
    jsonColumns: ['sessions_index'],
    ignoreColumns: ['updated_at'],
  },
  {
    name: 'project_memories',
    orderBy: 'project_slug',
    ignoreColumns: ['updated_at'],
  },
  {
    name: 'sessions',
    orderBy: 'id',
    ignoreColumns: ['updated_at'],
  },
  {
    name: 'messages',
    // `id` is AUTOINCREMENT; same insertion order on both sides gives the
    // same ids, but we key on (session_id, msg_index) which is UNIQUE so
    // the diff is robust even if ids drift.
    orderBy: 'session_id, msg_index',
    jsonColumns: ['data'],
    ignoreColumns: ['id'],
  },
  {
    name: 'subagents',
    orderBy: 'project_slug, session_id, workflow_id, agent_id',
    jsonColumns: ['messages'],
    ignoreColumns: ['id', 'updated_at'],
  },
  {
    name: 'workflows',
    orderBy: 'project_slug, session_id, workflow_id',
    jsonColumns: ['data', 'journal'],
    ignoreColumns: ['id', 'updated_at'],
  },
  {
    name: 'tool_results',
    orderBy: 'project_slug, session_id, tool_use_id',
    ignoreColumns: ['id', 'updated_at'],
  },
  {
    name: 'todos',
    orderBy: 'session_id, agent_id',
    jsonColumns: ['items'],
    ignoreColumns: ['id', 'updated_at'],
  },
  {
    name: 'tasks',
    orderBy: 'session_id',
    ignoreColumns: ['updated_at'],
  },
  {
    name: 'plans',
    orderBy: 'slug',
    ignoreColumns: ['updated_at'],
  },
  {
    name: 'config',
    orderBy: 'key',
    ignoreColumns: ['updated_at'],
  },
  {
    name: 'analytics',
    orderBy: 'key',
    ignoreColumns: ['updated_at'],
  },
  {
    name: 'file_history',
    orderBy: 'session_id',
    jsonColumns: ['data'],
    ignoreColumns: ['updated_at'],
  },
];

// ─── Dump + diff ────────────────────────────────────────────────────────────

type Row = Record<string, unknown>;

function dumpTable(db: DatabaseSync, spec: TableSpec): Row[] {
  const where = spec.where ? ` WHERE ${spec.where}` : '';
  const rows = db.prepare(`SELECT * FROM ${spec.name}${where} ORDER BY ${spec.orderBy}`).all() as Row[];
  return rows.map((row) => normaliseRow(row, spec));
}

function dumpSchemaShape(db: DatabaseSync, table: string): Row {
  const plainRows = (rows: unknown[]): Row[] =>
    rows.map((row) => Object.fromEntries(Object.entries(row as Record<string, unknown>)));
  const columns = plainRows(db.prepare('SELECT * FROM pragma_table_info(?) ORDER BY cid').all(table));
  const foreignKeys = plainRows(db.prepare('SELECT * FROM pragma_foreign_key_list(?) ORDER BY id, seq').all(table));
  const indexes = plainRows(db.prepare('SELECT * FROM pragma_index_list(?) ORDER BY name').all(table)).map((index) => ({
    ...index,
    columns: plainRows(db.prepare('SELECT * FROM pragma_index_info(?) ORDER BY seqno').all(String(index.name))),
  }));
  return { columns, foreignKeys, indexes };
}

/**
 * Normalize fs mtime floats so Node better-sqlite3 and Rust rusqlite agree.
 * CI checkouts don't preserve fixture utimes pins; Node and Rust can also
 * differ by ~0.0003 ms in f64 representation of the same stat. Round to
 * nearest whole millisecond for columns / nested keys named *mtime*.
 */
function normaliseMtimeValue(v: unknown): unknown {
  if (typeof v === 'number' && Number.isFinite(v)) {
    return Math.round(v);
  }
  return v;
}

function normaliseJsonValue(v: unknown): unknown {
  if (v && typeof v === 'object') {
    if (Array.isArray(v)) {
      return v.map(normaliseJsonValue);
    }
    const o = v as Record<string, unknown>;
    const out: Record<string, unknown> = {};
    for (const [k, val] of Object.entries(o)) {
      if (/mtime/i.test(k)) {
        out[k] = normaliseMtimeValue(val);
      } else {
        out[k] = normaliseJsonValue(val);
      }
    }
    return out;
  }
  return v;
}

function normaliseRow(row: Row, spec: TableSpec): Row {
  const out: Row = {};
  const ignore = new Set(spec.ignoreColumns ?? []);
  const jsonCols = new Set(spec.jsonColumns ?? []);
  for (const [k, v] of Object.entries(row)) {
    if (ignore.has(k)) continue;
    if (jsonCols.has(k) && typeof v === 'string' && v.length > 0) {
      try {
        out[k] = normaliseJsonValue(JSON.parse(v));
      } catch {
        out[k] = v;
      }
    } else if (/mtime/i.test(k)) {
      out[k] = normaliseMtimeValue(v);
    } else {
      out[k] = v;
    }
  }
  return out;
}

interface Diff {
  table: string;
  rowIndex: number;
  kind: 'row-count' | 'field' | 'ts-only-row' | 'rust-only-row';
  field?: string;
  tsValue?: unknown;
  rustValue?: unknown;
}

function canonical(v: unknown): string {
  // Stable JSON with sorted keys — used both for row-key comparison and
  // for rendering a readable diff. Order-independent so { a:1, b:2 } and
  // { b:2, a:1 } hash identically.
  return JSON.stringify(v, sortedReplacer(v), 2);
}

function sortedReplacer(root: unknown): (key: string, value: unknown) => unknown {
  return function (_key: string, value: unknown) {
    if (value === root) return value;
    if (value && typeof value === 'object' && !Array.isArray(value)) {
      const entries = Object.entries(value as object).sort(([a], [b]) => a.localeCompare(b));
      return Object.fromEntries(entries);
    }
    return value;
  };
}

function deepEqual(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (typeof a !== typeof b) return false;
  if (a === null || b === null) return a === b;
  if (typeof a !== 'object') return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  if (Array.isArray(a)) {
    const bArr = b as unknown[];
    if (a.length !== bArr.length) return false;
    for (let i = 0; i < a.length; i++) if (!deepEqual(a[i], bArr[i])) return false;
    return true;
  }
  const ao = a as Record<string, unknown>;
  const bo = b as Record<string, unknown>;
  const keysA = Object.keys(ao).sort();
  const keysB = Object.keys(bo).sort();
  if (keysA.length !== keysB.length) return false;
  for (let i = 0; i < keysA.length; i++) {
    if (keysA[i] !== keysB[i]) return false;
    if (!deepEqual(ao[keysA[i]], bo[keysA[i]])) return false;
  }
  return true;
}

function diffTable(tsRows: Row[], rustRows: Row[], spec: TableSpec): Diff[] {
  const diffs: Diff[] = [];

  if (tsRows.length !== rustRows.length) {
    diffs.push({
      table: spec.name,
      rowIndex: -1,
      kind: 'row-count',
      tsValue: tsRows.length,
      rustValue: rustRows.length,
    });
  }

  const limit = Math.min(tsRows.length, rustRows.length);
  for (let i = 0; i < limit; i++) {
    const t = tsRows[i];
    const r = rustRows[i];
    const allKeys = new Set<string>([...Object.keys(t), ...Object.keys(r)]);
    for (const key of allKeys) {
      if (!deepEqual(t[key], r[key])) {
        diffs.push({
          table: spec.name,
          rowIndex: i,
          kind: 'field',
          field: key,
          tsValue: t[key],
          rustValue: r[key],
        });
      }
    }
  }

  for (let i = limit; i < tsRows.length; i++) {
    diffs.push({ table: spec.name, rowIndex: i, kind: 'ts-only-row', tsValue: tsRows[i] });
  }
  for (let i = limit; i < rustRows.length; i++) {
    diffs.push({ table: spec.name, rowIndex: i, kind: 'rust-only-row', rustValue: rustRows[i] });
  }

  return diffs;
}

// ─── Orchestration ──────────────────────────────────────────────────────────

async function main(): Promise<void> {
  console.log(`mode:    ${mode}`);
  if (mode === 'cold') {
    console.log(`source:  ${coldSource}${nativeSourceId ? ` (sourceId=${nativeSourceId})` : ''}`);
    console.log(`fixture: ${fixtureRootDir}`);
  } else {
    console.log(`live-batch: ${liveBatchLines} lines, ${liveBatchChunk}-row chunks`);
  }
  console.log(`ts-db:   ${tsDbPath}`);
  console.log(`rust-db: ${rustDbPath}`);
  console.log('');

  if (mode === 'cold') {
    // Run TS first — it holds the DB handle open via better-sqlite3
    // inside the SDK. Calling shutdown() releases it. Only then do we
    // touch Rust.
    console.log('running TS ingest...');
    const ts = await runTsIngest();
    console.log(`  TS ingest: ${ts.durationMs}ms`);

    console.log('running Rust ingest...');
    const rust = await runRustIngest();
    console.log(`  Rust ingest: ${rust.durationMs}ms`);
    console.log(
      `  stats: projects=${rust.stats.projectsProcessed} sessions=${rust.stats.sessionsProcessed} messages=${rust.stats.messagesWritten} subagents=${rust.stats.subagentsWritten}`,
    );
    if (rust.stats.errors.length > 0) {
      console.log(`  WARN: Rust ingest recorded ${rust.stats.errors.length} parse errors:`);
      for (const e of rust.stats.errors.slice(0, 5)) {
        console.log(`    [${e.slug}] ${e.message}`);
      }
    }
  } else {
    // live-batch mode: build a synthetic batch and feed both writers.
    console.log('running TS live-batch...');
    const ts = await runTsLiveBatch();
    console.log(`  TS live-batch: ${ts.durationMs}ms (${ts.batchCount} batches)`);

    console.log('running Rust live-batch...');
    const rust = await runRustLiveBatch();
    console.log(`  Rust live-batch: ${rust.durationMs}ms (${rust.batchCount} batches)`);
  }

  console.log('');
  console.log('opening both DBs read-only for compare...');
  const tsDb = new DatabaseSync(tsDbPath, { readOnly: true });
  const rustDb = new DatabaseSync(rustDbPath, { readOnly: true });

  /**
   * Deterministic per-engine snapshot for the RFC 008 Phase 0 baseline.
   *
   * Row *contents* are hashed rather than dumped: the point is to detect that
   * something changed and where, not to review 35k rows in a diff. Counts stay
   * plain so a reviewer can see the shape at a glance without decoding a digest.
   *
   * Everything the diff harness already ignores as engine-local — `updated_at`,
   * `source_files.mtime_ms` — is ignored here for the same reason, so a snapshot
   * does not churn on every run.
   */
  function snapshotDb(db: Database.Database, label: string): Record<string, unknown> {
    const tables: Record<string, { rows: number; digest: string }> = {};
    for (const spec of TABLE_SPECS) {
      const exists = db.prepare(`SELECT name FROM sqlite_master WHERE type='table' AND name=?`).get(spec.name);
      if (!exists) continue;
      const rows = dumpTable(db, spec);
      tables[spec.name] = {
        rows: rows.length,
        digest: createHash('sha256').update(canonical(rows)).digest('hex').slice(0, 16),
      };
    }

    const scalar = (sql: string): number => {
      try {
        return (db.prepare(sql).get() as { v: number } | undefined)?.v ?? 0;
      } catch {
        return -1;
      }
    };

    return {
      engine: label,
      tables,
      // `source_files` is outside TABLE_SPECS because only the TS engine writes
      // it today (RFC 008 Phase 1 changes that). Counted, not digested, so the
      // baseline records the asymmetry instead of hiding it.
      fingerprints: scalar('SELECT COUNT(*) AS v FROM source_files'),
      fts: scalar('SELECT COUNT(*) AS v FROM search_fts'),
      tokens: {
        input: scalar('SELECT COALESCE(SUM(input_tokens),0) AS v FROM messages'),
        output: scalar('SELECT COALESCE(SUM(output_tokens),0) AS v FROM messages'),
        cacheCreation: scalar('SELECT COALESCE(SUM(cache_creation_tokens),0) AS v FROM messages'),
        cacheRead: scalar('SELECT COALESCE(SUM(cache_read_tokens),0) AS v FROM messages'),
        sessionsEstimated: scalar('SELECT COUNT(*) AS v FROM sessions WHERE tokens_estimated = 1'),
      },
    };
  }

  /** Fixed FTS queries — a search that silently stops matching is a real regression. */
  const FTS_PROBES = ['the', 'session', 'error'];

  function snapshotFts(db: Database.Database): Record<string, number> {
    const out: Record<string, number> = {};
    for (const q of FTS_PROBES) {
      try {
        const row = db.prepare('SELECT COUNT(*) AS v FROM search_fts WHERE search_fts MATCH ?').get(q) as
          | { v: number }
          | undefined;
        out[q] = row?.v ?? 0;
      } catch {
        out[q] = -1;
      }
    }
    return out;
  }

  const allDiffs: Diff[] = [];

  try {
    for (const spec of TABLE_SPECS) {
      // Sanity: both DBs must have the table (schema is owned identically by both).
      const tsExists = tsDb.prepare(`SELECT name FROM sqlite_master WHERE type='table' AND name=?`).get(spec.name);
      const rustExists = rustDb.prepare(`SELECT name FROM sqlite_master WHERE type='table' AND name=?`).get(spec.name);
      if (!tsExists || !rustExists) {
        allDiffs.push({
          table: spec.name,
          rowIndex: -1,
          kind: 'row-count',
          tsValue: tsExists ? 'present' : 'missing',
          rustValue: rustExists ? 'present' : 'missing',
        });
        continue;
      }

      const tsRows = dumpTable(tsDb, spec);
      const rustRows = dumpTable(rustDb, spec);
      const tableDiffs = diffTable(tsRows, rustRows, spec);
      if (tableDiffs.length === 0) {
        console.log(`  ✓ ${spec.name}: ${tsRows.length} rows, clean`);
      } else {
        console.log(`  ✗ ${spec.name}: ${tableDiffs.length} diff(s)`);
        allDiffs.push(...tableDiffs);
      }
    }

    // The RFC 011 tables are empty on both legacy ingest paths today, so row
    // parity alone cannot catch a misspelled column, FK, uniqueness rule, or
    // index in the transitional TS mirror. Compare SQLite's resolved schema
    // metadata directly until Rust becomes the only migration authority.
    for (const table of RFC011_SCHEMA_TABLES) {
      const tsShape = dumpSchemaShape(tsDb, table);
      const rustShape = dumpSchemaShape(rustDb, table);
      if (deepEqual(tsShape, rustShape)) {
        console.log(`  ✓ ${table} schema: clean`);
      } else {
        console.log(`  ✗ ${table} schema: differs`);
        allDiffs.push({
          table,
          rowIndex: -1,
          kind: 'field',
          field: 'schema',
          tsValue: tsShape,
          rustValue: rustShape,
        });
      }
    }

    // FTS sanity: row count equal on both sides.
    const tsFts = (tsDb.prepare('SELECT COUNT(*) AS c FROM search_fts').get() as { c: number }).c;
    const rustFts = (rustDb.prepare('SELECT COUNT(*) AS c FROM search_fts').get() as { c: number }).c;
    if (tsFts !== rustFts) {
      allDiffs.push({
        table: 'search_fts (row count)',
        rowIndex: -1,
        kind: 'row-count',
        tsValue: tsFts,
        rustValue: rustFts,
      });
    } else {
      console.log(`  ✓ search_fts: ${tsFts} rows (count match)`);
    }

    // RFC 008 Phase 0 baseline. Written from inside the try so it captures the
    // same DBs the diff just read — a snapshot taken after close would be a
    // different run.
    const snapshotPath = values['snapshot-json'];
    if (snapshotPath) {
      const snapshot = {
        source: coldSource,
        // Repo-relative and POSIX-separated: an absolute path would make the
        // committed baseline churn on every machine that regenerates it.
        fixture: path.relative(repoRoot, fixtureRootDir).split(path.sep).join('/'),
        schemaVersion: (
          tsDb.prepare("SELECT value AS v FROM schema_meta WHERE key='version'").get() as { v: string } | undefined
        )?.v,
        ts: { ...snapshotDb(tsDb, 'ts'), ftsProbes: snapshotFts(tsDb) },
        rust: { ...snapshotDb(rustDb, 'rs'), ftsProbes: snapshotFts(rustDb) },
        diffCount: allDiffs.length,
      };
      mkdirSync(path.dirname(snapshotPath), { recursive: true });
      writeFileSync(snapshotPath, `${JSON.stringify(snapshot, null, 2)}\n`);
      console.log(`  → snapshot: ${snapshotPath}`);
    }
  } finally {
    tsDb.close();
    rustDb.close();
  }

  console.log('');

  if (allDiffs.length === 0) {
    console.log('RESULT: zero diffs ✓');
    process.exit(0);
  }

  const shown = Number(process.env.INGEST_DIFF_SHOW ?? '10');
  console.log(`RESULT: ${allDiffs.length} diff(s) — first ${shown}:`);
  for (const d of allDiffs.slice(0, shown)) {
    const prefix = `  [${d.table}#${d.rowIndex}] ${d.kind}`;
    if (d.kind === 'row-count') {
      console.log(`${prefix}: ts=${d.tsValue} rust=${d.rustValue}`);
    } else if (d.kind === 'field') {
      console.log(`${prefix} field=${d.field}`);
      console.log(`    ts:   ${canonical(d.tsValue)}`);
      console.log(`    rust: ${canonical(d.rustValue)}`);
    } else if (d.kind === 'ts-only-row') {
      console.log(`${prefix}: ${canonical(d.tsValue)}`);
    } else {
      console.log(`${prefix}: ${canonical(d.rustValue)}`);
    }
  }
  process.exit(1);
}

main().catch((err) => {
  console.error(err);
  process.exit(2);
});
