> **Historical migration plan — superseded by RFC 011.** The TypeScript
> factory, ingest-plane, SQLite, and synchronous API details below no longer
> describe a production entry graph. See the
> [Phase 10 closure ledger](./rfcs/011-phase-10-closure.md).

# PR Plan: Align SDK to Three-Plane Shape

**Status:** Implemented (façades + factory composition)  
**Created:** 2026-07-10  
**Implemented:** 2026-07-10  
**Companion:** `docs/TWO-PLANE-INGEST-ARCHITECTURE.md`  
**Superseded in part:** 2026-08-08 — Plane 3 (RuntimeBridge, hooks, channels) was
removed by [RFC 007](./rfcs/007-retire-runtime-bridge.md). PR6 and every Plane 3
reference below are historical; the shipped architecture has two planes.  
**Goal:** Make the SDK composition match the north-star diagram **without** rewriting cold ingest, changing public query APIs, or breaking CLI.

```text
                    ┌──────────────────────────┐
                    │   AgentSource adapter    │  (claude-code today)
                    │   roots, formats, plugins│
                    └────────────┬─────────────┘
           ┌─────────────────────┼─────────────────────┐
           ▼                     ▼                     ▼
    StaticIngest            LiveDiskIngest        RuntimeBridge
    cold/warm/full          watch+delta           hooks/channels
           │                     │                     │
           └──────────┬──────────┴──────────┬──────────┘
                      ▼                     ▼
                 DurableStore          EventBus
                 (SQLite+FTS)     (typed Change + RuntimeEvent*)
                      │                     │
                      └──────────┬──────────┘
                                 ▼
                           SpaghettiAPI
```

\* `RuntimeEvent` is **stubbed / documented** in this stack; full runtime bus is a later stack.

---

## 1. Principles

| Do | Don't |
|---|---|
| Add thin façades and one Claude Code source module | Rewrite parsers or native ingest |
| Keep public `SpaghettiAPI` method signatures stable | Rename `getProjectList` / `search` / etc. |
| Prefer re-exports + wrappers over file moves in PR1–2 | Big-bang `git mv` of half the tree |
| One PR = one diagram box (or glue) | "Refactor everything" mega-PR |
| Tests green after every PR | Ship greenfield multi-agent |

**Success criteria for the whole stack**

- [ ] `createSpaghettiService` composes: `source` → three planes → store → API  
- [ ] New code can import `AgentSource`, `StaticIngest`, `LiveDiskIngest`, `RuntimeBridge`, `DurableStore` by name  
- [ ] `rootDir` / default `~/.claude` / `~/.spaghetti` defaults resolve through the source adapter  
- [ ] No intentional public API break; CLI and playground need zero call-site changes (or only optional new options)  
- [ ] `pnpm --filter @vibecook/spaghetti-sdk test` + `pnpm test:ingest-diff` green  

---

## 2. Target layout (end state after stack)

New folders under `packages/sdk/src/` — **additive**, existing paths stay until later cleanup PRs.

```text
packages/sdk/src/
  sources/
    types.ts                 # AgentSource interface
    claude-code/
      index.ts               # createClaudeCodeSource()
      paths.ts               # default roots, state dirs
      plugins.ts             # plugin id metadata (optional mirror of CLI)
  planes/
    static-ingest.ts         # façade over LifecycleOwner init path
    live-disk-ingest.ts      # façade over createLiveUpdates / LiveUpdates
    runtime-bridge.ts        # façade over hooks + channel discovery
  store/
    durable-store.ts         # façade type over QueryService + AgentDataStore
  events/
    disk-change.ts           # re-export live/change-events (Plane 2)
    runtime-event.ts         # stub RuntimeEvent union (Plane 3, types only)
  create.ts                  # wires the above
  ...existing modules unchanged...
```

**Important:** Do **not** move `parser/`, `data/`, `live/`, `io/` in this stack. Façades call into them. Physical relocation is optional follow-up after the shape is stable.

---

## 3. Name map (diagram → today → after)

