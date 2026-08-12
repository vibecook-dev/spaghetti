# RFC 011: Rust Spaghetti Engine — Unified Observation, Storage, Query, and Agent Adapters

- **Status:** Draft
- **Created:** 2026-08-10
- **Revised:** 2026-08-11
- **Authors:** James Yong, contributors
- **Target:** `spaghetti`
- **Type:** Architecture / migration / adapter contract / public API
- **Scope:** Rust ingest, live source observation, sole SQLite ownership, runtime-state projection, search and query, token usage, source adapters, durable subscriptions
- **Numbering note:** `packages/sdk/src/io/sqlite-service.ts` already identifies the `better-sqlite3` → `node:sqlite` migration as RFC 010. This architecture therefore uses RFC 011.
- **Phase records:** [Phase 0 baseline](./011-phase-0-baseline.md) · [Phase 1 engine shell](./011-phase-1-engine-shell.md) · [Phase 2 transactional catalog](./011-phase-2-transactional-catalog.md) · [Phase 4 Claude history/usage](./011-phase-4-claude-history-usage.md) · [Phase 5 delegation pack](./011-phase-5-delegation-pack.md) · [Phase 5 teams/inbox pack](./011-phase-5-teams-inbox-pack.md) · [Phase 5 presence pack](./011-phase-5-presence-pack.md) · [Phase 5 tasks/plans pack](./011-phase-5-tasks-plans-pack.md) · [Phase 5 artifacts pack](./011-phase-5-artifacts-pack.md) · [Phase 5 workflows pack](./011-phase-5-workflows-pack.md) · [Phase 5 session-index pack](./011-phase-5-session-index-pack.md) · [Phase 5 project-memory pack](./011-phase-5-project-memory-pack.md)
- **Related documents:**
  - `docs/TWO-PLANE-INGEST-ARCHITECTURE.md`
  - `docs/rfcs/003-rust-ingest-core.md`
  - `docs/rfcs/006-normalized-message-model.md`
  - `docs/rfcs/006-appendix-agent-survey.md`
  - `docs/rfcs/007-retire-runtime-bridge.md`
  - `docs/rfcs/009-retire-typescript-bulk-ingest.md`

## 1. Summary

Spaghetti is evolving from a transcript indexer into a local observation and query engine for coding agents. Transcript history remains important, but the same agent-owned sources also expose live operational facts that agent CLIs do not consistently provide through hooks: active sessions, subagent lifecycle, agent-team membership and inbox activity, token consumption, tasks, plans, artifacts, and other runtime evidence.

This RFC establishes one long-lived Rust engine that owns the complete correctness path from native source bytes to public query answers:

> **One ingest spine, one database authority, multiple transactional projections, and one typed query surface.**

The ingest spine has two temporal modes:

1. **Backfill/reconcile** — discover current source state, rebuild missing materializations, repair drift, and recover from dropped notifications.
2. **Live tail/observe** — incrementally consume active append streams and replaceable snapshots with low latency.

Both modes use the same source adapters, decoders, fact model, projection code, cursor semantics, and SQLite writer. They differ only in scheduling and batching policy.

The database side has two execution lanes under one Rust owner:

1. **Writer lane** — one long-lived connection applies ingest commits, migrations, projection maintenance, source cursors, and the durable outbox.
2. **Query lanes** — a bounded pool of long-lived read-only connections executes typed, cancellable, domain-level queries against committed projections.

Every successfully decoded source record may update several projections in one SQLite transaction:

1. durable transcript/history and search indexes;
2. durable observed runtime state;
3. token and cost materializations;
4. source cursors, decoder state, and projection versions;
5. a durable change log used for resumable subscriptions.

TypeScript becomes a client and presentation layer. It does not open the Spaghetti database, execute SQL, run migrations, repair projections, merge canonical search rankings, or implement cross-agent aggregation. Standalone Node/Electron consumers use an asynchronous N-API transport; Vibe Field may host the same engine inside `field-native` and reach it over IPC. Both transports expose the same semantic API and must make one coarse-grained call per logical operation.

The central architectural boundary is:

> **The common Rust engine owns mechanics, persistence, canonical semantics, and queries. Agent adapters own native interpretation.**

The common engine owns discovery scheduling, watcher lifecycle, record framing, cursor and generation management, retries, reconciliation, backpressure, transactions, projections, state reduction, query execution, durable event delivery, and observability. An agent adapter owns the native source map, native record decoding, native identifiers, agent-specific joins, and a truthful capability declaration. An adapter emits typed facts and evidence; it never mutates Spaghetti tables directly, implements public SQL, or owns delivery of public events.

This RFC supersedes RFC 009's decision to retain the TypeScript live writer and filesystem watchers. It also supersedes TypeScript SQLite ownership as the final destination of the RFC 010 driver migration. It does **not** reverse RFC 007: Spaghetti remains source-derived and does not restore the retired hook/plugin runtime bridge. Optional process probes may enrich a transient assessment, but they do not become historical truth.

## 2. Decision

Spaghetti will implement a long-lived Rust `SpaghettiEngine` with the following properties:

1. **Rust is the sole Spaghetti database owner.** Production TypeScript does not open, migrate, query, or write the Spaghetti SQLite database.
2. **Rust is the sole ingest writer.** Rust owns source reading through SQLite commit for cold, warm, repair, and live paths.
3. **Cold and live are one implementation.** There will be no independent TypeScript and Rust parsers or writers for the same source.
4. **One writer lane, bounded read lanes.** One long-lived write connection owns all mutations; a small pool of long-lived read-only connections owns queries.
5. **Queries are read-only.** A public query may never create, repair, or advance a projection. Projection maintenance belongs to the writer lane.
6. **Public database APIs are typed and domain-level.** Spaghetti will not expose arbitrary SQL to TypeScript or IPC clients.
7. **Database-backed public APIs are asynchronous.** N-API and IPC calls return promises/futures, support cancellation where useful, and do not run SQLite work on the Node/Electron event-loop thread.
8. **The native boundary is coarse-grained.** One logical search, timeline, usage, or snapshot request crosses N-API/IPC once and returns a complete page or bounded stream.
9. **Filesystem notifications are hints, not truth.** Durable cursors and reconciled source snapshots determine what has been ingested.
10. **Adapters emit facts, not SQL.** Adapters may read agent-owned files or databases through bounded source access, but they may not access Spaghetti connections, tables, migrations, or public query execution.
11. **Cursors, projections, and outbox changes commit atomically.** A crash cannot advance a source cursor without its corresponding rows, nor publish a change that did not commit.
12. **Observed state is evidence-backed.** Durable runtime state records what the sources prove. Timeouts, PID checks, and inactivity produce a separate transient assessment.
13. **Capabilities are declarative and qualified.** Support is reported as native, derived, estimated, or unsupported, with truthful scope and granularity.
14. **The adapter identifier is open-ended.** Core code uses a string/newtype identifier and must not use a closed `match` over Claude, Codex, and Grok.
15. **Built-in adapters are compiled Rust crates/modules in v1.** A stable third-party dynamic-plugin ABI is explicitly deferred.
16. **TypeScript remains a compatibility and presentation facade during migration.** It ultimately becomes a thin `SpaghettiClient` over either N-API or IPC and ceases to know source formats or the database schema.
17. **The old synchronous SDK surface is transitional.** The final database-backed API is asynchronous; compatibility is handled through an explicit API-version migration rather than synchronous native database calls.

## 3. Motivation

### 3.1 The current split duplicates semantics

The existing architecture has accumulated multiple ingestion paths:

- static/history ingestion;
- TypeScript live watchers and incremental parsers;
- native batch writers;
- source-specific token hooks;
- source-specific sidecar repair logic;
- process-local event sequencing.

This creates several forms of drift:

- cold and live parsers can interpret the same record differently;
- a record may be parsed once for history and again for runtime state or usage;
- source cursor updates may be outside the transaction that writes the derived rows;
- agent-specific watchers duplicate scheduling, retry, partial-line, and rewrite handling;
- source adapters can reach into storage-specific APIs rather than returning a common semantic result;
- live change sequence numbers cannot survive process restart;
- adding another agent requires copying an end-to-end pipeline instead of implementing one adapter.

A Rust rewrite that merely replaces `@parcel/watcher` with `notify` would preserve these architectural problems. The value comes from consolidating ownership, not from changing the callback language.

### 3.2 Runtime observation is a projection of durable sources

Claude Code and other agent CLIs often omit hooks for facts that Spaghetti needs in real time. Those facts nevertheless appear in disk artifacts:

- append-only or mostly append-only transcript JSONL;
- subagent transcript streams;
- team configuration and inbox files;
- active-session or presence files;
- task, plan, and artifact snapshots;
- usage records or summary sidecars;
- source-owned SQLite or key-value stores.

These are not a separate runtime plane. They are live observations of the same durable or semi-durable source material already used by history ingestion. The correct response is to add projections to a unified disk ingest spine, not to resurrect process hooks as an alternative source of truth.

### 3.3 Future agents are not all JSONL file trees

The current adapter abstraction is primarily path- and file-oriented. That is insufficient as Spaghetti expands. Agents may expose:

- append-delimited files;
- replace-on-write JSON documents;
- directory membership as state;
- SQLite tables;
- key-value databases;
- several artifacts that must be joined into one semantic entity;
- cumulative usage snapshots rather than per-message token counts.

The common abstraction must therefore be a **source record producer plus decoder**, not merely a path classifier.

### 3.4 Current repository baseline

This RFC is grounded in the current repository shape rather than a greenfield design:

- `packages/sdk/src/sources/types.ts` defines a closed `AgentSourceId` union and a narrow adapter seam consisting primarily of paths, `classify()`, and a message extractor. It also exposes `IngestHooks` and `SessionTokenApi`, allowing source-specific token behavior to mutate stored rows after extraction.
- `packages/sdk/src/sources/capabilities.ts` encodes product capabilities through source-ID switches rather than adapter manifests with quality and granularity.
- `packages/sdk/src/sources/registry.ts` centralizes lifecycle construction, but Codex and Grok intentionally retain TypeScript live writers and source-specific `LiveWatch` implementations.
- `packages/sdk/src/sources/codex/reader.ts` and `live-watch.ts` contain Codex-specific rollout discovery, metadata peeking, in-memory line/offset tracking, token-event handling, and live write scheduling.
- `packages/sdk/src/sources/grok/reader.ts`, `live-watch.ts`, and `sidecars.ts` join `chat_history.jsonl` with replaceable summary/events/signals sidecars, with sidecar reapplication performed as a distinct mutation path.
- Claude's live path carries a broader file taxonomy but does not yet make teams and all runtime-relevant sources one first-class, unified observation contract.
- RFC 006's cross-agent survey already shows that source production cannot be reduced to one JSONL path shape: lineage, token granularity, canonical-versus-UI streams, and even the storage substrate differ by agent.
- RFC 009 deliberately retains TypeScript live ingestion and deletes the unused native live-batch route. This RFC changes that end-state decision while preserving its staged-cutover discipline.

The migration in this RFC must remove these duplicated ownership patterns, not merely wrap them behind another facade.

### 3.5 TypeScript SQLite ownership has become a second engine

The current TypeScript database path is no longer a neutral wrapper. `packages/sdk/src/io/sqlite-service.ts` owns a synchronous `node:sqlite` `DatabaseSync` connection and exposes generic SQL operations. `packages/sdk/src/data/query-service.ts` knows table names, parses serialized JSON rows, merges and ranks result sets in JavaScript, uses `LIMIT/OFFSET` pagination, and can invoke `ensure*Projection` functions from read APIs. `packages/sdk/src/app-service.ts` performs additional canonical project/session aggregation and sorting.

That split causes five architectural problems:

1. **Two owners of schema semantics.** Rust writes canonical rows while TypeScript still decides how tables become product entities and answers.
2. **Query-time mutation.** A nominally read-only call can repair or materialize projections, defeating a clean read pool and making latency unpredictable.
3. **Synchronous event-loop work.** `DatabaseSync` and JavaScript row processing run on the calling Node thread, which may also own Electron main-process or daemon responsibilities.
4. **Excess boundary and allocation work.** Raw database JSON becomes JavaScript objects before canonical joining, ranking, slicing, or aggregation is complete.
5. **Adapter leakage into public queries.** Source-specific table knowledge and compatibility conditionals can spread upward instead of terminating at the fact/projection boundary.

Changing `better-sqlite3` to `node:sqlite` solved an installation dependency problem, but it did not solve database ownership. The target is not a better TypeScript SQLite wrapper. The target is no production TypeScript SQLite connection.

## 4. Goals

This RFC has the following goals:

1. Make Rust the authoritative implementation of all ingest, storage, projection, search, and canonical query behavior.
2. Parse each source record once and fan the resulting facts into all applicable projections.
3. Guarantee crash-safe convergence between source cursors, canonical rows, runtime state, usage totals, and emitted changes.
4. Support low-latency tailing without treating watcher events as a lossless log.
5. Define a hard, reviewable boundary between common engine code and agent-specific code.
6. Make adding an agent primarily an adapter-and-fixtures task rather than an engine or query-service modification.
7. Preserve native details without forcing all agents into a Claude-shaped schema.
8. Represent unsupported, derived, and estimated capabilities honestly.
9. Support file, directory, SQLite, and key-value source families.
10. Provide deterministic cold/live parity and a shared conformance harness.
11. Expose one versioned, typed, asynchronous query API over canonical capability packs and namespaced extensions.
12. Keep the Node/Electron event loop free from SQLite execution and large canonical aggregation work.
13. Give every query result a consistent committed-state watermark and stable pagination semantics.
14. Allow the same Rust engine to run in-process through N-API, embedded in `field-native`, or in a standalone daemon without changing semantics.
15. Keep current SDK, CLI, and TUI behavior available through an explicit staged migration and differential parity testing.
16. Retire production TypeScript SQL, migrations, projection repair, and database-specific domain logic.

## 5. Non-goals

This RFC does not:

1. define a stable dynamic plugin ABI for untrusted third-party adapters;
2. require an always-on standalone daemon in the first implementation;
3. expose a generic SQL endpoint to TypeScript, IPC clients, or adapters;
4. require zero-copy serialization for ordinary page-sized query results;
5. introduce network synchronization or a cloud service;
6. claim process lifecycle truth that is not present in observed sources;
7. guarantee a global causal order across independent files or databases;
8. force every vendor field into a universal normalized schema;
9. make filesystem events an audit log;
10. replace the source agent's own persistence or repair corrupted vendor data;
11. infer per-message token usage from a session-only aggregate without marking it as estimated;
12. preserve every transient intermediate filesystem state;
13. retain synchronous database-backed public APIs indefinitely;
14. standardize remote-agent transport; remote sources require a later RFC;
15. choose a permanent query-worker count without benchmark evidence;
16. move presentation-only concerns such as localization, display formatting, and React state management into Rust.

## 6. Relationship to prior RFCs

### 6.1 RFC 006: normalized message model and agent survey

This RFC builds on RFC 006. Spaghetti keeps a small cross-agent semantic core and preserves native/raw information. The fact and query layers are not a mandate to flatten every source into one lossy record type.

### 6.2 RFC 007: retire runtime bridge

RFC 007 remains in force. This design does not restore hook plugins, channel plugins, or a second process-adjacent runtime event facade. Source-derived evidence enters through the observation engine. Optional liveness probes are transient assessments and are never sufficient by themselves to create durable lifecycle history.

### 6.3 RFC 009: retire TypeScript bulk ingest

This RFC supersedes RFC 009's non-goal that keeps the TypeScript live writer and filesystem watchers. The desired ingest end state is now:

```text
Rust source drivers
  -> Rust adapter decoders
  -> Rust projections
  -> Rust SQLite transaction
  -> durable change log
```

RFC 009's broader goal of retiring duplicate TypeScript bulk ingest remains valid. Migration sequencing changes so that the current live path is retained only as a temporary fallback until the Rust engine replaces it. The old native live-batch bridge may still be deleted, but only after the persistent engine exists; it is not the target architecture.

### 6.4 RFC 010: `node:sqlite` migration

The repository's `sqlite-service.ts` identifies the migration from `better-sqlite3` to Node's built-in `node:sqlite` as RFC 010. That decision is treated as a successful transitional packaging improvement: it removes a native install-script dependency while preserving the SDK's existing database facade.

RFC 011 changes the final destination. `node:sqlite` remains useful as a temporary differential oracle and rollback path during query migration, but production TypeScript SQLite ownership is retired when this RFC completes.

### 6.5 Two-plane architecture

The product-level distinction between static/backfill and live source ingestion remains useful. The implementation is refined to:

> **Two temporal scheduling modes over one ingest spine, not two independent pipelines.**

The query side is not a third ingest plane. It is a read-only service over committed projections owned by the same Rust engine.

## 7. Terminology

| Term | Definition |
|---|---|
| **Adapter** | Agent-specific code that discovers native sources, declares streams, decodes native records, performs native joins, and declares capabilities. |
| **Source instance** | One discovered installation/account/profile/root for an adapter, such as a Claude home directory or a Codex state root. |
| **Stream** | A declared logical feed within a source instance, such as session transcripts, team configs, or a source-owned database query. |
| **Source object** | The independently cursorable unit inside a stream: one JSONL file, one replaceable document, one directory snapshot, one database partition, or one key range. |
| **Generation** | A monotonic epoch for a source object. Truncation, replacement, incompatible rewrite, or identity change creates a new generation. |
| **Cursor** | An opaque, durable position within one generation, such as a byte offset, row watermark, revision, content hash, or key range token. |
| **Source record** | One framed, provenance-bearing unit emitted by a source driver for adapter decoding. |
| **Native record** | The adapter's parsed representation of a source record before cross-agent semantic mapping. |
| **Fact** | A typed, idempotent semantic observation emitted by an adapter. Facts contain provenance and do not perform storage writes. |
| **Evidence** | A fact that supports a runtime-state conclusion, including its strength, subject, and source provenance. |
| **Projection** | A deterministic materialized view built from committed facts: history, runtime state, usage, search, or change feed. |
| **Observed state** | Durable state justified by committed evidence. |
| **Assessment** | Ephemeral interpretation using current time or optional process probes, such as `stale` or `possibly_waiting`. |
| **Commit sequence** | A database-monotonic identifier allocated to one committed ingest batch. It orders Spaghetti commits, not vendor causality. |
| **Change log/outbox** | Durable post-commit changes keyed by commit sequence and ordinal, used for replayable subscriptions. |
| **Database owner** | The one live `SpaghettiEngine` authorized to migrate and mutate a Spaghetti database and coordinate its read connections. |
| **Writer lane** | The single ordered Rust actor/connection that performs every Spaghetti database mutation. |
| **Query worker** | A bounded Rust worker that owns one long-lived read-only SQLite connection and executes typed requests. |
| **Query pack** | A versioned public query surface backed by a universal core or optional capability pack. |
| **Commit watermark** | The highest committed Spaghetti sequence represented by a query snapshot or result page. |
| **Client transport** | N-API or IPC implementation of the same typed `SpaghettiClient` semantics. |
| **Reconcile** | Compare source reality with committed source-object state and ingest the delta or rebuild an affected projection. |

