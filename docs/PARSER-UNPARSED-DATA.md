> **Historical gap inventory — superseded by RFC 011.** The TS-as-ground-truth
> and Rust-performance-port columns below do not describe the current common
> Rust adapter/source architecture. Retain them only as migration provenance;
> see [RFC 011](./rfcs/011-rust-observation-query-engine.md).

# Unparsed `.claude/` Data

**Status:** Historical dual-engine gap inventory.
**Updated:** 2026-07-03
**Scope:** `~/.claude/` on-disk state written by Claude Code that the spaghetti library does **not** currently ingest (or ingests incompletely). Separate doc: `PARSER-PIPELINE.md` for what *is* parsed.

Two axes are tracked per entry:

- **TS status**: ground-truth pipeline (`packages/sdk/src/`). Gaps here mean the data simply isn't in the system.
- **RS status**: performance port (`crates/spaghetti-napi/src/`). Gaps here mean the fast path is incomplete even if TS has coverage.

Severity uses this scale:

- **Critical** — actively used Claude Code feature, data now missing from every downstream surface (CLI, UI).
- **High** — frequently-written state, noticeable gap in search/analytics.
- **Medium** — niche but real data, or types already defined with no parser wired.
- **Low** — rarely read; can be deferred.

---

## 1. Directories / files with zero coverage

### 1.1 `~/.claude/teams/` — agent-teams infrastructure
- **Severity:** ~~Critical~~ → residual Medium (TS cold-start coverage landed 2026-07; Rust + live-watch remain).
- **Gated by:** `settings.json.env.CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`.
- **Shape:**
  - `teams/{team-name}/config.json`: `{ name, description?, createdAt, leadAgentId, leadSessionId, members[] }`. Each `member` has `{ agentId, name, agentType?, model?, prompt?, color?, planModeRequired?, joinedAt, tmuxPaneId, cwd, subscriptions[], backendType? }` — `description`/`model` absent in most real files; the lead often carries `agentType` but no `model`.
  - `teams/{team-name}/inboxes/{agent-name}.json`: per-agent message queue — a single JSON **array** of `{ from, text, summary?, timestamp, color?, read }` (not JSONL). `text` may itself contain embedded JSON (idle notifications, task assignments); parse leaves it as a raw string.
  - Orphaned team dirs exist in the wild: `inboxes/` present with no `config.json` (surfaced as `config: null`).
- **TS status:** **Parsed** (cold start). `config-parser.ts` walks `teams/` into `AgentConfig.teams: TeamDirectory[]`; in-memory config domain, not in SQLite/FTS. Live-updates does not yet watch `teams/`.
- **RS status:** No types, no parser (config domain is TS-only by design; revisit with the Rust config sprint).
- **Impact (remaining):** No FTS over inbox text; no live change events on team membership/inbox writes.

### 1.2 `~/.claude/backups/` — `.claude.json` state snapshots
- **Severity:** Medium.
- **Shape:** Timestamped `.json.backup` files (5 snapshots observed). Mirrors the global `.claude.json` schema at point-in-time.
- **TS status:** Type `ClaudeGlobalStateBackup` exists at `packages/sdk/src/types/backups-data.ts`, no parser wired in `analytics-parser.ts`.
- **RS status:** No types, no parser.
- **Impact:** No ability to diff or surface historical changes to global state.

### 1.3 `~/.claude/hooks/` — hook implementation scripts
- **Severity:** Medium (High if plugin dev is a primary use case).
- **Shape:** Arbitrary executables referenced from `settings.json.hooks[].command` (e.g. `claude-island-state.py`).
- **TS status:** Not read. `settings.json`'s hooks section is captured as a raw object, but the referenced scripts are never loaded, hashed, or cross-referenced.
- **RS status:** Not read.
- **Impact:** Cannot validate hook definitions, surface broken references, or display hook source in the UI.

### 1.4 `~/.claude/sessions/` — active-session PID registry
- **Severity:** Medium.
- **Shape:** `~/.claude/sessions/{pid}.json` with `{ pid, sessionId, cwd, startedAt, kind?, entrypoint?, name? }` (type `ActiveSessionFile` already defined at `packages/sdk/src/types/toplevel-files-data.ts`).
- **TS status:** Type defined, no reader. Nothing populates a live-session list.
- **RS status:** Not read.
- **Impact:** No way to know which sessions are currently running from within the library.