| Diagram | Today | After (this stack) |
|---|---|---|
| AgentSource | Implicit `rootDir` + hardcoded paths | `sources/claude-code` + `AgentSource` |
| StaticIngest | `LifecycleOwner` cold/warm/native | `planes/static-ingest.ts` wraps lifecycle |
| LiveDiskIngest | `live/live-updates.ts` | `planes/live-disk-ingest.ts` wraps it |
| RuntimeBridge | `io/hook-event-watcher`, `io/channel-*`, CLI plugins | `planes/runtime-bridge.ts` aggregates |
| DurableStore | `QueryService` + `AgentDataStore` | `store/durable-store.ts` type/façade |
| EventBus (disk) | `Change` + store subscribers + `api.live` | re-export as disk event bus; no rewrite |
| EventBus (runtime) | none unified | **types-only** stub in `events/runtime-event.ts` |
| SpaghettiAPI | `api.ts` / `app-service.ts` | unchanged public methods |

---

## 4. Public API policy

### Must not break

- `createSpaghettiService(options?)`  
- `SpaghettiServiceOptions`: `rootDir` (alias `claudeDir`), `dbPath`, `engine`, `live`, `dataService`, `errorSink`  
- All `SpaghettiAPI` methods currently documented  

### May add (optional, backward compatible)

```ts
// SpaghettiServiceOptions (additive)
source?: AgentSource;           // default: createClaudeCodeSource({ rootDir })
// rootDir preferred; claudeDir remains as deprecated alias

// SpaghettiAPI (additive, later PR — not required in PR1–4)
// readonly runtime?: SpaghettiRuntime;  // defer until RuntimeBridge is real
```

### Recommended resolution rule

```text
options.source
  ?? createClaudeCodeSource({ rootDir: options.rootDir ?? options.claudeDir ?? defaultClaudeDir() })
```

`dbPath` stays independent (index location ≠ agent source root).

---

## 5. PR DAG

```text
PR1  AgentSource (claude-code)
  │
  ├─► PR2  DurableStore façade (read/write ports, no behavior change)
  │
  ├─► PR3  StaticIngest façade  ──┐
  ├─► PR4  LiveDiskIngest façade ─┼─► PR5  create.ts composition
  └─► PR6  RuntimeBridge façade ──┘         (can land after PR3+PR4;
                                              PR6 can parallel PR3–5)
  │
  └─► PR7  Docs + barrel exports + architecture cross-links
```

PR3 / PR4 / PR6 are independent after PR1.  
PR5 depends on PR1 + PR2 + PR3 + PR4 (PR6 optional for PR5).  
PR7 last.

Suggested Graphite / stack order if sequential: **1 → 2 → 3 → 4 → 5 → 6 → 7**.

---

## 6. PR details

### PR1 — `AgentSource` + Claude Code adapter

**Intent:** One place owns roots and path conventions for Claude Code.

**Add**

| File | Responsibility |
|---|---|
| `packages/sdk/src/sources/types.ts` | `AgentSource` interface |
| `packages/sdk/src/sources/claude-code/paths.ts` | `defaultClaudeDir()`, `defaultSpaghettiStateDir()`, hook events path, channel sessions dir |
| `packages/sdk/src/sources/claude-code/index.ts` | `createClaudeCodeSource(opts?)` |
| `packages/sdk/src/sources/index.ts` | barrel |

**Interface sketch**

```ts
/** Stable id for a supported agent product. */
export type AgentSourceId = 'claude-code'; // extend later

export interface AgentSource {
  readonly id: AgentSourceId;
  /** Agent product data root, e.g. ~/.claude */
  readonly rootDir: string;
  /** Spaghetti-owned state for this machine, e.g. ~/.spaghetti */
  readonly stateDir: string;
  /** Paths derived from root/state (hooks JSONL, channel discovery, …) */
  readonly paths: {
    projectsDir: string;
    todosDir: string;
    plansDir: string;
    tasksDir: string;
    fileHistoryDir: string;
    settingsFile: string;
    hookEventsFile: string;
    channelSessionsDir: string;
  };
}

export function createClaudeCodeSource(options?: {
  rootDir?: string;
  stateDir?: string;
}): AgentSource;
```

**Change lightly**

- `create.ts`: resolve `source` once; pass `source.rootDir` where `resolvedRootDir` is used today  
- Prefer **not** to thread `AgentSource` through all of `LifecycleOwner` in PR1 — only factory + path helpers  

**Optional follow-in-PR1**

