> **Historical pre-RFC 011 topology — superseded.** This document describes
> the transitional TypeScript ingest planes, not the production owner after
> the Rust observation/query cutover. See [RFC 011](./rfcs/011-rust-observation-query-engine.md)
> and the [Phase 10 closure ledger](./rfcs/011-phase-10-closure.md).

# Two-Plane Ingest Architecture

**Status:** Historical transitional map; superseded by RFC 011
**Created:** 2026-07-10 (as the three-plane map)  
**Revised:** 2026-08-08 — Plane 3 retired, see `docs/rfcs/007-retire-runtime-bridge.md`  
**Scope:** How Spaghetti ingests local agent data today (Claude Code first), and how the two ingest planes fit the long-term goal: a tool and a set of well-designed APIs for indexing, querying, and reacting to agent history and live state.

**Related:**

- `docs/PR-PLAN-THREE-PLANE-SHAPE.md` — **implementation PR stack** (AgentSource + plane façades)
- `docs/PARSER-PIPELINE.md` — what static disk data is parsed
- `docs/PARSER-UNPARSED-DATA.md` — coverage gaps on disk
- `docs/rfcs/005-live-updates.md` / `docs/LIVE-UPDATES-DESIGN.md` — Plane 2 design
- `packages/sdk/src/api.ts` — public query + live surface

---

## 1. Ultimate goal

Provide a **local-first agent-data platform**:

1. **Ingest** local agent data (currently Claude Code’s `~/.claude` and related Spaghetti state).
2. **Organize** it into a searchable, durable dataset (SQLite + FTS5).
3. **Expose well-designed APIs** so users and apps can query history, follow updates, and observe live agent activity.

Claude Code is the first **agent source**, not the permanent identity of the system. Multi-agent support is a later adapter problem once the two planes are coherent.

---

## 2. The two planes

| Plane | Name | Question it answers | Time scale | Source of truth |
|---|---|---|---|---|
| **1** | Static disk | What has this agent ever done on this machine? | Historical, bulk | Files already on disk (e.g. `~/.claude`) |
| **2** | Live disk Δ | What just changed in those files? | Seconds / sub-second | Same files, as they grow or rewrite |

Both planes share one data model: paths under the agent home → normalized rows
in SQLite. The index is a pure function of files on disk.

A third plane once existed — process-adjacent runtime state from hook and
channel plugins. It was retired in RFC 007. It never wrote to the index, it
duplicated transcript content at lower latency, and it carried two Claude Code
plugin packages plus a WebSocket dependency for surfaces that went unused.
Spaghetti reads bytes agents leave on disk; it does not observe agent processes.

```text
+---------------------------------------------------------------------+
|                        Consumer surfaces                            |
|         CLI / TUI - SDK - React - (future apps / MCP)               |
+--------------------------------^------------------------------------+
                                 |  SpaghettiAPI  (+ api.live)
+--------------------------------+------------------------------------+
|                     Local index + event bus                         |
|              SQLite (searchable, durable)  -  typed Change events   |
+--------^-------------------^----------------------------------------+
         |                   |
    +----+----+         +----+----+
    | Plane 1 |         | Plane 2 |
    | Static  |         |  Live   |
    |  disk   |         |  disk d |
    | ~/.xxx  |         | watchers|
    +---------+         +---------+
      cold/warm           incremental
      full reparse        file deltas
```

---

## 3. Target architecture (north star)

```text
                    +--------------------------+
                    |   AgentSource adapter    |  (claude-code, codex, grok)
                    |     roots, formats       |
                    +------------+-------------+
           +---------------------+---------------------+
           v                                           v
    StaticIngest                                 LiveDiskIngest
    cold/warm/full                               watch+delta
           |                                           |
           +---------------------+---------------------+
                                 v
                    DurableStore  +  EventBus
                    (SQLite+FTS)     (typed Change)
                                 |
                                 v
                           SpaghettiAPI
```

### Design principles (already aligned with shipped work)

1. **One durable store for disk-derived truth.**
2. **Events after commit**, not instead of commit (plane 2) — no reliance on SQLite update hooks.
3. **The index is a pure function of files on disk.** Anything that matters historically is written to a file by its emitter and ingested by Planes 1–2. Spaghetti never holds a handle to an agent process.
4. **Source adapters** so “agent data folder” is not hardcoded forever.

### Non-goals (keep these)

