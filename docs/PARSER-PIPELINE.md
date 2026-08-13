> **Historical pre-RFC 011 map — superseded.** The dual TypeScript/Rust
> parser, writer, SQLite, and synchronous API topology below is not the shipped
> architecture. Rust is now the sole production observation/query owner; see
> [RFC 011](./rfcs/011-rust-observation-query-engine.md) and its
> [Phase 10 closure ledger](./rfcs/011-phase-10-closure.md).

# Parser Pipeline

**Status:** Historical map of the retired dual-engine parsing pipeline.
**Updated:** 2026-04-19
**Companion doc:** `PARSER-UNPARSED-DATA.md` covers the inverse — data the pipeline does not yet ingest.

Two implementations share the same shape:

- **TS SDK** (`packages/sdk/src/`) — ground-truth, comprehensive, dev-iterates-here.
- **RS crate** (`crates/spaghetti-napi/src/`) — performance port via napi-rs. Scoped to project-session ingest (RFC 003). All Rust events end up in the same SQLite schema as the TS pipeline.

Both engines write to a single SQLite database. At runtime the engine is chosen via `createSpaghettiService({ engine })` (`packages/sdk/src/create.ts`), resolved from the app-scoped `settings.json`.

---

## 1. Top-level architecture

```
┌───────────────────────────────────────────────────────────────────┐
│                    createSpaghettiService(opts)                    │
│                          create.ts:45                              │
│                                                                    │
│  FileService ──► ClaudeCodeParserImpl                              │
│                    ├─ ProjectParserImpl     (sessions, subagents,  │
│                    │                         tool-results,         │
│                    │                         file-history, todos,  │
│                    │                         tasks, plans, memory) │
│                    ├─ ConfigParserImpl      (settings, plugins,    │
│                    │                         statsig, ide, cache,  │
│                    │                         shell-snapshots,      │
│                    │                         statusline)           │
│                    └─ AnalyticsParserImpl   (history.jsonl,        │
│                                              stats-cache,          │
│                                              telemetry, debug,     │
│                                              paste-cache,          │
│                                              session-env)          │
│                                                                    │
│  SqliteService (single writer) ──► QueryService (read)             │
│                                  └─ IngestService (write sink)     │
│                                                                    │
│  ┌──── Optional fast path (Rust) ────┐                             │
│  │ native.ts: loadNativeAddon()      │                             │
│  │   └─► spaghetti-napi.ingest()     │                             │
│  │        ├─ project_parser.rs       │ writes same SQLite schema   │
│  │        ├─ jsonl_reader.rs         │                             │
│  │        ├─ parse_sink IngestEvent  │                             │
│  │        └─ writer.rs (rusqlite)    │                             │
│  └──────────────────────────────────┘                              │
│                                                                    │
│                              ▼                                     │
│                    AgentDataServiceImpl                            │
│                    agent-data-service.ts:153                       │
│                              ▼                                     │
│                    SpaghettiAppService (public API)                │
│                              ▼                                     │
│                    React hooks / CLI / Electron app                │
└───────────────────────────────────────────────────────────────────┘
```

Key properties:

- **Single SQLite writer.** Whether TS or Rust ingest runs, the `IngestService` / Rust writer thread is the only process that writes to the DB — avoids `SQLITE_BUSY`.
- **Cold vs warm start.** `AgentDataServiceImpl.performColdStart` does a full re-ingest via a worker pool (TS) or rayon pool (Rust). `performWarmStart(fingerprints)` compares stored fingerprints to filesystem mtimes and re-parses only changed files.
- **Fingerprint-based incrementality.** Both engines persist `source_files` rows (`path, mtime_ms, size, byte_position, category, project_slug, session_id`) so subsequent runs can skip unchanged files and resume session-JSONL parsing from a byte offset.
- **Streaming JSONL.** Both engines read 64KB buffered chunks with UTF-8-safe carry-over and invoke a callback per line, so session files don't need to fit in RAM.
- **No parsing in React.** `packages/sdk/src/react/` is pure consumption — `SpaghettiProvider`, `useSpaghettiAPI`, and display components only call API methods.

---

## 2. Data category × parser matrix

One row per source file / directory under `~/.claude/`. Engine column values:

