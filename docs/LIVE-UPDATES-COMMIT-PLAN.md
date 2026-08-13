> **Historical implementation sequence — superseded by RFC 011.** The
> TypeScript watcher/writer and `api.live` commits below are retained as
> provenance for the repository oracle, not as current work. See the
> [Phase 10 closure ledger](./rfcs/011-phase-10-closure.md).

# Live Updates — Commit Plan

**Status:** Historical RFC 005 implementation sequence.
**Created:** 2026-04-20
**Companion docs:** `docs/rfcs/005-live-updates.md`, `docs/LIVE-UPDATES-DESIGN.md`.

Each commit below is scoped to a single, reviewable change that leaves the repo in a green state (`pnpm typecheck && pnpm validate` must pass after each). Commit messages follow the existing conventional-commits style with a `(RFC 005)` suffix.

Branch strategy: one long-lived branch `rfc-005-live-updates` off `main`. Sub-branches per phase optional. Ship to `main` either per-phase (preferred) or once the full arc lands.

---

## Phase 0 — Docs (pre-implementation)

- **C0.1** `docs(parser): add pipeline, class-diagram, unparsed-data reference` — commits the three `PARSER-*.md` files.
- **C0.2** `docs(rfc): add RFC 005 live-updates + design + commit plan` — commits `rfcs/005-live-updates.md`, `LIVE-UPDATES-DESIGN.md`, this file.

---

## Phase 1 — Store / lifecycle split (pure refactor)

**Goal:** `AgentDataServiceImpl` loses its god-class status. Reads + caches move into a new `AgentDataStoreImpl`. No behavior change observable from the public API.

- **C1.1** `refactor(sdk): introduce AgentDataStore skeleton (RFC 005)` — new file `packages/sdk/src/data/agent-data-store.ts`. Interface + impl that owns a `QueryService` internally and exposes the same read methods by delegation. Not yet wired into the service. Unit tests against a prepared SQLite fixture prove it works in isolation.
- **C1.2** `refactor(sdk): move config/analytics caches into AgentDataStore (RFC 005)` — `cachedConfig`/`cachedAnalytics` + `getConfig`/`getAnalytics`/`setConfig`/`setAnalytics` move from `AgentDataServiceImpl` to the store. Service now delegates those calls.
- **C1.3** `refactor(sdk): route reads from AgentDataService through store (RFC 005)` — every `getX()` method on `AgentDataServiceImpl` becomes a one-line delegation to `store.getX()`. Public API surface unchanged.
- **C1.4** `refactor(sdk): rename AgentDataServiceImpl internals to LifecycleOwner (RFC 005)` — file rename (`agent-data-service.ts` → `lifecycle-owner.ts`), keep the exported class name `AgentDataService` as a re-export so consumers don't churn. Add stub `store.emit(change)` + `store.subscribe()` that no-ops but compiles against the full `Change` union.

Checkpoint: `pnpm test` green, public SDK types unchanged, playground app still works.

---

## Phase 2 — `LiveUpdates` skeleton (no subscribers yet)

**Goal:** New components exist and work end-to-end against `projects/` + `todos/` only. `store.emit()` still no-ops so no events escape. Cold/warm start paths untouched.

- **C2.1** `feat(sdk): add Watcher interface + @parcel/watcher impl (RFC 005)` — `live/watcher.ts`, adds `@parcel/watcher` dep, `createParcelWatcher()` + `createChokidarWatcher()` fallback. Unit tests with a temp dir.
- **C2.2** `feat(sdk): add CheckpointStore for byte-offset state (RFC 005)` — `live/checkpoints.ts`, atomic-rename persistence to `~/.claude/.spaghetti-live-state.json`. Unit tests: roundtrip, partial write recovery.
- **C2.3** `feat(sdk): add CoalescingQueue (RFC 005)` — `live/coalescing-queue.ts`, dedup-by-path, trailing-edge debounce, drain windowing. Unit tests: collapse priority, drain boundaries.
- **C2.4** `feat(sdk): add IncrementalParser for JSONL tail (RFC 005)` — `live/incremental-parser.ts`, reuses existing `readJsonlStreaming` with `fromBytePosition`. Handles inode change, size decrease, partial final line. Unit tests.
- **C2.5** `feat(sdk): add path Router for live categories (RFC 005)` — `live/router.ts`, pure classification function. Exhaustive unit tests including ignored paths.
- **C2.6** `feat(sdk): add IngestService.writeBatch for live commits (RFC 005)` — `BEGIN IMMEDIATE` → per-category dispatch → `COMMIT`. Integration test: call with a synthetic `ParsedRow[]`, assert SQLite rows + FTS hit.

  **Resolution pass — 2026-04-20 (C2.4b + C2.6b).** The initial C2.6 landing (`4cafc4a`) intentionally shipped as scaffolding + an explicit throw because the thin `{ category, payload }` ParsedRow couldn't feed the writer's `onX(slug, sessionId, domainObject, ...)` methods without adapter logic. Two follow-up commits close the gap:
  - `ba682c4` (C2.4b) — reshape `ParsedRow` into a discriminated union whose per-variant payloads match `onX` signatures and `Change` variant fields verbatim (`msgIndex` + `byteOffset` for messages, aggregated `SubagentTranscript` for subagents, filename-extracted IDs for tool-results / todos / file-history, full `PlanFile` for plans).
  - `6a5171a` (C2.6b) — replace the scaffold throw with real `BEGIN IMMEDIATE` → discriminated-union dispatch → `COMMIT`, plus per-category `Change` construction stamped with `seq` + `ts`. `project_memory` and `session_index` rows write through but emit no `Change` (no matching union variant).