## 8. North-star architecture

```text
Agent-owned sources
  files / directories / source SQLite / KV
                    |
                    v
+----------------------------------------------------------+
| SpaghettiEngine                                          |
|                                                          |
|  +---------------- Observation runtime ----------------+ |
|  | discovery / watch / poll / reconcile                | |
|  | per-object scheduling / source drivers              | |
|  | adapter decode / native joins / FactBatch           | |
|  +---------------------------+--------------------------+ |
|                              |                            |
|                              v                            |
|  +---------------------- WriterActor ------------------+ |
|  | one long-lived read/write SQLite connection         | |
|  | migrations / canonical history / FTS                | |
|  | runtime evidence and state / usage contributions    | |
|  | source cursor and decoder state / durable outbox    | |
|  | projection maintenance and repair                   | |
|  +---------------------------+--------------------------+ |
|                              | COMMIT                     |
|             +----------------+----------------+           |
|             |                                 |           |
|             v                                 v           |
|  +-----------------------+        +---------------------+ |
|  | QueryPool             |        | ChangePublisher     | |
|  | bounded read workers  |        | replay by commit_seq| |
|  | read-only connections |        +---------------------+ |
|  | typed query packs     |                                 |
|  +-----------+-----------+        +---------------------+ |
|              |                    | Runtime cache       | |
|              |                    | hydrated from DB    | |
|              |                    +---------------------+ |
+--------------+-------------------------------------------+
               |
       typed async responses
       + durable change batches
               |
      +--------+-------------------------+
      |                                  |
      v                                  v
N-API transport                    IPC transport
standalone Node/Electron           field-native / daemon
      |                                  |
      +---------------+------------------+
                      v
              TypeScript SpaghettiClient
              SDK / CLI / TUI / React
              no SQLite and no source parsing
```

The writer and query lanes are one engine authority, not two applications pointed at the same file. The query pool may observe the database concurrently through WAL, but it cannot mutate projections or bypass engine lifecycle.

The data and query paths are library-first. Standalone consumers use:

```text
Node / Electron
  -> asynchronous N-API transport
  -> SpaghettiEngine
  -> SQLite
```

Vibe Field uses:

```text
Electron / fieldd
  -> framed IPC
  -> field-native
  -> embedded SpaghettiEngine
  -> SQLite
```

A standalone daemon may host the same engine later. Hosting changes call transport and lifecycle ownership, not source semantics, adapter behavior, transaction boundaries, query contracts, or pagination.

## 9. Architectural invariants

The following invariants are normative.

### I1. One database authority

For one Spaghetti database and source set, exactly one live `SpaghettiEngine` owns migrations, writer lifecycle, projection maintenance, query-pool lifecycle, and subscriptions. Multiple hosts must connect to the existing owner or fail with structured owner metadata. No TypeScript process opens a second production connection independently.

### I2. Same decoder for backfill and live

A source record has one adapter decoder. Backfill, warm repair, and live tailing may frame records differently by cursor range, but they must invoke the same decode implementation and projections.

### I3. Adapter facts are side-effect free

Decoding may read adapter-owned dependencies through an explicitly provided read-only source-access interface. It may not:

- open or mutate the Spaghetti database;
- execute public Spaghetti queries;
- publish public events;
- advance cursors;
- own retry loops or watcher handles;
- spawn unbounded background tasks;
- mutate a process-global token accumulator.

### I4. Cursor and projections are atomic

A source cursor or snapshot revision advances only in the same SQLite transaction that commits all facts derived from that range and the matching change-log entries.

### I5. Events publish after commit

Subscribers never observe uncommitted changes. A committed but not yet published change is recovered from the durable change log.

### I6. Per-object order, no invented global order

Records from one source object and generation are applied in cursor order. Different source objects may decode concurrently. Cross-object semantic correlation must tolerate late arrival and may not rely on callback order.

### I7. Overflow becomes reconciliation

A native watcher overflow, internal queue overflow, backend error, invalid cursor, root replacement, or source identity mismatch marks the relevant scope dirty. The engine reconciles; it does not silently continue a potentially incomplete incremental stream.

### I8. Complete malformed records do not stall the stream

An unterminated trailing record remains pending and does not advance the cursor. A complete but malformed record is quarantined transactionally with its provenance, then the cursor advances so one vendor-format change cannot permanently block live ingestion.

### I9. Capability honesty

A capability is never inferred solely because a table has a column for it. The adapter declares whether the value is native, derived, estimated, or unsupported, and at what scope.

### I10. Core has no agent branching

No common-engine, store, or common-query module may branch on `claude-code`, `codex`, `grok`, or future adapter identifiers. Agent identifiers may appear only in registry construction, diagnostics, adapter-owned code, namespaced extensions, compatibility facades, and presentation metadata.

### I11. Queries are pure reads

A query connection uses read-only/query-only mode and never runs migrations, repairs a dirty projection, updates an access timestamp, advances a cursor, or writes a cache table. Missing or stale projections are handled by readiness state, a read-only fallback where explicitly supported, or asynchronous wait—not hidden mutation.

### I12. TypeScript has no schema authority

Production TypeScript does not contain Spaghetti SQL, table names used for canonical reads, migration logic, projection-repair logic, or database row-to-domain assembly. It consumes versioned request/response types and presentation metadata.

### I13. One boundary crossing per logical operation

A logical query such as search, timeline page, usage report, or runtime snapshot crosses N-API/IPC once, aside from explicitly streamed page chunks. Clients do not orchestrate multiple native SQL calls or call native code once per row.

### I14. Database work is asynchronous and cancellable

Public database-backed client methods are asynchronous. N-API work runs away from the Node event-loop thread; IPC naturally resolves asynchronously. Expensive searches, exports, and timeline requests support cancellation or supersession.

### I15. Query results identify their committed snapshot

Every database-backed page or snapshot carries an `at_commit_seq` watermark. Multi-statement logical queries run in one read transaction or equivalent snapshot so counts, facets, rows, and cursors refer to one committed state.

## 10. Common code versus agent-specific code

### 10.1 Decision rule

A behavior belongs in **common code** when it is one of the following:

1. required for ingest correctness regardless of agent;
2. a reusable transport/read semantic;
3. a storage, transaction, scheduling, delivery, or observability mechanism;
4. a cross-agent semantic used by at least two adapters; or
5. an explicit Spaghetti product abstraction with a precise capability contract; or
6. canonical query semantics over common facts or an approved capability pack.

A behavior belongs in an **agent adapter** when it answers one of these questions:

1. Where does this agent store its data?
2. What does a native record mean?
3. Which native identifier represents a session, run, child, team, or artifact?
4. How do multiple native files/rows join into one entity?
5. Is a usage number a delta, cumulative counter, estimate, or exact observation?
6. Which capabilities can this agent truthfully provide?
7. How should a native schema version be decoded or migrated?
8. Which native facts can honestly satisfy a common query-pack contract?

### 10.2 The rule of two, with an explicit product-contract exception

New universal fields should not be added merely because Claude has them. A semantic enters the shared model when:

- at least two adapters expose meaningfully equivalent data and Spaghetti uses it; **or**
- Spaghetti deliberately defines it as an optional product-level capability pack with precise semantics and honest unsupported states.

The second path allows Spaghetti to model a feature such as teams before two vendors expose identical structures, without pretending that every agent has teams.

### 10.3 Three semantic layers

To avoid both over-normalization and adapter leakage, the model has three layers:

#### Layer A: universal core

Small, stable concepts required by most projections:

- source provenance;
- session/conversation identity;
- message/content blocks;
- run/agent identity;
- relationships;
- runtime evidence;
- usage observations;
- timestamps with quality metadata;
- raw/native payload references;
- unknown-record quarantine.

#### Layer B: optional capability packs

Shared schemas and reducers activated only when declared:

- delegation/subagents;
- teams and team inboxes;
- approvals;
- tasks/plans;
- compaction;
- artifacts;
- active-session presence;
- native-TUI or terminal-related observations.

A capability pack may initially have one adapter, but it must define vendor-neutral behavior and conformance tests.

#### Layer C: namespaced extensions

Agent-specific data that has not earned a shared abstraction:

```text
claude-code/*
codex/*
grok/*
```

Extensions retain structured native information without forcing changes into common tables. Promotion from an extension to a common fact requires a schema review and migration.

### 10.4 Hard dependency boundary

The desired dependency graph is:

```text
spaghetti-model / adapter-api
          ^
          |
  +-------+---------------------------+
  |                  |                |
claude adapter    codex adapter    grok adapter
  |                  |                |
  +------------------+----------------+
                     |
          registered by host/bootstrap

spaghetti-engine -> spaghetti-model / adapter-api
spaghetti-store  -> spaghetti-model
spaghetti-query  -> spaghetti-model + store read API
spaghetti-host   -> engine + store + query + selected adapters
```

Normative constraints:

- core/engine/store crates must not depend on adapter crates;
- adapter crates depend on model and adapter API, not on `rusqlite`, `notify`, N-API, or UI packages;
- adapters may use source-reader helpers exposed by the adapter API;
- source-specific SQL is allowed only for read-only access to an agent-owned database, never for the Spaghetti database;
- migrations and public queries for Spaghetti-owned tables remain centrally owned;
- adapters never receive a Spaghetti connection, SQL executor, table name, or public query callback;
- production TypeScript depends on generated/shared request and response types, not schema modules;
- adding an adapter should require one registry entry, not edits throughout the engine or common query packs unless it introduces an approved new capability.

### 10.5 Boundary examples

| Concern | Owner | Reason |
|---|---|---|
| Watch a root and recover from overflow | Common engine | Correctness mechanism |
| Determine that `projects/*/*.jsonl` is a Claude session | Claude adapter | Native layout |
| Frame newline-terminated records and retain partial tail | Common source driver | Reusable append semantic |
| Interpret a Claude `assistant` object | Claude adapter | Native schema |
| Assign `source_object_id`, generation, and cursor | Common engine | Idempotency and recovery |
| Correlate a Claude subagent transcript to its parent | Claude adapter | Native identifiers/joins |
| Reduce evidence to current run state | Common reducer | Product semantics |
| Decide which Claude event means explicit completion | Claude adapter | Native interpretation |
| Write `messages`, usage materializations, and outbox | Common store/projections | Atomic consistency |
| Read Codex rollout rows or Grok sidecars | Corresponding adapter through source access | Native source |
| Report usage as exact/estimated and its scope | Adapter declares; common reducer enforces | Native semantics plus common honesty |
| Reconnect a subscriber from sequence 42,000 | Common engine | Delivery contract |
| Execute cross-agent FTS ranking and pagination | Common query engine | Canonical product semantics |
| Decode a Claude-only native field into an extension fact | Claude adapter | Native interpretation |
| Expose a versioned Claude extension query | Common extension-query registry over stored extension facts | Stable storage and client contract |
| Format a timestamp or localize a status label | TypeScript presentation layer | UI-only concern |

### 10.6 Query boundary

The common query engine operates over canonical projections, capability-pack projections, and centrally stored extension facts. It does not call an adapter during an ordinary public query. Query behavior must remain available after source roots are temporarily offline and must be reproducible from the Spaghetti database alone.

The boundary is:

```text
adapter: native source -> typed facts + capability declaration
common writer: facts -> canonical/capability/extension projections
common query: projections -> typed public result
TypeScript: typed result -> presentation
```

An adapter may help define the semantic contract for a new capability or namespaced extension during development, but the released query implementation lives in common Rust code and uses centrally versioned storage. This prevents an adapter from becoming a hidden query service and allows query parity, cancellation, pagination, and snapshot guarantees to be tested independently of the vendor source.

## 11. Adapter contract

### 11.1 Open adapter identity

The current closed union of known source identifiers must become an open identifier:

```rust
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AdapterId(std::sync::Arc<str>);
```

An adapter manifest includes a stable adapter ID and an independent contract version. The ID identifies the source family; the contract version identifies the meaning of emitted facts and declared streams.

```rust
pub struct AdapterManifest {
    pub id: AdapterId,
    pub display_name: &'static str,
    pub adapter_version: semver::Version,
    pub contract_version: u32,
    pub source_schema_versions: &'static [SourceSchemaVersion],
    pub capabilities: CapabilityManifest,
}
```

Changing native parsing without changing emitted semantics may increment `adapter_version`. A change that alters fact identity, attribution, or meaning must increment `contract_version` and trigger the corresponding materialization repair.

### 11.2 Primary adapter trait

The exact Rust syntax may evolve, but the semantic contract is:

```rust
pub trait AgentAdapter: Send + Sync + 'static {
    fn manifest(&self) -> &AdapterManifest;

    fn discover(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<SourceInstanceSpec>, AdapterError>;

    fn streams(
        &self,
        instance: &SourceInstance,
    ) -> Result<Vec<StreamSpec>, AdapterError>;

    fn bootstrap_object(
        &self,
        _ctx: BootstrapContext<'_>,
        _object: &SourceObjectDescriptor,
    ) -> Result<AdapterObjectContext, AdapterError> {
        Ok(AdapterObjectContext::empty())
    }

    fn decode(
        &self,
        ctx: DecodeContext<'_>,
        record: SourceRecordRef<'_>,
        out: &mut FactBatch,
    ) -> Result<DecodeDisposition, AdapterError>;

    fn reconcile_entity(
        &self,
        _ctx: ReconcileContext<'_>,
        _request: EntityReconcileRequest<'_>,
        _out: &mut FactBatch,
    ) -> Result<ReconcileDisposition, AdapterError> {
        Ok(ReconcileDisposition::NotRequired)
    }
}
```

The common path is declarative: `discover` returns source instances, `streams` declares how to produce records, `bootstrap_object` derives bounded stable context when needed, and `decode` turns records into facts. `reconcile_entity` is an escape hatch for agent-specific multi-object joins, not a replacement for generic driver logic.

### 11.3 Source instance declaration

```rust
pub struct SourceInstanceSpec {
    pub stable_key: SourceInstanceKey,
    pub display_name: String,
    pub roots: Vec<SourceRoot>,
    pub configuration: AdapterConfig,
}
```

A source instance key must be stable across process restarts and ordinary path spelling differences. It may derive from a canonical root, source-owned installation identifier, account ID, or a combination. Secrets must not be embedded in the key.

### 11.4 Stream declaration

```rust
pub struct StreamSpec {
    pub id: StreamId,
    pub driver: DriverSpec,
    pub selector: ObjectSelector,
    pub decoder: DecoderId,
    pub authority: StreamAuthority,
    pub entity_scope: EntityScope,
    pub priority: IngestPriority,
    pub consistency: ConsistencyPolicy,
    pub deletion: DeletionPolicy,
    pub retention: RawRetentionPolicy,
}
```

A stream declares mechanics without placing mechanics inside the adapter:

- `driver` selects a common source driver;
- `selector` identifies source objects;
- `decoder` routes the record back to an adapter decoder;
- `authority` distinguishes canonical sources from supplemental sidecars and derived streams that must be ignored;
- `entity_scope` assists batching and reconciliation;
- `priority` distinguishes active transcript tails from low-priority historical scans;
- `consistency` declares whether incremental cursors are valid or snapshots must be diffed;
- `deletion` declares how confirmed source removal retracts owned facts;
- `retention` controls whether raw payloads are retained, hashed, or provenance-only.

`StreamAuthority` is one of:

```text
Canonical     authoritative source for canonical facts
Supplemental  enriches canonical entities but is not a duplicate transcript
Diagnostic    retained as extension/diagnostic data only
IgnoredDerived known UI/cache/projection stream that must not be ingested
```

This prevents duplicate ingestion when an agent stores both a model-facing canonical transcript and a larger UI/telemetry projection of the same turns.

The default durable deletion policy is `MirrorSource`: once absence is confirmed by reconciliation, facts owned exclusively by that source object/generation are retracted in the same transaction that records the deletion. Presence streams additionally emit absence evidence. Preserving history after the source deletes it would change Spaghetti from a rebuildable projection into an archival system and requires a separate product decision.

A rename is resolved by reconciliation. If native file identity is stable and the path is non-semantic, the engine may update the object's display location. If the path participates in adapter identity or entity meaning, the move is handled as remove/add or generation replacement according to the stream policy.

### 11.5 Adapter access to source dependencies

Some records cannot be interpreted in isolation. Grok may require sidecar data; Codex may store metadata separately; a database-backed agent may require a read-only join.

Adapters receive a bounded, read-only `SourceAccess` capability:

```rust
pub trait SourceAccess {
    fn read_object(
        &self,
        key: &SourceObjectKey,
    ) -> Result<SourceSnapshot, SourceReadError>;

    fn read_json_value(
        &self,
        key: &SourceObjectKey,
    ) -> Result<JsonSourceSnapshot, SourceReadError>;

    fn query_source_db(
        &self,
        query: SourceQueryRef<'_>,
    ) -> Result<SourceRows, SourceReadError>;

    fn list_objects(
        &self,
        selector: &ObjectSelector,
    ) -> Result<Vec<SourceObjectDescriptor>, SourceReadError>;
}
```

The engine controls concurrency, timeouts, path confinement, read-only database flags, and cancellation. Adapters cannot retain this access outside the decode/reconcile call. A database-backed adapter may define named read-only source query text and row schemas; the common source driver opens and executes them. Those queries may reference only the agent-owned source database and have no access to Spaghetti migrations or write handles.

### 11.6 No adapter-owned public event types

Adapters may emit extension facts, but they do not create arbitrary public event channels. All public changes are derived by common projections and use stable topics. This prevents each adapter from inventing incompatible replay, ordering, and lifecycle semantics.

### 11.7 Object bootstrap and bounded decoder state

Some source objects cannot decode every record in isolation. For example, a header record may establish session identity and project path for all later records. The engine therefore supports two adapter-owned but engine-managed values:

1. **Object context** — stable metadata derived from the path, source descriptor, bounded prefix, or companion metadata, such as native session ID, cwd, schema version, or child/root classification.
2. **Decoder state** — bounded rolling state needed to interpret the next record, such as a native turn identity or cumulative-counter baseline.

```rust
pub struct DecodeContext<'a> {
    pub decoder: &'a DecoderId,
    pub object_context: &'a AdapterObjectContext,
    pub decoder_state: &'a AdapterDecoderState,
    pub source_access: &'a dyn SourceAccess,
}

pub struct FactBatch {
    pub facts: Vec<Fact>,
    pub entity_repairs: Vec<EntityRepairHint>,
    pub dependency_reads: Vec<DependencyRevision>,
    pub next_decoder_state: Option<AdapterDecoderState>,
    pub diagnostics: Vec<AdapterDiagnostic>,
}
```

