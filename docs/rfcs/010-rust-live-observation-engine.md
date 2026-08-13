# RFC 010: Rust Observation Engine — Unified Ingest, Live State, and Agent Adapters

- **Status:** Implemented transitional observation design; superseded by RFC 011
- **Created:** 2026-08-10
- **Revised:** 2026-08-12
- **Authors:** James Yong, contributors
- **Target:** `spaghetti`
- **Type:** Architecture / migration / adapter contract
- **Scope:** Rust ingest, live disk observation, runtime-state projection, token usage, source adapters, durable subscriptions
- **Related documents:**
  - `docs/TWO-PLANE-INGEST-ARCHITECTURE.md`
  - `docs/rfcs/003-rust-ingest.md`
  - `docs/rfcs/006-thin-normalized-model.md`
  - `docs/rfcs/007-retire-runtime-bridge.md`
  - `docs/rfcs/009-retire-typescript-bulk-ingest.md`
  - `docs/rfcs/011-rust-observation-query-engine.md`

> **Completion/supersession note:** The unified Rust driver, adapter, cursor,
> projection, and durable-observation direction in this RFC was implemented.
> [RFC 011](./011-rust-observation-query-engine.md) then made the persistent
> Rust engine the sole production SQLite and query owner, completed the async
> client cutover, and retired production TypeScript storage reachability. RFC
> 010 is therefore a completed transitional driver migration, not the final
> database/client architecture.

## 1. Summary

Spaghetti is evolving from a transcript indexer into a local observation engine for coding agents. Transcript history remains important, but the same on-disk sources also expose live operational facts that agent CLIs do not consistently provide through hooks: active sessions, subagent lifecycle, agent-team membership and inbox activity, token consumption, tasks, plans, artifacts, and other runtime evidence.

This RFC establishes one Rust-owned ingest architecture for both history and live observation:

> **One ingest spine, two temporal modes, multiple transactional projections.**

The two temporal modes are:

1. **Backfill/reconcile** — discover current source state, rebuild missing materializations, repair drift, and recover from dropped notifications.
2. **Live tail/observe** — incrementally consume active append streams and replaceable snapshots with low latency.

Both modes use the same source adapters, decoders, fact model, projection code, cursor semantics, and SQLite writer. They differ only in scheduling and batching policy.

Every successfully decoded source record may update several projections in one SQLite transaction:

1. durable transcript/history and search indexes;
2. durable observed runtime state;
3. token and cost materializations;
4. source cursors and projection versions;
5. a durable change log used for resumable subscriptions.

The central architectural boundary is:

> **The common engine owns mechanics and correctness. Agent adapters own interpretation.**

The common engine owns discovery scheduling, watcher lifecycle, record framing, cursor and generation management, retries, reconciliation, backpressure, transactions, projections, state reduction, durable event delivery, and observability. An agent adapter owns the native source map, native record decoding, native identifiers, agent-specific joins, and a truthful capability declaration. An adapter emits typed facts and evidence; it never mutates Spaghetti tables directly and never owns delivery of public events.

This RFC supersedes RFC 009's decision to retain the TypeScript live writer and filesystem watchers. It does **not** reverse RFC 007: Spaghetti remains disk-derived and does not restore the retired hook/plugin runtime bridge. Optional process probes may enrich a transient assessment, but they do not become historical truth.

## 2. Decision

Spaghetti will implement a long-lived Rust `ObservationEngine` with the following properties:

1. **Rust is the sole ingest writer.** Rust owns source reading through SQLite commit for cold, warm, repair, and live paths.
2. **Cold and live are one implementation.** There will be no independent TypeScript and Rust parsers or writers for the same source.
3. **Filesystem notifications are hints, not truth.** Durable cursors and reconciled source snapshots determine what has been ingested.
4. **Adapters emit facts, not SQL.** Adapters may read agent-owned files or databases, but they may not write Spaghetti-owned tables or publish public change events.
5. **Cursors, projections, and outbox changes commit atomically.** A crash cannot advance a source cursor without its corresponding rows, nor publish a change that did not commit.
6. **Observed state is evidence-backed.** Durable runtime state records what the sources prove. Timeouts, PID checks, and inactivity produce a separate transient assessment.
7. **Capabilities are declarative and qualified.** Support is reported as native, derived, estimated, or unsupported, with truthful scope and granularity.
8. **The adapter identifier is open-ended.** Core code uses a string/newtype identifier and must not use a closed `match` over Claude, Codex, and Grok.
9. **Built-in adapters are compiled Rust crates/modules in v1.** A stable third-party dynamic-plugin ABI is explicitly deferred.
10. **Current TypeScript APIs remain a compatibility facade during migration.** They become thin clients of the Rust engine and cease to implement source semantics.

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

## 4. Goals

This RFC has the following goals:

1. Make Rust the authoritative implementation of all ingest and live observation.
2. Parse each source record once and fan the resulting facts into all applicable projections.
3. Guarantee crash-safe convergence between source cursors, canonical rows, runtime state, usage totals, and emitted changes.
4. Support low-latency tailing without treating watcher events as a lossless log.
5. Define a hard, reviewable boundary between common engine code and agent-specific code.
6. Make adding an agent primarily an adapter-and-fixtures task rather than an engine modification.
7. Preserve native details without forcing all agents into a Claude-shaped schema.
8. Represent unsupported, derived, and estimated capabilities honestly.
9. Support file, directory, SQLite, and key-value source families.
10. Provide deterministic cold/live parity and a shared conformance harness.
11. Enable embedding in `field-native` or hosting in a standalone Spaghetti process without changing ingest semantics.
12. Keep current SDK and CLI surfaces working throughout migration.

