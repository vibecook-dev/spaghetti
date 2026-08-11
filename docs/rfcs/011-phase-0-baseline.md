# RFC 011 Phase 0: ownership baseline and architecture freeze

Status: complete as a migration baseline on 2026-08-11

Baseline commit: `7d24381c6bf971eadde549377dd8831faa7898b1`

RFC 011 is the canonical destination for the observation and query engine. It
supersedes RFC 009's live-writer destination. RFC 010's `node:sqlite` move is
still useful compatibility work, but it is transitional: production database
ownership ultimately belongs to the persistent Rust engine.

## Ownership map

| Responsibility | Phase 0 owner | RFC 011 destination |
| --- | --- | --- |
| Source discovery and decoding | TypeScript and one-shot Rust readers | Rust adapter contract plus common stream drivers |
| Live filesystem observation | Shared TS watcher primitives plus source-local TS runtimes | Persistent Rust scheduler and drivers |
| Checkpoints and decoder state | TS memory/JSON state, source-specific | Transactional Rust stream cursors and decoder state |
| Schema and migration | Duplicated TS and Rust schema version 16 | Rust writer only |
| Canonical writes | Sync TS connection and short-lived native calls | One long-lived Rust writer connection |
| Projection maintenance | TS ingest plus query-triggered repair | Rust commit transaction before publication |
| Canonical queries | Synchronous TS SQL | Typed async Rust query workers |
| Workspace and usage aggregation | TS `AppService`, `QueryService`, and token projections | Versioned Rust read models |
| Change publication | Process-local TS sequence/event bus | Durable transactional outbox |
| Lifecycle and shutdown | TS service/source owners | One `SpaghettiEngine` owner |

The machine-readable inventory lives in
`scripts/architecture/rfc011-legacy-boundaries.json`. Its check discovers the
current production surface and rejects new exceptions while allowing existing
ones to be removed.

## Frozen legacy inventory

The baseline contains:

- 14 production TypeScript modules coupled to `SqliteService` or a SQLite driver;
- 2 production TypeScript SQLite driver importers;
- 1 query module that can repair/rebuild projections while serving a read;
- 14 source-local lifecycle/live/watcher/checkpoint modules;
- 4 common Rust modules containing source-id dispatch outside adapters.

The detailed paths are deliberately kept in the ratchet manifest rather than
duplicated here. CI runs the check through `pnpm validate` on every supported
platform.

Database migrations and table definitions currently live in both
`packages/sdk/src/data/schema.ts` and
`crates/spaghetti-napi/src/core/schema.rs`. Query-triggered repair is confined
to `packages/sdk/src/data/query-service.ts`, calling the timeline and subagent
projection modules. Aggregation is primarily split between
`packages/sdk/src/app-service.ts`, `query-service.ts`, and
`token-activity.ts`.

Every database-backed public read on `SpaghettiAPI` is synchronous today:
source ids, projects, project activity, sessions, messages, timeline/facets,
memory, todos/plans/tasks/tool results, subagents/workflows, search, stats, and
teams. `initialize`, `rebuildIndex`, and `dispose` are the existing async
lifecycle exceptions. This is the compatibility surface that the future
`SpaghettiClient` must replace deliberately rather than accidentally.

## Fixture corpus and differential oracle

The committed, synthetic fixture corpus contains:

| Fixture | Purpose | Oracle command |
| --- | --- | --- |
| `fixtures/small` | Claude hot path and artifacts | `pnpm test:ingest-diff` |
| `fixtures/medium` | Claude rare record/content variants | `pnpm test:ingest-diff:medium` |
| `fixtures/small-codex` | Codex lifecycle and token-attribution shapes | `pnpm test:ingest-diff:codex` |
| `fixtures/small-grok` | Grok transcript and sidecar shapes | `pnpm test:ingest-diff:grok` |

`scripts/ingest-diff.ts` is the current Rust-versus-TypeScript canonical-table
oracle. SDK query tests are the TypeScript output oracle until typed Rust query
parity is introduced. Live output semantics are covered by the shared live
router/watcher tests and source-specific live suites.

## Reproducible baseline commands

Run these from the repository root after `pnpm build`:

```bash
# Cold and warm ingest timing on the committed corpus
pnpm bench:ingest --runs 10 --mode cold
pnpm bench:ingest --runs 10 --mode warm --scenario unchanged
pnpm bench:ingest --runs 10 --mode warm --scenario growth
pnpm bench:ingest --runs 10 --mode warm --scenario deletion
pnpm bench:ingest --runs 10 --mode warm --scenario repair

# Correctness outputs (cold/warm writer parity and query/live behavior)
pnpm test:ingest-diff
pnpm test:ingest-diff:medium
pnpm test:ingest-diff:codex
pnpm test:ingest-diff:grok
pnpm test:packages

# Architecture freeze
pnpm validate
```

### Committed-corpus timing snapshot

Observed 2026-08-11 on an Apple M1 Max (10 logical CPUs), arm64 macOS
25.5.0, Node 26.5.0, native addon 0.7.0. Each figure is the median of five
measured runs after one warmup against `fixtures/small`; it is evidence for
this baseline, not a cross-machine gate.

| Scenario | Rust median | TypeScript median | Rust speedup |
| --- | ---: | ---: | ---: |
| Cold ingest | 21.5 ms | 53.6 ms | 2.49x |
| Warm, unchanged | 1.87 ms | 18.4 ms | 9.82x |

The exact commands were:

```bash
pnpm bench:ingest --runs 5 --warmup 1 --mode cold
pnpm bench:ingest --runs 5 --warmup 1 --mode warm --scenario unchanged
```

For an accepted private real-data corpus, pass its root with
`--fixture /absolute/path` and persist machine metadata plus JSON output using
`--report-json`. Private transcript data is never committed. The committed
`.github/bench-baselines.json` remains the hardware-independent regression
policy; fixture and real-corpus reports are evidence artifacts, not universal
latency promises.

## Phase 1 handoff gates

The first engine slice must keep every legacy path operational while proving:

1. exclusive database ownership with diagnosable owner metadata;
2. one persistent writer connection and a bounded read-only query pool;
3. a typed async health/overview query that cannot mutate the database;
4. cancellation-aware shutdown and deterministic disposal;
5. lifecycle tests with no worker or lock leak.