The common engine owns storage, versioning, rollback, size limits, and reset of these values. The adapter owns their opaque meaning.

Normative rules:

- stateless decoding is preferred;
- state must be deterministic from committed source records and declared dependencies;
- state is bounded and versioned;
- the next state commits atomically with facts and cursor;
- a failed transaction leaves both cursor and decoder state unchanged;
- generation or incompatible adapter-contract change resets state and replays from a safe boundary;
- state may not contain open handles, tasks, wall-clock timers, or mutable SQL connections;
- a source record should emit usage at its truthful native scope rather than use hidden state to fabricate message attribution.

Codex `session_meta`, for example, can populate object context once; subsequent rollout records receive that context without rescanning the file or keeping TypeScript-only maps.

### 11.8 Dependency revisions

Multi-object decoding must be race-aware. `SourceAccess` returns snapshots with stable revision stamps. If an adapter reads a sibling sidecar or source database row set, it adds that revision to `dependency_reads`.

The engine records or validates those revisions around commit. If a dependency changes during decode/commit, the current transaction may still commit a valid observation of the stamped revision, but the affected entity is immediately marked dirty and reconciled again. This provides eventual convergence without pretending that independent files share an atomic write transaction.

Adapters do not advance dependency cursors merely by reading them. The dependency's own stream remains responsible for its durable cursor.

### 11.9 No adapter-owned Spaghetti queries

Adapters may define named, read-only queries against an **agent-owned source database** through `SourceAccess`; that is source decoding. They may not define arbitrary SQL against the Spaghetti database, return database rows directly to clients, or register a callback that executes during a public query.

A vendor-only public surface uses one of two paths:

1. promote the semantics into a reviewed capability pack and common query pack; or
2. store a namespaced extension fact and expose it through a centrally versioned extension-query contract.

In both cases the adapter stops at fact emission. Storage layout, migrations, pagination, authorization, serialization, and public API versioning remain common responsibilities.

## 12. Source-driver model

### 12.1 Why drivers are separate from adapters

The same native storage behavior recurs across agents. It should be implemented once, tested once, and parameterized by an adapter stream declaration.

A source driver is responsible for:

- identifying source objects;
- opening and reading them safely;
- framing records;
- producing cursor ranges;
- detecting truncation, replacement, and deletion;
- reporting generation changes;
- exposing a deterministic reconcile operation;
- never interpreting agent semantics.

### 12.2 Required v1 drivers

#### A. `AppendDelimitedFile`

For JSONL and other delimiter-framed logs.

Properties:

- cursor is a byte offset plus framing state;
- only complete delimiter-terminated records are emitted;
- an incomplete suffix remains pending;
- append continues within the same generation;
- truncation, file identity replacement, or incompatible prefix change creates a new generation;
- the driver may batch multiple records without losing individual provenance.

JSONL is configured as delimiter `\n` with optional CRLF normalization. The adapter, not the driver, parses JSON.

#### B. `ReplaceDocument`

For `config.json`, `summary.json`, settings, and replace-on-write documents.

Properties:

- cursor is a content revision/fingerprint;
- the entire stable document is one source record;
- atomic rename and in-place write produce the same semantic result;
- transient parse failure during an active write schedules a retry before quarantine;
- a confirmed complete malformed revision is quarantined;
- projections use snapshot replacement or diff semantics declared by the fact type.

#### C. `DirectorySnapshot`

For directory membership and sets of independently replaceable child documents.

Properties:

- the engine enumerates a path-confined directory;
- selector and ignore rules apply before expensive metadata reads;
- the snapshot records object identity, names, and revisions;
- additions, removals, and replacements are reconciled even if watcher events were dropped;
- child contents may be delegated to another declared stream.

#### D. `PresenceObject`

For active-session files, leases, locks, and other existence-oriented state.

Properties:

- creation/update/removal all have semantic meaning;
- content may be decoded, but absence is also emitted as an observation;
- expiration based on current time is an assessment, not a durable source fact.

#### E. `SqliteSnapshot`

For read-only agent-owned SQLite stores.

Properties:

- opens with read-only/query-only semantics where supported;
- adapter supplies named source queries and row decoding, not arbitrary Spaghetti-store SQL;
- filesystem changes to the main database, WAL, or related files are only wake-up hints;
- a source snapshot is read under a consistent read transaction;
- cursor may be a source revision/data-version signal, monotonic row key, update timestamp, or stable snapshot hash;
- when no trustworthy incremental watermark exists, the driver performs a bounded snapshot diff;
- source database locks and busy errors are retried without blocking the main writer lane.

#### F. `KeyValueSnapshot`

For source-owned key-value databases such as VS Code state stores.

Properties:

- adapter declares key prefixes or exact keys;
- the driver emits stable key/value records and removals;
- cursor is a database revision when available, otherwise a snapshot fingerprint;
- decoding remains adapter-specific.

### 12.3 Custom producer escape hatch

A future source may not fit a built-in driver. A built-in adapter may implement a custom record producer, but it must obey the same engine contract:

- records have stable source-object identity, generation, and cursor ranges;
- reconcile is deterministic;
- reads are cancelable and bounded;
- no Spaghetti database writes;
- no public event publication;
- no hidden process-global cursor;
- the core conformance pack still applies.

A custom producer is an exception requiring architecture review. If a second adapter needs the same behavior, it should be promoted into a common driver.

## 13. Watch, scan, and reconcile

### 13.1 Watchers produce dirty hints

Native filesystem events are not a semantic event stream. The watcher layer emits only invalidation hints:

```rust
pub enum DirtyHint {
    Object(SourceObjectKey),
    Subtree(SourcePathKey),
    Stream(StreamKey),
    Instance(SourceInstanceId),
}

pub enum DirtyReason {
    NativeEvent,
    PollDetectedChange,
    WatcherOverflow,
    InternalQueueOverflow,
    BackendError,
    CursorInvalid,
    IdentityChanged,
    RootMoved,
    Recovery,
    ManualRepair,
}
```

Hints may be duplicated or coalesced. Once a scope is marked dirty for an overflow-class reason, the engine must reconcile that scope before claiming it is live again.

### 13.2 Initial scan race

The startup sequence is:

```text
1. Discover source instance.
2. Register watcher/poller.
3. Begin buffering dirty hints.
4. Scan and ingest current source state.
5. Replay/coalesce buffered hints.
6. Reconcile any changed objects.
7. Mark stream Live at a known commit sequence.
```

Scanning before watch registration is prohibited because it creates a permanent missed-change window.

### 13.3 Scheduling model

The scheduler guarantees:

```text
same source object + generation: serial
independent source objects: bounded parallel read/decode
SQLite projection commit: one ordered writer lane
```

A permanent task per file is not required. The engine maintains compact per-object state and schedules work through bounded pools.

Priority classes are:

1. **Interactive:** active transcript, usage, presence, explicit lifecycle evidence;
2. **Foreground repair:** objects touched by recent hints;
3. **Backfill:** undiscovered or stale historical objects;
4. **Maintenance:** audit, compaction, projection rebuild.

Lower priorities must not starve, and interactive work must not create unbounded queues.

### 13.4 Coalescing

Coalescing operates on dirty objects, not on semantic records. Suggested initial live policy:

```text
soft batch window: 8–20 ms
hard flush deadline: 50–100 ms
max dirty objects per dispatch: bounded
max bytes/records per commit: bounded
```

These are tuning defaults, not protocol guarantees. Active streams may use a lower latency budget than bulk backfill.

### 13.5 Polling backstop

Native watchers may be unavailable, delayed, or unreliable on network filesystems, WSL boundaries, container mounts, or unusual source databases. The engine supports:

- adaptive polling for active objects;
- lower-frequency stream reconciliation;
- explicit manual repair;
- automatic fallback after repeated watcher failure.

Active JSONL objects with an incomplete trailing record receive a short retry timer even if no subsequent native event arrives.

### 13.6 Symlink and path policy

Source roots are path-confined. The engine does not follow arbitrary symlinks outside a declared source root unless an adapter explicitly declares and the host approves that behavior. Internal path identity must preserve non-UTF-8 names where the platform permits them; display paths are separate from identity keys.

## 14. Source record and provenance model

```rust
pub struct SourceRecord {
    pub source_instance_id: SourceInstanceId,
    pub stream_id: StreamId,
    pub object_id: SourceObjectId,
    pub generation: u64,
    pub cursor_start: SourceCursor,
    pub cursor_end: SourceCursor,
    pub ordinal_in_batch: u32,
    pub observed_at: Timestamp,
    pub source_timestamp_hint: Option<Timestamp>,
    pub media_type: SourceMediaType,
    pub payload: Bytes,
    pub payload_hash: RecordHash,
}
```

The stable source-record identity is derived from:

```text
(source_object_id, generation, cursor_start, cursor_end, payload_hash policy)
```

For strictly append-only objects, cursor range is normally sufficient. A hash protects against incompatible rewrites at the same offset. Snapshot records use object revision/fingerprint as their cursor.

Every durable canonical row affected by a source record must be traceable to source provenance, either directly or through a fact/application table. `msg_index` remains useful for display order but is not the sole idempotency key.

### 14.1 File identity and generations

Where available, the engine records platform-native file identity:

```text
Unix: device + inode
Windows: volume identity + file ID
Fallback: confined path key + size/mtime/prefix fingerprint
```

A generation increments when:

- size shrinks below committed offset;
- native file identity changes at the same path;
- a verified prefix no longer matches committed content;
- a snapshot object is replaced incompatibly;
- the adapter contract requires reinterpretation from the beginning.

A new generation is not silently appended to the old projection. The engine invokes generation-replacement semantics for rows owned by the old generation and reprojects the new source.

### 14.2 Record parse outcomes

```rust
pub enum DecodeDisposition {
    Applied,
    IgnoredKnown,
    PreservedUnknown,
    RetryTransient,
}
```

- `Applied`: one or more facts emitted.
- `IgnoredKnown`: the adapter deliberately recognizes a record that produces no user-visible fact.
- `PreservedUnknown`: complete native record is stored/quarantined for future decoder versions and the cursor may advance.
- `RetryTransient`: the record or its dependencies are not yet stable; the cursor does not advance.

Unexpected adapter errors are classified as transient, record-local permanent, stream-fatal, or adapter-fatal. The scheduler applies bounded retry and circuit-breaking policy centrally.

## 15. Fact model

### 15.1 Design principles

Facts are:

- typed;
- deterministic from source records and bounded source dependencies;
- idempotent by fact identity;
- provenance-bearing;
- storage-agnostic;
- honest about certainty and granularity;
- suitable for multiple projections;
- capable of preserving native fields without polluting universal tables.

Adapters return the `FactBatch` defined in §11.7; they do not invoke projection APIs directly. The batch carries facts, repair hints, stamped dependency reads, optional next decoder state, and diagnostics under one cursor transaction.

### 15.2 Universal facts

Illustrative shape:

```rust
pub enum Fact {
    Session(SessionFact),
    Message(MessageFact),
    Run(RunFact),
    RunEvidence(RunEvidenceFact),
    Relation(RelationFact),
    Usage(UsageFact),
    Presence(PresenceFact),
    Artifact(ArtifactFact),
    CapabilityPack(CapabilityPackFact),
    Extension(ExtensionFact),
    Unknown(UnknownRecordFact),
}
```

This enum is a conceptual model. Implementation may use versioned structs or an internal tagged schema to reduce churn.

### 15.3 Entity and fact identity

Raw vendor UUIDs are not assumed globally unique. A canonical entity key is namespaced by at least:

```text
(adapter_id, source_instance_id, entity_kind, native_entity_key)
```

When no stable native key exists, the adapter derives a deterministic synthetic key from stable native attributes and documents the collision policy. Random UUID generation during ingest is prohibited because it breaks rebuild convergence.

Each fact also has a deterministic identity:

```text
native fact/event key, when stable
    otherwise
(source_record_identity, fact_kind, local_fact_ordinal)
```

Snapshot-owned facts additionally carry an ownership scope and snapshot revision. Re-decoding the same source record updates the same fact identity. A correction changes its contribution; it does not create a duplicate. A fact removed from a newer snapshot revision is retracted according to the stream's deletion policy.

Project/workspace identity likewise includes source instance and a native/canonical path key. A display slug is never the sole database identity.

### 15.4 Time and ordering

Every relevant fact distinguishes:

```text
source_time   time asserted by the agent, optional and quality-qualified
observed_at   time Spaghetti read the source record
commit_seq    order in which Spaghetti durably committed the batch
source_order  object generation + cursor position
```

A qualified source timestamp records origin and precision, for example native exact, native approximate, file metadata fallback, or derived. File mtime may support freshness and reconciliation but is not silently presented as an exact message timestamp.

Consumers choose the appropriate dimension:

- transcript presentation normally prefers qualified source time and then stable source order;
- resumable delivery uses commit sequence;
- per-object replay uses source order;
- runtime freshness uses observed time plus explicit evidence;
- cross-object causality is represented by relations, not inferred from callback timing.

### 15.5 Message facts

A message fact contains only common message semantics:

- stable adapter-native message identity when available;
- session identity;
- role/kind;
- ordered content blocks rather than one forced prose string;
- source and observation timestamps with quality;
- model/agent attribution when native;
- parent/turn relation when native;
- raw/native reference;
- provenance.

Tool calls, tool results, reasoning blocks, attachments, and vendor-specific content remain typed blocks or extensions. They are not flattened into text and discarded.

### 15.6 Run and relation facts

A `run` is a unit of agent activity that can be a root agent, child/subagent, delegated task, or another adapter-declared execution entity.

Relations are explicit:

```text
session contains run
run spawned child run
run delegated task
run belongs to team
message emitted by run
artifact produced by run
```

An adapter may emit a relation before both endpoints are present. The common relation projection supports pending endpoints and late repair.

### 15.7 Evidence facts

```rust
pub struct RunEvidenceFact {
    pub subject: SubjectRef,
    pub kind: EvidenceKind,
    pub strength: EvidenceStrength,
    pub native_state: Option<NativeStateCode>,
    pub source_time: Option<QualifiedTimestamp>,
    pub detail: Option<ExtensionValue>,
    pub provenance: FactProvenance,
}
```

Common evidence kinds include:

```text
RunDeclared
RunStarted
ActivityObserved
WaitingObserved
InputRequested
ChildSpawned
ChildCompleted
TerminalSucceeded
TerminalFailed
TerminalCancelled
PresenceAdded
PresenceRemoved
TeamMembershipAdded
TeamMembershipRemoved
```

Adapters map native events into these meanings. Unknown native states remain extensions; adapters must not force them into an incorrect common state.

### 15.8 Usage facts

Usage must preserve native accounting semantics:

```rust
pub struct UsageFact {
    pub subject: UsageSubject,
    pub scope: UsageScope,
    pub accounting: UsageAccounting,
    pub quality: ValueQuality,
    pub values: TokenUsage,
    pub model: Option<ModelRef>,
    pub source_time: Option<QualifiedTimestamp>,
    pub provenance: FactProvenance,
}

pub enum UsageScope {
    Record,
    Message,
    Turn,
    Run,
    Session,
    Team,
    Project,
}

pub enum UsageAccounting {
    Delta,
    Cumulative,
    Snapshot,
}

pub enum ValueQuality {
    NativeExact,
    NativeApproximate,
    DerivedExact,
    Estimated,
}
```

The common reducer converts facts into contribution deltas. It must not assign a session-only aggregate to the final message merely to fill message columns. Legacy per-message fields may be populated only when attribution is native or explicitly estimated.

“Real-time usage” means low-latency observation after the agent materializes usage in its local source. Spaghetti cannot claim provider-side instantaneous metering. Usage APIs expose source time when available, observed time, commit sequence, scope, and quality so this latency is visible.

### 15.9 Capability-pack facts

Capability packs define optional shared semantics and dedicated conformance tests. Initial packs are:

- `delegation`: child runs/subagents and parent relationships;
- `teams`: teams, members, membership revisions, inbox messages, assignments;
- `tasks`: tasks, plans, and status snapshots;
- `approvals`: requested/allowed/denied/timed-out/disconnected;
- `compaction`: compaction markers and lineage;
- `presence`: active-session or native-presence evidence;
- `artifacts`: files or other outputs attributed to sessions/runs.

An adapter declares only packs it can support. A pack may define both facts and projection behavior, but it cannot bypass the common transaction or outbox.

#### Delegation pack minimum contract

The delegation pack preserves execution lineage rather than flattening child activity into the parent transcript. It defines:

- child run identity;
- parent run/session relation;
- optional delegated task/prompt identity;
- child activity and terminal evidence;
- optional execution context such as cwd/worktree when native;
- late and unresolved relation handling;
- truthful distinction between a child process, a forked conversation, and a vendor-native subagent when the source distinguishes them.

A child transcript may exist without a discovered parent. It remains queryable and is linked later. The pack never treats filesystem nesting alone as terminal or causal evidence unless the adapter contract declares that layout authoritative.

#### Teams pack minimum contract

The teams pack defines:

- team identity and snapshot revision;
- member identity and membership add/remove;
- member-to-run/session relations when available;
- inbox message identity, sender, recipient, content, timestamp quality, and read state;
- assignments/tasks as references or pack facts;
- team-level runtime summaries derived from member evidence.

Membership in a config file proves membership, not that the member is currently executing. Member runtime state is joined from run/session evidence. A team snapshot removes members absent from the new revision, but historical inbox messages follow the source stream's mirror/deletion policy.

### 15.10 Extension facts

```rust
pub struct ExtensionFact {
    pub namespace: ExtensionNamespace,
    pub kind: ExtensionKind,
    pub subject: Option<SubjectRef>,
    pub schema_version: u32,
    pub payload: serde_json::Value,
    pub provenance: FactProvenance,
}
```

Extensions are queryable and replayable, but they do not automatically become common API fields. Their namespace must match the adapter or an approved capability pack.

## 16. Runtime-state model

### 16.1 Durable observation versus transient assessment

Spaghetti must distinguish what the source proves from what the engine currently suspects.

```rust
pub struct ObservedRunState {
    pub state: RunState,
    pub decisive_evidence_id: FactId,
    pub last_activity_at: Option<Timestamp>,
    pub terminal_at: Option<Timestamp>,
    pub last_commit_seq: CommitSeq,
}

pub struct RunAssessment {
    pub assessment: AssessmentKind,
    pub evaluated_at: Timestamp,
    pub basis: AssessmentBasis,
    pub expires_at: Option<Timestamp>,
}
```

Durable observed states may include:

```text
Declared
Active
Waiting
Succeeded
Failed
Cancelled
Unknown
```