### 1.5 `~/.claude/settings.local.json`
- **Severity:** High.
- **Shape:** Same schema as `settings.json`; overrides global permissions/hooks/env per-project.
- **TS status:** **Not parsed.** `config-parser.ts` only reads `settings.json`.
- **RS status:** Not read.
- **Impact:** Displayed permissions don't reflect real effective permissions in a working directory.

### 1.6 `~/.claude/CLAUDE.md` (root-level, not per-project)
- **Severity:** Low.
- **Shape:** Markdown with user's global instructions.
- **TS status:** Not indexed. (Per-project `memory/MEMORY.md` under `projects/{slug}/` is parsed.)
- **RS status:** Not read.
- **Impact:** User's global Claude instructions never surface in search or UI.

### 1.7 `~/.claude/vercel-plugin-*` and `~/.claude/.idea/`
- **Severity:** Low.
- **TS status:** Not read.
- **RS status:** Not read.
- **Impact:** Minor. `.idea/` is IntelliJ-private; `vercel-plugin-device-id` / `vercel-plugin-telemetry-preference` are a few bytes each. Document as out-of-scope unless Vercel integration becomes first-class.

### 1.8 `~/.claude/.credentials.json`
- **Severity:** Must-not-parse.
- **TS status:** Correctly ignored.
- **RS status:** Correctly ignored.
- **Impact:** None. Flag in docs so future contributors don't "fix" the omission.

---

## 2. Partial coverage (data is read, but fields/records are lost)

### 2.1 Session JSONL — new fields since March 2026
- **Severity:** High.
- **Observed new fields** on live session lines:
  - `isSidechain: boolean` — flags subagent / sidechannel output.
  - `parentUuid: string | null` — parent session reference for sidechains.
  - `entrypoint: 'cli' | 'web' | 'mobile' | …` — origin of the session.
  - nested `attachment.{hookName, hookEvent, content, stdout, stderr, exitCode, command, durationMs}` — hook-result attachments.
- **TS status:** `SessionMessage` union (`packages/sdk/src/types/projects.ts`) has fields present but verify they flow through `IngestService.onMessage()` and land in `messages.data`. Nested `attachment` hook fields look incomplete in the current type — double-check.
- **RS status:** `BaseMessageFields` at `crates/spaghetti-napi/src/claude/types/session.rs` includes `is_sidechain`, `parent_uuid`, `entrypoint`. The `attachment` variant lives in the enum but the nested `attachment` payload is stored as raw JSON — not promoted to dedicated columns.
- **Impact:** Sidechain/subagent tree rendering, hook-result drill-down, and entrypoint filtering can't be implemented reliably against the DB as-is.

### 2.2 Subagent `.meta.json` sidecar
- **Severity:** Medium.
- **Shape:** `projects/{slug}/{sessionId}/subagents/agent-{id}.meta.json` with `{ agentType, description }`.
- **TS status:** Type `SubagentMeta` is defined at `packages/sdk/src/types/projects.ts:88`, and `SubagentTranscript.meta` is optional, but `ProjectParserImpl.parseSubagents()` at `project-parser.ts:433` does not read the sidecar. Subagent type is inferred from filename regex (`task` / `prompt_suggestion` / `compact`) — loses new variants.
- **RS status:** `SubagentTranscript.meta?: SubagentMeta` exists in `types/project.rs`, but `project_parser.rs` doesn't populate it either.
- **Impact:** Subagent type detection is brittle, description is lost.

### 2.3 `~/.claude/tasks/{sessionId}/` — items never loaded
- **Severity:** Medium.
- **Shape:** Directory contains `.lock`, `.highwatermark`, and numbered `{N}.json` files (actual task items).
- **TS status:** `ProjectParserImpl.parseTasks()` at `project-parser.ts:571` captures only `{ lockExists, hasHighwatermark, highwatermark }`. The numbered item files are **never read**, despite `TaskItem` existing in `packages/sdk/src/types/tasks.ts`.
- **RS status:** `TaskEntry.items?: Vec<TaskItem>` is defined in `types/artifacts.rs:55` but `project_parser.rs` populates only the lock/hwm fields.
- **Impact:** Tasks appear as metadata-only stubs; task body text is invisible to both query and search.

### 2.4 `settings.json` hook matchers
- **Severity:** High.
- **Shape:** Rich tree of `{ event: 'PreToolUse'|..., matcher: {...}, command|prompt: string }` entries under `settings.json.hooks`.
- **TS status:** The parent `SettingsFile.hooks` field exists, but matchers/commands are not promoted to first-class rows, not joined against `hooks/` script files, and not surfaced individually in the query layer.
- **RS status:** Settings aren't parsed at all (see §3.1).
- **Impact:** "What hooks run on SessionStart?" cannot be answered from SQL today.