- **TS** = parsed by TypeScript SDK.
- **RS** = parsed by Rust crate (runs when `engine: 'rust'` is selected).
- **✗** = not parsed (see `PARSER-UNPARSED-DATA.md` for details).

### 2.1 Per-project state (under `~/.claude/projects/{slug}/`)

| Source | TS parser (file:line) | RS parser | SQLite table |
|---|---|---|---|
| `sessions-index.json` | `ProjectParserImpl.parseAllProjects` — `project-parser.ts:71` reads + merges with on-disk discovery | `ProjectParser::parse_project` — `project_parser.rs:103` | `projects.sessions_index` (JSON column) |
| `{sessionId}.jsonl` | Streaming: `FileServiceImpl.readJsonlStreaming` → `IngestService.onMessage` sink — `project-parser.ts:176` | `read_jsonl_streaming` — `jsonl_reader.rs:49`, deserialized to `SessionMessage` enum | `messages` (+ `search_fts` via trigger) |
| `{sessionId}/subagents/agent-*.jsonl` | `ProjectParserImpl.parseSubagents` — `project-parser.ts:433` | `project_parser.rs` walks subagent dir per session | `subagents` |
| `{sessionId}/tool-results/*.txt` | `ProjectParserImpl.parseToolResults` — `project-parser.ts:470` | Rust parser emits `IngestEvent::ToolResult` | `tool_results` |
| `memory/MEMORY.md` | `ProjectParserImpl.parseProjectMemory` — `project-parser.ts:494` | Emits `IngestEvent::ProjectMemory` | `project_memories` |

### 2.2 Cross-project artifacts

| Source | TS parser | RS parser | SQLite table |
|---|---|---|---|
| `file-history/{sessionId}/{hash}@v{n}` | `ProjectParserImpl.parseFileHistory` — `project-parser.ts:503` | `project_parser.rs:594` emits `IngestEvent::FileHistory` | `file_histories` |
| `todos/{sessionId}-agent-*.json` | `ProjectParserImpl.parseTodos` — `project-parser.ts:538` | `project_parser.rs:638` emits `IngestEvent::Todo` | `todos` |
| `tasks/{sessionId}/.lock` + `.highwatermark` | `ProjectParserImpl.parseTasks` — `project-parser.ts:571` (metadata only, items never loaded — see unparsed doc §2.3) | `project_parser.rs:683` — same metadata-only gap | `tasks` |
| `plans/*.md` | `ProjectParserImpl.buildPlanIndex` — `project-parser.ts:596` | Event defined but **no emitter** (see unparsed doc §2.7) | `plans` |

### 2.3 Global config (under `~/.claude/`)

All of §2.3 is **TS-only**. None of these are parsed by the Rust crate.

| Source | TS parser (file:line) | SQLite / output |
|---|---|---|
| `settings.json` | `ConfigParserImpl.parseConfig` — `config-parser.ts:70` | In-memory `SettingsFile`, exposed via `AgentDataServiceImpl.getConfig()` |
| `settings.local.json` | ✗ (not parsed — see unparsed doc §1.5) | — |
| `plugins/installed_plugins.json` | `config-parser.ts:77` | `PluginsDirectory.installedPlugins` |
| `plugins/known_marketplaces.json` | `config-parser.ts:81` | `PluginsDirectory.knownMarketplaces` |
| `plugins/install-counts-cache.json` | `config-parser.ts:85` | `PluginsDirectory.installCountsCache` |
| `plugins/cache/**` (plugin manifests + MCP configs) | `config-parser.ts:95-151` | `PluginsDirectory.cache[]` |
| `plugins/marketplaces/*/` | `config-parser.ts:153-171` | `PluginsDirectory.marketplaces[]` |
| `statsig/*` | `ConfigParserImpl.parseStatsig` — `config-parser.ts:173-204` | `StatsigDirectory` |
| `ide/*.lock` | `ConfigParserImpl.parseIde` — `config-parser.ts:206-221` | `IdeDirectory` |
| `shell-snapshots/snapshot-*.sh` | `ConfigParserImpl.parseShellSnapshots` — `config-parser.ts:223-244` | `ShellSnapshotsDirectory` (latest snapshot by default; `allShellSnapshots: true` returns all) |
| `cache/changelog.md` | `ConfigParserImpl.parseCache` — `config-parser.ts:286-299` | `CacheDirectory.changelog` |
| `statusline-command.sh` | `config-parser.ts:301-310` | `StatusLineCommandFile` |