Transient assessments may include:

```text
LikelyActive
PossiblyWaiting
Stale
ProcessMissing
SourceUnavailable
```

`Stale` is not synonymous with `Completed`. Silence cannot prove completion.

Assessments are computed from durable observed state plus `now` and optional host probes. They are not inserted into historical projections or the durable change log. Runtime query responses include `evaluated_at` and, where useful, `next_evaluation_at`, allowing a client to refresh without fabricating a committed source event.

### 16.2 Evidence precedence

The common reducer applies deterministic precedence:

1. an explicit terminal fact is stronger than inferred inactivity;
2. explicit native start/activity is stronger than file-existence evidence;
3. source order governs records from the same source object and generation;
4. a newer native run generation may supersede a terminal state for an earlier run generation;
5. cross-object callback order is never used as causality;
6. source timestamps assist correlation but do not override stronger provenance when clocks are missing or inconsistent;
7. conflicting strong evidence is preserved and surfaced as a diagnostic rather than silently discarded.

The adapter is responsible for mapping native events to evidence kind and strength. The common reducer is responsible for consistent state transitions.

### 16.3 Late correlation

Parent transcript, subagent transcript, team config, and sidecar events may arrive in any order. Facts therefore use stable adapter-native subject keys and relations that can be unresolved.

```text
child activity arrives
    -> create provisional run keyed by native child ID

parent spawn record arrives later
    -> attach parent relation
    -> recompute affected runtime projection
    -> emit relation/runtime changes in a new commit
```

A missing relation is not grounds to drop the child data. Reconciliation may repair links after source restart or adapter upgrade.

### 16.4 Optional process probes

A host may enable a process-liveness provider. Its output:

- never creates transcript history;
- never advances a vendor source cursor;
- never changes a durable terminal state by itself;
- remains in a volatile assessment cache with an expiry rather than the durable source projection;
- must be clearly identified as host-derived rather than agent-native.

This preserves RFC 007's disk-derived architecture while allowing responsive UI hints.

## 17. Projection model

### 17.1 Projection families

The first implementation has the following projection families:

1. **Canonical history** — sessions, messages, content blocks, runs, relations, artifacts, and source provenance.
2. **Search** — FTS and supporting rank/filter projections derived from canonical content.
3. **Runtime evidence** — append/replace-safe evidence records.
4. **Runtime current state** — deterministic materialized state per session/run/team/member.
5. **Usage contributions and totals** — truthful scope-aware token and cost materializations.
6. **Capability-pack tables** — teams, inboxes, tasks, approvals, and other optional features.
7. **Typed read models** — query-oriented columns/tables that avoid repeatedly decoding full vendor payloads for ordinary UI operations.
8. **Namespaced extension storage** — versioned native facts that have not been promoted into a common capability.
9. **Durable change log** — replayable post-commit changes.

### 17.2 Projection ownership

Adapters do not choose tables. The fact type determines which common projector runs. Agent-specific extension facts use one centrally managed extension store unless and until promoted into a shared pack.

Projection code is deterministic, versioned, and owned by the writer lane. When a projection implementation changes, the engine rebuilds or incrementally repairs it from source records or canonical facts without requiring adapter SQL.

A query never invokes `ensure*Projection`, a migration, or a repair routine. The writer maintains a durable projection-readiness catalog. Query behavior for a not-ready projection is explicit:

```text
Ready       -> serve from the projection
StaleSafe   -> serve last complete version and report its watermark/version
Pending     -> optionally wait asynchronously or return ProjectionPending
Unavailable -> return capability/readiness error
```

A read-only fallback query is allowed only when it is deterministic, does not mutate state, and its slower semantics are part of the query contract.

### 17.3 Typed read models and raw fidelity

Raw/native payload retention remains important for audit, future reprojection, and detailed inspection. It is not the default representation for every list/search/timeline request.

Frequently queried identity, ordering, filter, ranking, usage, and runtime fields should live in typed columns or read models so the hot query path does not repeatedly perform:

```text
SQLite text/blob
  -> JavaScript JSON.parse
  -> JavaScript joins/sorts
  -> final result
```

A detailed-record endpoint may still return raw or extension payloads on demand. Read-model design must preserve provenance back to the canonical/native record.

### 17.4 Contribution-based usage updates

The hot path must update token totals in O(changed facts), not O(total session length).

For each usage fact, the store tracks its current normalized contribution. On insert, correction, generation replacement, or deletion:

```text
new contribution - old contribution = aggregate delta
```

This handles:

- duplicate source records: delta zero;
- corrected cumulative counts: replace old contribution;
- generation replacement: subtract old generation, add new;
- snapshot deletion: subtract removed facts;
- exact versus estimated values: update separate buckets.

Accounting rules are explicit:

- `Delta` contributes its values directly once;
- `Cumulative` contributes the non-negative difference from the prior committed counter in the same adapter-declared counter series, with reset/generation semantics for decreases;
- `Snapshot` replaces the previous contribution for the same subject/snapshot owner;
- corrections recompute old-versus-new contribution in one transaction;
- quality buckets remain separate so an estimate never silently becomes exact through aggregation.

Full recomputation remains available for audit, migration, and repair.

### 17.5 Snapshot replacement semantics

Replaceable documents and directory snapshots emit facts with an ownership scope and revision. The common projector can then apply:

```text
upsert all facts in revision N
remove prior facts owned by the same snapshot scope but absent from N
advance snapshot cursor/revision
```

This is required for team membership, inbox read-state, task lists, presence sets, and sidecar metadata.

## 18. Transaction and delivery semantics

### 18.1 Atomic commit

Each ingest batch uses one SQLite transaction conceptually equivalent to:

```sql
BEGIN IMMEDIATE;

INSERT INTO ingest_commits (
    source_instance_id,
    started_at,
    committed_at,
    reason
) VALUES (?, ?, NULL, ?)
RETURNING commit_seq;

-- Apply idempotent canonical facts.
-- Update history/search projections.
-- Insert runtime evidence and reduce current state.
-- Apply usage contribution deltas.
-- Apply capability-pack snapshots.
-- Store complete unknown/malformed records when required.

UPDATE source_objects
SET generation = ?,
    committed_cursor = ?,
    observed_revision = ?,
    decoder_contract_version = ?,
    last_commit_seq = ?
WHERE source_object_id = ?;

INSERT INTO change_log (
    commit_seq,
    ordinal,
    topic,
    schema_version,
    entity_key,
    operation,
    payload
) VALUES (?, ?, ?, ?, ?, ?, ?);

UPDATE ingest_commits
SET committed_at = ?
WHERE commit_seq = ?;

COMMIT;
```

The actual schema and prepared statements may differ, but the atomic boundary may not.

### 18.2 Failure cases

#### Crash before commit

No fact, projection, cursor, or outbox change is visible. The same source range is read again.

#### Crash after commit but before in-memory publication

The rows, cursor, and change log all exist. On restart, the publisher resumes from the durable change log.

#### Client disconnect after receiving but before acknowledging

The client may receive the same change again. It deduplicates by:

```text
(commit_seq, ordinal)
```

#### Retry of an already committed source range

Fact identities and transactional source cursors make the application idempotent. Duplicate delivery is allowed; duplicate projection effect is not.

### 18.3 Guarantee language

Spaghetti guarantees:

> **At-least-once source reading, idempotent fact application, and exactly-once projection effect for a committed source-record identity.**

It does not claim global exactly-once event delivery to disconnected clients, nor a total causal order across independent vendor sources.

### 18.4 Durable subscriptions

Subscription request:

```rust
pub struct SubscribeRequest {
    pub from: Option<ChangeCursor>,
    pub topics: TopicFilter,
    pub entity_filter: Option<EntityFilter>,
}
```

The server behavior is:

```text
from cursor retained:
    replay changes after cursor
    then continue live

cursor older than retained history:
    return ResetRequired(current_commit_seq)
    client reads a current snapshot
    client resubscribes from returned commit_seq
```

A snapshot response and its sequence watermark must be consistent, either through a read transaction or an explicit snapshot token.

### 18.5 Change topics

Initial stable topics include:

```text
history.session.changed
history.message.changed
history.relation.changed

runtime.session.changed
runtime.run.changed
runtime.team.changed
runtime.team_member.changed
runtime.presence.changed
runtime.usage.changed

source.instance.changed
source.stream.state_changed
source.resync_started
source.resync_completed
source.error
```

Changes are projection-level summaries, not raw watcher or raw JSONL events. Multiple record changes may collapse into one entity change inside a commit.

### 18.6 Change-log retention

The change log is bounded by a retention policy based on age and size. Pruning must preserve a minimum resumable window and publish metrics for the oldest retained cursor. Compaction may collapse old intermediate changes only after they are outside the guaranteed replay window.

### 18.7 Query snapshots and commit watermarks

The writer allocates monotonically increasing commit sequences. A query worker starts a read transaction, reads the current committed watermark, executes all statements for the logical request, and returns that watermark with the result.

For a compound request such as timeline rows plus facets plus total count:

```text
BEGIN read transaction
read current committed watermark W
read rows
read facets
read total / cursor boundary
COMMIT read transaction
return at_commit_seq = W
```

A query may observe an older committed snapshot while a newer write is in progress; it may never combine statements from different snapshots without declaring weaker semantics. Subscriptions can use the returned watermark to request changes strictly after the snapshot.

## 19. Storage model

The following tables are conceptual. Existing tables should be migrated rather than duplicated where possible.

### 19.1 Source catalog

```sql
CREATE TABLE source_instances (
    source_instance_id INTEGER PRIMARY KEY,
    adapter_id TEXT NOT NULL,
    stable_key BLOB NOT NULL,
    display_name TEXT NOT NULL,
    adapter_contract_version INTEGER NOT NULL,
    discovered_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    UNIQUE(adapter_id, stable_key)
);

CREATE TABLE source_streams (
    source_stream_id INTEGER PRIMARY KEY,
    source_instance_id INTEGER NOT NULL,
    stream_key TEXT NOT NULL,
    driver_kind TEXT NOT NULL,
    decoder_key TEXT NOT NULL,
    stream_state TEXT NOT NULL,
    last_reconciled_at INTEGER,
    last_commit_seq INTEGER,
    UNIQUE(source_instance_id, stream_key)
);

CREATE TABLE source_objects (
    source_object_id INTEGER PRIMARY KEY,
    source_stream_id INTEGER NOT NULL,
    object_key BLOB NOT NULL,
    display_path TEXT,
    native_identity BLOB,
    generation INTEGER NOT NULL,
    committed_cursor BLOB NOT NULL,
    observed_revision BLOB,
    adapter_object_context BLOB,
    decoder_state BLOB,
    decoder_state_version INTEGER,
    size_bytes INTEGER,
    mtime_ns INTEGER,
    decoder_contract_version INTEGER NOT NULL,
    last_commit_seq INTEGER,
    state TEXT NOT NULL,
    UNIQUE(source_stream_id, object_key)
);
```

`object_key`, `native_identity`, and `committed_cursor` are BLOB-capable because paths and source cursors are not universally UTF-8 strings or integers.

### 19.2 Commit and outbox

```sql
CREATE TABLE ingest_commits (
    commit_seq INTEGER PRIMARY KEY AUTOINCREMENT,
    source_instance_id INTEGER NOT NULL,
    reason TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    committed_at INTEGER,
    fact_count INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE change_log (
    commit_seq INTEGER NOT NULL,
    ordinal INTEGER NOT NULL,
    topic TEXT NOT NULL,
    schema_version INTEGER NOT NULL,
    entity_key BLOB NOT NULL,
    operation TEXT NOT NULL,
    payload BLOB NOT NULL,
    PRIMARY KEY (commit_seq, ordinal)
);
```

### 19.3 Record diagnostics and quarantine

```sql
CREATE TABLE source_record_errors (
    source_object_id INTEGER NOT NULL,
    generation INTEGER NOT NULL,
    cursor_start BLOB NOT NULL,
    cursor_end BLOB NOT NULL,
    payload_hash BLOB NOT NULL,
    media_type TEXT NOT NULL,
    raw_payload BLOB,
    error_class TEXT NOT NULL,
    error_message TEXT NOT NULL,
    adapter_version TEXT NOT NULL,
    contract_version INTEGER NOT NULL,
    first_commit_seq INTEGER NOT NULL,
    last_retry_at INTEGER,
    retry_count INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (
        source_object_id,
        generation,
        cursor_start,
        cursor_end
    )
);
```

Raw payload retention follows policy because transcripts may contain secrets. A hash and bounded diagnostic excerpt may be used when full raw retention is disabled.

### 19.4 Fact provenance

Canonical rows should include or reference:

```text
source_object_id
generation
record cursor/range
fact identity
adapter contract version
first/last commit sequence
quality metadata
```

The current message index remains a presentation/order field. It is not the only uniqueness constraint.

### 19.5 Projection versions

The current source materialization concept evolves into independent version tracking:

```text
adapter contract version
canonical projection version
runtime reducer version
usage reducer version
search projection version
capability-pack projection versions
```

A version mismatch marks only the required scope dirty. It must not force a full global rebuild when a narrower repair is safe.

### 19.6 SQLite operating mode

The engine uses:

- one long-lived read/write connection owned exclusively by the writer actor;
- WAL mode where supported;
- prepared statement caching on every long-lived connection;
- explicit small transactions for interactive ingest;
- larger bounded transactions for backfill;
- a small bounded pool of long-lived read-only/query-only connections;
- bounded busy retry with cancellation awareness;
- controlled checkpointing based on WAL size, write rate, and oldest active reader;
- explicit read transactions for multi-statement logical queries.

Opening a new SQLite connection per live batch or per public query is not the target design. A single `rusqlite::Connection` behind a global mutex is also not the target design: it serializes readers with each other and with the writer, and it obscures ownership.

### 19.7 Connection topology and lifecycle

```text
WriterActor
  -> Connection W (read/write, migrations, all mutations)

QueryWorker 0
  -> Connection R0 (read-only, query_only)

QueryWorker 1
  -> Connection R1 (read-only, query_only)

QueryWorker N
  -> Connection RN (read-only, query_only)
```

Each worker owns its connection on the thread/runtime context where it executes. Requests are routed through bounded queues. The initial worker count is conservative—normally two to four—and is benchmarked against the target corpus and host. More connections are not automatically faster because they increase page-cache pressure and can prolong WAL retention.

Engine shutdown stops new requests, cancels or drains in-flight work according to policy, closes read workers, checkpoints where safe, closes the writer, and then releases the ownership lock.

## 20. Database ownership, query engine, and client API

### 20.1 Rust owns the Spaghetti database, not only the writer

The final ownership boundary is process- and language-level:

```text
Rust SpaghettiEngine
  owns database path, lock, migrations, writer, readers,
  projections, queries, subscriptions, and shutdown

TypeScript
  owns typed client ergonomics and presentation only
```

Production TypeScript must not import `node:sqlite`, `better-sqlite3`, a Spaghetti SQL service, schema constants, or migration helpers. A source adapter reading an agent-owned SQLite database is a separate concern and remains allowed through the bounded read-only source-driver API.

The engine returns structured ownership errors when another engine already owns the database. It never falls back to a second independent TypeScript connection that bypasses the owner.

### 20.2 Writer actor and query pool

The writer actor is the only mutation authority. Query workers are read-only peers coordinated by the same engine lifecycle.

```rust
pub struct SpaghettiEngine {
    observation: ObservationRuntime,
    writer: WriterHandle,
    queries: QueryPoolHandle,
    runtime: RuntimeStateHandle,
    changes: ChangeFeedHandle,
    health: HealthHandle,
}
```

The query pool API is typed rather than connection-oriented:

```rust
pub trait QueryService: Send + Sync {
    async fn list_projects(
        &self,
        request: ListProjectsRequest,
        cancel: CancellationToken,
    ) -> Result<Page<ProjectSummary>, QueryError>;

    async fn timeline(
        &self,
        request: TimelineRequest,
        cancel: CancellationToken,
    ) -> Result<TimelinePage, QueryError>;

    async fn search(
        &self,
        request: SearchRequest,
        cancel: CancellationToken,
    ) -> Result<SearchPage, QueryError>;

    async fn usage(
        &self,
        request: UsageRequest,
        cancel: CancellationToken,
    ) -> Result<UsageReport, QueryError>;

    async fn runtime_snapshot(
        &self,
        request: RuntimeSnapshotRequest,
        cancel: CancellationToken,
    ) -> Result<RuntimeSnapshot, QueryError>;
}
```

The exact async trait mechanism is an implementation choice. The semantic requirement is that callers submit one logical request and receive one consistent result, not a borrowed connection or SQL executor.

### 20.3 No generic SQL API

The following is explicitly rejected as a public surface:

```ts
await client.query('SELECT ... FROM messages WHERE ...', params);
```

It would move execution into Rust while leaving schema ownership, migrations, row interpretation, ranking, and compatibility in TypeScript. It would also prevent the store from changing internal projections without breaking clients.

Public methods are domain-level and versioned:

```text
core history query pack
  listProjects
  listSessions
  getSession
  getMessages
  getTimeline
  search

usage query pack
  getUsage
  getUsageActivity

runtime query pack
  getRuntimeSnapshot
  getRunState

optional capability packs
  delegation / teams / tasks / approvals / artifacts / presence

extension query namespace
  versioned vendor-only projections
```

Internal Rust administration may retain privileged diagnostic SQL tooling, but it is not reachable through the normal SDK or IPC protocol and must not become an application dependency.

### 20.4 Query packs follow fact and capability boundaries

Common facts feed common projections, and common projections feed common query packs. An adapter does not implement `searchClaude`, `listCodexRollouts`, or `getGrokUsage` as separate end-to-end database paths.

```text
Claude / Codex / Grok adapters
             |
             v
     common facts and packs
             |
             v
 common projections + query packs
             |
             v
 one cross-agent client API
```

When an adapter does not support a capability, the corresponding query pack reports unsupported or omits that source according to the contract. It does not fabricate empty-but-supported data.

A native concept with no honest common representation remains a namespaced extension. The adapter emits an extension fact; a central extension projector and versioned extension query expose it. This retains vendor specificity without giving the adapter database authority.

### 20.5 Public TypeScript client

The TypeScript SDK exposes one transport-neutral asynchronous interface:

```ts
export interface SpaghettiClient {
  listProjects(
    request?: ListProjectsRequest,
    options?: QueryOptions,
  ): Promise<Page<ProjectSummary>>;

  listSessions(
    request: ListSessionsRequest,
    options?: QueryOptions,
  ): Promise<Page<SessionSummary>>;

  getTimeline(
    request: TimelineRequest,
    options?: QueryOptions,
  ): Promise<TimelinePage>;

  search(
    request: SearchRequest,
    options?: QueryOptions,
  ): Promise<SearchPage>;

  getUsage(
    request: UsageRequest,
    options?: QueryOptions,
  ): Promise<UsageReport>;

  getRuntimeSnapshot(
    request?: RuntimeSnapshotRequest,
    options?: QueryOptions,
  ): Promise<RuntimeSnapshot>;

  subscribe(
    request: SubscribeRequest,
    options?: SubscribeOptions,
  ): AsyncIterable<CommittedChangeBatch>;

  dispose(): Promise<void>;
}

export interface QueryOptions {
  signal?: AbortSignal;
}
```