### 2.5 `settings.json` new top-level fields
- **Severity:** Medium.
- **Fields:** `extraKnownMarketplaces`, `skipAutoPermissionPrompt`, `env.CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS`.
- **TS status:** `SettingsFile` type at `packages/sdk/src/types/toplevel-files-data.ts` needs fresh audit; any missing fields fall into `unknown` / dropped.
- **RS status:** N/A (settings unparsed).
- **Impact:** Plugin marketplace discovery, experimental-feature gating, and permission-prompt behaviour aren't visible to consumers.

### 2.6 `cache/` — only `changelog.md` is read
- **Severity:** Low.
- **TS status:** `config-parser.ts:286-299` reads only `cache/changelog.md`. Other files under `cache/` (3 subdirs observed) are ignored.
- **RS status:** Not read.
- **Impact:** Changelog is surfaced; other cache contents are not triaged — unknown whether any are useful.

### 2.7 `plans/` in Rust — event defined but never emitted
- **Severity:** High (Rust drift — active bug).
- **Shape:** `~/.claude/plans/*.md`.
- **TS status:** Parsed by `ProjectParserImpl.buildPlanIndex()` at `project-parser.ts:596`.
- **RS status:** `IngestEvent::Plan` exists in `parse_sink.rs` and `SQL_INSERT_PLAN` exists in `writer.rs`, but **no code emits it** — `project_parser.rs` has no `read_plan` function. The `plans` table is created empty and stays empty when the Rust engine runs.
- **Impact:** Switching from TS engine to Rust engine silently drops plan data.

---

## 3. Covered in TS, absent in Rust (pure drift)

The Rust crate is intentionally scoped to project-sessions ingest (RFC 003). **Scope note (verified by the 2026-07-02 engine-flow audit):** the SDK always parses config + analytics on the TS side regardless of engine (`lifecycle-owner.ts` `initialize()` runs `parseSync({ skipProjects: true })` after either ingest), so rows below marked "Not parsed" in Rust still reach `AgentConfig`/`AgentAnalytic` identically on both engines — the table describes crate scope, not production data loss. The production-visible rs-engine losses are the **DB-resident** items: `plans/` (0 rows on rs vs 35 on ts against real data) and the FTS `text_content` narrowing listed below.

### 3.1 Engine-flow audit findings (2026-07-02, real ~/.claude: 1.4 GB, 126 projects, 304k messages)

- **Cold parity**: `scripts/ingest-diff.ts` cold mode — zero diffs on both fixtures; real-data table counts match modulo the items below. rs cold 13.5 s vs ts cold 35.2 s (2.6×).
- **Live-batch parity**: zero diffs after fixing the harness (it sent a malformed `session_index` payload that Rust correctly rejected — 178 bogus diffs masked the gate; real on-disk `sessions-index.json` files deserialize fine).
- **FTS recall drift**: ~~Rust missed tool_result text~~ **fixed 2026-07-02** — the extraction logic was already at parity; the real culprit was `ToolUseResult` typed strictly to the Read tool's shape, so ANY session line with a different tool result (Bash, Edit, …) failed typed deserialization and stored empty `text_content`. Now an opaque `string | object` like TS. Real-data probe after fix: identical hit counts across engines; parity fixtures now include tool_result messages.
- **`.DS_Store` project row (ts-side)**: ~~TS cold start treats stray files under `projects/` as project slugs~~ **fixed 2026-07-02** — project scans use `directoriesOnly` (`ScanOptions.directoriesOnly`), matching Rust's `scan_project_slugs`.
- **Warm-start msg_index corruption (ts-side, was CRITICAL)**: ~~ts warm missed real session growth~~ — root cause found and **fixed 2026-07-02**: the streaming reader's line index restarts at 0 on resumed reads and `messages` upserts on `(session_id, msg_index)`, so the grown-file incremental path wrote appended messages over the HEAD of active sessions (and the live tailer had the same hole on the first post-restart append). Fixed by basing indexes at `MAX(msg_index)+1`; fingerprints now stamp from a pre-parse stat snapshot (TOCTOU); a one-shot `schema_meta` heal (`heal_msg_index_v1`) full-reparses old DBs to restore clobbered rows. rs warm remains all-or-nothing (any change → full re-ingest) — coarse but correct.
- **Everything above the DB is engine-agnostic**: `getTeams()`, config, analytics, CLI one-off outputs (`projects/sessions/todos/plan/subagents/search/stats --json`) byte-identical across engines; TUI (incl. the Team tab) drives correctly on both.