- Replace hardcoded `join(homedir(), '.spaghetti', 'hooks', ...)` in `hook-event-watcher.ts` with `source.paths.hookEventsFile` **only if** the watcher is constructed from create (today CLI often constructs it). If CLI constructs alone, export path helpers and use them from CLI in PR6.

**Tests**

- Unit: `createClaudeCodeSource()` defaults; overrides for `rootDir` / `stateDir`  
- Existing integration tests still pass with `rootDir` (or deprecated `claudeDir`) option  

**Out of scope**

- Moving parsers under `sources/claude-code/parser`  
- Multi-agent  

---

### PR2 — `DurableStore` façade

**Intent:** Name the shared SQLite read/write surface the two disk planes share.

**Add**

| File | Responsibility |
|---|---|
| `packages/sdk/src/store/durable-store.ts` | Types + thin factory |

**Sketch**

```ts
export interface DurableStore {
  readonly query: QueryService;
  readonly ingest: IngestService;   // write sink / ProjectParseSink
  readonly data: AgentDataStore;    // reads + config/analytics cache + emit
  readonly sqlite: SqliteService;   // shared connection owner
}

export function createDurableStore(deps: {
  sqlite: SqliteService;
  errorSink?: ErrorSink;
  engine?: IngestEngine;
  native?: NativeAddon | null;
}): DurableStore;
```

**Change**

- `create.ts` builds store via `createDurableStore` instead of ad-hoc `createQueryService` + `createIngestService` + `createAgentDataStore`  
- Behavior identical (same shared sqlite factory rule)

**Tests**

- Existing package tests; no new integration required if wiring is mechanical  

**Out of scope**

- Schema changes, FTS changes, moving `data/schema.ts`  

---

### PR3 — `StaticIngest` façade

**Intent:** Name cold/warm/full rebuild without extracting logic out of `LifecycleOwner` yet.

**Add**

| File | Responsibility |
|---|---|
| `packages/sdk/src/planes/static-ingest.ts` | Types describing static ingest deps + thin re-export / comment surface |

**Minimal viable shape**

```ts
/**
 * StaticIngest — Plane 1.
 * Today: cold/warm/native paths live on LifecycleOwner.
 * This module documents the boundary and exposes helpers used by create.ts.
 */
export interface StaticIngestDeps {
  source: AgentSource;
  store: DurableStore;
  fileService: FileService;
  parser: ClaudeCodeParser;
  engine?: IngestEngine;
  dbPath?: string;
}

/** Options slice that LifecycleOwner already understands. */
export function toLifecycleOptions(
  deps: StaticIngestDeps,
): AgentDataServiceOptions;
```

**Change**

- `create.ts` uses `toLifecycleOptions` when constructing `AgentDataServiceImpl`  
- JSDoc on `LifecycleOwner` pointing to Plane 1 / this module  

**Deeper extraction (optional same PR or follow-up)**

Only if small: move `getDefaultDbPath` resolution next to source/store so create.ts doesn’t duplicate `defaultDbPathForEngine` vs live’s `cache.db` bug.

**Known bug to fix while touching create.ts (recommended in PR3 or PR5)**

```ts
// create.ts today (live path):
const resolvedDbPath = options?.dbPath ?? path.join(os.homedir(), '.spaghetti', 'cache.db');
// LifecycleOwner uses defaultDbPathForEngine(engine) → spaghetti-rs.db / spaghetti-ts.db
```

These can disagree when `live: true` without explicit `dbPath`. **Align live default to `defaultDbPathForEngine(resolvedEngine)`** in PR3 or PR5.

**Out of scope**

- Splitting `LifecycleOwner` into multiple classes  

---

### PR4 — `LiveDiskIngest` façade

**Intent:** Name Plane 2 at the factory boundary.

**Add**

| File | Responsibility |
|---|---|
| `packages/sdk/src/planes/live-disk-ingest.ts` | `createLiveDiskIngest(...)` → existing `createLiveUpdates` |

**Sketch**