Generated or shared wire/domain types keep N-API and IPC transports semantically identical. The TypeScript implementation may add React hooks, caching of immutable result pages, request supersession, display formatting, localization, and UI state. It may not change entity identity, membership, ranking, pagination, token totals, or cross-agent aggregation.

### 20.6 Async API and cancellation

All database-backed public calls are asynchronous in the final API. A synchronous method that merely calls synchronous N-API would still block Electron main or `fieldd` while SQLite and native row conversion execute.

For N-API hosting:

```text
JavaScript call
  -> validate/copy bounded request
  -> enqueue native query task
  -> execute on query worker and its connection
  -> convert final bounded DTO
  -> resolve Promise
```

For IPC hosting:

```text
client request
  -> framed request to engine owner
  -> execute on query worker
  -> framed bounded response or stream
```

Cancellation is cooperative. Before expensive phases and while iterating large result sets, the worker checks the request token and interrupts where supported. Search-as-you-type clients cancel superseded requests instead of allowing stale work to accumulate.

### 20.7 Query purity and projection readiness

A query worker cannot repair a projection. The current pattern where a query invokes `ensureTimelineProjection`, `ensureSessionSubagentProjections`, or similar write helpers must disappear from production query execution.

Projection maintenance paths are:

```text
ingest commit
  -> incremental projector update

engine startup / version mismatch / audit
  -> writer schedules bounded repair

query
  -> reads readiness + completed projection only
```

A response may include:

```rust
pub struct ProjectionStatus {
    pub state: ProjectionReadiness,
    pub projection_version: u32,
    pub at_commit_seq: CommitSeq,
    pub stale_since: Option<CommitSeq>,
}
```

The public contract defines whether a method waits for readiness, returns a last-complete snapshot, uses a documented read-only fallback, or returns `ProjectionPending`. Hidden writes are prohibited.

### 20.8 Snapshot consistency and `at_commit_seq`

All page and snapshot responses carry a committed watermark:

```rust
pub struct Page<T> {
    pub at_commit_seq: CommitSeq,
    pub items: Vec<T>,
    pub next_cursor: Option<PageCursor>,
}
```

Compound queries execute inside one SQLite read transaction. A timeline response cannot combine rows from commit 100 with facets from commit 101 while reporting one total. The watermark allows a client to:

1. render a coherent snapshot;
2. subscribe from `at_commit_seq` for subsequent changes;
3. discard an older response when a newer request has already completed;
4. include the snapshot identity in diagnostics and parity tests.

### 20.9 Pagination

New list and timeline APIs use stable keyset cursors whenever a deterministic order exists:

```sql
WHERE (timestamp, stable_id) < (?, ?)
ORDER BY timestamp DESC, stable_id DESC
LIMIT ?
```

or, for a forward timeline:

```sql
WHERE timeline_index > ?
ORDER BY timeline_index ASC
LIMIT ?
```

Cursor payloads are opaque and versioned. They include the order key and any required source/filter identity. Offset pagination may remain in a compatibility API or bounded search semantics, but it is not the default for deep, live-changing timelines because it becomes slower and can shift under inserts.

### 20.10 Search, ranking, and cross-agent aggregation

Search is one logical Rust query operation. The engine may query several FTS/read projections, normalize score domains, overfetch bounded candidates, merge, rank, and slice natively before crossing N-API/IPC.

The client must not:

```text
call parent search
call subagent search
merge arrays
sort ranks
slice offset
```

Project/session identity aggregation, usage rollups, team/member joins, and runtime summaries follow the same rule. If the operation affects canonical membership, ordering, ranking, identity, or totals, it belongs in Rust.

Search contracts must define:

- tokenization/query syntax;
- score direction and stability expectations;
- source and capability filters;
- snippet generation and escaping;
- tie-breaking;
- pagination behavior;
- whether totals are exact, bounded, or omitted for expensive queries.

### 20.11 Coarse-grained boundary and result encoding

One logical request crosses the native boundary once. The following is prohibited:

```text
TS search
  -> native count
  -> native search parent
  -> native search subagent
  -> native lookup project per row
  -> TS merge/sort
```

The target is:

```text
TS search(request)
  -> one Rust operation
  -> one SearchPage
```

For ordinary pages containing tens or hundreds of items, typed N-API objects or a compact versioned IPC encoding are preferred. Serializing a complete result to JSON in Rust and immediately calling `JSON.parse` in TypeScript is not the default path.

Large exports, diagnostics, or long change replays use bounded streaming, binary chunks, or a generated file. They do not return one unbounded JavaScript array.

### 20.12 Hosting topologies

#### Standalone SDK / Electron

```text
TypeScript SpaghettiClient
  -> NapiTransport
  -> in-process SpaghettiEngine
```

This avoids a process hop and is expected to be the lowest-overhead standalone integration for coarse queries.

#### Vibe Field

```text
TypeScript SpaghettiClient
  -> IpcTransport through fieldd/Electron
  -> field-native-owned SpaghettiEngine
```

This adds IPC serialization but preserves one native infrastructure owner for watchers, ingest, runtime state, database lifecycle, queries, and subscriptions. The architecture optimizes total system ownership rather than a one-row microbenchmark.

#### Standalone daemon

```text
CLI / Electron / scripts
  -> local socket or named pipe
  -> spag-daemon-owned SpaghettiEngine
```

All topologies share requests, results, errors, watermarks, and subscription semantics. A database may not be simultaneously owned by an N-API engine and a `field-native`/daemon engine.

### 20.13 Error model

Public query errors are structured and transport-neutral:

```rust
pub enum QueryError {
    InvalidRequest { field: String, reason: String },
    UnsupportedCapability { capability: CapabilityId },
    ProjectionPending { projection: ProjectionId, retry_after_ms: Option<u64> },
    CursorInvalid { reason: CursorInvalidReason },
    Cancelled,
    DeadlineExceeded,
    EngineStopping,
    DatabaseBusy,
    Internal { diagnostic_id: DiagnosticId },
}
```

Raw SQL, filesystem secrets, and transcript payloads are not included in public error messages. Internal diagnostics retain enough structured context to investigate failures.

### 20.14 Query lifecycle and fairness

Query work uses bounded queues and separate fairness from source ingest. An expensive export cannot consume every query worker while interactive search waits indefinitely. Suggested classes are:

```text
interactive   search, visible timeline, runtime snapshot
normal        lists, usage pages, details
bulk          export, audit, large replay
```

The writer retains priority for short live commits. Long read transactions are observed and can be cancelled because they delay WAL checkpoint progress. Backfill, repair, and query concurrency are tuned together rather than as independent pools.

### 20.15 Query performance expectations

Rust does not make SQLite's C execution intrinsically faster than every direct `node:sqlite` call. Tiny warm metadata lookups may be similar. The expected system gains come from:

- moving synchronous database work off the Node/Electron event loop;
- keeping connections and prepared statements alive;
- avoiding repeated JS row allocation and JSON parsing for canonical views;
- performing joins, rank merging, aggregation, and slicing before the boundary;
- avoiding chatty N-API/IPC call sequences;
- sharing one committed snapshot and one database owner;
- cancelling stale interactive queries.

These are benchmark hypotheses and architectural guarantees, not unmeasured speedup claims.

## 21. Capability model

### 21.1 Why booleans are insufficient

A boolean such as `supportsTokenUsage` hides material differences:

- exact per-message usage;
- cumulative turn usage;
- session-only summary;
- estimated attribution;
- data available only after completion;
- live availability versus backfill only.

Capabilities therefore declare support quality, granularity, and temporal availability.

```rust
pub struct CapabilitySupport {
    pub level: SupportLevel,
    pub granularity: CapabilityGranularity,
    pub availability: Availability,
    pub notes: Option<&'static str>,
}

pub enum SupportLevel {
    Native,
    Derived,
    Estimated,
    Unsupported,
}

pub enum Availability {
    Live,
    EventuallyLive,
    CompletionOnly,
    BackfillOnly,
}
```

### 21.2 Initial capability namespace

```text
history.sessions
history.messages
history.content_blocks
history.timestamps
history.model_identity

context.project_memory

runtime.session_activity
runtime.run_lifecycle
runtime.presence
runtime.subagents
runtime.teams
runtime.team_inbox
runtime.tasks
runtime.approvals
runtime.compaction
runtime.artifacts

usage.input_tokens
usage.output_tokens
usage.cache_tokens
usage.reasoning_tokens
usage.cost

source.live
source.reconcile
source.resume_cursor
```

### 21.3 Capability truth flows through the API

The UI and SDK must not infer support from non-null columns. Query results include quality metadata where relevant, and the adapter manifest is available at runtime. Unsupported values are absent/unknown, not zero.

### 21.4 Capability packs and tests

Declaring a capability pack automatically enables its conformance suite. For example, an adapter declaring `runtime.subagents` must pass:

- parent/child identity;
- child-first and parent-first arrival;
- activity and terminal evidence;
- restart/reconcile;
- unknown event preservation;
- no invented completion;
- cold/live convergence.

### 21.5 Capability query packs

A declared capability controls both projection and query availability. The adapter declares evidence quality and granularity; the common capability pack owns tables, reducers, public request/result types, and query implementation. Clients discover support through the manifest rather than inferring it from non-null rows or vendor IDs.

## 22. Adapter registry and configuration

### 22.1 Registry

The host constructs the registry from compiled adapters:

```rust
let registry = AdapterRegistry::builder()
    .register(ClaudeCodeAdapter::new())?
    .register(CodexAdapter::new())?
    .register(GrokAdapter::new())?
    .build()?;
```

The common engine uses only the `AgentAdapter` trait. Duplicate IDs or incompatible contract versions fail at startup.

### 22.2 Configuration

Configuration may override:

- source roots;
- enabled streams or capability packs;
- ignore rules;
- polling fallback;
- raw-record retention;
- backfill range;
- privacy exclusions;
- adapter-specific read-only options.

Adapter-specific configuration is namespaced and schema-validated. The core owns global scheduling, storage, and safety settings.

### 22.3 Discovery

Discovery is explicit and inspectable. An adapter reports:

- candidate source instances;
- why each candidate was selected;
- version/schema evidence;
- inaccessible roots or permission errors;
- conflicting duplicate instances.

The engine does not silently merge source instances merely because paths overlap.

## 23. Agent adaptation plan

The adaptation plans below end at fact and capability emission. Once an adapter satisfies a common pack, its data becomes available through the corresponding common Rust query pack automatically. No plan includes TypeScript SQL or an adapter-specific public database service. Vendor-only details use namespaced extension facts and centrally versioned extension queries.

### 23.1 Claude Code adapter

Claude is the first reference adapter because it exercises the broadest set of required source semantics.

#### Declared streams

At minimum:

| Stream | Driver | Semantic output |
|---|---|---|
| Parent session transcripts | `AppendDelimitedFile` | sessions, messages, content blocks, usage, run evidence |
| Subagent transcripts | `AppendDelimitedFile` | child runs, messages, activity, terminal evidence, parent links |
| Team configuration | `ReplaceDocument` or `DirectorySnapshot` | teams, membership snapshots, member metadata |
| Team inboxes | `ReplaceDocument` per inbox or directory snapshot | inbox messages, read state, sender/recipient relations |
| Active-session files | `PresenceObject` | presence evidence and source availability |
| Tasks/todos/plans | `ReplaceDocument` / `DirectorySnapshot` | capability-pack snapshots |
| Artifacts/file history | appropriate snapshot driver | artifact relations and history metadata |
| Settings relevant to interpretation | `ReplaceDocument` | adapter configuration/version evidence, not general user settings export |

#### Native responsibilities

The Claude adapter owns:

- path/layout classification;
- native JSON record variants;
- native session, agent, subagent, and team identifiers;
- correlation between parent spawn/tool-result records and child transcript objects;
- team config and inbox semantics;
- mapping explicit terminal events to common evidence;
- native token field interpretation;
- schema-version detection and unknown record preservation.

#### Common responsibilities

The engine owns:

- tail offsets and partial lines for parent and subagent JSONL;
- rewrite/generation handling;
- team snapshot replacement;
- cursor transactions;
- runtime state reduction;
- usage contribution totals;
- late relation repair scheduling;
- replayable changes.

Parent and subagent JSONL must use the same append driver. Whole-file reparsing of a growing subagent transcript is not an accepted steady-state path.

#### Runtime-state policy

Recent append activity supports `Active` evidence. Explicit completion/failure/cancellation records support terminal evidence. A quiet file supports only a transient `stale` or `possibly_waiting` assessment; it does not prove completion.

### 23.2 Codex adapter

Codex uses its own rollout format and metadata flow; it should not be forced through Claude path assumptions.

#### Declared streams

Likely streams include:

| Stream | Driver | Semantic output |
|---|---|---|
| Rollout/session logs | `AppendDelimitedFile` | session metadata, messages, runs, lifecycle evidence |
| Token-count records | same append stream, adapter decode route | scoped usage facts with native accounting semantics |
| Child/internal rollouts when supported | append or snapshot as discovered | delegation relations and child runtime facts |
| Source metadata snapshots | `ReplaceDocument` where needed | session/model/project metadata |

#### Migration from hooks

Current source-specific token hooks and in-memory attribution state are replaced by adapter-emitted `UsageFact`s. The adapter records whether a native token record is delta, cumulative, or snapshot and which subject/scope it can truthfully identify. The common usage reducer owns deduplication and totals.

Metadata found inside initial rollout records is emitted as ordinary facts from the same decoder. Metadata fingerprinting and source cursor advancement commit atomically rather than in separate operations.

#### Capability policy

Internal or child rollouts are exposed only when their identity and relationship are stable enough to pass the delegation conformance pack. Otherwise they remain namespaced extension facts rather than invented subagents.

### 23.3 Grok adapter

Grok demonstrates why one logical session may depend on an append stream plus replaceable sidecars.

#### Declared streams

| Stream | Driver | Semantic output |
|---|---|---|
| `chat_history.jsonl` | `AppendDelimitedFile` | messages and base session facts |
| `summary.json` | `ReplaceDocument` | summary/session metadata |
| `events.jsonl` or equivalent | append/snapshot according to native behavior | timestamps, lifecycle, extension facts |
| `signals.json` or equivalent | `ReplaceDocument` | usage/session sidecar observations |
| Session directory membership | `DirectorySnapshot` | session creation/removal and reconcile triggers |

#### Entity reconciliation

The adapter declares these streams as dependencies of one session entity. When a sidecar changes before or after chat ingestion, the engine schedules an entity reconciliation. The adapter reads the required source-owned objects through `SourceAccess` and emits a replacement fact set. The resulting canonical, timestamp, and usage changes commit with the triggering cursor.

Sidecar writes must not be separate best-effort SQL updates after transcript commit.

#### Usage policy

When Grok exposes only session-level usage or when message attribution is heuristic, the adapter emits session-scoped usage with `Estimated` or the appropriate native quality. The canonical store must not manufacture exact per-message attribution.

### 23.4 Future file-backed adapters

A conventional file-backed adapter should normally require only:

1. source discovery;
2. `StreamSpec` declarations using common drivers;
3. native decoders;
4. capability manifest;
5. fixtures and pack tests.

It should not implement a watcher, checkpoint file, SQLite writer, event bus, or retry scheduler.

### 23.5 Future SQLite-backed adapters

An agent such as OpenCode may require a source-owned SQLite reader. Its adapter:

- declares a read-only `SqliteSnapshot` stream;
- provides named source queries and row decoders;
- chooses a trustworthy watermark when available;
- otherwise uses snapshot diff;
- emits the same common facts as file-backed adapters;
- never shares the source database connection with Spaghetti's writer connection.

### 23.6 Future key-value-backed adapters

An agent stored in a VS Code-style state database may use `KeyValueSnapshot`:

- declare exact keys or prefixes;
- parse values in the adapter;
- emit facts and capability quality;
- use revision/fingerprint reconciliation when no change log exists.

This is why `AgentAdapter` cannot be restricted to path classification or JSONL parsing.

## 24. Standard adapter-development workflow

Adding an adapter is a staged evidence exercise, not a path to a quick `match` arm.

### Stage A: source survey

Document:

- installation/profile discovery;
- source roots and ownership;
- source schema/version markers;
- record families;
- append, replace, snapshot, database, and key-value semantics;
- native IDs and relation keys;
- timestamp clocks and precision;
- usage accounting behavior;
- rewrite, rotation, cleanup, and retention behavior;
- privacy-sensitive fields;
- unsupported or ambiguous semantics.

The survey produces a source map checked into the adapter's test fixtures/documentation.

### Stage B: capability manifest

Declare support before implementing projections. Every capability must specify:

- support level;
- granularity;
- availability;
- source/evidence basis;
- known ambiguity.

This prevents implementation pressure from turning an estimate into an exact-looking API field.

### Stage C: stream design

Choose common drivers and define object identity, generations, and cursors. A custom producer requires an explicit explanation of why existing drivers cannot represent the source.

### Stage D: native decoder and golden fixtures

Build anonymized fixtures for every known native record family, including unknown/forward-compatible records. Golden tests verify:

```text
native bytes/rows
    -> facts + provenance + diagnostics
```

Fixtures must include schema-version changes and malformed-but-complete records.

### Stage E: projection, capability-pack, and query-pack tests

Run universal history/runtime/usage tests plus every declared capability pack. Verify that emitted facts satisfy the corresponding common query pack without adapter-specific SQL or result assembly. The adapter should not need private projection/query code unless the feature remains a namespaced extension, in which case the extension contract is still centrally versioned.

### Stage F: cold/live convergence

Run identical event traces through:

1. fresh backfill;
2. incremental live ingestion;
3. live ingestion with dropped/duplicated hints;
4. restart from committed cursors;
5. forced full reconcile.

The canonical and materialized results must converge.

### Stage G: shadow mode

Before replacing an existing adapter path, run the new Rust adapter beside the legacy implementation on fixtures and selected real corpora. It writes to an isolated database or audit projection and reports deterministic differences. Shadow mode must never create two writers for the production database.

### Stage H: release and contract versioning

The adapter ships only after:

- core and declared capability packs pass;
- unknown-record behavior is tested;
- performance and memory budgets pass;
- migration/repair behavior is tested;
- the manifest contract version is set;
- source-format drift diagnostics are exposed.