Table form for density:

| Data | TS parser | Rust status | Severity |
|---|---|---|---|
| `settings.json` | `ConfigParserImpl.parseSettings` (`config-parser.ts:70`) | Not parsed | High |
| `settings.local.json` | Not parsed in TS either — see §1.5 | Not parsed | High (when TS adds it) |
| `plugins/` (installed, marketplaces, cache, manifests) | `ConfigParserImpl.parsePlugins` (`config-parser.ts:74-171`) | Not parsed | Medium |
| `statsig/` | `ConfigParserImpl.parseStatsig` (`config-parser.ts:173-204`) | Not parsed | Low |
| `ide/*.lock` | `ConfigParserImpl.parseIde` (`config-parser.ts:206-221`) | Not parsed | Medium |
| `shell-snapshots/` | `ConfigParserImpl.parseShellSnapshots` (`config-parser.ts:223-244`) | Not parsed | Medium |
| `cache/changelog.md` | `ConfigParserImpl.parseCache` (`config-parser.ts:286-299`) | Not parsed | Low |
| `statusline-command.sh` | `ConfigParserImpl.parseStatusLine` (`config-parser.ts:301-310`) | Not parsed | Low |
| `history.jsonl` | `AnalyticsParserImpl.parseHistory` (`analytics-parser.ts:79-87`) | Not parsed | High |
| `stats-cache.json` | `AnalyticsParserImpl.parseStatsCache` (`analytics-parser.ts:70-77`) | Not parsed | High |
| `telemetry/` | `AnalyticsParserImpl.parseTelemetry` (`analytics-parser.ts:89-110`) | Not parsed | Medium |
| `debug/` | `AnalyticsParserImpl.parseDebugLogs` (`analytics-parser.ts:128-164`) | Not parsed | Medium |
| `paste-cache/` | `AnalyticsParserImpl.parsePasteCache` (`analytics-parser.ts:219-241`) | Not parsed | Low |
| `session-env/` | `AnalyticsParserImpl.parseSessionEnv` (`analytics-parser.ts:243-255`) | Not parsed | Low |
| `plans/` | `ProjectParserImpl.buildPlanIndex` (`project-parser.ts:596`) | Event defined, **no emitter** — see §2.7 | High (drift) |

## 4. Type-level drift (values parsed but collapsed to `String`)

- **`tool_name` field** — TS narrows to a discriminated union (~30 tools + `mcp__${string}`) at `packages/sdk/src/types/projects.ts:290`. Rust stores `String` in `types/content.rs:45`. New tools don't fail the type but also don't get narrowed — downstream consumers can't exhaustively match.
- **`stop_reason` field** — TS has `'end_turn' | 'tool_use' | 'stop_sequence' | 'max_tokens' | null`. Rust uses `String` (`types/session.rs:362`).
- **`TodoItem.status`** — TS uses `'pending' | 'in_progress' | 'completed'`. Rust keeps it as `String`.

Severity: Low individually, but accumulates — all three would benefit from Rust enums with `#[serde(rename_all)]`.

---

## 5. Resolved since March 2026 audit

Keep for institutional memory:

- `RedactedThinkingBlock` content type — present.
- `max_tokens` stop_reason — present in TS union.
- Tool-name enum includes `ToolSearch`, `EnterWorktree`, `ExitWorktree`, `SendMessage`, `CronCreate`/`CronDelete`/`CronList`, `LSP`, `TeamCreate`, `TeamDelete`, `TaskGet`.
- `SettingsFile` gained `effortLevel`, `enabledPlugins`, `alwaysThinkingEnabled`.
- 14 session-message variants at parity between TS and Rust.

---

## 5b. 2026-07-02/03 re-audit — shipped

The full multi-agent re-audit (`PARSER-AUDIT-2026-07-02.md`) is landed. Fixes since:

- ~~**HIGH data loss** — workflow-orchestration artifacts~~ **done**. Nested subagent transcripts (`subagents/workflows/wf_*/agent-*.jsonl`) + session-level run records (`workflows/wf_*.json`) + `journal.jsonl` are ingested by both engines under schema v4 (new `workflows` table; `subagents.workflow_id` groups nested transcripts under their run). API `getSessionWorkflows`/`getWorkflowSubagents` + a **Workflow** TUI tab. (PRs #48, #49.)
- ~~**MEDIUM** — new session message types~~ **done**. `ai-title`/`mode`/`bridge-session` + system subtypes `away_summary`/`informational` modeled in both unions; `ai-title` + system `content` indexed into FTS. Both Rust enums gained a `#[serde(other)]` backstop so a *future* unknown `type`/`subtype` no longer fails the typed parse (which had nulled ~4,900 real lines' FTS). (PR #46.)
- ~~**HIGH security** — `settings.local.json`~~ **done**. Promoted into `AgentConfig.settingsLocal` at cold start; 5 new `settings.json` keys + `PluginManifest.commands/skills/agents` typed. (PR #50.)
- ~~**LOW** — `ActiveSessionFile` + `HistoryPastedContent`~~ **done**. Stale shapes refreshed (sessions/{pid}.json new fields; history `content`→`contentHash` migration). (PR #51.)

- ~~**LOW** — config + analytics reader refresh~~ **done**. `mcp-needs-auth-cache.json` + plugins `blocklist.json` readers wired into `AgentConfig` (PR #53); telemetry event-type refresh + `session-env/*.sh` inner-script listing (PR #55); subagent `agent-{id}.meta.json` sidecar — the real free-form `agentType`/`name`/`description`, both engines (PR #56).
- ~~**LOW** — workflow live-watch (nested transcripts)~~ **done**. The live router now classifies `subagents/workflows/<wf>/agent-*.jsonl` and threads `workflowId` end-to-end, so an active workflow's nested transcripts stream into SQLite grouped under their run — via the existing `subagent`→`subagent.updated` path (both engines; the native live handler already round-trips `SubagentTranscript.workflowId`) — instead of waiting for the next cold/warm re-parse. No schema/category/Rust change. (PR #57.)

Remaining, consciously deferred (LOW value ≫ cost — documented, not built):
- **`workflows/wf_*.json` run-record live-refresh.** During an active run the run-record *summary* row (status, token/tool totals, agent count) still refreshes only on the next re-parse. Deferred: it's derived analytics already captured correctly at cold start, and the substantive content — the nested transcripts — now streams live (above). A live path would need a first-class `workflow` category + `Change`/`ChangeTopic`/`topicToKey`/`candidateKeysFor` + native `live_ingest` variant across both engines, for marginal live-summary freshness.
- **Workflow `result`-text FTS.** The run record's `data`/`journal` result text isn't in FTS. Deferred: the searchable "what the agents did" content is the transcripts, already FTS-indexed via `messages` (grouped by `workflow_id` since PR #48); a `workflows_fts` table would need a SCHEMA_VERSION bump + triggers + Rust mirror for low marginal recall over mostly-structured metadata.

## 6. Prioritized fix list

1. ~~**Critical** — Add `teams/` parser~~ — **done (TS cold start, 2026-07)**. Remaining: live-watch `teams/`, FTS over inbox text, Rust port with the config sprint (item 8).
2. ~~**High** — Parse `settings.local.json`~~ — **done (2026-07-03, PR #50)**: `AgentConfig.settingsLocal` populated at cold start; effective-permission merge documented on the field.
3. ~~**High** — Fix Rust `plans/`~~ — **done (2026-07-02)**: `parse_plans` mirrors `buildPlanIndex` (slug/title/content/size), emitted before the project fan-out under a pseudo-slug transaction; 35/35 plan rows on real data, plans now in the parity fixtures. Note: `sessions.plan_slug` is populated by NEITHER engine's DB path (the TS linkage only runs in the non-streaming in-memory parse) — tracked as a shared gap.
3b. ~~**High** — Rust FTS extraction~~ — **done (2026-07-02)**: root cause was the strict `ToolUseResult` type, not the extractor (see §3.1); real-data search hit counts now identical across engines.
3c. ~~**Medium** — TS warm-start freshness + `.DS_Store` project slug~~ — **done (2026-07-02)**: msg_index rebasing + snapshot fingerprints + one-shot heal; `directoriesOnly` project scans (see §3.1).
4. **High** — Promote session-JSONL new fields (`isSidechain`, `parentUuid`, `entrypoint`, nested `attachment`) to dedicated columns / writer fields.
5. **High** — First-class hook matchers from `settings.json.hooks[]`.
6. **Medium** — Subagent `.meta.json` sidecar reader (both pipelines).
7. **Medium** — `tasks/{sessionId}/{N}.json` item files (both pipelines).
8. **Medium** — Rust parity sprint: analytics pipeline, settings, plugins, statsig, ide, cache, shell-snapshots.
9. **Low/Medium** — `backups/`, `sessions/{pid}.json`, `hooks/` directory, root `CLAUDE.md`.
10. **Low** — Enum-narrow `tool_name`, `stop_reason`, `TodoItem.status` in Rust.