```ts
export interface LiveDiskIngestOptions {
  source: AgentSource;
  store: DurableStore;
  fileService: FileService;
  errorSink?: ErrorSink;
  /** Absolute DB path (must match StaticIngest / LifecycleOwner). */
  dbPath: string;
  batchWindowMs?: number;
  // …pass-through LiveUpdatesOptions as needed
}

export type LiveDiskIngest = LiveUpdates; // structural alias for now

export function createLiveDiskIngest(
  options: LiveDiskIngestOptions,
): LiveDiskIngest {
  return createLiveUpdates(
    {
      fileService: options.fileService,
      ingestService: options.store.ingest,
      store: options.store.data,
      sqlite: options.store.sqlite,
      dbPath: options.dbPath,
    },
    {
      rootDir: options.source.rootDir,
      errorSink: options.errorSink,
      // ...
    },
  );
}
```

**Change**

- `create.ts`: `options.live ? createLiveDiskIngest(...) : undefined`  
- JSDoc: `api.live` = Plane 2 event surface  

**Tests**

- Existing `live/__tests__/*` unchanged  
- Optional: one test that façade forwards `rootDir`  

**Out of scope**

- Enabling `live: true` by default in CLI/TUI (product decision; separate PR)  
- Watching teams / more categories  

---

### PR5 — `create.ts` composition (diagram-shaped factory)

**Intent:** Factory reads top-to-bottom like the architecture diagram.

**Target structure (illustrative)**

```ts
export function createSpaghettiService(options?: SpaghettiServiceOptions): SpaghettiAPI {
  const errorSink = options?.errorSink ?? createConsoleErrorSink('[spaghetti-sdk]');
  if (options?.dataService) {
    return createSpaghettiAppService(options.dataService, errorSink);
  }

  const source =
    options?.source ??
    createClaudeCodeSource({ rootDir: options?.rootDir ?? options?.claudeDir });

  const fileService = createFileService();
  const sharedSqlite = createSqliteService();
  const engine = options?.engine ?? resolveEngine();
  const native = engine === 'rs' ? loadNativeAddon() : null;

  const store = createDurableStore({
    sqlite: sharedSqlite,
    errorSink,
    engine,
    native,
  });

  const dbPath = options?.dbPath ?? defaultDbPathForEngine(engine);

  const liveDisk = options?.live
    ? createLiveDiskIngest({
        source,
        store,
        fileService,
        errorSink,
        dbPath,
      })
    : undefined;

  const parser = createClaudeCodeParser(fileService);
  const dataService = new AgentDataServiceImpl(
    fileService,
    parser,
    store.query,
    store.ingest,
    store.data,
    toLifecycleOptions({ source, store, fileService, parser, engine, dbPath }),
    liveDisk,
  );

  // RuntimeBridge: wire in PR6 when ready (optional field on app service later)
  return createSpaghettiAppService(dataService, errorSink);
}
```

**Change**

- Fix dbPath alignment for live (see PR3 note)  
- Export new types from package barrel only if ready for public use (see PR7)  

**Tests**

- Full SDK test suite  
- `pnpm test:ingest-diff` (or note CI already covers)  
- Manual: `createSpaghettiService({ live: true, engine: 'rs' })` uses same db file as without live  

---

### PR6 — `RuntimeBridge` façade (hooks + channels)

**Intent:** Give Plane 3 a named home without merging it into SQLite.

**Add**

| File | Responsibility |
|---|---|
| `packages/sdk/src/planes/runtime-bridge.ts` | Factory for hook path + channel registry helpers |
| `packages/sdk/src/events/runtime-event.ts` | **Stub** `RuntimeEvent` discriminated union (types only) |

**Sketch**

```ts
export interface RuntimeBridge {
  readonly source: AgentSource;
  /** Default hook events JSONL path for this source */
  hookEventsPath(): string;
  /** Channel session discovery directory */
  channelSessionsDir(): string;
  // Future: start()/stop(), onEvent(), listActiveSessions()
}

export function createRuntimeBridge(source: AgentSource): RuntimeBridge;
```

**RuntimeEvent stub (document as unstable / future)**

```ts
export type RuntimeEvent =
  | { type: 'hook'; name: string; payload: unknown; ts: number }
  | { type: 'channel.message'; sessionId: string; payload: unknown; ts: number }
  | { type: 'session.active'; sessionId: string; pid?: number; ts: number };
// expand when implementing api.runtime
```

**Change**

- Point `hook-event-watcher` default path helper at `createClaudeCodeSource().paths.hookEventsFile`  
- CLI `plugins.ts` / hooks command: optionally import path from SDK source (reduces drift)  
- **Do not** add `api.runtime` to public `SpaghettiAPI` until a follow-up that implements subscribe  