## 25. Conformance framework

### 25.1 Command

The repository will provide one vendor-neutral command, for example:

```bash
cargo xtask adapter-check --adapter claude-code
cargo xtask adapter-check --adapter codex
cargo xtask adapter-check --adapter grok
cargo xtask adapter-check --all
```

The exact command name may change, but one executable report is required. Broad package-test success is not a substitute for an explicit adapter result.

### 25.2 Mandatory core pack

Every adapter must pass:

- stable adapter/source identity;
- deterministic discovery;
- source-record provenance;
- duplicate record idempotency;
- unknown-record retention;
- malformed complete record quarantine;
- partial record retry without cursor advance;
- cold/live convergence;
- restart from durable cursor;
- source deletion and replacement;
- queue/watcher loss followed by reconcile;
- commit-before-publish;
- replay after publisher restart;
- cancellation and disposal;
- no post-disposal events;
- no orphaned watchers, tasks, file handles, database handles, or sockets.

### 25.3 Driver packs

Adapters run the packs for every driver they declare:

#### Append pack

- one record per write;
- many records per write;
- split record across writes;
- CRLF where allowed;
- duplicate dirty hints;
- truncate;
- atomic replace;
- same-size incompatible rewrite;
- active-file retry without a second native event;
- large burst;
- file rotation/removal.

#### Replace-document pack

- in-place write;
- temp-file rename;
- transient invalid JSON during write;
- confirmed malformed revision;
- unchanged-content notification;
- deletion and reappearance;
- snapshot replacement of absent facts.

#### Directory-snapshot pack

- add/remove/rename child;
- dropped notification;
- nested ignored path;
- symlink escape attempt;
- permission failure;
- stable rescan result.

#### Source-database pack

- source busy/locked;
- read-only enforcement;
- watermark continuation;
- watermark invalidation;
- snapshot diff;
- source schema change;
- source database replacement.

### 25.4 Capability packs

Capability-dependent packs include:

- usage;
- delegation/subagents;
- teams/inboxes;
- tasks/plans;
- approvals;
- presence;
- compaction;
- artifacts.

The engine never invents scenarios for an unsupported capability. A capability cannot be declared without its pack.

### 25.5 Differential oracle

For any supported source trace:

```text
live incremental projection
    == fresh full rebuild projection
    == projection after forced reconcile
    == projection hydrated after process restart
```

Equality excludes expected operational metadata such as commit sequence and observation time, but includes all semantic rows, quality classifications, relations, and totals.

### 25.6 Query conformance pack

Every public query implementation passes a transport-independent suite covering:

- query execution never changes database contents or projection versions;
- N-API and IPC transports return semantically identical normalized results;
- result pages include a valid committed watermark;
- compound rows/facets/counts come from one snapshot;
- keyset pagination has no duplicate or missing row over a fixed snapshot;
- cancellation stops or suppresses superseded work;
- unsupported capability behavior is explicit;
- malformed/expired cursors return structured errors;
- query execution remains correct while live ingest commits concurrently;
- a cold hydrated engine and a continuously running engine return equivalent results;
- extension queries cannot read another adapter namespace without an approved contract;
- query calls do not invoke adapter code or source access.

During migration, the existing TypeScript query implementation serves as a differential oracle. It moves to test-only status after Rust parity and is deleted once accepted differences and the final API contract are recorded.

## 26. Process and crate architecture

### 26.1 Logical components

The target logical layout is:

```text
crates/
  spaghetti-model/              facts, capabilities, IDs, query DTOs
  spaghetti-engine/             lifecycle, observation, writer coordination
  spaghetti-store-sqlite/       schema, migrations, writer, query connections
  spaghetti-query/              query packs, pagination, ranking, snapshots
  spaghetti-source/             common file/dir/source-DB/KV drivers
  spaghetti-adapter-claude/     Claude discovery and decoding
  spaghetti-adapter-codex/      Codex discovery and decoding
  spaghetti-adapter-grok/       Grok discovery and decoding
  spaghetti-napi/               thin asynchronous Node transport
  spaghetti-daemon/             optional local IPC host
  spaghetti-cli/                optional direct Rust client/host

packages/
  sdk/                          typed SpaghettiClient + NAPI/IPC transports
  react/                        hooks and presentation state, if retained/split
```

This is a dependency target, not a requirement to split every crate immediately. Implementation may begin as modules inside the existing Rust crate, but module boundaries and dependency tests must match the target. Crates split when the seams are stable enough to improve compile isolation and ownership.

### 26.2 Persistent engine object

```rust
pub struct SpaghettiEngine {
    owner_lock: DatabaseOwnerLock,
    observation: ObservationRuntime,
    writer: WriterHandle,
    queries: QueryPoolHandle,
    runtime: RuntimeStateHandle,
    changes: ChangeFeedHandle,
    health: HealthHandle,
}
```

The object is long-lived. It retains:

- one writer connection and prepared statements;
- a bounded set of read-only query connections and prepared statements;
- source catalog and cursor cache;
- active-object scheduling state;
- watcher registrations;
- bounded read/decode/query pools;
- reducer caches that can be rehydrated from SQLite;
- projection readiness state;
- subscriber watermarks;
- cancellation and shutdown coordination.

Opening the engine performs ownership acquisition, schema/version checks, migrations, connection creation, and recovery planning before public readiness is reported.

### 26.3 Thin asynchronous N-API host

The Node host exposes a persistent class/handle rather than a collection of one-shot database functions:

```text
SpaghettiEngine.open(config) -> Promise<handle>
handle.start()               -> Promise<void>
handle.status()              -> Promise<EngineStatus>
handle.search(request)       -> Promise<SearchPage>
handle.timeline(request)     -> Promise<TimelinePage>
handle.usage(request)        -> Promise<UsageReport>
handle.runtimeSnapshot(req)  -> Promise<RuntimeSnapshot>
handle.subscribe(cursor, filters) -> AsyncIterable<CommittedChangeBatch>
handle.reconcile(scope)      -> Promise<ReconcileResult>
handle.dispose()             -> Promise<void>
```

It must not:

- parse source JSON;
- own source checkpoints;
- open a TypeScript SQLite connection;
- maintain token attribution state;
- call one native function per filesystem event, SQL statement, or result row;
- issue source-specific write batches;
- synthesize process-local sequence numbers;
- execute long database work synchronously on the Node thread.

High-frequency changes cross N-API in committed batches. Query requests cross once per logical operation. `AbortSignal` or equivalent cancellation is mapped to the native request token.

### 26.4 TypeScript client transports

The SDK owns a semantic client facade with interchangeable transports:

```text
SpaghettiClient
  |- NapiTransport
  `- IpcTransport
```

Transport code handles request IDs, version negotiation, cancellation, bounded serialization, and error mapping. Domain behavior is not duplicated between transports. React hooks and CLI/TUI commands depend on `SpaghettiClient`, not directly on N-API or a database service.

### 26.5 Embedded and daemon hosting

The engine is library-first so both are possible:

#### Embedded standalone

A Node/Electron process owns an in-process engine through N-API. This is the default standalone SDK topology when no other native owner exists.

#### Embedded in Vibe Field

`field-native` owns the engine directly. `fieldd` and Electron use the IPC transport. This minimizes duplicate native infrastructure and allows one owner for watcher, observation, database, queries, and subscriptions.

#### Standalone daemon

A future `spag-daemon` owns the engine and exposes Unix-socket/named-pipe IPC. CLI, Electron, and scripts share one database authority.

The decision to make the daemon mandatory is deferred. All modes enforce one owner per database/source set.

### 26.6 Ownership lock and host selection

Startup acquires a cross-process lock keyed by database identity. Failure returns a structured error containing owner metadata and a connectable endpoint when available.

Host selection policy is explicit:

```text
field-native owner configured/reachable
  -> use IPC client

standalone daemon configured/reachable
  -> use IPC client

no external owner
  -> open local N-API engine
```

The SDK never silently opens an N-API engine after discovering an active external owner. A database is not simultaneously owned by `field-native`, `spag-daemon`, and an in-process Node addon.

### 26.7 Shutdown ordering

Shutdown proceeds in this order:

1. reject new client requests;
2. stop source discovery and watcher scheduling;
3. cancel or drain query/export work according to request class;
4. drain bounded ingest commits or record dirty recovery state;
5. flush durable changes already committed;
6. close query workers;
7. checkpoint when safe and close the writer;
8. release source handles and ownership lock.

Disposal tests verify no orphaned watcher, task, database handle, native callback, IPC socket, or Node reference remains.

## 27. Performance architecture

### 27.1 Optimize work elimination first

The desired ingest hot path is:

```text
one source read
  -> one frame operation
  -> one native JSON/row decode
  -> one FactBatch
  -> one transaction updating many projections
  -> one committed change batch
```

The desired query hot path is:

```text
one typed request
  -> one query worker and snapshot
  -> native filtering/joining/ranking/pagination
  -> one bounded DTO/page
  -> one N-API/IPC response