## 5. Non-goals

This RFC does not:

1. define a stable dynamic plugin ABI for untrusted third-party adapters;
2. require an always-on standalone daemon in the first implementation;
3. move all query-side SQL and SDK APIs into Rust immediately;
4. introduce network synchronization or a cloud service;
5. claim process lifecycle truth that is not present in observed sources;
6. guarantee a global causal order across independent files or databases;
7. force every vendor field into a universal normalized schema;
8. make filesystem events an audit log;
9. replace the source agent's own persistence or repair corrupted vendor data;
10. infer per-message token usage from a session-only aggregate without marking it as estimated;
11. preserve every transient intermediate filesystem state;
12. standardize remote-agent transport; remote sources require a later RFC.

## 6. Relationship to prior RFCs

### 6.1 RFC 006: thin normalized model

This RFC builds on RFC 006. Spaghetti keeps a small cross-agent semantic core and preserves native/raw information. The new fact layer is not a mandate to flatten every source into one lossy record type.

### 6.2 RFC 007: retire runtime bridge

RFC 007 remains in force. This design does not restore hook plugins, channel plugins, or a second process-adjacent runtime event facade. Disk-derived evidence enters through the observation engine. Optional liveness probes are transient assessments and are never sufficient by themselves to create durable lifecycle history.

### 6.3 RFC 009: retire TypeScript bulk ingest

This RFC supersedes RFC 009's non-goal that keeps the TypeScript live writer and filesystem watchers. The desired end state is now:

```text
Rust source drivers
  -> Rust adapter decoders
  -> Rust projections
  -> Rust SQLite transaction
  -> durable change log
```

RFC 009's broader goal of retiring duplicate TypeScript bulk ingest remains valid. Migration sequencing changes so that the current live path is retained only as a temporary fallback until the Rust observation engine replaces it. The old native live-batch bridge may still be deleted, but only after the persistent engine exists; it is not the target architecture.

### 6.4 Two-plane architecture

The product-level distinction between static/backfill and live disk ingestion remains useful. The implementation is refined to:

> **Two temporal scheduling modes over one ingest spine, not two independent pipelines.**

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
| **Reconcile** | Compare source reality with committed source-object state and ingest the delta or rebuild an affected projection. |

## 8. North-star architecture

```text
Agent-owned sources
  files / directories / SQLite / KV
                |
                v
+-------------------------------------------+
| SourceSupervisor                           |
| discovery, watch registration, polling,    |
| dirty hints, overflow detection             |
+----------------------+--------------------+
                       |
                       v
+-------------------------------------------+
| Scheduler                                  |
| per-object ordering, cross-object          |
| parallelism, priorities, bounded queues    |
+----------------------+--------------------+
                       |
                       v
+-------------------------------------------+
| Source drivers                              |
| append tail / replace snapshot / directory |
| snapshot / SQLite query / KV / custom       |
+----------------------+--------------------+
                       |
                 SourceRecord
                       |
                       v
+-------------------------------------------+
| Agent adapter                               |
| native decode, IDs, joins, capabilities,   |
| FactBatch emission                          |
+----------------------+--------------------+
                       |
                    FactBatch
                       |
                       v
+-------------------------------------------+
| CommitCoordinator                           |
| one SQLite writer lane                      |
|                                             |
|  canonical history + FTS                    |
|  runtime evidence + current-state reducer   |
|  usage contribution reducer                 |
|  source cursor/generation                   |
|  projection versions                        |
|  durable change log                         |
+----------------------+--------------------+
                       |
                     COMMIT
                       |
          +------------+-------------+
          |                          |
          v                          v
  in-memory read cache       subscription publisher
  hydrated from SQLite       replay by commit_seq
```

The data path is library-first. It can be hosted by:

```text
spaghetti CLI / Node SDK
        -> thin N-API host
        -> ObservationEngine
```

or:

```text
field-native
        -> embedded ObservationEngine
```

A standalone daemon may host the same engine later. The host does not change source semantics, adapter behavior, or transaction boundaries.

## 9. Architectural invariants

The following invariants are normative.

### I1. One writer owner

For one Spaghetti database and source set, exactly one live `ObservationEngine` owns ingestion. Query connections may be concurrent. Multiple hosts must use a process lock or connect to the existing owner.

### I2. Same decoder for backfill and live

A source record has one adapter decoder. Backfill, warm repair, and live tailing may frame records differently by cursor range, but they must invoke the same decode implementation and projections.

### I3. Adapter facts are side-effect free

Decoding may read adapter-owned dependencies through an explicitly provided read-only source-access interface. It may not:

- open or mutate the Spaghetti database;
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

No common-engine module may branch on `claude-code`, `codex`, `grok`, or future adapter identifiers. Agent identifiers may appear only in registry construction, diagnostics, adapter-owned code, compatibility facades, and presentation metadata.

## 10. Common code versus agent-specific code

### 10.1 Decision rule

A behavior belongs in **common code** when it is one of the following:

1. required for ingest correctness regardless of agent;
2. a reusable transport/read semantic;
3. a storage, transaction, scheduling, delivery, or observability mechanism;
4. a cross-agent semantic used by at least two adapters; or
5. an explicit Spaghetti product abstraction with a precise capability contract.

A behavior belongs in an **agent adapter** when it answers one of these questions:

1. Where does this agent store its data?
2. What does a native record mean?
3. Which native identifier represents a session, run, child, team, or artifact?
4. How do multiple native files/rows join into one entity?
5. Is a usage number a delta, cumulative counter, estimate, or exact observation?
6. Which capabilities can this agent truthfully provide?
7. How should a native schema version be decoded or migrated?

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
spaghetti-host   -> engine + store + selected adapters
```

Normative constraints:

- core/engine/store crates must not depend on adapter crates;
- adapter crates depend on model and adapter API, not on `rusqlite`, `notify`, N-API, or UI packages;
- adapters may use source-reader helpers exposed by the adapter API;
- source-specific SQL is allowed only for read-only access to an agent-owned database, never for the Spaghetti database;
- migrations for Spaghetti-owned tables remain centrally owned;
- adding an adapter should require one registry entry, not edits throughout the engine.

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
2. **Search** — FTS or future search indexes derived from canonical content.
3. **Runtime evidence** — append/replace-safe evidence records.
4. **Runtime current state** — deterministic materialized state per session/run/team/member.
5. **Usage contributions and totals** — truthful scope-aware token and cost materializations.
6. **Capability-pack tables** — teams, inboxes, tasks, approvals, and other optional features.
7. **Durable change log** — replayable post-commit changes.

### 17.2 Projection ownership

Adapters do not choose tables. The fact type determines which common projector runs. Agent-specific extension facts use one centrally managed extension store unless and until promoted into a shared pack.

Projection code must be deterministic and versioned. When a projection implementation changes, the engine can rebuild it from source records or canonical facts without requiring the adapter to issue custom SQL.

### 17.3 Contribution-based usage updates

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

### 17.4 Snapshot replacement semantics

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

- one long-lived write connection;
- WAL mode where supported;
- prepared statement caching;
- explicit small transactions for interactive ingest;
- larger bounded transactions for backfill;
- separate read-only/query connections;
- bounded busy retry;
- controlled checkpointing based on WAL size and reader age.

Opening a new SQLite connection per live batch is not the target design.

## 20. Capability model

### 20.1 Why booleans are insufficient

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

### 20.2 Initial capability namespace

```text
history.sessions
history.messages
history.content_blocks
history.timestamps
history.model_identity

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

### 20.3 Capability truth flows through the API

The UI and SDK must not infer support from non-null columns. Query results include quality metadata where relevant, and the adapter manifest is available at runtime. Unsupported values are absent/unknown, not zero.

### 20.4 Capability packs and tests

Declaring a capability pack automatically enables its conformance suite. For example, an adapter declaring `runtime.subagents` must pass:

- parent/child identity;
- child-first and parent-first arrival;
- activity and terminal evidence;
- restart/reconcile;
- unknown event preservation;
- no invented completion;
- cold/live convergence.

## 21. Adapter registry and configuration

### 21.1 Registry

The host constructs the registry from compiled adapters:

```rust
let registry = AdapterRegistry::builder()
    .register(ClaudeCodeAdapter::new())?
    .register(CodexAdapter::new())?
    .register(GrokAdapter::new())?
    .build()?;
```

The common engine uses only the `AgentAdapter` trait. Duplicate IDs or incompatible contract versions fail at startup.

### 21.2 Configuration

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

### 21.3 Discovery

Discovery is explicit and inspectable. An adapter reports:

- candidate source instances;
- why each candidate was selected;
- version/schema evidence;
- inaccessible roots or permission errors;
- conflicting duplicate instances.

The engine does not silently merge source instances merely because paths overlap.

## 22. Agent adaptation plan

### 22.1 Claude Code adapter

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

### 22.2 Codex adapter

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

### 22.3 Grok adapter

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

### 22.4 Future file-backed adapters

A conventional file-backed adapter should normally require only:

1. source discovery;
2. `StreamSpec` declarations using common drivers;
3. native decoders;
4. capability manifest;
5. fixtures and pack tests.

It should not implement a watcher, checkpoint file, SQLite writer, event bus, or retry scheduler.

### 22.5 Future SQLite-backed adapters

An agent such as OpenCode may require a source-owned SQLite reader. Its adapter:

- declares a read-only `SqliteSnapshot` stream;
- provides named source queries and row decoders;
- chooses a trustworthy watermark when available;
- otherwise uses snapshot diff;
- emits the same common facts as file-backed adapters;
- never shares the source database connection with Spaghetti's writer connection.

### 22.6 Future key-value-backed adapters

An agent stored in a VS Code-style state database may use `KeyValueSnapshot`:

- declare exact keys or prefixes;
- parse values in the adapter;
- emit facts and capability quality;
- use revision/fingerprint reconciliation when no change log exists.

This is why `AgentAdapter` cannot be restricted to path classification or JSONL parsing.

## 23. Standard adapter-development workflow

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

### Stage E: projection and capability-pack tests

Run universal history/runtime/usage tests plus every declared pack. The adapter should not need private projection code unless the feature remains a namespaced extension.

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

## 24. Conformance framework

### 24.1 Command

The repository will provide one vendor-neutral command, for example:

```bash
cargo xtask adapter-check --adapter claude-code
cargo xtask adapter-check --adapter codex
cargo xtask adapter-check --adapter grok
cargo xtask adapter-check --all
```

The exact command name may change, but one executable report is required. Broad package-test success is not a substitute for an explicit adapter result.

### 24.2 Mandatory core pack

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

### 24.3 Driver packs

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

### 24.4 Capability packs

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

### 24.5 Differential oracle

For any supported source trace:

```text
live incremental projection
    == fresh full rebuild projection
    == projection after forced reconcile
    == projection hydrated after process restart
```

Equality excludes expected operational metadata such as commit sequence and observation time, but includes all semantic rows, quality classifications, relations, and totals.

## 25. Process and crate architecture