- Do not make filesystem watching simulate process lifecycle.
- Do not re-introduce a process-adjacent runtime plane (RFC 007).
- Do not build a multi-process CRDT / sync layer for v1.

---

## 4. Plane 1 — Static local agent data

### Intent

Parse the agent home directory into a searchable, well-organized dataset.

Examples: `~/.claude/projects/**/*.jsonl`, memory, todos, plans, subagents, workflows, config, analytics.

### Current status: **strong / product-ready core**

This is Spaghetti’s mature center of gravity.

| Piece | Status |
|---|---|
| Streaming JSONL parse | Done (TS + Rust) |
| Dedicated SQLite schema + FTS5 | Done (schema v4) |
| Cold start / warm start + fingerprints | Done |
| Dual engine (`rs` default, `ts` fallback + parity harness) | Done |
| Public query API (`getProjectList`, `search`, messages, subagents, workflows, …) | Done |
| CLI + TUI consumers | Done |
| Coverage | Strong on sessions / messages / subagents / workflows / todos / memory; config & analytics **TS-only**; residual gaps in `PARSER-UNPARSED-DATA.md` |

**Key modules**

- `packages/sdk/src/create.ts` — service wiring
- `packages/sdk/src/data/lifecycle-owner.ts` — cold/warm/native init
- `packages/sdk/src/parser/*` — project / config / analytics
- `packages/sdk/src/data/query-service.ts` + `ingest-service.ts`
- `crates/spaghetti-napi/` — native bulk ingest

**Strengths**

- Stream → single writer → durable FTS
- Performance path (native) without abandoning TS as ground truth
- Stable `SpaghettiAPI` for consumers of the index

**Gaps**

- Not yet a multi-agent abstraction (Claude Code–shaped types and paths)
- Incomplete disk coverage (teams live-watch, backups, active-session PID files, some config corners)
- Rust path intentionally scoped to project/session bulk ingest, not full config/analytics
- APIs are strong for **read/query**; less formal for **ingest control** (cancel, multi-root, pluggable sources)

**API maturity:** high for consumers of the index; medium for operators of ingest.

---

## 5. Plane 2 — Live increments of static data

### Intent

Watch the agent folder; notify callers of updates; write deltas into SQLite promptly so search and UI stay warm.

### Current status: **implemented as infrastructure, under-adopted as product default**

RFC 005 is largely built in the SDK:

```text
@parcel/watcher → classify → coalesce → incremental parse
    → writeBatch (TS or native liveIngestBatch) → store.emit(Change)
    → api.live.onChange / events() / React live hooks
```

| Piece | Status |
|---|---|
| Watcher + coalescing queue + checkpoints | Done |
| Incremental JSONL (byte-offset resume) | Done |
| Scopes: projects, todos, tasks, file-history, plans, settings | Wired |
| Typed `Change` union + `api.live` | Done |
| React `useLive*` hooks | Done |
| Opt-in `createSpaghettiService({ live: true })` | Done |
| Default for CLI one-shots / bare TUI | **Off** (by design today) |

**Key modules**

- `packages/sdk/src/live/*` — watcher, queue, parser, router, `spaghetti-live.ts`
- `packages/sdk/src/create.ts` — constructs `LiveUpdates` only when `live: true`
- `crates/spaghetti-napi/src/orchestrate/live_ingest.rs` — native batch writer for live path

**Strengths**

- Post-COMMIT application-layer events (portable across TS/Rust writers)
- Shared writer semantics with cold ingest
- Explicit non-goals respected (not a CRDT, not multi-process sync)

**Gaps**

- Still opt-in — most `spag` invocations never enable live path
- Watched set ≠ full `~/.claude` (e.g. teams; noisy analytics intentionally skipped)
- No first-class “always-on daemon” product mode yet
- Delivery is fire-and-forget for UI (`seq` is in-memory); restart reconciliation is warm-start
- Saturation / lag signals exist more in design than in polished product UX

**API maturity:** high when `live: true`; low as a default end-user experience.

---

## 6. Scorecard (honest snapshot)

| Plane | Capability | Productization | API cleanliness | Multi-agent readiness |
|---|---|---|---|---|
| **1 Static** | ★★★★★ | ★★★★☆ | ★★★★☆ | ★★☆☆☆ |
| **2 Live disk** | ★★★★☆ | ★★☆☆☆ | ★★★★☆ | ★★☆☆☆ |