```

The design explicitly avoids:

- parsing once for history and again for runtime state;
- full-file reparse on normal append;
- per-record or per-query database connection setup;
- per-event, per-SQL-statement, or per-row N-API calls;
- recalculating an entire session's token total for each message;
- repeated JavaScript `JSON.parse` for ordinary canonical rows;
- JavaScript merging/sorting of canonical cross-agent search results;
- multiple overlapping watchers for the same physical root;
- unbounded Tokio tasks, query jobs, or channels.

### 27.2 Watch-root consolidation

The supervisor consolidates overlapping physical roots where semantics and permissions allow, then routes dirty paths to logical streams. Multiple consumers subscribe to one engine rather than each creating a watcher.

### 27.3 Bounded concurrency and fairness

Separate resource budgets are maintained for:

- source I/O;
- JSON/native decoding;
- agent-owned database reads;
- SQLite commits;
- interactive queries;
- bulk queries/exports;
- backfill and repair;
- live interactive streams.

A flood in one adapter cannot allocate unbounded memory or starve all active sessions. Queue saturation escalates to dirty/reconcile for ingest and structured overload/cancellation for queries.

The writer is one lane because SQLite serializes writes. Reads begin with two to four workers, each owning a read-only connection. Worker count is tuned against CPU, page-cache duplication, WAL reader age, and target query concurrency.

### 27.4 Micro-batching

Interactive ingest batches are bounded by both time and size. Backfill uses larger batches. The writer may combine facts from independent source objects in one transaction only if it preserves each object's cursor atomicity and failure reporting.

Committed change publication is batched by transaction. Query requests are not artificially combined unless a specific API defines a batch request; request coalescing must not increase interactive tail latency unpredictably.

### 27.5 Compact source and query state

The engine stores one compact state entry per active/discovered source object, not one task or full path copy per record. Paths may use interned segments or stable IDs where profiling justifies it.

Query cursors are compact opaque tokens containing only versioned ordering/filter state. The engine does not retain server-side state for every ordinary page cursor unless a query explicitly uses a leased snapshot.

### 27.6 Lazy content and payload decoding

Full-content hashing is not performed on every file event. It is used only for ambiguous rewrite, snapshot revision, deduplication, audit, or a source without reliable metadata.

Likewise, full native/raw JSON is not decoded for every list item if typed read-model columns satisfy the request. Detailed payload decoding is lazy and endpoint-specific.

### 27.7 Token aggregation complexity

Interactive token updates are proportional to changed usage facts. Full session/day/project rebuilds are repair operations, not the default append path.

### 27.8 SQLite tuning

Performance tuning remains subordinate to correctness. Expected mechanisms include:

- WAL;
- long-lived writer and read connections;
- prepared-statement caches per connection;
- page-cache sizing informed by total connection count;
- bounded transaction sizes;
- explicit checkpoint policy;
- indexes aligned with fact identity and query order;
- typed read models for hot paths;
- keyset pagination;
- avoiding a global connection mutex;
- cancellation/interrupt support for long queries;
- monitoring oldest reader to avoid unbounded WAL growth.

### 27.9 Native boundary and serialization

Boundary cost is controlled by reducing crossings before selecting a complex encoding. For ordinary pages, use typed N-API values or a compact versioned IPC format. Measure:

- encoded bytes;
- Rust allocations;
- JavaScript heap growth;
- conversion time;
- event-loop delay;
- number of native/IPC calls per logical request.

Binary streaming is introduced only for result classes that exceed bounded page semantics, such as export and long replay.

### 27.10 Performance targets

Reference benchmarks define exact hardware and corpora. Initial product gates are:

- ordinary active-file append to durable commit: **p50 <= 25 ms, p99 <= 100 ms**, excluding delay before the agent flushes to disk;
- no full-file reparse for a valid append continuation;
- no steady-state growth in memory with transcript line count after committed content is released;
- bounded memory under burst and forced queue overflow;
- idle watcher/engine CPU near platform baseline;
- backfill throughput no worse than the accepted current native bulk path on the same corpus;
- no SQLite work on the Node/Electron event-loop thread;
- one native/IPC request per ordinary logical query;
- Rust query results semantically match the accepted oracle before cutover;
- live ingest does not materially degrade interactive query p95/p99 for concurrent readers;
- cancelled search-as-you-type requests do not build an unbounded stale-work queue;
- source notification loss affects latency, not final correctness.

Exact search/timeline latency gates are established by the benchmark harness rather than invented in this RFC.

### 27.11 Benchmark matrix

The benchmark suite compares:

```text
A. current TypeScript node:sqlite query path
B. persistent in-process Rust N-API engine
C. field-native/daemon Rust engine over IPC
```

Workloads include:

- one warm metadata lookup;
- project and session list aggregation;
- FTS top 50 across parent and subagent corpora;
- timeline page plus facets and total;
- usage report over large histories;
- ten concurrent readers during live ingest;
- search-as-you-type cancellation;
- deep pagination versus keyset cursor;
- large detail payload and export;
- long-running reader and WAL checkpoint behavior.

Metrics include end-to-end p50/p95/p99, SQLite time, queue time, conversion time, event-loop delay, JS heap allocation, Rust allocation, response bytes, writer commit latency, WAL size, and RSS.

### 27.12 Initial implementation choices

The first implementation should prefer mature components and replace them only after profiling isolates a limitation:

| Concern | Initial choice | Policy |
|---|---|---|
| Filesystem notifications | `notify` | Events are dirty hints; direct per-OS backends only after measured need |
| Reconcile traversal | `ignore` parallel walker | Apply source selectors and ignore rules during traversal |
| Native callback ingress | bounded channel | Queue full marks dirty; never silently trust a dropped stream |
| SQLite | `rusqlite` long-lived connections | One writer, small read pool, WAL, prepared statements |
| Query scheduling | bounded Rust worker pool | Connection per worker; interactive/bulk fairness |
| N-API | asynchronous task/Promise surface | No synchronous database work on Node thread |
| IPC | framed versioned request/response | Same domain DTOs and errors as N-API |
| Pagination | keyset cursor by default | Offset retained only for bounded compatibility cases |
| Content hashing | `blake3` on demand | Never hash every append by default |
| Metrics | `tracing` plus counters/histograms | Include queue, query, boundary, WAL, and convergence metrics |

## 28. Observability and operations

### 28.1 Metrics

At minimum, expose per engine, adapter, instance, and stream:

```text
watcher_hints_total
watcher_overflows_total
internal_queue_overflows_total
dirty_objects_current
reconciles_total
rescans_total
source_records_total
source_bytes_read_total
decode_errors_total
unknown_records_total
quarantined_records_total
facts_emitted_total
facts_per_record
commit_batches_total
commit_facts_histogram
commit_latency_ms
source_to_commit_latency_ms
change_log_oldest_seq
change_log_latest_seq
subscriber_lag
active_source_objects
writer_busy_ms
source_db_busy_ms
query_queue_depth_by_class
query_execution_latency_ms
query_total_latency_ms
query_cancelled_total
query_superseded_total
query_worker_utilization
oldest_reader_age_ms
projection_pending_total
napi_requests_total
ipc_requests_total
result_conversion_latency_ms
response_bytes
wal_size_bytes
checkpoint_blocked_by_reader_ms
```

Host integrations and benchmark builds also report Node/Electron event-loop delay so a fast native query cannot mask a blocking transport/conversion path.

### 28.2 Stream state

Each stream reports an explicit state:

```text
Discovering
Scanning
CatchingUp
Live
Dirty
Reconciling
DegradedPolling
Blocked
Stopped
```

`Live` means the initial scan/reconcile boundary is complete and no known loss condition remains. It does not mean the source agent itself is active.

### 28.3 Diagnostics

Errors include:

- adapter ID and version;
- source instance and stream;
- source object display path or safe identifier;
- generation and cursor range;
- retry classification;
- whether cursor advanced;
- whether a reconcile was scheduled;
- whether raw payload was retained;
- privacy-safe excerpts only.

### 28.4 Health API

The host exposes a health snapshot that distinguishes:

- engine/writer health;
- query-pool availability, saturation, and oldest reader;
- source availability;
- watcher degradation;
- projection readiness and repair lag;
- subscriber lag;
- WAL/checkpoint pressure;
- adapter-format drift;
- quarantined records;
- current replay window.

## 29. Security and privacy

### 29.1 Local-only default

The observation engine performs no network transmission. Adapters read only configured local sources. Any future remote source or telemetry requires a separate explicit design and opt-in.

### 29.2 Least privilege

- source-owned databases are opened read-only;
- source roots are path-confined;
- symlink escape is denied by default;
- Spaghetti database and IPC endpoints use restrictive local permissions;
- adapters receive scoped read capabilities rather than arbitrary filesystem handles;
- custom producers are compiled/trusted in v1.

### 29.3 Sensitive payloads

Transcripts, prompts, tool results, inboxes, and artifacts may contain credentials or private code. Raw-record retention is configurable by stream:

```text
None
HashOnly
DiagnosticExcerpt
Full
```

Canonical history requirements may still require message content storage. The distinction controls duplicate raw/native copies and quarantine payloads.

### 29.4 Log hygiene

Raw content is never included in ordinary logs. Diagnostics use stable record IDs, lengths, hashes, and bounded redacted excerpts. Adapter tests include secret-like fixtures to prevent accidental logging.

### 29.5 Database and query confinement

Public clients receive no raw SQL executor, database path write handle, or migration primitive. Read-only query workers use query-only mode where supported. Source-database access is confined to adapter-declared read-only roots and named queries. Diagnostic errors redact SQL parameters and native payloads by default.

## 30. Migration plan

Migration is staged so every phase has one owner, a differential oracle, and a rollback boundary. New TypeScript source or database architecture is frozen except for critical fixes and parity instrumentation.

### Phase 0 — RFC, baseline, and freeze

- Adopt RFC 011 and record that it supersedes RFC 009's live-writer destination.
- Record RFC 010 `node:sqlite` as a transitional driver migration, not the final database boundary.
- Inventory every production TypeScript SQL caller, migration, projection repair, aggregation, and sync public API.
- Capture current cold/live/query outputs and real-corpus benchmark baselines.
- Add architecture checks preventing new source-specific watcher/writer/query services.

**Exit:** ownership map, fixture corpus, differential oracle, and benchmark commands are committed.

### Phase 1 — Persistent `SpaghettiEngine` shell

- Extract library-first engine lifecycle from the existing N-API crate.
- Add database owner lock and structured owner metadata.
- Open one long-lived writer connection and an initial read-only query pool.
- Add cancellation, shutdown coordination, status, and health handles.
- Keep current TypeScript ingest/query paths operational behind an explicit legacy mode.

**Exit:** the persistent engine opens, reports health, executes a trivial typed async query, and disposes without leaked handles.

### Phase 2 — Transactional source catalog and durable outbox

- Add source instances, streams, objects, generations, cursors, decoder state, ingest commits, projection versions, and change log.
- Make source cursor, facts, projections, usage, and outbox atomic.
- Add crash injection around every transaction stage.
- Existing TypeScript watcher may temporarily submit dirty paths, but cannot pre-advance durable cursors.

**Exit:** commit-before-publish and restart replay are proven.

### Phase 3 — Common append and snapshot drivers

- Implement `AppendDelimitedFile`, `ReplaceDocument`, `DirectorySnapshot`, and `PresenceObject`.
- Implement watch-before-scan, bounded scheduling, overflow recovery, generation handling, partial-line retry, and quarantine.
- Add driver conformance packs and adaptive polling fallback.

**Exit:** synthetic adapters pass all driver and recovery tests.

### Phase 4 — Claude history and usage in Rust

- Port Claude discovery and transcript decoding into the adapter contract.
- Use the shared append driver for parent and subagent JSONL.
- Emit canonical history, relations, evidence, and usage facts.
- Replace hot-path token total rebuilds with contribution deltas while retaining audit rebuild.
- Run shadow parity against current cold and live implementations.

**Exit:** Claude history/usage cold-live-reconcile parity is zero on fixtures and accepted real corpora.

### Phase 5 — Claude runtime capability packs

- Add delegation/subagent projections.
- Add teams/config/inbox snapshots.
- Add active-session presence.
- Add tasks/plans/artifacts as capability packs where semantics are sufficiently defined.
- Add workflow summaries and append journals without inferring child terminal state.
- Add replaceable session-index metadata without fabricating transcript history.
- Add independently replaceable project-memory documents without inventing runtime evidence.
- Add late-correlation and conflict tests.
- Switch Claude live observation to the Rust engine.

**Exit:** Claude runtime state survives watcher loss and process restart and does not invent completion.

### Phase 6 — Codex adapter migration

- Port rollout discovery, framing, metadata, messages, and lifecycle decoding.
- Replace `SessionTokenApi` and source-specific SQL hooks with usage facts.
- Make metadata/cursor/projection atomic.
- Add child rollout/delegation only for semantics that pass the pack.
- Shadow, cut over, then remove Codex TypeScript live ingest.

**Exit:** Codex passes core, append, usage, and every declared capability pack.

### Phase 7 — Grok adapter migration

- Port chat-history append decoding.
- Model summary/events/signals as declared snapshot/dependency streams.
- Implement entity reconciliation for sidecar-first and transcript-first arrival.
- Store usage at truthful scope and quality.
- Shadow, cut over, then remove Grok TypeScript live ingest.

**Exit:** Grok cold/live/sidecar/reconcile parity is proven without separate post-commit sidecar SQL.

### Phase 8 — Rust becomes sole ingest/observation writer

Delete or reduce to non-production parity tools:

- TypeScript source watchers;
- TypeScript incremental parsers;
- JSON checkpoint sidecars;
- source-specific `writeBatch` logic;
- token mutation hooks;
- process-local live sequence bus;
- old N-API live-batch row serializer;
- duplicated source classifiers no longer needed by presentation.

The TypeScript query path may still operate temporarily against the same database only in the explicitly controlled migration configuration. It cannot write projections and cannot coexist with an external engine owner that forbids independent connections.

**Exit:** no TypeScript code reads agent-owned source content for ingest or writes ingest projections.

### Phase 9 — Rust canonical query parity

Port typed Rust queries in this order:

1. FTS search and rank merging;
2. timeline pages, branch joins, and facets;
3. subagent/workflow queries;
4. project/session aggregation and summaries;
5. usage activity and totals;
6. runtime snapshots and teams;
7. details, stats, and simple lookups.

For each query:

- define versioned request/result types;
- remove query-triggered projection repair by moving it to the writer;
- add `at_commit_seq` and stable cursor semantics;
- compare normalized Rust output against the TypeScript oracle;
- benchmark N-API and IPC topology;
- classify every accepted difference.

**Exit:** all production query capabilities exist in Rust, query conformance passes, and no Rust query mutates the database.

### Phase 10 — Async client cutover and TypeScript SQLite retirement

- Introduce the asynchronous `SpaghettiClient` and N-API/IPC transports.
- Migrate CLI, TUI, playground, Electron, and SDK consumers.
- Add request cancellation and stale-result suppression.
- Deprecate then remove/convert synchronous database-backed APIs in the appropriate major release.
- Move the TypeScript query implementation to test-only parity use, then delete it.
- Delete production `SqliteService`, TypeScript schema/migrations, projection repair, SQL strings, and database row assembly.
- Add a CI architecture gate that rejects production imports of `node:sqlite`, `better-sqlite3`, or Spaghetti schema internals.

**Exit:** production TypeScript cannot open or query the Spaghetti database; every canonical database operation goes through `SpaghettiClient` to Rust.

### Phase 11 — Optional host consolidation

Evaluate:

- embedding in `field-native` as Vibe Field's sole owner;
- a standalone `spag-daemon`;
- multi-client IPC and endpoint discovery;
- whether standalone N-API ownership remains the default SDK topology.

This phase must not alter adapter, database, query, transaction, or client semantics.

**Exit:** any selected host passes the same engine, query, lifecycle, and transport conformance suites.

## 31. Migration compatibility

### 31.1 Database migration

Existing canonical history tables should be preserved where practical. Add provenance, source-object mappings, projection readiness, commit watermark support, and typed read models through nullable/backfilled columns or companion tables, then make them required for new Rust commits.

Migrations are executed only by the Rust writer owner. During the dual-query parity window, the TypeScript oracle is read-only and must understand the migration version it is validating. It is not allowed to run its own migration sequence.

### 31.2 Public API migration

The current SDK is predominantly synchronous. The target is asynchronous. Compatibility is explicit:

1. introduce `SpaghettiClient` as a new async surface;
2. provide adapters/wrappers for CLI/TUI call sites;
3. deprecate synchronous database-backed methods;
4. convert or remove them in a documented major release;
5. keep purely in-memory getters synchronous only where they remain genuinely local and non-blocking.

The final implementation does not simulate synchronous APIs with blocking N-API calls or nested event loops.

### 31.3 Transport compatibility

N-API and IPC share protocol/domain versions, request/result semantics, errors, cursor encoding, and commit watermarks. Clients negotiate a supported contract version at open/connect time. A transport may optimize encoding but cannot change result meaning.

### 31.4 Checkpoint migration

Existing JSON checkpoint sidecars are imported once into the Rust source catalog only when their file identity, size, and prefix validation prove the offset safe. Otherwise the engine replays from zero or a verified boundary. Sidecars become read-only during the transition and are deleted after cutover.

### 31.5 Query differential oracle

The TypeScript query implementation remains temporarily available in test/shadow builds. It reads an isolated copy or a controlled read-only connection, normalizes operational metadata, and compares results with Rust.

It is not a permanent fallback. Every difference is classified as:

```text
Rust bug
legacy TypeScript bug
intentional contract change
fixture/oracle defect
unresolved blocker
```

### 31.6 Rollback

Before sole ownership, rollback selects one complete legacy mode and disables the Rust owner. After schema/query cutover, rollback requires a migration-compatible prior release or a database backup; it never permits concurrent legacy and Rust mutation.

Shadow observation or query comparison uses a separate database or read-only snapshot. It never creates two writers against production state.

## 32. Testing and verification

### 32.1 Unit tests

Common engine tests cover:

- cursor encoding/ordering;
- generation transitions;
- dirty-scope coalescing;
- scheduler fairness and boundedness;
- evidence precedence;
- usage contribution deltas;
- snapshot replacement;
- outbox replay and pruning;
- capability validation;
- adapter/core dependency constraints.

Adapter unit tests cover native decode and native correlation only.

### 32.2 Integration traces

Every adapter maintains deterministic traces for:

- first discovery and full backfill;
- warm start with no changes;
- one live append;
- burst append;
- partial final record;
- malformed complete record;
- duplicate and out-of-order hints;
- file truncate/rewrite;
- atomic replacement;
- source object delete/recreate;
- source-root move or temporary unavailability;
- process restart;
- projection-version change;
- adapter-contract-version change;
- unknown future native record.

### 32.3 Runtime-specific traces

For adapters declaring runtime packs:

- parent before child;
- child before parent;
- child activity without a parent relation;
- explicit completion;
- failure/cancellation;
- inactivity without completion;
- team member add/remove;
- inbox message update/read-state change;
- sidecar before transcript;
- transcript before sidecar;
- active presence removed while transcript remains quiet;
- conflicting evidence;
- source timestamps that are equal, missing, or skewed.

### 32.4 Crash injection

The test store injects process-like failure at:

```text
after source read
mid decode
before transaction
mid canonical projection
mid runtime projection
mid usage projection
after cursor update statement
after outbox insert
immediately before COMMIT
immediately after COMMIT
before in-memory publish
mid subscriber delivery
```

After restart, the database must satisfy invariants and converge to fresh rebuild output.

### 32.5 Fault injection

Inject:

- watcher overflow;
- internal channel full;
- permission denied;
- transient source database lock;
- SQLite writer busy/full/I/O error;
- source read cancellation;
- adapter panic converted to controlled failure boundary where possible;
- corrupt cursor;
- unsupported source schema;
- outbox retention gap;
- slow subscriber;
- shutdown during backfill and during live commit.

### 32.6 Real-corpus differential tests

Anonymized real corpora remain essential because vendor formats have undocumented variants. Differential reports compare:

- row counts and stable IDs;
- content blocks;
- timestamps and quality;
- usage and quality buckets;
- parent/child/team relations;
- current runtime state;
- unknown/quarantined records;
- source cursor positions;
- cold/live convergence.

A lower difference count is not sufficient; every accepted difference must be categorized as a bug fix, intentional contract change, legacy defect, or unresolved blocker.

### 32.7 Property tests

Where practical, generate event traces from a source model and assert:

- duplicate hints do not change final state;
- arbitrary hint loss followed by reconcile converges;
- batching boundaries do not change semantics;
- decode parallelism does not change semantics;
- crash/restart at any transaction boundary converges;
- source-object order is preserved;
- cross-object order is not assumed.

### 32.8 Query correctness and transport tests

Tests cover:

- search, timeline, usage, runtime, team, and extension request/result fixtures;
- stable keyset pagination and cursor version errors;
- exact snapshot consistency for rows/facets/counts;
- `at_commit_seq` handoff to durable subscription replay;
- query purity verified by database page/hash or change counters before and after calls;
- projection pending/stale/ready behavior;
- cancellation before dispatch, during SQL iteration, and during result conversion;
- N-API and IPC semantic parity;
- no Node event-loop blocking regression in integration benchmarks;
- no unbounded JavaScript array or stale search queue;
- concurrent live writer plus multiple readers;
- long reader/WAL checkpoint pressure;
- engine shutdown with in-flight queries.

### 32.9 Architecture tests

CI scans production TypeScript bundles/source graphs and fails if they:

- import `node:sqlite` or `better-sqlite3` for Spaghetti storage;
- contain approved Spaghetti table names or migration SQL outside test fixtures;
- import source decoders/checkpoint modules;
- bypass `SpaghettiClient` to call transport-specific domain methods;
- add adapter-ID branches to common Rust query modules.

Equivalent Rust dependency tests prevent adapters from importing store/query implementation crates.

## 33. Acceptance criteria

RFC 011 is complete only when all of the following are true.

### Architecture

- [ ] Rust owns source discovery through durable commit for Claude, Codex, and Grok.
- [ ] Rust is the sole production owner of the Spaghetti SQLite database, migrations, writer, query pool, and projection maintenance.
- [ ] Cold/backfill and live use the same adapter decoders and projectors.
- [ ] The common engine and query layer have no branches over built-in adapter IDs.
- [ ] Adapters do not import Spaghetti-store/query SQL, `notify`, N-API, or public event publishers.
- [ ] No source-specific mutable token hook remains.
- [ ] One owner lock prevents concurrent database authorities.
- [ ] N-API and IPC are thin async transports over the same typed engine API.
- [ ] Production TypeScript contains no Spaghetti SQL, migration, projection repair, or database connection.

### Correctness

- [ ] Cursor, projection, runtime state, usage, and change log commit atomically.
- [ ] Watcher/internal overflow forces reconcile and is tested.
- [ ] Partial records never advance cursors.
- [ ] Complete unknown/malformed records cannot permanently stall a stream.
- [ ] Truncate, replace, delete, and recreate use generation-safe semantics.
- [ ] Runtime state distinguishes durable observation from transient assessment.
- [ ] Cross-file late correlation is supported.
- [ ] Usage scope, accounting mode, and quality are preserved.
- [ ] Restart replay uses durable commit sequence and ordinal.
- [ ] Live, fresh rebuild, forced reconcile, and restart hydration converge.
- [ ] Queries never mutate the database or invoke projection repair.
- [ ] Compound query responses are snapshot-consistent and carry `at_commit_seq`.
- [ ] Keyset pagination has stable no-duplicate/no-gap behavior for a fixed snapshot.

### Adapter support

- [ ] Claude passes core, append, snapshot, presence, usage, delegation, and teams packs.
- [ ] Codex passes core, append, usage, and every capability it declares.
- [ ] Grok passes core, append, snapshot/entity-reconcile, usage, and every capability it declares.
- [ ] One vendor-neutral adapter check produces explicit pass/fail reports.
- [ ] Unknown native events are retained with provenance.
- [ ] Capability manifests are surfaced through the SDK.
- [ ] Adapters expose common query functionality only through emitted facts and declared capability packs.

### Query and API

- [ ] Search, timeline, project/session, usage, runtime, and declared capability queries are implemented in Rust.
- [ ] Public database-backed methods are asynchronous and cancellable where applicable.
- [ ] N-API and IPC normalized results are semantically identical.
- [ ] One ordinary logical query performs one native/IPC request.
- [ ] Query pages carry versioned opaque cursors and a commit watermark.
- [ ] Unsupported capabilities and pending projections return explicit structured states.
- [ ] Existing synchronous database-backed APIs are removed or converted under the documented API-version migration.

### Operations

- [ ] Stream, writer, query-pool, projection-readiness, and health state are observable.
- [ ] Queue depth, overflows, reconcile, query latency, cancellation, boundary bytes, WAL state, errors, and subscriber lag are measured.
- [ ] Change-log retention and reset behavior are implemented.
- [ ] Raw/error retention follows privacy policy.
- [ ] Shutdown leaves no orphaned watchers, tasks, file handles, database handles, native callbacks, or sockets.

### Performance

- [ ] No normal append causes whole-file reparse.
- [ ] No database connection is opened per live record, batch, or public query.
- [ ] No raw watcher event crosses N-API individually.
- [ ] No SQLite work executes on the Node/Electron event-loop thread.
- [ ] No canonical cross-agent search/timeline merge is performed in TypeScript.
- [ ] Usage hot path is O(changed usage facts).
- [ ] Reference live-latency, backfill, query, memory, burst, cancellation, and concurrency benchmarks pass.
- [ ] Correctness remains intact under forced saturation, notification loss, and long readers.

### Retirement

- [ ] TypeScript source watchers and checkpoint sidecars are deleted.
- [ ] TypeScript source parsers/writers are deleted or retained only in isolated test tooling.
- [ ] Production `SqliteService`, TypeScript schema/migrations, SQL query service, and projection repair are deleted.
- [ ] Process-local live change sequence is retired.
- [ ] Legacy per-source write-batch bridges are retired.
- [ ] The TypeScript differential oracle is removed after accepted parity is recorded.
- [ ] RFC 009 documentation is updated to reflect the superseding decision.
- [ ] RFC 010 is documented as a completed transitional driver migration rather than the final database architecture.

## 34. Risks and mitigations

### 34.1 Risk: abstracting before enough agents are understood

A generic adapter API can accidentally encode Claude assumptions.

**Mitigation:** distinguish universal core, optional capability packs, and namespaced extensions; use the rule of two/product-contract review; provide a custom producer escape hatch; validate the API against Claude, Codex, Grok, and at least one non-file-backed source design before declaring it stable.

### 34.2 Risk: over-normalization loses native meaning

A thin common fact may omit vendor-specific fields needed later.

**Mitigation:** ordered content blocks, raw/native provenance, namespaced extension facts, unknown-record preservation, and adapter contract versioning. Promotion to common schema is deliberate and migratable.

### 34.3 Risk: adapters become hidden mini-engines

A permissive adapter can recreate private watchers, cursors, SQL, retries, and state.

**Mitigation:** hard crate dependencies, code-review checklist, architecture linting, bounded `SourceAccess`, one registry, and conformance tests. Custom producers require review and still cannot own Spaghetti storage or public delivery.

### 34.4 Risk: single SQLite writer becomes a bottleneck

Many live sessions and backfill may compete.

**Mitigation:** parallel bounded source read/decode, one efficient prepared writer lane, priority queues, micro-batching, WAL, contribution updates, and backfill throttling. Add sharding only after measurements prove one database cannot meet requirements.

### 34.5 Risk: durable outbox grows indefinitely

A replayable change log consumes storage.

**Mitigation:** bounded retention, oldest-cursor metrics, snapshot/reset protocol, and compaction outside the guaranteed replay window.

### 34.6 Risk: runtime inference becomes misleading

Users may interpret `stale` as completed or presence as proof of active execution.

**Mitigation:** durable observation versus transient assessment, evidence/quality in APIs, explicit terminal precedence, and UI language tied to confidence.

### 34.7 Risk: vendor formats change without notice

A decoder may begin quarantining or misinterpreting records.

**Mitigation:** schema/version detection, unknown preservation, drift metrics, fixture updates, circuit breakers, adapter contract versions, and targeted re-materialization.

### 34.8 Risk: source reads race active writes

Replace-on-write documents may be briefly incomplete; source databases may be locked.

**Mitigation:** stability retries, atomic-replace-neutral snapshot drivers, read-only busy retry, bounded backoff, and cursor advancement only after successful decode/commit.

### 34.9 Risk: dual ownership during migration

Legacy TypeScript and Rust paths could both write.

**Mitigation:** explicit owner lock, feature flag that selects exactly one writer, isolated shadow database, and phase exits that require legacy shutdown before cutover.

### 34.10 Risk: raw retention increases privacy exposure

Unknown/quarantined records may duplicate sensitive content.

**Mitigation:** per-stream raw retention, hash-only option, redacted diagnostics, restrictive database permissions, and no raw payload in logs.

### 34.11 Risk: long readers prevent WAL checkpoint progress

An export or expensive query can keep an old snapshot alive and allow the WAL to grow.

**Mitigation:** bounded page/stream design, query deadlines and cancellation, oldest-reader metrics, separate bulk fairness, controlled checkpointing, and no unbounded read transaction for ordinary APIs.

### 34.12 Risk: async API migration disrupts SDK consumers

The current synchronous surface is convenient for CLI and scripts.

**Mitigation:** introduce a parallel async client, migrate first-party consumers, publish deprecation and codemod guidance, retain compatibility for a defined release window, and do not hide blocking behavior behind a synchronous native facade.

### 34.13 Risk: a chatty typed API recreates SQL-level overhead

A client could make many tiny domain calls and lose the advantage of native aggregation.

**Mitigation:** design page/snapshot requests around complete user operations, instrument calls per interaction, add batch endpoints only for demonstrated cases, and reject per-row lookup patterns in review.

### 34.14 Risk: query/read-model abstractions become Claude-shaped

Moving query semantics into common Rust code can still encode the first adapter's assumptions.

**Mitigation:** query packs mirror the universal/capability/extension split, capability honesty remains explicit, adapter conformance spans multiple vendors, and vendor-only fields stay in extension projections until promoted by review.

### 34.15 Risk: result conversion shifts rather than removes cost

Rust may execute SQL quickly but spend significant time constructing JavaScript objects or IPC payloads.

**Mitigation:** benchmark conversion separately, return bounded pages, use typed read models, avoid Rust-JSON-to-JS-JSON double parsing, and introduce binary streaming only for measured large-result classes.

## 35. Alternatives considered

### 35.1 Keep TypeScript watchers and move only parsing/writes to Rust

Rejected as the final architecture. It leaves source scheduling, checkpoints, lifecycle, and cross-source joins split across runtimes, adds serialization overhead, and preserves duplicate ownership. It may be used temporarily during migration by submitting dirty paths to the Rust engine.

### 35.2 Give each adapter an end-to-end pipeline

Rejected. Claude, Codex, and Grok would each implement watcher/scanner, cursor, retry, writer, runtime reducer, token reducer, and event delivery. Correctness fixes would need to be repeated and would drift.

### 35.3 Treat watcher events as the canonical event log

Rejected. Native events can be duplicated, coalesced, lost, reordered, and platform-dependent. They are invalidation hints. Source content and durable cursors are authoritative.

### 35.4 Normalize every native record into one large universal schema

Rejected. Agents differ in content structure, usage granularity, teams, subagents, sidecars, and source storage. A forced schema would either become Claude-shaped or lose information. The accepted design uses a thin core, capability packs, and extensions.

### 35.5 Store only current runtime state

Rejected. Without evidence/provenance, state cannot be audited, rebuilt, or corrected after adapter changes. Durable evidence plus a materialized current view provides both speed and repairability.

### 35.6 Publish only in-memory events

Rejected. Process restart creates gaps and sequence reset. A durable outbox permits replay and defines commit-before-publish precisely.

### 35.7 Use a separate runtime database

Rejected initially. History, runtime evidence, usage, cursors, and outbox need an atomic commit boundary. Splitting stores would require distributed transaction or compensating recovery. Read scaling can use WAL/read replicas or a later derived cache.

### 35.8 Start with a dynamic adapter plugin ABI

Rejected for v1. Rust ABI stability, migration authority, source permissions, and untrusted code make this a separate problem. Built-in adapters establish the contract first. A future plugin system may use a process boundary and versioned wire protocol rather than a Rust dynamic-library ABI.

### 35.9 Depend on Watchman as the engine

Rejected as a mandatory dependency. Watchman can be an optional source-driver backend for suitable environments, but it does not replace agent decoding, source database support, transactional cursors, projections, or the durable outbox. Shipping another daemon also complicates packaging and ownership.

### 35.10 Model silence as completion

Rejected. A quiet transcript may represent thinking, waiting, a blocked tool, a crashed process, a disconnected source, or completion. Only explicit evidence can create a durable terminal state.

### 35.11 Keep TypeScript `node:sqlite` for all reads

Rejected as the final architecture. SQLite execution itself may be comparable for tiny calls, but TypeScript would continue to own schema semantics, synchronous event-loop work, query-time projection repair, canonical aggregation, and a second database lifecycle. It remains useful only as a temporary oracle during migration.

### 35.12 Expose generic Rust SQL to TypeScript

Rejected. It moves the driver but not ownership: TypeScript would still know tables, migrations, ranking, and row semantics. Internal schema evolution would remain a public breaking change.

### 35.13 Use synchronous N-API query methods

Rejected for production database-backed APIs. They can block Electron main or `fieldd` for the full SQLite and conversion duration. The final client API is asynchronous.

### 35.14 Put one `rusqlite::Connection` behind a mutex

Rejected. It serializes readers with each other and often with writes, makes cancellation and fairness coarse, and does not match connection ownership expectations. The target is one writer connection plus a bounded read-only pool.

### 35.15 Let queries repair projections lazily

Rejected. Read latency becomes unpredictable, a read pool is no longer read-only, and concurrent callers can race maintenance. The writer owns projection readiness and repair.

### 35.16 Maintain separate Rust N-API and field-native query implementations

Rejected. Hosting transports may differ, but query semantics, DTOs, pagination, and errors come from one `SpaghettiEngine`/query crate. Two query engines would recreate the drift this RFC removes.

## 36. Resolved design questions

### Q1. Is runtime observation a third plane?

No. It is a projection of live source ingestion. The implementation has one ingest spine and multiple projections.

### Q2. Who owns filesystem watching?

The common Rust source supervisor. Adapters declare streams and selectors.

### Q3. Who owns source JSON/native parsing?

The Rust agent adapter. Common drivers frame records and enforce cursor semantics but do not interpret vendor content.

### Q4. Who owns the Spaghetti SQLite database?

One Rust `SpaghettiEngine`. It owns migrations, one writer connection, the read-only query pool, projection maintenance, subscriptions, checkpoint policy, and shutdown. Production TypeScript owns no connection.

### Q5. Can an adapter read a source-owned SQLite database?

Yes, through bounded read-only source-driver/access APIs. The prohibition is against adapter access to the Spaghetti database.

### Q6. Who owns public search and queries?

Common Rust query packs over committed projections. Adapters emit facts and capabilities; TypeScript calls typed async APIs and formats results.

### Q7. Can TypeScript send arbitrary SQL to Rust?

No. The public surface is domain-level. A privileged internal diagnostic tool is not part of the SDK contract.

### Q8. May a query repair a missing projection?

No. The writer maintains and repairs projections. A query reports readiness, waits asynchronously when contracted, or uses a documented read-only fallback.

### Q9. Are public database APIs synchronous?

Not in the final architecture. They return promises/futures and support cancellation where useful. Synchronous legacy APIs are migrated explicitly.

### Q10. How are agent-specific fields retained and queried?

Through raw/native provenance and namespaced extension facts. A centrally versioned extension projector/query exposes them; the adapter does not receive SQL authority.

### Q11. How are tokens normalized?

Adapters declare subject, scope, accounting mode, and quality. The common reducer computes idempotent contributions and totals without inventing precision.

### Q12. How are cross-file subagents or sidecars ordered?

They are not assigned a fabricated global source order. Facts use stable IDs, per-object cursor order, commit order, source timestamp metadata, and late correlation.

### Q13. How does a query snapshot connect to live updates?

The result carries `at_commit_seq`. The client subscribes from that watermark and receives committed changes after the snapshot.

### Q14. Is the daemon required?

No. The engine is library-first. N-API, `field-native`, and standalone daemon hosting share semantics. One-owner enforcement is required in every mode.

### Q15. Are third-party adapters supported?

Not through a stable plugin ABI in this RFC. New built-in adapters follow the same contract. A safe external adapter protocol is future work.

## 37. Deferred follow-up RFCs

The following are explicitly outside this RFC and should not block it:

1. mandatory standalone daemon and multi-client IPC deployment policy;
2. third-party adapter process/plugin protocol;
3. remote source ingestion and synchronization;
4. encrypted-at-rest raw transcript storage;
5. database sharding or distributed materialization;
6. generalized cost pricing/version feeds;
7. user-defined extension-fact projectors;
8. source-driver support for remote object stores;
9. optional leased historical snapshots across many paginated requests;
10. specialized analytical/columnar export engines after SQLite query benchmarks justify them.

## 38. Implementation checklist by module

### `spaghetti-model` / adapter API

- [ ] open `AdapterId`;
- [ ] manifest and capability quality model;
- [ ] source instance/stream/object IDs;
- [ ] opaque cursor and generation types;
- [ ] source record/provenance;
- [ ] universal facts;
- [ ] capability-pack fact envelopes;
- [ ] extension/unknown facts;
- [ ] evidence and usage semantics;
- [ ] query request/result DTOs;
- [ ] commit watermark and opaque page cursor types;
- [ ] transport-neutral engine/query errors;
- [ ] adapter diagnostics/error classes.

### `spaghetti-source`

- [ ] discovery coordinator;
- [ ] watch-before-scan supervisor;
- [ ] append-delimited driver;
- [ ] replace-document driver;
- [ ] directory-snapshot driver;
- [ ] presence driver;
- [ ] read-only source-SQLite driver;
- [ ] key-value driver;
- [ ] bounded `SourceAccess`;
- [ ] polling fallback;
- [ ] generation and identity detection;
- [ ] non-UTF-8 path identity;
- [ ] cancellation/disposal.

### `spaghetti-engine`

- [ ] database owner lock and host metadata;
- [ ] persistent engine lifecycle;
- [ ] per-object ordered scheduler;
- [ ] priority/fairness policy;
- [ ] bounded coalescing;
- [ ] adapter registry;
- [ ] decode runner and panic/error boundary;
- [ ] entity reconciliation;
- [ ] runtime evidence reducer;
- [ ] usage contribution reducer;
- [ ] projection version/readiness planner;
- [ ] durable publisher/replay;
- [ ] health, cancellation, and metrics;
- [ ] ordered shutdown.

### `spaghetti-store-sqlite`

- [ ] source catalog migrations;
- [ ] provenance/idempotency keys;
- [ ] ingest commits;
- [ ] durable change log;
- [ ] quarantine store;
- [ ] canonical fact projectors;
- [ ] runtime and usage tables;
- [ ] capability-pack and extension tables;
- [ ] typed read models;
- [ ] snapshot/generation replacement;
- [ ] one long-lived writer connection;
- [ ] bounded read-only/query-only connection pool;
- [ ] prepared statements per connection;
- [ ] WAL/oldest-reader/checkpoint policy;
- [ ] crash injection hooks;
- [ ] audit/full-rebuild comparison.

### `spaghetti-query`

- [ ] core history query pack;
- [ ] FTS search and cross-projection rank merge;
- [ ] timeline/branch/facet query;
- [ ] project/session canonical aggregation;
- [ ] usage query pack;
- [ ] runtime query pack;
- [ ] teams/delegation/other capability packs;
- [ ] namespaced extension-query registry;
- [ ] read-only projection readiness handling;
- [ ] read-transaction snapshot watermark;
- [ ] keyset cursor encoding/versioning;
- [ ] cancellation and request fairness;
- [ ] bounded result/stream encoding;
- [ ] query purity tests.

### Adapter crates/modules

- [ ] source survey;
- [ ] discovery;
- [ ] stream specs;
- [ ] native decoder;
- [ ] IDs and joins;
- [ ] capability manifest;
- [ ] fixtures/goldens;
- [ ] core and driver packs;
- [ ] declared capability packs;
- [ ] extension fact schema where required;
- [ ] no Spaghetti SQL or public query implementation;
- [ ] shadow parity report;
- [ ] contract version/migration notes.

### N-API, IPC, and TypeScript SDK

- [ ] persistent async engine handle;
- [ ] N-API Promise tasks off the Node thread;
- [ ] `AbortSignal` cancellation mapping;
- [ ] versioned IPC request/response transport;
- [ ] transport-neutral `SpaghettiClient`;
- [ ] batched committed changes and resumable cursors;
- [ ] capability and projection-readiness exposure;
- [ ] health/status APIs;
- [ ] migrate CLI/TUI/Electron/React consumers;
- [ ] delete production TypeScript SQLite service, SQL, schema, and migrations;
- [ ] no source parsing/checkpoints in TypeScript;
- [ ] architecture import/dependency gates;
- [ ] clean disposal with in-flight query tests.

## 39. Appendix A — illustrative adapter declaration

```rust
impl AgentAdapter for ClaudeCodeAdapter {
    fn manifest(&self) -> &AdapterManifest {
        &CLAUDE_MANIFEST
    }