### 25.1 Logical components

The target logical layout is:

```text
crates/
  spaghetti-model/              facts, capabilities, IDs, adapter API
  spaghetti-engine/             supervisor, scheduler, reducers, outbox
  spaghetti-store/              SQLite implementation and migrations
  spaghetti-source/             common file/dir/SQLite/KV drivers
  spaghetti-adapter-claude/     Claude discovery and decoding
  spaghetti-adapter-codex/      Codex discovery and decoding
  spaghetti-adapter-grok/       Grok discovery and decoding
  spaghetti-napi/               thin Node host/facade
  spaghetti-cli/                optional direct Rust host
```

This is a dependency target, not a requirement to split every crate immediately. Implementation may begin as modules inside the existing Rust crate, but module boundaries and dependency tests must match the target. Crates should split when the seams are stable enough to improve compile isolation and ownership.

### 25.2 Persistent engine object

```rust
pub struct ObservationEngine {
    registry: AdapterRegistry,
    supervisor: SourceSupervisor,
    scheduler: IngestScheduler,
    store: SqliteStore,
    publisher: ChangePublisher,
    metrics: ObservationMetrics,
}
```

The object is long-lived. It retains:

- one writer connection;
- prepared statements;
- source catalog and cursor cache;
- active-object scheduling state;
- watcher registrations;
- bounded worker pools;
- reducer caches that can be rehydrated from SQLite;
- subscriber watermarks.

### 25.3 Thin N-API host

The Node layer may expose:

```text
engine.open(config)
engine.start()
engine.stop()
engine.status()
engine.subscribe(cursor, filters)
engine.reconcile(scope)
engine.query(... compatibility facade ...)
```

It must not:

- parse source JSON;
- own source checkpoints;
- maintain token attribution state;
- call one native function per filesystem event;
- issue source-specific write batches;
- synthesize process-local sequence numbers.

High-frequency changes cross N-API in committed batches, not as one callback per raw record.

### 25.4 Embedded and daemon hosting

The engine is library-first so both are possible:

#### Embedded

`field-native` owns the engine directly. This minimizes processes and allows one native infrastructure owner.

#### Standalone

A future `spag-daemon` owns the engine and exposes Unix-socket/named-pipe IPC. CLI, Electron, and other clients share one writer and subscription stream.

The decision to make the daemon mandatory is deferred. Both modes must enforce one owner per database/source set.

### 25.5 Ownership lock

Until a daemon arbitrates ownership, startup acquires a cross-process lock keyed by database identity. Failure returns a structured error containing owner metadata when available. The engine never allows two independent live writers to race on the same source cursors.

## 26. Performance architecture

### 26.1 Optimize work elimination first

The desired hot path is:

```text
one source read
  -> one frame operation
  -> one native JSON/row decode
  -> one FactBatch
  -> one transaction updating many projections
  -> one committed change batch
```

The design explicitly avoids:

- parsing once for history and again for runtime state;
- full-file reparse on normal append;
- per-record database connection setup;
- per-event N-API calls;
- recalculating an entire session's token total for each message;
- multiple overlapping watchers for the same physical root;
- unbounded Tokio tasks or channels.

### 26.2 Watch-root consolidation

The supervisor consolidates overlapping physical roots where semantics and permissions allow, then routes dirty paths to logical streams. Multiple consumers subscribe to one observation engine rather than each creating a watcher.

### 26.3 Bounded concurrency

Separate resource budgets are maintained for:

- source I/O;
- JSON/native decoding;
- agent-owned database reads;
- SQLite commits;
- backfill;
- live interactive streams.

A flood in one adapter cannot allocate unbounded memory or starve all active sessions. Queue saturation escalates to a dirty/reconcile state instead of silently dropping correctness.

### 26.4 Micro-batching

Interactive ingest batches are bounded by both time and size. Backfill uses larger batches. The writer may combine facts from independent source objects in one transaction only if it preserves each object's cursor atomicity and failure reporting.

### 26.5 Compact source-object state

The engine stores one compact state entry per active/discovered source object, not one task or full path copy per record. Paths may use interned segments or stable IDs where profiling justifies it. Optimization must preserve correct non-UTF-8 path identity.

### 26.6 Lazy content hashing

Full-content hashing is not performed on every file event. It is used when required to:

- verify an ambiguous rewrite;
- identify a snapshot revision;
- deduplicate a record without a stable native ID;
- audit a source generation;
- support an adapter that lacks reliable metadata.

Fast metadata and prefix checks are preferred on normal append paths. Hash policy is driver-specific and benchmarked.

### 26.7 Token aggregation complexity

Interactive token updates must be proportional to the changed usage facts. Full session/day/project rebuilds are repair operations, not the default append path.

### 26.8 SQLite tuning

Performance tuning must remain subordinate to transaction correctness. Expected mechanisms include:

- WAL;
- long-lived writer;
- prepared statements;
- page-cache sizing;
- bounded transaction sizes;
- explicit checkpoint policy;
- indexes aligned with fact identity and projection queries;
- avoiding JSON serialization inside repeated SQL loops when a typed binary path is cheaper.

### 26.9 Performance targets

Reference benchmarks will define exact hardware and corpora. Initial product targets are:

- ordinary active-file append to durable commit: **p50 <= 25 ms, p99 <= 100 ms**, excluding delay before the agent flushes to disk;
- no full-file reparse for a valid append continuation;
- no steady-state growth in memory with transcript line count after committed content is released;
- bounded memory under burst and forced queue overflow;
- idle watcher/engine CPU near platform baseline;
- backfill throughput no worse than the current native bulk path on the same corpus;
- live workload must not materially degrade query latency for concurrent readers;
- source notification loss must affect latency, not final correctness.