### 2.4 Analytics / telemetry

All of §2.4 is **TS-only**.

| Source | TS parser (file:line) | Output type |
|---|---|---|
| `history.jsonl` | `AnalyticsParserImpl.parseHistory` — `analytics-parser.ts:79-87` | `HistoryFile` |
| `stats-cache.json` | `analytics-parser.ts:70-77` | `StatsCacheFile` |
| `telemetry/1p_failed_events.*.json` | `analytics-parser.ts:89-110` | `TelemetryDirectory` |
| `debug/*.txt` | `AnalyticsParserImpl.parseDebugLogs` — `analytics-parser.ts:128-164` (regex-based line parsing with continuation-line handling) | `DebugLogFile[]` |
| `debug/latest` (symlink) | `analytics-parser.ts:207` | `DebugLatestSymlink` |
| `paste-cache/*.txt` | `analytics-parser.ts:219-241` | `PasteCacheDirectory` |
| `session-env/*/` | `analytics-parser.ts:243-255` | `SessionEnvDirectory` |

---

## 3. TS pipeline details

### 3.1 Public entry points

- `createSpaghettiService(options?)` at `packages/sdk/src/create.ts:45` — factory. Creates `FileService`, shared `SqliteService`, `QueryService`, `IngestService`, `ClaudeCodeParserImpl`, wraps in `AgentDataServiceImpl`, exposes via `SpaghettiAppService`.
- `SpaghettiAPI` interface at `packages/sdk/src/api.ts:73` — stable public contract used by CLI, Electron app, React hooks.
- `AgentDataServiceImpl.initialize()` at `packages/sdk/src/data/agent-data-service.ts:176` — entry to cold/warm-start logic; delegates to Rust via `initializeWithNative(native)` at line 253 when configured.

### 3.2 Orchestrator

- `ClaudeCodeParserImpl` at `packages/sdk/src/parser/claude-code-parser.ts:43`. Composes the three sub-parsers via factories: `createProjectParser(fileService)`, `createConfigParser(fileService)`, `createAnalyticsParser(fileService)`.
- `parseSync(options?)` at line 58 — returns `ClaudeCodeAgentData` with projects, config, and analytics filled in.
- `parseStreaming(sink, options?)` at line 80 — streaming variant; only projects stream through a `ProjectParseSink` so worker threads can forward events to the main-thread writer.
- `parseProjectStreaming(rootDir, slug, sink)` at line 94 — per-project update path.

### 3.3 Sub-parsers

- **`ProjectParserImpl`** — `packages/sdk/src/parser/project-parser.ts:50`. Public methods: `parseAllProjects` (line 71), `parseAllProjectsStreaming` (97), `parseProjectStreaming` (124), `parseProject` (231), `parseSession` (261). Missing files return empty collections (silent). Session index is merged with on-disk discovery at line 314 so stale indices don't hide sessions.
- **`ConfigParserImpl`** — `packages/sdk/src/parser/config-parser.ts:37`. `parseConfig` (40) orchestrates the seven sub-sections listed in §2.3. Uses `readJsonSafe` at line 312 for missing-file tolerance.
- **`AnalyticsParserImpl`** — `packages/sdk/src/parser/analytics-parser.ts:31`. `parseAnalytics` (34) covers the seven sources listed in §2.4. Debug logs default to latest-only; `allDebugLogs: true` returns all.

### 3.4 File I/O layer (`packages/sdk/src/io/`)

- **`FileServiceImpl`** consolidates: `readFileSync`, `readJsonSync`, `readJsonlSync`, `readJsonlStreaming`, `scanDirectorySync`, `getStats`, `exists`, `watchDirectory` (chokidar), `watchFile` (node `fs.watch`).
- **`readJsonlStreaming<T>(filePath, callback, options?)`** at `streaming-jsonl-reader.ts:48`. 64KB buffer (`BUFFER_SIZE = 65536`). Calls `callback(entry, lineIndex, byteOffset)` per complete line. Supports `fromBytePosition` for warm-start resume. Returns `{ totalLines, processedLines, finalBytePosition, errorCount }`.
- **`SqliteServiceImpl`** wraps `better-sqlite3`. Methods: `open(config)`, `prepare<T>(sql)`, `transaction<T>(fn)`, `exec(sql)`, `getDb()`. Shared single instance across `IngestService` and `QueryService` in the factory.