    fn discover(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<SourceInstanceSpec>, AdapterError> {
        // Locate configured/default Claude roots and return stable instances.
        // No watchers or SQLite writes are created here.
        discover_claude_instances(ctx)
    }

    fn streams(
        &self,
        instance: &SourceInstance,
    ) -> Result<Vec<StreamSpec>, AdapterError> {
        Ok(vec![
            StreamSpec::append_jsonl(
                "session-transcripts",
                instance.root("projects")?,
                "**/*.jsonl",
                "claude-session-record",
            )
            .interactive(),

            StreamSpec::append_jsonl(
                "subagent-transcripts",
                instance.root("projects")?,
                "**/subagents/*.jsonl",
                "claude-subagent-record",
            )
            .interactive()
            .capability("delegation"),

            StreamSpec::replace_json(
                "team-configs",
                instance.root("teams")?,
                "*/config.json",
                "claude-team-config",
            )
            .capability("teams"),

            StreamSpec::replace_json(
                "team-inboxes",
                instance.root("teams")?,
                "*/inboxes/*.json",
                "claude-team-inbox",
            )
            .capability("teams"),

            StreamSpec::presence(
                "active-sessions",
                instance.root("sessions")?,
                "*.json",
                "claude-active-session",
            )
            .interactive()
            .capability("presence"),
        ])
    }

    fn decode(
        &self,
        ctx: DecodeContext<'_>,
        record: SourceRecordRef<'_>,
        out: &mut FactBatch,
    ) -> Result<DecodeDisposition, AdapterError> {
        match ctx.decoder.as_str() {
            "claude-session-record" => decode_session_record(record, out),
            "claude-subagent-record" => decode_subagent_record(record, out),
            "claude-team-config" => decode_team_config(record, out),
            "claude-team-inbox" => decode_team_inbox(record, out),
            "claude-active-session" => decode_active_session(record, out),
            unknown => Err(AdapterError::unknown_decoder(unknown)),
        }
    }
}
```

The example is intentionally declarative. The adapter contains no `notify` callback, checkpoint file, `rusqlite::Connection`, public event emitter, or retry loop.

## 40. Appendix B — illustrative commit batch

A Claude parent record and a child record may be decoded concurrently but committed in one bounded batch:

```text
FactBatch A
  SessionObserved(session S)
  MessageObserved(message M1 in S)
  RunEvidenceObserved(root run R active)
  UsageObserved(message M1, exact delta)
  RelationObserved(message M1 emitted_by R)

FactBatch B
  RunObserved(child C)
  MessageObserved(message M2 emitted_by C)
  RunEvidenceObserved(child C active)
  RelationObserved(C child_of R)   // or pending if R is unresolved
```

One transaction applies:

```text
canonical session/message/run rows
relations and pending-link repair
runtime evidence/current state
usage contributions/session totals
cursor A and cursor B
change_log entries under commit_seq 8124
```

Only after commit does the publisher deliver:

```json
{
  "commitSeq": 8124,
  "changes": [
    { "ordinal": 0, "topic": "history.message.changed", "entity": "M1" },
    { "ordinal": 1, "topic": "history.message.changed", "entity": "M2" },
    { "ordinal": 2, "topic": "runtime.run.changed", "entity": "R" },
    { "ordinal": 3, "topic": "runtime.run.changed", "entity": "C" },
    { "ordinal": 4, "topic": "runtime.usage.changed", "entity": "S" }
  ]
}
```

A crash before commit replays both source ranges. A crash after commit replays the change batch from `change_log`.

## 41. Appendix C — architecture review rubric for new common abstractions

Before adding a common driver, fact, capability pack, or reducer, answer:

1. Is the behavior required for engine correctness, or is it native interpretation?
2. Do at least two adapters need equivalent semantics?
3. If only one adapter needs it, is Spaghetti deliberately defining a product-level capability?
4. Can it remain a namespaced extension without harming the product?
5. Does the proposed abstraction preserve uncertainty, scope, and native detail?
6. Can cold, live, restart, and reconcile share it?
7. Can it be tested independently of a vendor?
8. Does it force a source adapter to import store, query, watcher, host, or event-delivery code?
9. Does it force TypeScript to know canonical table names, SQL, ranking, identity, or pagination rules?
10. Would adding a fourth adapter require changing this abstraction or only registering new code?
11. Are its schema, projection, cursor, and public query versions centrally owned?
12. Can one logical client operation be answered without a chatty sequence of native/IPC calls?

A proposal that fails questions 1–4 belongs in the adapter. A proposal that fails questions 5–12 is not ready for common code.

## 42. Final position

Spaghetti's durable product is no longer merely a table of parsed transcripts. It is a local, continuously convergent observation and query system over heterogeneous agent-owned sources.

The architecture centers on five separations:

```text
source mechanics       != native interpretation
native interpretation  != common semantic facts
semantic facts         != materialized projections
materialized state     != public query transport
committed projections  != transient delivery
```

Rust owns the full correctness and performance envelope from source discovery through database commit, projection repair, search, pagination, runtime snapshots, and durable delivery. TypeScript owns client ergonomics and presentation, not persistence or canonical semantics.

Adapters remain narrow enough to add agents without cloning the engine, but expressive enough to handle JSONL, replaceable snapshots, sidecars, source databases, and key-value state without lying about equivalence. They stop at truthful facts and capabilities. The common engine turns those facts into stable projections and query packs.

The decisive tests are therefore not only whether Spaghetti can watch another file, but whether:

1. a new agent can be adapted by declaring sources, decoding native records, emitting truthful facts, and passing shared conformance—without adding another watcher, checkpoint system, writer, token accumulator, SQL service, or event bus; and
2. every client can ask one typed asynchronous question and receive one consistent committed answer—without knowing which agent produced the data or how Spaghetti stores it.