These targets are gates only after a reproducible benchmark harness exists; they are not claims about all network or virtual filesystems.

### 26.10 Initial implementation choices

The first implementation should prefer mature cross-platform components and replace them only after profiling isolates a backend limitation:

| Concern | Initial choice | Policy |
|---|---|---|
| Filesystem notifications | `notify` | Treat events as dirty hints; direct per-OS backends only after measured need |
| Initial/reconcile traversal | `ignore` parallel walker | Apply source selectors and ignore rules during traversal |
| Native callback ingress | bounded `crossbeam-channel` or equivalent | Callback performs no I/O/parse; full queue marks scope dirty |
| Async control plane | Tokio | Structured cancellation, timers, source DB I/O, host lifecycle |
| Byte ownership | `bytes`/bounded buffers | Release committed payloads; avoid repeated copies |
| JSON decode | `serde_json` first | Consider alternate parser only with representative profile evidence |
| SQLite | `rusqlite` long-lived writer | One writer lane, WAL, prepared statements, separate readers |
| Fingerprints | lazy BLAKE3 | Hash only where metadata/cursor semantics require it |
| Diagnostics | `tracing` plus metrics facade | No raw transcript content in ordinary spans |

Dependency versions are pinned by the repository release process rather than this RFC. The architecture must not depend on undocumented event shapes from a particular watcher crate.

## 27. Observability and operations