### 3.5 Storage / ingest (`packages/sdk/src/data/`)

- **`IngestService`** — implements `ProjectParseSink`. Key methods: `open(dbPath)`, `onProject`, `onMessage`, `onSubagent`, `onToolResult`, `onFileHistory`, `onTodo`, `onTask`, `onPlan`, plus `getFingerprint` / `upsertFingerprint` for warm-start, and `beginBulkIngest` / `endBulkIngest` to drop FTS triggers and apply fast PRAGMAs during cold start.
- **`QueryService`** — read-only path: `getProjectSlugs`, `getProjectSummaries`, `getSessionSummaries`, `getSessionMessages` (paginated), `getSessionSubagents`, `getSubagentMessages`, `getProjectMemory`, `getSessionTodos`, `getSessionPlan`, `getSessionTask`, `getToolResult`, `search` (FTS).
- **Schema** — `packages/sdk/src/data/schema.ts`, version 3. Tables: `projects`, `sessions`, `messages`, `subagents`, `tool_results`, `file_histories`, `todos`, `tasks`, `plans`, plus FTS5 virtual table `messages_fts`, `source_files` for fingerprints, and `schema_meta` for version tracking.
- **FTS text extraction** — inline helper in `ingest-service.ts:82-100`; truncates to 2000 chars; extracts user/assistant `text` blocks and emits `[tool:NAME]` markers for `tool_use` blocks.

### 3.6 Worker pool (`packages/sdk/src/workers/`)

- **`WorkerPoolImpl.parseProjects(rootDir, slugs[], onMessage)`** at `worker-pool.ts:58`. Spawns `min(cpus - 1, 8)` workers. Queue-based distribution assigns slugs to idle workers.
- **`parse-worker.ts`** — worker thread. Receives `'parse-project'` messages on `parentPort`, parses via `ProjectParserImpl.parseProjectStreaming`, batches 150 messages per `postMessage()` back to main thread.
- Main thread's `IngestService` is the sole writer; workers never touch SQLite.

### 3.7 Types layer (`packages/sdk/src/types/`)

Module → primary type:

- `projects.ts` — `SessionMessage` union (14 variants: `user`, `assistant`, `system`, `summary`, `agent-name`, `attachment`, `custom-title`, `permission-mode`, `pr-link`, `progress`, `file-history-snapshot`, `saved_hook_context`, `queue-operation`, `last-prompt`), plus `AssistantContentBlock` / `UserContentBlock`, `ToolName` enum, `SubagentMeta`, `SubagentTranscript`.
- `tasks.ts` — `TaskEntry`, `TaskItem`.
- `todos.ts` — `TodoFile`, `TodoItem`.
- `plans-data.ts` — `PlanFile`.
- `debug.ts` — `DebugLogEntry`, `DebugLogFile`, `DebugLatestSymlink`.
- `session-env.ts` — `SessionEnvEntry`.
- `file-history-data.ts` — `FileHistorySession`.
- `shell-snapshots-data.ts` — `ShellSnapshotFile`.
- `paste-cache-data.ts` — `PasteCacheFile`.
- `plugins-data.ts` — `PluginsDirectory`.
- `telemetry-data.ts` — `TelemetryFile`.
- `statsig-data.ts` — `StatsigDirectory`.
- `ide-data.ts` — `IdeDirectory`.
- `cache-data.ts` — `CacheDirectory`.
- `toplevel-files-data.ts` — `SettingsFile`, `HistoryFile`, `StatsCacheFile`, `StatusLineCommandFile`, `ActiveSessionFile`.
- `teams-data.ts` — `TeamDirectory`, `TeamConfig`, `TeamMember` (types only; no parser — see unparsed doc §1.1).
- `backups-data.ts` — `ClaudeGlobalStateBackup` (types only; no parser — unparsed doc §1.2).
- `hook-events.ts` — `HookEvent` union.
- `channel-messages.ts` — `ChannelMessage` union (separate from file parsing; used by the MCP channel plugin).