- **C2.7** `feat(sdk): wire LiveUpdates orchestrator (projects/ + todos/) (RFC 005)` — `live/live-updates.ts`. `createSpaghettiService({ live: true })` now constructs it; `start()` loads checkpoints + spawns writer loop but doesn't attach watchers yet. Integration test: write a fixture JSONL line with `onChange` registered (no-op callback), assert SQLite row appears.

Checkpoint: live writes reach SQLite when watchers are manually attached in tests; no public event surface yet.

---

## Phase 3 — Change events + React hooks

**Goal:** Consumers can subscribe and receive typed events. Lazy watcher attachment lands. React hooks work.

- **C3.1** `feat(sdk): add Change union + subscriber registry (RFC 005)` — `live/change-events.ts` + `live/subscriber-registry.ts`. Topic matching, throttle options, dispose handles. Unit tests for topic matrix.
- **C3.2** `feat(sdk): lazy watcher attachment + ref-counting (RFC 005)` — `LiveUpdates` only attaches a watcher when the first matching subscription/prewarm lands, detaches when ref-count hits zero. Tests.
- **C3.3** `feat(sdk): emit change events after live commits (RFC 005)` — `IngestService.writeBatch` returns `WriteResult.changes`; `LiveUpdates` calls `store.emit(change)` for each. Integration test: write JSONL line → subscriber receives `session.message.added` → SQLite row + FTS hit < 200 ms.
- **C3.4** `feat(sdk): expose api.live public surface (RFC 005)` — `onChange`/`events()`/`prewarm()`/`[Symbol.asyncDispose]` on `SpaghettiAppService`. `events()` as ring-buffered sugar over `onChange`. Tests.
- **C3.5** `feat(sdk): add React live hooks (RFC 005)` — `react/live/use-live-session-messages.ts` + siblings, all built on `useSyncExternalStore`. `@testing-library/react` tests: mount, emit change, assert exactly one rerender with fresh data.

Checkpoint: end-to-end demo in playground app — chat view auto-updates when a session JSONL is appended externally.

---

## Phase 4 — Rust parity

**Goal:** When `engine: 'rust'`, live writes go through the native addon's writer for identical performance + schema behavior.

- **C4.1** `refactor(napi): expose writer::write_batch_with_tx (RFC 005)` — writer.rs gains a reusable batch-write API with the same transaction semantics cold-start uses. No behavior change.
- **C4.2** `feat(napi): add live_ingest_batch NAPI export (RFC 005)` — `src/live_ingest.rs` + `#[napi]` binding. Rust-side tests against a synthetic row list. Updates `crates/spaghetti-napi/index.d.ts`.
- **C4.3** `feat(sdk): route IngestService.writeBatch through native when engine=rust (RFC 005)` — `writeBatch` detects native mode and calls `nativeAddon.live_ingest_batch(rows)`. Diff harness gains a `live-batch/` fixture: same live sequence applied via TS and Rust engines, assert bit-identical DB state.

Checkpoint: live updates work under both engines; existing diff-harness CI gates parity.

---

## Phase 5 — Maintenance + expanded coverage

**Goal:** Production-ready: idle compaction, every live-capable `.claude/` dir covered, settings file live-refreshes.

- **C5.1** `feat(sdk): add IdleMaintenance (WAL + FTS merge) (RFC 005)` — 60 s idle timer that runs `PRAGMA wal_checkpoint(TRUNCATE)` (if WAL > 4 MB), `INSERT INTO messages_fts(messages_fts) VALUES('merge', 200)`, `PRAGMA optimize`. Pause during active writes.
- **C5.2** `feat(sdk): live updates for tasks/ (RFC 005)` — router category + incremental parser handling + `task.updated` event emission.
- **C5.3** `feat(sdk): live updates for file-history/ (RFC 005)` — same pattern; emit `file-history.added`.
- **C5.4** `feat(sdk): live updates for plans/ (RFC 005)` — same pattern; emit `plan.upserted`.
- **C5.5** `feat(sdk): live updates for settings + settings.local (RFC 005)` — atomic-rename detection (150 ms coalescer), full re-parse, `store.setConfig(...)`, emit `settings.changed`. No SQLite write.

Checkpoint: RFC 005 closed.

---

## Gate criteria per commit

Every commit must pass, in this order, before the next commit starts:

1. `pnpm typecheck` — no TS errors.
2. `pnpm validate` — formatting + lint + per-package gates.
3. `pnpm -r build` — all packages compile.
4. New/affected tests green.

If any gate fails, the commit is fixed in place (amend is OK pre-push only) before proceeding.

## Non-goals for this rollout

- No Rust work until Phase 4.
- No CLI or Electron app changes — consumers follow automatically.
- No plugin changes (channels, hooks plugins).
- No schema migration at any phase (`SCHEMA_VERSION` stays at 3).
- No changes to cold/warm start behavior.