**One-line summary**

> Spaghetti has a **production-grade static index** and a **real live-disk pipeline that is not yet the default product path**. Scope is now deliberately two planes: everything the tool knows, it learned from a file.

```text
  [████████████████████░░░░]  Plane 1  — core product; harden coverage
  [████████████░░░░░░░░░░░░]  Plane 2  — built; needs defaultization + UX
  [██░░░░░░░░░░░░░░░░░░░░░░]  Multi-agent / pluggable sources
```

---

## 7. Strategic priorities

### Near term — sharpen the platform story

1. **Keep this two-plane model** in product docs (this file, SDK README, public docs site when ready).
2. **Promote Plane 2** for long-lived consumers only:
   - TUI, Electron playground, any “monitor” mode → `live: true` by default
   - One-shot CLI commands stay cold/warm only (no watcher overhead)
3. **Formalize API layers** in naming and docs:
   - **Ingest:** `initialize`, `rebuildIndex`, engine selection, progress
   - **Query:** lists, messages, search, stats, artifacts
   - **Live disk:** `api.live`

### Medium term — deepen disk coverage

4. **Close Plane 1 gaps** recorded in `PARSER-UNPARSED-DATA.md`, prioritising what
   search and browse actually surface.
5. **Correlation within the index:** a timeline that stitches JSONL appends to
   the artifacts (todos, plans, tasks) written alongside them.
6. **Active sessions** from `~/.claude/sessions/{pid}.json` — read directly by
   `listActiveSessionsFromDir`, no bridge required.

### Longer term — multi-agent

7. **`AgentSource` interface:** roots, file categories, runtime plugin IDs
8. Additional agent sources land as adapters once both planes feel coherent on one API surface

---

## 8. Suggested public API shape (sketch, not implemented)

```ts
// Conceptual — direction only
interface SpaghettiAPI {
  // Plane 1 (and shared lifecycle)
  initialize(): Promise<void>;
  rebuildIndex(): Promise<{ durationMs: number }>;
  // … query methods …

  // Plane 2
  readonly live?: SpaghettiLive; // present when { live: true }
}
```

Plane 2 events imply **store mutation** (or reconciliation with the store).
There is no second event channel: if something is worth observing, it is worth
writing to a file first.

## 9. What “done” looks like for the platform story

| Plane | Done means |
|---|---|
| **1** | Cold/warm ingest of Claude Code disk is complete enough that search + browse cover the workflows users care about; gaps are documented and low severity |
| **2** | Long-lived apps get live disk updates by default; search stays current within ~100ms of append; API is boring and reliable |
| **Platform** | A new consumer can build a tool using only published APIs, without knowing internal parser modules |

---

## 10. Implementation map (code)

| Diagram box | Module(s) |
|---|---|
| AgentSource | `packages/sdk/src/sources/` (`createClaudeCodeSource`) |
| StaticIngest | `packages/sdk/src/planes/static-ingest.ts` → `LifecycleOwner` |
| LiveDiskIngest | `packages/sdk/src/planes/live-disk-ingest.ts` → `live/live-updates.ts` |
| DurableStore | `packages/sdk/src/store/durable-store.ts` |
| EventBus | `live/change-events.ts` + `api.live` |
| Factory | `packages/sdk/src/create.ts` |

**Product defaults (2026-08):** CLI TUI and Electron playground construct the service with `{ live: true }`. One-shot CLI commands remain pull-only. Doctor reports engine, index DB, live defaults, and Claude Code active-session counts via `listActiveSessionsFromDir`.

PR stack: `docs/PR-PLAN-THREE-PLANE-SHAPE.md`.

---

## 11. Bottom line

The hard architectural bets for planes **1** and **2** are already correct: streaming parse, single SQLite writer, dual engines, live updates as strictly additive.

The main strategic work is no longer “more parsers alone.” It is:

1. **Productizing live disk** for always-on surfaces  
2. **Closing the remaining disk-coverage gaps** so search matches what users expect  
3. **Keeping Claude Code as Adapter #1**, not the permanent name of the system  

Control-plane concerns — spawning agents, injecting hooks, driving sessions — are explicitly out of scope: they belong to the sibling runtime project (chopsticks). Spaghetti stays read-only: it reads bytes agents leave on disk and never holds a handle to an agent process. RFC 007 removed the one surface that blurred that line.

This document is the shared map for that work.
