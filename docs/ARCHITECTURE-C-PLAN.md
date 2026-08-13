> **Historical implementation plan — superseded by RFC 011.** Architecture C
> is retained as provenance for the retired TypeScript query/store oracle; it
> is not the current production database topology. See the
> [Phase 10 closure ledger](./rfcs/011-phase-10-closure.md).

# Architecture C Implementation Plan

**Status**: All 4 phases implemented (2026-03-21)
**Created**: 2026-03-21
**Target**: Fast cold start (~1.5-3s) with everything ready, no lazy loading

---

## Overview

Replace the current generic segment store (msgpack blobs in SQLite) with dedicated tables, persistent content-synced FTS5, streaming ingest, and worker threads.

## Performance Targets

| Phase | Cold Start (500MB) | Warm Start (0 changes) | Warm Start (5 files) |
|-------|---:|---:|---:|
| Current | 15-30s | 2-5s | 3-8s |
| Phase 1: Drop msgpack | 15-30s | 2-5s | 3-8s |
| Phase 2: Streaming parser | 5-10s | 2-5s | 2-5s |
| Phase 3: Dedicated tables + persistent FTS5 | 3-6s | 50-200ms | 200-500ms |
| Phase 4: Worker threads | **1.5-3s** | **50-200ms** | **200-500ms** |

---

## Phase 1: Drop Msgpack → JSON Storage

### Goal
Remove `@msgpack/msgpack`. Store segment data as JSON TEXT instead of msgpack BLOB.

### Files to Modify
- `data/segment-store.ts` — Change `data BLOB` to `data TEXT`, replace msgpack encode/decode with JSON.stringify/JSON.parse, remove msgpackService parameter
- `io/msgpack-service.ts` — **DELETE**
- `io/index.ts` — Remove MessagePackService export
- `create.ts` — Remove msgpack import and export
- `package.json` — Remove `@msgpack/msgpack` dependency
- `vite.config.ts` — Remove from rollupOptions.external

### Schema Change
```sql
-- data column changes from BLOB to TEXT
CREATE TABLE segments (
  key TEXT PRIMARY KEY,
  type TEXT NOT NULL,
  data TEXT NOT NULL,  -- was BLOB
  version INTEGER NOT NULL DEFAULT 1,
  updated_at INTEGER NOT NULL
);
```

### Migration
Delete old DB file. Add `schema_version` table to detect old format.

### API Impact: None
### Risk: Low
### Estimated effort: 1-2 days

---

## Phase 2: Streaming JSONL Parser + Direct SQLite Ingest

### Goal
Eliminate the monolithic `ClaudeCodeAgentData` in-memory tree. Parse JSONL line-by-line and INSERT into SQLite during parsing.

### Files to Create
- `io/streaming-jsonl-reader.ts` — Buffer-based line reader with byte offset tracking

### Files to Modify
- `io/file-service.ts` — Add `readJsonlStreaming()` method
- `parser/project-parser.ts` — Add `parseAllProjectsStreaming(sink)` with callback pattern

### New Interface: ProjectParseSink
```typescript
export interface ProjectParseSink {
  onProject(slug: string, originalPath: string, sessionsIndex: SessionsIndex): void;
  onProjectMemory(slug: string, content: string): void;
  onSession(slug: string, session: SessionIndexEntry): void;
  onMessage(slug: string, sessionId: string, message: SessionMessage, index: number, byteOffset: number): void;
  onSubagent(slug: string, sessionId: string, transcript: SubagentTranscript): void;
  onToolResult(slug: string, sessionId: string, toolResult: PersistedToolResult): void;
  onFileHistory(sessionId: string, history: FileHistorySession): void;
  onTodo(sessionId: string, todo: TodoFile): void;
  onTask(sessionId: string, task: TaskEntry): void;
  onPlan(slug: string, plan: PlanFile): void;
  onSessionComplete(slug: string, sessionId: string, messageCount: number, lastBytePosition: number): void;
  onProjectComplete(slug: string): void;
}
```

### API Impact: None (internal optimization)
### Risk: Medium (buffer boundary handling, malformed JSONL)
### Estimated effort: 3-5 days

---

## Phase 3: Dedicated Table Schema + Persistent FTS5

### Goal
Replace generic `segments` table with purpose-built tables. Content-synced FTS5 persists across restarts (no more FTS rebuild on warm start).

### New SQL Schema