---

## 4. Rust pipeline details

### 4.1 NAPI boundary

- Compiled artifact: `crates/spaghetti-napi/spaghetti.darwin-arm64.node`, published as `@vibecook/spaghetti-sdk-native`. Loaded on demand by `packages/sdk/src/native.ts` (`loadNativeAddon`, `isNativeIngestEnabled`).
- Exported functions (`lib.rs`, line numbers from `ingest.rs`):
  - `ingest(opts: IngestOptions, on_progress?: ThreadsafeFunction) -> AsyncTask<IngestStats>` — `ingest.rs:129`. Runs on a libuv worker thread via `AsyncTask`; returns a `Promise<IngestStats>` in JS.
  - `native_version() -> String` — `lib.rs:28`. Returns `CARGO_PKG_VERSION`.
- `IngestOptions` fields: `agent_dir` (NAPI; Claude data root), `db_path`, `mode` (`"cold" | "warm"`), `progress_interval_ms?`, `parallelism?`.
- Progress callback: invoked non-blocking with `{ phase: 'scanning' | 'parsing' | 'finalizing', … }`.

### 4.2 Ingest orchestrator — `src/ingest.rs`

- `run_ingest(opts, on_progress)` at `ingest.rs:277`. Steps:
  1. Warm-start pre-check (line 285): compute fingerprint diff; if empty, return early.
  2. Project discovery: scan `<root_dir>/projects/` sequentially (line 292).
  3. Rayon thread pool built per-call, parallelism clamped to `[1, 8]`, default `min(cpu_count, 8)` (`resolve_parallelism` at line 196).
  4. Each worker calls `ProjectParser::parse_project`, pushes `IngestEvent`s to a per-project unbounded `crossbeam_channel`, then drains into a shared bounded channel.
  5. A single dedicated writer thread consumes the shared channel and writes via rusqlite (line 318).
  6. After all projects finish, fingerprints for every discovered file are emitted (line 406).
- `IngestStats` returned: `{ duration_ms, projects_processed, sessions_processed, messages_written, subagents_written, errors: Vec<IngestError> }`.

### 4.3 Project parser — `src/project_parser.rs`

- `ProjectParser::parse_project(&self, root_dir, slug, events)` at line 83. Walk order:
  1. Read/synthesize `sessions-index.json` → emit `IngestEvent::Project`.
  2. Read `memory/MEMORY.md` → `IngestEvent::ProjectMemory`.
  3. Merge index entries with discovered on-disk sessions.
  4. For each session: JSONL via `read_jsonl_streaming` (→ `Message`), subagents dir, tool-results dir, `~/.claude/file-history/<session>/`, `~/.claude/todos/<session>-agent-*.json`, `~/.claude/tasks/<session>/.lock|.highwatermark`.
  5. Emit `SessionComplete`, then `ProjectComplete` after all sessions.
- Per-line parse failures are swallowed and re-emitted as `IngestEvent::WorkerError` — only a `ChannelClosed` is fatal (writer died).

### 4.4 JSONL reader — `src/jsonl_reader.rs`

- `read_jsonl_streaming<F: FnMut(&str, u32, u64)>(path, from_byte_position, on_line) -> Result<StreamingResult, JsonlError>` at line 49. 64KB buffer, UTF-8 multi-byte-safe carry-over, resumable from arbitrary byte position. Missing files return `Ok(empty)` (not an error). Blank lines are skipped but bytes consumed.
- Callers are responsible for `serde_json::from_str` on the returned `&str` — the reader does not deserialize.

### 4.5 Event model — `src/parse_sink.rs`

`IngestEvent` variants (15 total):

- **Structural**: `Project`, `Session`, `ProjectComplete`, `SessionComplete`, `WorkerError`.
- **Content**: `ProjectMemory`, `Message` (pre-extracts `msg_type`, `uuid`, `timestamp`, token counts, FTS text), `Subagent`, `ToolResult`, `FileHistory`, `Todo`, `Task`, `Plan` (defined but currently unused — see unparsed doc §2.7).
- **Fingerprinting**: `Fingerprint`, `ClearSourceFiles`.