### 27.1 Metrics

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
```

### 27.2 Stream state

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

### 27.3 Diagnostics

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

### 27.4 Health API

The host exposes a health snapshot that distinguishes:

- engine/writer health;
- source availability;
- watcher degradation;
- projection lag;
- subscriber lag;
- adapter-format drift;
- quarantined records;
- current replay window.

## 28. Security and privacy

### 28.1 Local-only default

The observation engine performs no network transmission. Adapters read only configured local sources. Any future remote source or telemetry requires a separate explicit design and opt-in.

### 28.2 Least privilege

- source-owned databases are opened read-only;
- source roots are path-confined;
- symlink escape is denied by default;
- Spaghetti database and IPC endpoints use restrictive local permissions;
- adapters receive scoped read capabilities rather than arbitrary filesystem handles;
- custom producers are compiled/trusted in v1.

### 28.3 Sensitive payloads

Transcripts, prompts, tool results, inboxes, and artifacts may contain credentials or private code. Raw-record retention is configurable by stream:

```text
None
HashOnly
DiagnosticExcerpt
Full
```

Canonical history requirements may still require message content storage. The distinction controls duplicate raw/native copies and quarantine payloads.

### 28.4 Log hygiene

Raw content is never included in ordinary logs. Diagnostics use stable record IDs, lengths, hashes, and bounded redacted excerpts. Adapter tests include secret-like fixtures to prevent accidental logging.

## 29. Migration plan

Migration must preserve a working production path at each phase. No phase introduces two live writers for the same database.

### Phase 0 — RFC, baseline, and freeze

- Accept this RFC and mark the affected RFC 009 clauses as superseded.
- Document current cold/live outputs for Claude, Codex, and Grok.
- Build real-corpus and synthetic differential fixtures.
- Freeze new source-specific TypeScript watcher/writer architecture except critical fixes.
- Add architecture lint/tests preventing further direct source-specific SQL hooks.

**Exit:** reviewed invariants, fixture corpus, and baseline reports exist.

### Phase 1 — Extract the persistent engine shell

- Create logical `model`, `engine`, `store`, `source`, and `adapter` module boundaries in Rust.
- Move reusable code out of N-API orchestration modules.
- Introduce open `AdapterId`, registry, manifest, and capability schema.
- Open one long-lived SQLite writer connection.
- Add owner locking and lifecycle/disposal tests.
- Keep current TypeScript live path operational.

**Exit:** a no-op/test adapter can run through discovery, scheduling, commit, and shutdown without Node-owned state.

### Phase 2 — Transactional source catalog and durable outbox

- Add source instances, streams, objects, generations, opaque cursors, commits, and change log.
- Make cursor/projection/outbox advancement one transaction.
- Implement resumable subscription and `ResetRequired` behavior.
- Add crash injection around every transaction stage.
- Existing TypeScript watcher may temporarily submit dirty paths, but not pre-advance durable cursors.

**Exit:** commit-before-publish and restart replay are proven.

### Phase 3 — Common append and snapshot drivers

- Implement `AppendDelimitedFile`, `ReplaceDocument`, `DirectorySnapshot`, and `PresenceObject`.
- Implement watch-before-scan, bounded scheduling, overflow recovery, generation handling, partial-line retry, and quarantine.
- Add driver conformance packs.
- Integrate adaptive polling fallback.

**Exit:** synthetic adapters pass all driver and recovery tests.

### Phase 4 — Claude history and usage in Rust

- Port Claude discovery and transcript decoding into the adapter contract.
- Use the shared append driver for both parent and subagent JSONL.
- Emit canonical history, relations, evidence, and usage facts.
- Replace hot-path token total rebuilds with contribution deltas while retaining audit rebuild.
- Run shadow parity against current cold and live implementations.

**Exit:** Claude history/usage cold-live-reconcile parity is zero on fixtures and accepted real corpora.

### Phase 5 — Claude runtime capability packs

- Add delegation/subagent projections.
- Add teams/config/inbox snapshots.
- Add active-session presence.
- Add tasks/plans/artifacts as capability packs where semantics are sufficiently defined.
- Add late-correlation and conflict tests.
- Switch Claude live observation to the Rust engine.

**Exit:** Claude runtime state survives watcher loss and process restart and does not invent completion.

### Phase 6 — Codex adapter migration

- Port rollout discovery, framing, metadata, messages, and lifecycle decoding.
- Replace `SessionTokenApi`/source-specific SQL hooks with usage facts.
- Make metadata/cursor/projection atomic.
- Add child rollout/delegation only for semantics that pass the pack.
- Shadow, cut over, then remove Codex TypeScript live ingest.

**Exit:** Codex adapter passes core, append, usage, and any declared delegation packs.

### Phase 7 — Grok adapter migration

- Port chat-history append decoding.
- Model summary/events/signals as declared snapshot/dependency streams.
- Implement entity reconciliation for sidecar-first and transcript-first arrival.
- Store usage at truthful scope and quality.
- Shadow, cut over, then remove Grok TypeScript live ingest.

**Exit:** Grok cold/live/sidecar/reconcile parity is proven without separate post-commit sidecar SQL.

### Phase 8 — Rust becomes sole ingest/observation writer

Delete or reduce to compatibility facades:

- TypeScript source watchers;
- TypeScript incremental parsers;
- JSON checkpoint sidecars;
- source-specific `writeBatch` logic;
- token mutation hooks;
- process-local live sequence bus;
- old N-API live-batch row serializer;
- duplicated source classifiers no longer needed by UI.

Retain TypeScript query and public API wrappers as thin calls into Rust/SQLite until a separate query migration is justified.

**Exit:** no TypeScript code reads agent-owned source content for ingest or writes ingest projections.

### Phase 9 — Optional host consolidation

Evaluate, in a separate decision:

- embedding in `field-native`;
- a standalone `spag-daemon`;
- shared IPC protocol;
- multi-client query ownership.

This phase must not alter adapter or transaction semantics.

## 30. Migration compatibility

### 30.1 Database migration

Existing canonical history tables should be preserved. Add provenance and source-object mappings through nullable/backfilled columns or companion tables, then make them required for new Rust commits. Existing rows can be attributed during a controlled rebuild.

### 30.2 API compatibility

The Node SDK continues to expose current session/message/search APIs. Runtime APIs gain capability/quality metadata and resumable cursors. Deprecated process-local `seq` behavior remains only behind a compatibility adapter until consumers migrate.

### 30.3 Checkpoint migration

Legacy checkpoint sidecars are imported as hints, not unquestioned truth. On first Rust ownership:

1. inspect source identity, size, and prefix/fingerprint;
2. compare with durable canonical rows/materialization versions;
3. accept a cursor only if consistency can be proven;
4. otherwise perform bounded reconcile or full reingest.

The imported cursor is stored transactionally in `source_objects`; the sidecar is no longer authoritative.

### 30.4 Rollback

During adapter-specific cutover phases, rollback means stopping the Rust owner and re-enabling the legacy owner after an explicit database compatibility check. Two owners never run simultaneously. Once schema/provenance migrations become irreversible, rollback uses a database backup rather than mixed writers.

## 31. Testing and verification

### 31.1 Unit tests

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

### 31.2 Integration traces

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

### 31.3 Runtime-specific traces

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

### 31.4 Crash injection

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

### 31.5 Fault injection

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

### 31.6 Real-corpus differential tests

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

### 31.7 Property tests

Where practical, generate event traces from a source model and assert:

- duplicate hints do not change final state;
- arbitrary hint loss followed by reconcile converges;
- batching boundaries do not change semantics;
- decode parallelism does not change semantics;
- crash/restart at any transaction boundary converges;
- source-object order is preserved;
- cross-object order is not assumed.

## 32. Acceptance criteria

RFC 010 is complete only when all of the following are true.

### Architecture

- [ ] Rust owns source discovery through durable commit for Claude, Codex, and Grok.
- [ ] Cold/backfill and live use the same adapter decoders and projectors.
- [ ] The core engine has no branches over built-in adapter IDs.
- [ ] Adapters do not import Spaghetti-store SQL, `notify`, N-API, or public event publishers.
- [ ] No source-specific mutable token hook remains.
- [ ] One owner lock prevents concurrent live writers.
- [ ] N-API is a thin host and transports committed batches.

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

### Adapter support

- [ ] Claude passes core, append, snapshot, presence, usage, delegation, and teams packs.
- [ ] Codex passes core, append, usage, and every capability it declares.
- [ ] Grok passes core, append, snapshot/entity-reconcile, usage, and every capability it declares.
- [ ] One vendor-neutral adapter check produces explicit pass/fail reports.
- [ ] Unknown native events are retained with provenance.
- [ ] Capability manifests are surfaced through the SDK.

### Operations

- [ ] Stream state and health are observable.
- [ ] Queue depth, overflows, reconcile, latency, errors, and subscriber lag are measured.
- [ ] Change-log retention and reset behavior are implemented.
- [ ] Raw/error retention follows privacy policy.
- [ ] Shutdown leaves no orphaned watchers, tasks, file handles, database handles, or sockets.

### Performance

- [ ] No normal append causes whole-file reparse.
- [ ] No database connection is opened per live record/batch.
- [ ] No raw watcher event crosses N-API individually.
- [ ] Usage hot path is O(changed usage facts).
- [ ] Reference live-latency, backfill, memory, burst, and query-concurrency benchmarks pass.
- [ ] Correctness remains intact under forced saturation and notification loss.

### Retirement

- [ ] TypeScript source watchers and checkpoint sidecars are deleted.
- [ ] TypeScript source parsers/writers are deleted or retained only for non-ingest tooling with explicit ownership.
- [ ] Process-local live change sequence is retired.
- [ ] Legacy per-source write-batch bridges are retired.
- [ ] RFC 009 documentation is updated to reflect the superseding decision.

## 33. Risks and mitigations

### 33.1 Risk: abstracting before enough agents are understood

A generic adapter API can accidentally encode Claude assumptions.

**Mitigation:** distinguish universal core, optional capability packs, and namespaced extensions; use the rule of two/product-contract review; provide a custom producer escape hatch; validate the API against Claude, Codex, Grok, and at least one non-file-backed source design before declaring it stable.

### 33.2 Risk: over-normalization loses native meaning

A thin common fact may omit vendor-specific fields needed later.

**Mitigation:** ordered content blocks, raw/native provenance, namespaced extension facts, unknown-record preservation, and adapter contract versioning. Promotion to common schema is deliberate and migratable.

### 33.3 Risk: adapters become hidden mini-engines

A permissive adapter can recreate private watchers, cursors, SQL, retries, and state.

**Mitigation:** hard crate dependencies, code-review checklist, architecture linting, bounded `SourceAccess`, one registry, and conformance tests. Custom producers require review and still cannot own Spaghetti storage or public delivery.

### 33.4 Risk: single SQLite writer becomes a bottleneck

Many live sessions and backfill may compete.

**Mitigation:** parallel bounded source read/decode, one efficient prepared writer lane, priority queues, micro-batching, WAL, contribution updates, and backfill throttling. Add sharding only after measurements prove one database cannot meet requirements.

### 33.5 Risk: durable outbox grows indefinitely

A replayable change log consumes storage.

**Mitigation:** bounded retention, oldest-cursor metrics, snapshot/reset protocol, and compaction outside the guaranteed replay window.

### 33.6 Risk: runtime inference becomes misleading

Users may interpret `stale` as completed or presence as proof of active execution.

**Mitigation:** durable observation versus transient assessment, evidence/quality in APIs, explicit terminal precedence, and UI language tied to confidence.

### 33.7 Risk: vendor formats change without notice

A decoder may begin quarantining or misinterpreting records.

**Mitigation:** schema/version detection, unknown preservation, drift metrics, fixture updates, circuit breakers, adapter contract versions, and targeted re-materialization.

### 33.8 Risk: source reads race active writes

Replace-on-write documents may be briefly incomplete; source databases may be locked.

**Mitigation:** stability retries, atomic-replace-neutral snapshot drivers, read-only busy retry, bounded backoff, and cursor advancement only after successful decode/commit.

### 33.9 Risk: dual ownership during migration

Legacy TypeScript and Rust paths could both write.

**Mitigation:** explicit owner lock, feature flag that selects exactly one writer, isolated shadow database, and phase exits that require legacy shutdown before cutover.

### 33.10 Risk: raw retention increases privacy exposure

Unknown/quarantined records may duplicate sensitive content.

**Mitigation:** per-stream raw retention, hash-only option, redacted diagnostics, restrictive database permissions, and no raw payload in logs.

## 34. Alternatives considered

### 34.1 Keep TypeScript watchers and move only parsing/writes to Rust

Rejected as the final architecture. It leaves source scheduling, checkpoints, lifecycle, and cross-source joins split across runtimes, adds serialization overhead, and preserves duplicate ownership. It may be used temporarily during migration by submitting dirty paths to the Rust engine.

### 34.2 Give each adapter an end-to-end pipeline

Rejected. Claude, Codex, and Grok would each implement watcher/scanner, cursor, retry, writer, runtime reducer, token reducer, and event delivery. Correctness fixes would need to be repeated and would drift.

### 34.3 Treat watcher events as the canonical event log

Rejected. Native events can be duplicated, coalesced, lost, reordered, and platform-dependent. They are invalidation hints. Source content and durable cursors are authoritative.

### 34.4 Normalize every native record into one large universal schema

Rejected. Agents differ in content structure, usage granularity, teams, subagents, sidecars, and source storage. A forced schema would either become Claude-shaped or lose information. The accepted design uses a thin core, capability packs, and extensions.

### 34.5 Store only current runtime state

Rejected. Without evidence/provenance, state cannot be audited, rebuilt, or corrected after adapter changes. Durable evidence plus a materialized current view provides both speed and repairability.

### 34.6 Publish only in-memory events

Rejected. Process restart creates gaps and sequence reset. A durable outbox permits replay and defines commit-before-publish precisely.

### 34.7 Use a separate runtime database

Rejected initially. History, runtime evidence, usage, cursors, and outbox need an atomic commit boundary. Splitting stores would require distributed transaction or compensating recovery. Read scaling can use WAL/read replicas or a later derived cache.

### 34.8 Start with a dynamic adapter plugin ABI

Rejected for v1. Rust ABI stability, migration authority, source permissions, and untrusted code make this a separate problem. Built-in adapters establish the contract first. A future plugin system may use a process boundary and versioned wire protocol rather than a Rust dynamic-library ABI.

### 34.9 Depend on Watchman as the engine

Rejected as a mandatory dependency. Watchman can be an optional source-driver backend for suitable environments, but it does not replace agent decoding, source database support, transactional cursors, projections, or the durable outbox. Shipping another daemon also complicates packaging and ownership.

### 34.10 Model silence as completion

Rejected. A quiet transcript may represent thinking, waiting, a blocked tool, a crashed process, a disconnected source, or completion. Only explicit evidence can create a durable terminal state.

## 35. Resolved design questions

### Q1. Is runtime observation a third plane?

No. It is a projection of live disk/source ingestion. The implementation has one ingest spine and multiple projections.

### Q2. Who owns filesystem watching?

The common Rust source supervisor. Adapters declare streams and selectors.

### Q3. Who owns source JSON parsing?

The Rust agent adapter. The common append driver frames records but does not interpret JSON.

### Q4. Who owns SQLite writes?

The common Rust store/projectors. Adapters emit facts only.

### Q5. Can an adapter read a source-owned SQLite database?

Yes, through bounded read-only source-driver/access APIs. The prohibition is against adapter-owned writes to the Spaghetti database.

### Q6. How are agent-specific fields retained?

Through raw/native provenance and namespaced extension facts. They are promoted into common core or capability packs only by review.

### Q7. How are tokens normalized?

Adapters declare subject, scope, accounting mode, and quality. The common reducer computes idempotent contributions and totals without inventing precision.

### Q8. How are cross-file subagents or sidecars ordered?

They are not assigned a fabricated global source order. Facts use stable IDs, per-object cursor order, commit order, source timestamp metadata, and late correlation.

### Q9. Is the daemon required?

No. The engine is library-first. Embedded and standalone hosting share the same semantics. One-owner enforcement is required in either mode.

### Q10. Are third-party adapters supported?

Not through a stable plugin ABI in this RFC. New built-in adapters follow the same contract. A safe external adapter protocol is future work.

## 36. Deferred follow-up RFCs

The following are explicitly outside this RFC and should not block it:

1. mandatory standalone daemon and multi-client IPC protocol;
2. third-party adapter process/plugin protocol;
3. Rust ownership of all query APIs;
4. remote source ingestion and synchronization;
5. encrypted-at-rest raw transcript storage;
6. database sharding or distributed materialization;
7. generalized cost pricing/version feeds;
8. user-defined extension-fact projectors;
9. source-driver support for remote object stores.

## 37. Implementation checklist by module

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
- [ ] adapter diagnostics/error classes.

### `spaghetti-source`

- [ ] discovery coordinator;
- [ ] watch-before-scan supervisor;
- [ ] append-delimited driver;
- [ ] replace-document driver;
- [ ] directory-snapshot driver;
- [ ] presence driver;
- [ ] read-only SQLite driver;
- [ ] key-value driver;
- [ ] bounded `SourceAccess`;
- [ ] polling fallback;
- [ ] generation and identity detection;
- [ ] non-UTF-8 path identity;
- [ ] cancellation/disposal.

### `spaghetti-engine`

- [ ] per-object ordered scheduler;
- [ ] priority/fairness policy;
- [ ] bounded coalescing;
- [ ] adapter registry;
- [ ] decode runner and panic/error boundary;
- [ ] entity reconciliation;
- [ ] runtime evidence reducer;
- [ ] usage contribution reducer;
- [ ] projection version planner;
- [ ] durable publisher/replay;
- [ ] health and metrics;
- [ ] owner lock and lifecycle.

### `spaghetti-store`

- [ ] source catalog migrations;
- [ ] provenance/idempotency keys;
- [ ] ingest commits;
- [ ] durable change log;
- [ ] quarantine store;
- [ ] canonical fact projectors;
- [ ] runtime and usage tables;
- [ ] capability-pack tables;
- [ ] snapshot replacement;
- [ ] generation replacement;
- [ ] WAL/read connection policy;
- [ ] crash injection hooks;
- [ ] audit/full-rebuild comparison.

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
- [ ] shadow parity report;
- [ ] contract version/migration notes.

### Host and SDK

- [ ] persistent engine lifecycle;
- [ ] batched committed changes;
- [ ] resumable cursors;
- [ ] capability exposure;
- [ ] health/status APIs;
- [ ] compatibility query facade;
- [ ] no source parsing/checkpoints in TypeScript;
- [ ] clean disposal tests.

## 38. Appendix A — illustrative adapter declaration

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

## 39. Appendix B — illustrative commit batch

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

## 40. Appendix C — architecture review rubric for new common abstractions

Before adding a common driver, fact, capability pack, or reducer, answer:

1. Is the behavior required for engine correctness, or is it native interpretation?
2. Do at least two adapters need equivalent semantics?
3. If only one adapter needs it, is Spaghetti deliberately defining a product-level capability?
4. Can it remain a namespaced extension without harming the product?
5. Does the proposed abstraction preserve uncertainty, scope, and native detail?
6. Can cold, live, restart, and reconcile share it?
7. Can it be tested independently of a vendor?
8. Does it force a source adapter to import store, watcher, host, or event-delivery code?
9. Would adding a fourth adapter require changing this abstraction or only registering new code?
10. Is its schema/version migration centrally owned?

A proposal that fails questions 1–4 belongs in the adapter. A proposal that fails questions 5–10 is not ready for common code.

## 41. Final position

Spaghetti's durable product is no longer merely a table of parsed transcripts. It is a local, continuously convergent observation system over heterogeneous agent-owned sources.

The architecture therefore centers on four separations:

```text
source mechanics     != native interpretation
native interpretation != common semantic facts
semantic facts        != materialized projections
committed projections != transient delivery
```

Rust owns the full correctness envelope around those boundaries. Adapters remain narrow enough to add agents without cloning the engine, but expressive enough to handle JSONL, replaceable snapshots, sidecars, source databases, and key-value state without lying about equivalence.

The decisive test is not whether Spaghetti can watch another file. It is whether a new agent can be adapted by declaring sources, decoding native records, emitting truthful facts, and passing shared conformance—without adding another watcher, checkpoint system, writer, token accumulator, or event bus.