```sql
-- Meta
CREATE TABLE schema_meta (key TEXT PK, value TEXT NOT NULL);

-- Source file tracking
CREATE TABLE source_files (
  path TEXT PK, mtime_ms REAL, size INT, byte_position INT,
  category TEXT, project_slug TEXT, session_id TEXT
);

-- Core entities
CREATE TABLE projects (slug TEXT PK, original_path TEXT, sessions_index TEXT, updated_at INT);
CREATE TABLE project_memories (project_slug TEXT PK, content TEXT, updated_at INT);
CREATE TABLE sessions (
  id TEXT PK, project_slug TEXT, full_path TEXT, first_prompt TEXT, summary TEXT,
  git_branch TEXT, project_path TEXT, is_sidechain INT, created_at TEXT, modified_at TEXT,
  file_mtime REAL, plan_slug TEXT, has_task INT, updated_at INT
);
CREATE TABLE messages (
  id INTEGER PK AUTOINCREMENT, project_slug TEXT, session_id TEXT, msg_index INT,
  msg_type TEXT, uuid TEXT, timestamp TEXT, data TEXT,
  input_tokens INT DEFAULT 0, output_tokens INT DEFAULT 0,
  cache_creation_tokens INT DEFAULT 0, cache_read_tokens INT DEFAULT 0,
  text_content TEXT DEFAULT '', byte_offset INT,
  UNIQUE(session_id, msg_index)
);
CREATE TABLE subagents (
  id INTEGER PK, project_slug TEXT, session_id TEXT, agent_id TEXT,
  agent_type TEXT, file_name TEXT, messages TEXT, message_count INT, updated_at INT,
  UNIQUE(project_slug, session_id, agent_id)
);
CREATE TABLE tool_results (
  id INTEGER PK, project_slug TEXT, session_id TEXT,
  tool_use_id TEXT, content TEXT, updated_at INT,
  UNIQUE(project_slug, session_id, tool_use_id)
);
CREATE TABLE todos (id INTEGER PK, session_id TEXT, agent_id TEXT, items TEXT, updated_at INT, UNIQUE(session_id, agent_id));
CREATE TABLE tasks (session_id TEXT PK, has_highwatermark INT, highwatermark INT, lock_exists INT, updated_at INT);
CREATE TABLE plans (slug TEXT PK, title TEXT, content TEXT, size INT, updated_at INT);
CREATE TABLE config (key TEXT PK, data TEXT, updated_at INT);
CREATE TABLE analytics (key TEXT PK, data TEXT, updated_at INT);
CREATE TABLE file_history (session_id TEXT PK, data TEXT, updated_at INT);

-- Persistent FTS5 (content-synced with messages)
CREATE VIRTUAL TABLE search_fts USING fts5(text_content, content='messages', content_rowid='id');

-- Auto-sync triggers
CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
  INSERT INTO search_fts(rowid, text_content) VALUES (new.id, new.text_content);
END;
CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
  INSERT INTO search_fts(search_fts, rowid, text_content) VALUES ('delete', old.id, old.text_content);
END;
CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN
  INSERT INTO search_fts(search_fts, rowid, text_content) VALUES ('delete', old.id, old.text_content);
  INSERT INTO search_fts(rowid, text_content) VALUES (new.id, new.text_content);
END;

-- Auxiliary search (memories, plans, todos, etc.)
CREATE VIRTUAL TABLE search_aux USING fts5(entity_type, entity_key, project_slug, session_id, text_content, tags);
```

### Files to Create
- `data/schema.ts` — SQL schema constants, version, migration logic
- `data/query-service.ts` — All read queries (replaces SegmentStore reads)
- `data/ingest-service.ts` — All write operations, implements ProjectParseSink

### Files to Modify/Delete
- `data/segment-store.ts` — DELETE or deprecate
- `data/agent-data-service.ts` — Rewrite impl to use QueryService + IngestService
- `data/search-indexer.ts` — Simplify (text extraction only)
- `app-service.ts` — Summaries now come from SQL aggregation

### Key SQL Queries (replace JS aggregation)
Session and project summaries computed via SQL JOINs + GROUP BY on the denormalized token columns.

### API Impact: None (SpaghettiAPI unchanged)
### Risk: High (largest phase, core data model change)
### Estimated effort: 5-7 days

---

## Phase 4: Worker Threads for Parallel Parsing

### Goal
Parse multiple projects in parallel. Main thread owns SQLite writer. Workers parse and send structured data back.

### Files to Create
- `workers/parse-worker.ts` — Worker thread entry point
- `workers/worker-pool.ts` — Pool manager with round-robin distribution
- `workers/worker-types.ts` — Shared message types

### Worker Message Protocol
Workers send batches of 100-200 pre-extracted messages:
```typescript
interface PreExtractedMessage {
  json: string;           // raw JSON line (for data column)
  msgType: string;
  uuid: string | null;
  timestamp: string | null;
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  textContent: string;    // for FTS
}
```

Main thread does INSERT only — no parsing needed.

### Graceful Degradation
Falls back to sequential parsing if `worker_threads` unavailable.

### API Impact: None
### Risk: Medium (single-writer bottleneck, backpressure needed)
### Estimated effort: 3-5 days

---

## Migration Strategy (all phases)

Each phase deletes the old DB and rebuilds on first launch. A `schema_meta` table tracks the version. When version mismatch is detected, all tables are dropped and recreated.

## API Contract

`SpaghettiAPI` interface is **unchanged across all 4 phases**. All changes are internal to the data service implementation.