All events flow through `crossbeam_channel` (bounded per parallelism), consumed in order by the writer thread.

### 4.6 Writer & schema — `src/writer.rs`, `src/schema.rs`

- Driver: `rusqlite` with `prepared_cached` statement caching.
- Schema matches TS — version 3 — so both engines share the same DB file.
- Event-to-SQL: each `IngestEvent` variant has a dedicated UPSERT path in `writer.rs:383-615`.
- Transactions: one per project (opens on first data event, commits on `ProjectComplete`). `WorkerError` rolls back mid-project.
- Bulk mode (`writer.rs:314-327`): drops FTS triggers, sets `synchronous=OFF`, `journal_mode=MEMORY`, `cache_size=-256MB`, `mmap_size=30GB`. FTS triggers are recreated at finish.

### 4.7 Fingerprint store — `src/fingerprint.rs`

- `SourceFingerprint { path, mtime_ms, size, byte_position?, category, project_slug?, session_id? }`.
- `compute_diff(root_dir, stored)` walks `projects/`, `file-history/`, `todos/`, `tasks/`, returns `{ added, modified, deleted }` sets. Used by `run_ingest` for warm-start skipping.
- Eight tracked categories: `session`, `subagent`, `tool_result`, `memory`, `sessions_index`, `todo`, `task`, `file_history`.

### 4.8 FTS extraction — `src/fts_text.rs`

- `extract_message_text(&SessionMessage) -> String` at line 59. Matches TS behaviour exactly: extracts user text + tool-result strings; assistant `text` blocks plus `[tool:NAME]` markers; `summary.summary`; all other variants emit empty string. Joined with `\n`, truncated to 2000 bytes at a UTF-8 boundary.

### 4.9 Types module — `src/types/`

- `session.rs`: `SessionMessage` enum (14 variants, `#[serde(tag = "type", rename_all = "kebab-case")]`), `BaseMessageFields` flattened into each variant (`uuid`, `parent_uuid?`, `timestamp`, `session_id`, `cwd`, `version`, `git_branch`, `is_sidechain`, `user_type`, `slug?`, `permission_mode?`, `entrypoint?`), `UserMessage` / `AssistantMessage` payloads, `TodoItem`.
- `content.rs`: `AssistantContentBlock` (`thinking`, `redacted_thinking`, `text`, `tool_use`), `UserContentBlock` (`tool_result`, `text`, `document`, `image`), `ToolResultContent` untagged (`Text(String)` | `Blocks(…)`).
- `project.rs`: `SessionsIndex`, `SessionIndexEntry`, `SubagentType` (`task`/`prompt_suggestion`/`compact`), `SubagentTranscript` (with optional `SubagentMeta`), `PersistedToolResult`, `ProjectMemory`.
- `artifacts.rs`: `FileHistorySession`, `FileHistorySnapshotFile`, `TodoFile`, `TaskEntry`, `TaskItem`, `PlanFile`.
- Liberal use of `#[serde(default)]` for lenient deserialization against evolving Claude Code schemas.

---

## 5. How to extend

Checklist when adding parse coverage for a new `.claude/` artifact:

1. **TS first.** Add or update the type in `packages/sdk/src/types/`.
2. Wire a reader into the correct sub-parser (`project-parser.ts`, `config-parser.ts`, or `analytics-parser.ts`).
3. If the data is large enough to stream, add a `ProjectParseSink` method and emit from the streaming path. Otherwise return it as part of `AgentConfig` / `AgentAnalytic` / `Project` and load eagerly.
4. For persistent storage: add a table to `packages/sdk/src/data/schema.ts`, bump `SCHEMA_VERSION`, update `IngestService` with an `onX` method and prepared statement, add `QueryService` accessors.
5. **Then Rust.** Mirror the type in `crates/spaghetti-napi/src/claude/types/`, add an `IngestEvent` variant in `core/event.rs`, emit from `claude/project_parser.rs` (or a new module), and add a writer path in `core/writer.rs`.
6. Add a fingerprint category in `fingerprint.rs` if the file should participate in warm-start incrementality.
7. Update `PARSER-UNPARSED-DATA.md` to reflect the newly-closed gap.