**Tests**

- Path helper unit tests  
- Existing hooks/channel tests if any  

**Out of scope**

- Persisting hooks into main agent DB  
- Unifying TUI hooks monitor onto `api.live`  

---

### PR7 — Docs, barrels, exports

**Intent:** Make the shape discoverable.

**Change**

- `packages/sdk/src/index.ts` — export:

  ```ts
  export type { AgentSource, AgentSourceId } from './sources/types.js';
  export { createClaudeCodeSource } from './sources/claude-code/index.js';
  // Optional: export façade types; avoid exporting LiveUpdates impl details
  ```

- `packages/sdk/README.md` — short “Architecture” section with three planes + link to docs  
- `docs/THREE-PLANE-INGEST-ARCHITECTURE.md` — “Implementation map” linking to new files  
- `docs/PR-PLAN-THREE-PLANE-SHAPE.md` — mark PRs done as they merge  
- Site docs (optional later): one diagram subsection  

**Tests**

- Typecheck / build package  

---

## 7. Explicit non-goals for this stack

| Non-goal | Why |
|---|---|
| Multi-agent sources | Only Claude Code exists |
| `api.runtime` on SpaghettiAPI | Needs real subscribe semantics |
| Moving `parser/` under `sources/claude-code/` | Large churn; do after façades stick |
| Enabling `live: true` by default in CLI | Product PR, not shape PR |
| Schema / FTS changes | Orthogonal |
| Renaming `LifecycleOwner` publicly | Shim already exists; rename later if ever |
| Merging hooks JSONL into `messages` | Different plane |

---

## 8. Risk register

| Risk | Mitigation |
|---|---|
| `dbPath` live vs static mismatch | Fix in PR3/PR5; add assertion test |
| Exporting too much internal surface | Export types + source factory only; keep LiveUpdates private |
| CLI still hardcodes `~/.spaghetti` | PR6 path helpers; leave CLI working if import fails |
| Reviewer fatigue | Keep PRs under ~300 LOC where possible; PR1 and PR5 are the substantive ones |
| Accidental behavior change in create order | Diff create.ts carefully; same construction order of sqlite → ingest → store → live → lifecycle |

---

## 9. Test plan (every PR)

```bash
pnpm --filter @vibecook/spaghetti-sdk typecheck
pnpm --filter @vibecook/spaghetti-sdk test
# at least once on PR5 and before merge of stack tip:
pnpm test:ingest-diff
```

Manual smoke after PR5:

```ts
const a = createSpaghettiService();
const b = createSpaghettiService({ live: true });
// both should use spaghetti-{rs,ts}.db under ~/.spaghetti/cache unless dbPath set
await a.initialize();
await b.initialize();
```

---

## 10. Suggested first implementation session

1. Land **PR1** (AgentSource + paths + create.ts uses `source.rootDir`)  
2. Land **PR2** (DurableStore factory)  
3. Land **PR4** (LiveDiskIngest wrapper — small)  
4. Land **PR3** + **PR5** together if tiny, else separate  
5. **PR6** when ready for Plane 3 naming  
6. **PR7** docs/exports  

---

## 11. Follow-up stacks

| Stack | Work | Status |
|---|---|---|
| Productize Plane 2 | TUI/playground default `live: true`; doctor shows live status | **Done** (TUI + playground + doctor Index & live) |
| Runtime API | `SpaghettiRuntime`, hook/channel subscribe | **Done** — `api.runtime` + subscribe shipped; SQLite persistence **rejected 2026-07-12** (the index stays a pure function of files on disk; runtime streams are ephemeral by design) |
| Active sessions | Parse `~/.claude/sessions/{pid}.json` into RuntimeBridge | **Done** — `listActiveSessions` / `active-sessions.ts` |
| Source-local parsers | Move Claude path classification next to source; keep engines generic | Pending |
| Second AgentSource | Only after above | Pending |

---

## 12. One-sentence charter for implementers

> **Wrap, don’t rewrite:** introduce `AgentSource` and named plane façades so `create.ts` matches the architecture diagram, keep every public query API and the native/TS ingest engines working exactly as today, and leave runtime event bus + multi-agent for later stacks.
