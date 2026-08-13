# RFC 005: Live Updates

**Status**: Historical TypeScript live-update design; superseded by
[RFC 011](./011-rust-observation-query-engine.md)
**Created**: 2026-04-20
**Author**: James Yong + Claude

---

## Summary

Add opt-in live-update support to `@vibecook/spaghetti-sdk`. While the library is running, changes to `~/.claude/` incrementally flow into the SQLite cache (including FTS5) and fan out to subscribers as typed events. Cold/warm start stays exactly as today — live updates are strictly additive.

To make room for the new component cleanly, split the current `AgentDataServiceImpl` into three collaborators: a slim lifecycle owner, a data store that owns reads plus in-memory config/analytics, and a new `LiveUpdates` component that owns the watcher, offset checkpoints, and incremental writes. Event emission happens at the application layer, after each `COMMIT` returns — no SQLite hooks involved.

Target: search stays current within ~100 ms of a file append, readers never block, both TS and Rust engines can drive the live path through the same interface.

---

## Motivation

Today the library is strictly pull-based. `AgentDataServiceImpl.initialize()` does a cold or warm start, then reads are served from SQLite (and in-memory caches for config/analytics). If `~/.claude/` changes while the app is open, consumers see nothing until someone explicitly calls `rebuild()`.

The Electron app, the CLI's hooks monitor, and any future web surface all want the opposite shape:

1. The library knows when data changes.
2. It surfaces those changes as events consumers can subscribe to.
3. Search over the newly-arrived data works immediately.

Two earlier design directions were considered and rejected:

- **Pure memory-overlay live updates** (keep the SQLite cache stable during the session, layer new data in memory, reconcile on next warm-start). Clean lifecycle, but SQLite FTS5 can't see the overlay — search would be stale until the next app open. Unacceptable.
- **Event emission via SQLite hooks** (`update_hook` / `commit_hook`). `better-sqlite3` does not expose these APIs ([#62](https://github.com/WiseLibs/better-sqlite3/issues/62)), only `rusqlite` does. Building change events on hooks would fork the TS and Rust engines into fundamentally different architectures. Also, hooks fire per-row inside the write transaction and cannot query the database — a poor fit for emitting typed, payload-carrying events anyway.

The remaining shape — incremental writes to the real DB under WAL, with application-layer event emission after `COMMIT` — is what every production log-ingest system (Vector, Fluent Bit, Loki) converges on, and it ports cleanly between the two bindings because nothing depends on hook APIs.

Separately, `AgentDataServiceImpl` has drifted into a god class: it owns lifecycle, caches, read pass-through, engine selection, progress events, and fingerprints. Adding live updates without first splitting responsibilities would compound the drift. The refactor is a prerequisite, not a follow-up.

---

## Non-Goals

1. **Not replacing cold/warm start.** Live updates only apply while the process is alive. On next open, the existing warm-start path reconciles the disk, exactly as today.
2. **Not a CRDT or sync layer.** No cross-process coordination, no replication, no Electric/crsqlite-style shape-sync. Single-writer, single-process.
3. **Not optimizing FTS5 fragmentation beyond an idle `('merge', N)` timer.** Full `'optimize'` stays gated to cold-start after a large diff.
4. **Not reworking the worker pool.** Cold-start parallelism stays as is. Live updates run on a dedicated writer thread that's separate from the cold-start workers.
5. **Not making Rust drive the watcher.** `@parcel/watcher` in TS owns the filesystem side. The Rust crate gains a single new NAPI entry (`live_ingest_batch`) that wraps the existing writer for parity; it never touches the watcher.
6. **Not changing the SQLite schema.** Live updates write to existing tables only. No seq counter persists to disk — events are fire-and-forget for live UI; restart reconciliation is handled by the existing warm-start path.
7. **Not adding live updates for `debug/`, `telemetry/`, `paste-cache/`, `session-env/`.** They're noisy and low-value; pull-on-demand stays fine.

---

## Architecture Overview

Four components, each with a narrow responsibility:

```
┌──────────────────────────────────────────────────────────────────┐
│  SpaghettiAppService (unchanged public API surface)              │
│                                                                  │
│  ┌───────────────────────┐   ┌──────────────────────────────┐   │
│  │ LifecycleOwner        │   │ AgentDataStore               │   │
│  │ (was AgentDataService)│──►│ - SQLite reads (was Query)   │   │
│  │ - cold/warm/native    │   │ - config/analytics caches    │   │
│  │ - engine selection    │   │ - subscriber registry        │   │
│  │ - progress events     │   │ - snapshot read for useSync  │   │
│  │ - start/stop live     │   │                              │   │
│  └──────────┬────────────┘   └──────────────┬───────────────┘   │
│             │                               ▲                   │
│             │ starts (opt-in)               │ emits events       │
│             ▼                               │                   │
│  ┌──────────────────────────────────────────┴──────────────┐    │
│  │ LiveUpdates                                              │    │
│  │ - @parcel/watcher subscriptions per fingerprint cat.     │    │
│  │ - per-path byte-offset checkpoints (persisted)           │    │
│  │ - coalescing queue (50–100 ms trailing-edge debounce)    │    │
│  │ - incremental parser (reuses project-parser helpers)     │    │
│  │ - writer: BEGIN IMMEDIATE → writes → COMMIT → emit       │    │
│  └──────────────────────────────────────────────────────────┘    │
└──────────────────────────────────────────────────────────────────┘
```

### Lifecycle ownership

- `LifecycleOwner` (new name for the trimmed `AgentDataServiceImpl`) retains: `initialize()`, `rebuild()`, engine selection (TS workers vs Rust NAPI), cold/warm start orchestration, progress event emission.
- On `initialize({ live: true })`, after cold/warm start succeeds, it constructs and starts a `LiveUpdates` instance wired to the same `IngestService` (for writes) and the same `AgentDataStore` (for event fan-out).

### Read ownership

- `AgentDataStore` absorbs today's `QueryService` plus the `cachedConfig` / `cachedAnalytics` fields. Becomes the single "give me data" surface for `SpaghettiAppService` and the React layer.
- Also owns the **subscriber registry**: a map of `topic key → Set<listener>` and a `globalFirehose: Set<listener>`. Not a general event bus — it's typed to `Change` (see below).
- Exposes `getSnapshot()` accessors shaped for `useSyncExternalStore` semantics.

### Live writes

- `LiveUpdates` owns the hot path. On file events, it parses deltas (byte-offset tail for JSONL; full re-read for atomic-write files), buffers parsed rows in a coalescing queue, and every 50–100 ms flushes one `BEGIN IMMEDIATE` → write-rows → `COMMIT` through the existing `IngestService`.
- Immediately after `COMMIT` returns, it computes the resulting `Change[]` from what it just wrote and calls `store.emit(change)` for each. The write payload carries enough context that subscribers don't need to re-query.

### Engine parity

- TS path: `LiveUpdates.flushBatch()` calls `IngestService.onMessage` / etc. directly.
- Rust path: `LiveUpdates.flushBatch()` calls a new `nativeAddon.live_ingest_batch(rows)` NAPI entry that wraps the existing Rust writer. Event emission still happens in TS (the NAPI call returns the effective row list; TS emits). The Rust side gains ~40 LOC; no parallel event pipeline.

---

## SQLite & FTS5 Configuration

Set identically in both engines on every connection open:

```
journal_mode = WAL
synchronous  = NORMAL
busy_timeout = 5000
wal_autocheckpoint = 1000
temp_store   = MEMORY
cache_size   = -65536
mmap_size    = 268435456
```

Write discipline:

- **One writer connection**, period. Readers are separate connections (current `QueryService` pattern already does this).
- **`BEGIN IMMEDIATE`** for every batch — avoids the "upgrade from read to write lock" deadlock trap.
- **Time-windowed batching** in `LiveUpdates`: drain events until queue hits `MAX_BATCH=200` or `BATCH_WINDOW_MS=75` elapses, whichever first. Flush as one transaction.
- **FTS5** stays on the existing content triggers. No per-commit `'optimize'`.

Idle maintenance (new): a 60-second idle timer owned by `LifecycleOwner`. When triggered and the process is idle:

- `PRAGMA wal_checkpoint(TRUNCATE)` if WAL file > 4 MB.
- `INSERT INTO messages_fts(messages_fts) VALUES('merge', 200)` — incremental FTS5 compaction.
- `PRAGMA optimize`.

Full `INSERT INTO messages_fts(messages_fts) VALUES('optimize')` runs only at cold-start after a diff rebuilds >10% of rows.

### Event sequence numbering

Every emitted `Change` carries `{ seq, ts, ... }` where `seq` is an in-memory monotonic counter reset on each process start. This is purely for debugging/observability — it is **not persisted** and there is no replay API. Subscribers that attach mid-session see only events from that point forward; to catch up, they read current state from SQLite via the normal `store.getX()` methods, which already reflect every live-committed row. Across restarts, warm-start reconciles the disk and the UI's next snapshot read is authoritative.

---

## Filesystem Watcher

- **Library: `@parcel/watcher`.** Native, VS Code-proven, spaghetti already ships a napi prebuild pipeline so the binary matrix is priced in. Uses single-epoll on Linux (no inotify quota explosion), correct Windows buffer sizing, and has a `writeSnapshot`/`getEventsSince` API that solves "what changed while we were down".
- **Fallback:** thin `Watcher` interface so `chokidar` can be dropped in for tests or any platform where `@parcel/watcher` fails to build.

### Watch topology

| Path | Mode | Ignored |
|---|---|---|
| `~/.claude/projects/` | recursive | `**/node_modules/**` (defensive) |
| `~/.claude/tasks/` | recursive | — |
| `~/.claude/file-history/` | recursive | — |
| `~/.claude/todos/` | non-recursive | — |
| `~/.claude/plans/` | non-recursive | — |
| `~/.claude/` | non-recursive, filtered to specific files | rest of tree |

Hard ignore anywhere: `**/debug/**`, `**/telemetry/**`, `**/paste-cache/**`, `**/session-env/**`, `**/*.tmp`, `**/.DS_Store`. Not negotiable — these would dominate event traffic and they're not on the live-update path anyway.

### Byte-offset tailing

Per-file state `Checkpoint { path, inode, size, lastOffset, lastMtimeMs }`, persisted to `~/.claude/.spaghetti-live-state.json` via a 2-second debounced atomic-rename write.

On each file event, after 30 ms trailing-edge debounce (hard flush at 200 ms):

1. `fstat` the path.
2. If `inode` changed OR `size < lastOffset` → treat as rewrite. Reset `lastOffset = 0` and reparse the whole file. Emit a `session.rewritten` (or category-appropriate) event.
3. Else if `size > lastOffset` → tail `[lastOffset, size)`, split on `\n`, keep the partial final chunk in a trailing buffer. Parse complete lines only. Advance `lastOffset` to the end of the last complete line.
4. Else no-op.

### Backpressure

If the writer queue stays saturated >5 seconds on a particular file, `LiveUpdates` downgrades that file to a catch-up warm-start re-ingest (one-shot, not mid-session persistent) and logs. Simpler than trying to backpressure FSEvents.

---

## Public API

Opt-in at construction. Not a breaking change.

```ts
const api = createSpaghettiService({ live: true });
await api.initialize();

// Firehose
const dispose = api.live?.onChange((e) => console.log(e.type));

// Scoped + throttled
api.live?.onChange(
  { kind: 'session', slug: 'p008-spaghetti', sessionId: 'abc-...' },
  (e) => { /* handler narrowed to session events */ },
  { throttleMs: 250, latest: true },
);

// Async iteration (CLI tailing) — sugar over onChange + bounded ring buffer
for await (const e of api.live!.events()) { /* ... */ }

// Pre-warm a watcher before any subscription lands (optional)
const unprewarm = api.live?.prewarm({ kind: 'session', slug });

// Disposal
await api[Symbol.asyncDispose]();
```

If `live: false` (the default), `api.live` is `undefined`. The type-level opt-in means CLI one-shots can't accidentally pay for watcher infrastructure they'll never read.

### Lazy watching + ref-counting

Watchers are **attached on demand**. `LiveUpdates.start()` loads checkpoints and wires up nothing else. When the first subscription (or `prewarm` call) matches a watch scope — e.g. `{ kind: 'session', slug: 'foo' }` — the corresponding `~/.claude/projects/foo/` watcher attaches. When the last listener for that scope disposes, the watcher detaches. This keeps CPU/FD usage proportional to what the consumer is actually reading. Tradeoff: first-subscriber pays watcher setup (~100 ms on a big tree); `prewarm` is the escape hatch for surfaces that want the cost amortized.

### Per-subscription throttling

`onChange` takes an optional `{ throttleMs, latest }` option:

- `throttleMs`: minimum gap between listener invocations.
- `latest: true` — drop intermediate events, emit only the most recent at each throttle boundary.
- `latest: false` (default when `throttleMs` is set) — coalesce into an array, emit all since the last tick.

Useful for banners and stats panels that want "redraw ≤4×/sec regardless of append rate". React hooks don't need this — React's own batching already coalesces renders — but non-React consumers benefit.

Event shape is a discriminated union:

```ts
type Change =
  | { type: 'session.message.added'; seq: number; ts: number;
      slug: string; sessionId: string; message: SessionMessage }
  | { type: 'session.created';       seq: number; ts: number;
      slug: string; sessionId: string; entry: SessionIndexEntry }
  | { type: 'session.rewritten';     seq: number; ts: number;
      slug: string; sessionId: string }
  | { type: 'subagent.updated';      seq: number; ts: number;
      slug: string; sessionId: string; agentId: string }
  | { type: 'tool-result.added';     seq: number; ts: number;
      slug: string; sessionId: string; toolUseId: string }
  | { type: 'file-history.added';    seq: number; ts: number;
      sessionId: string; hash: string; version: number }
  | { type: 'todo.updated';          seq: number; ts: number;
      sessionId: string; agentId: string }
  | { type: 'task.updated';          seq: number; ts: number;
      sessionId: string }
  | { type: 'plan.upserted';         seq: number; ts: number;
      slug: string }
  | { type: 'settings.changed';      seq: number; ts: number;
      file: 'settings' | 'settings.local' };
```

`seq` is an in-memory counter (per process lifetime) useful for logs and ordering; not persisted. `ts` is `Date.now()` at emit time.

Settings/config are tracked as "changed" only (the new payload lives on `store.getConfig()`); they don't go through SQLite live ingest, only the in-memory cache is refreshed and a notification fires.

React hooks live in `packages/sdk/src/react/live/` and are built on `useSyncExternalStore` (same pattern TanStack Query v4+ uses internally):

```ts
useLiveSessionMessages(slug, sessionId): { messages, isLoading }
useLiveSessionList(slug?): SessionSummary[]
useLiveSettings(): Settings
useLiveChanges(topic?): Change | null  // last event, for banners
```

---

## Rollout Plan

Five phases. Each is independently shippable and independently valuable.

### Phase 1 — Refactor `AgentDataService` into `LifecycleOwner` + `AgentDataStore`

Pure refactor. No behavior change. Move read methods and cached config/analytics from `AgentDataServiceImpl` into a new `AgentDataStoreImpl`. Trim `AgentDataServiceImpl` (renamed to `LifecycleOwner` internally, public class name `AgentDataService` kept for now to avoid churn). Add a stub `store.emit(change)` that does nothing yet. Unit tests: store works against a prepared SQLite file without needing the parser layer — that's the whole point.

### Phase 2 — `LiveUpdates` skeleton + `@parcel/watcher` wiring

Add the `LiveUpdates` component with `@parcel/watcher` watches on `projects/` and `todos/` only (the two most common live cases). Offset checkpoints. Incremental JSONL tail. `BEGIN IMMEDIATE` / batch-commit / empty `store.emit()` call-site — still no subscribers. Verify no regressions in cold/warm start.

### Phase 3 — Change event types, subscriber registry, React hooks

Wire the `Change` union, subscriber registry on `AgentDataStore`, public `api.live` surface. Connect `LiveUpdates` commit site to `store.emit()`. Ship `useSyncExternalStore`-based hooks. Integration tests: write a fixture JSONL line while a test subscriber is listening; assert event + SQLite row + search hit within <200ms.

### Phase 4 — Rust NAPI `live_ingest_batch` entry

Add one exported function to `spaghetti-napi` that takes a list of parsed rows and writes them via the existing Rust writer, returning the effective row list. `LiveUpdates` routes through it when `engine: 'rust'`. Diff harness gains a live-batch fixture.

### Phase 5 — Idle maintenance + expanded category coverage

60 s idle timer for `wal_checkpoint`, FTS5 `('merge', 200)`, `PRAGMA optimize`. Extend `LiveUpdates` to `tasks/`, `file-history/`, `plans/`, and the settings files.

---

## Risks and Mitigations

| Risk | Likelihood | Mitigation |
|---|---|---|
| `@parcel/watcher` prebuild fails on some platform | Medium | Thin `Watcher` interface; `chokidar` fallback path; CI builds both. |
| inotify limit exhausted on Linux power users | Medium | Parcel uses single epoll, not per-dir inotify — mostly priced-in. Detect EMFILE/ENOSPC and surface a clear error pointing to the sysctl fix. |
| Writer queue saturates under a hot session | Medium | Bounded channel + per-file dedup + >5 s fallback to warm-start re-ingest. |
| better-sqlite3 version drift introduces silent FTS regressions | Low | Pin both bindings' bundled SQLite to ≥3.51.3 (a known WAL corruption bug existed in 3.7.0–3.51.2). |
| Subscribers leak memory via forgotten unsubscribes | Medium | Dispose-handle API only (no string-keyed listeners); `useSyncExternalStore` ties lifecycle to React components automatically. |
| Atomic-rename saves flap as delete-create | High on settings files | 150 ms coalescer around delete/create on paths known to be atomic-write style. |
| Event bursts cause React rerender storms | Medium | `useLiveSessionMessages` batches via `queueMicrotask`; hook consumers choose their granularity via topic filters. |

---

## Open Questions

1. **Do config/analytics changes live-update, or stay pull-only?** Current lean: settings yes (small and high-value), analytics no (debug logs would dominate events). Deferred to Phase 5.

### Resolved during design

- **Lazy vs. eager watcher attachment.** Lazy, with ref-counting per scope and a `prewarm({ topic })` escape hatch for surfaces that want startup cost amortized.
- **Per-subscription throttling.** Supported via `{ throttleMs, latest }` option on `onChange`.
- **`onChange` vs `events()`.** `onChange` is the primitive; `events()` is sugar built on top (bounded ring buffer, drop-oldest on overflow with optional `onDrop` callback). React always uses `onChange` via `useSyncExternalStore`.
- **Cross-process coordination.** Not supported. Documented as "single live-instance per user"; CLI `live` mode + Electron app simultaneously on the same `~/.claude/` is undefined behavior. Revisit only if real demand appears.
- **Replay across process restarts.** Not needed. Warm-start reconciles disk on next open, and `getSnapshot` reads current SQLite — UI never relies on event history for correctness. No `seq` persistence, no `replaySince` API.

---

## References

- [SQLite: Write-Ahead Logging](https://www.sqlite.org/wal.html) — WAL semantics.
- [SQLite FTS5](https://www.sqlite.org/fts5.html) — `'merge'`, `'optimize'`, `'rebuild'`.
- [better-sqlite3 #62](https://github.com/WiseLibs/better-sqlite3/issues/62) — confirmed no `update_hook`/`commit_hook`.
- [rusqlite hooks](https://docs.rs/rusqlite/latest/rusqlite/hooks/index.html) — available but we intentionally don't use them (parity).
- [@parcel/watcher](https://github.com/parcel-bundler/watcher) — VS Code's watcher since 2021.
- [Fluent Bit tail](https://docs.fluentbit.io/manual/data-pipeline/inputs/tail), [Vector buffering](https://vector.dev/docs/architecture/buffering-model/) — backpressure patterns.
- [TanStack Query v4](https://tanstack.com/blog/announcing-tanstack-query-v4) — `useSyncExternalStore` integration reference.
- `docs/PARSER-PIPELINE.md`, `docs/PARSER-CLASS-DIAGRAM.md`, `docs/PARSER-UNPARSED-DATA.md` — current architecture.
