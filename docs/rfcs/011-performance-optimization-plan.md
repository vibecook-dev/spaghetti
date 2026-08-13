# RFC 011 performance optimization design and execution plan

Status: active; first optimization wave implemented and measured on 2026-08-13

This record turns the RFC 010/011 performance requirements into a reproducible
baseline, documents the production-path findings and rejected spikes, and
defines the next storage and bootstrap changes. It does not weaken the RFC's
atomic cursor/fact/projection/outbox boundary, sole-writer rule, query-only read
pool, or readiness contract.

## Decision summary

The poor cold-start result was not evidence that Rust parsing is intrinsically
slow. The production engine was doing excessive SQLite work:

- append transactions were artificially capped at 64 records;
- same-generation appends repeatedly searched for old generations;
- every transcript commit invoked unrelated replace-document projectors, some
  of which scanned the transcript object's complete and growing fact ledger;
- repeated row-level statements and aggregate reads were not coalesced;
- SQLite ran with a very small default cache and checkpoint cadence;
- startup reported ready before all bounded backlog passes had converged; and
- the committed ingestion benchmark exercised the retired bulk/oracle path,
  not the production observation host.

The first wave fixes those issues without changing durable semantics. On the
same deterministic 16,384-record production-path corpus, honest convergence
improved from 30.24 seconds to 2.08 seconds at the fastest implementation point,
a 14.5x speedup. The final checkpoint-balanced configuration takes 2.58 seconds,
still an 11.7x improvement, while also meeting the live tail-latency gates below.
The no-op projector dispatch fix alone reduced 65,536 records from 24.44 seconds
to 13.43 seconds and 131,072 records from 89.09 seconds to 42.14 seconds before
the checkpoint trade-off was selected.

The remaining curve is still superlinear: in the final configuration an 8x
increase from 16,384 to 131,072 records takes about 18.8x as long. This is not
accepted as the final backfill architecture. Measurement attributes the
remaining cost primarily to maintaining a wide normalized schema and its
B-trees one row at a time.

## Contract and gates

RFC 010 section 26.9 and RFC 011 section 27.9 remain authoritative:

- ordinary active-file append to durable commit: p50 <= 25 ms and p99 <= 100
  ms, excluding time before the agent flushes;
- valid appends do not reparse the complete file;
- memory stays bounded as committed transcript length grows;
- backfill is no slower than the accepted native bulk path on the same corpus;
- concurrent live ingestion does not materially degrade query latency; and
- notification loss affects latency, not eventual correctness.

The optimization work adds these engineering gates:

1. `bench:observation` must measure the production
   adapter -> driver -> fact -> projection -> SQLite path and must assert true
   convergence before stopping its clock.
2. Same-host medians at 16k and 64k may not regress by more than 10% over the
   accepted report without an explained semantic or durability change.
3. The 4x cold-load scale ratio `T(64k) / T(16k)` must reach <= 5.25 before the
   synthetic scale gate is accepted. The current 6.46 ratio fails this gate.
4. Live latency is evaluated over at least 100 appends in one long-lived host;
   p50, p95, and p99 are reported. Recreating a database per sample is invalid.
5. The real-corpus gate compares both engines on identical selected files,
   retention policy, durable outputs, and cold filesystem state. The retired
   tiny-fixture benchmark is not a substitute.
6. Reports include input bytes/records, facts, commits, canonical rows,
   change-log rows/bytes, main DB/WAL bytes, peak RSS, and query latency with
   and without concurrent backfill.
7. A database is never reported ready while backlog, FTS/index finalization,
   recovery, or an integrity validation remains.

Wall-clock thresholds are reference-machine gates, not portable CI constants.
CI should enforce correctness and relative/scaling budgets; a maintainer-owned
reference runner accepts absolute release thresholds.

## Reproducible baseline

Reference machine for the measurements below:

- Apple M1 Max MacBook Pro, 10 cores, 64 GiB RAM;
- macOS 26.5.2 arm64;
- Node 26.5.0, pnpm 11.13.1, Rust 1.97.1;
- release N-API build with the bundled `libsqlite3-sys` 0.28 line;
- synthetic Claude JSONL with one deliberately large append object, alternating
  user/assistant messages, deterministic identities, and exact assistant usage.

Command shape:

```sh
pnpm --filter @vibecook/spaghetti-sdk-native build
pnpm bench:observation --records 16384 --runs 3 --warmup 1
pnpm bench:observation --records 65536 --runs 1 --warmup 0
pnpm bench:observation --scenario live-append --records 8192 \
  --append-records 1 --runs 100 --warmup 10
```

### Cold-load results

| Implementation point                         | Records |          Time | Records/s | Commits |           DB |
| -------------------------------------------- | ------: | ------------: | --------: | ------: | -----------: |
| Original production path, honest convergence |  16,384 |       30.24 s |       542 |     257 |     97.9 MiB |
| Generation gating only                       |  16,384 |       26.86 s |       610 |     257 | about 98 MiB |
| 1,024-record bounded commits                 |  16,384 |       10.55 s |     1,553 |      17 | about 98 MiB |
| Statement/aggregate/coalescing wave          |  16,384 |        2.65 s |     6,186 |      17 |     98.2 MiB |
| Unrelated-projector fast paths               |  16,384 |        2.08 s |     7,880 |      17 |     98.3 MiB |
| Before unrelated-projector fast paths        |  65,536 |       24.44 s |     2,681 |      65 |    390.4 MiB |
| After unrelated-projector fast paths         |  65,536 |       13.43 s |     4,882 |      65 |    390.4 MiB |
| Before unrelated-projector fast paths        | 131,072 |       89.09 s |     1,471 |     129 |    781.0 MiB |
| After unrelated-projector fast paths         | 131,072 |       42.14 s |     3,111 |     129 |    781.0 MiB |
| Final 32 MiB checkpoint policy               |  16,384 | 2.58 s median |     6,349 |      17 |     98.2 MiB |
| Final 32 MiB checkpoint policy               |  65,536 |       16.66 s |     3,934 |      65 |    390.5 MiB |
| Final 32 MiB checkpoint policy               | 131,072 |       48.48 s |     2,704 |     129 |    780.9 MiB |

The final live results used one long-lived host and native query-pool validation
after every sample:

| Append size | Samples |     p50 |     p95 |     p99 |      Max |
| ----------- | ------: | ------: | ------: | ------: | -------: |
| 1 record    |     100 |  6.0 ms |  6.5 ms |  8.2 ms |   8.5 ms |
| 64 records  |     100 | 19.1 ms | 85.5 ms | 91.4 ms | 100.5 ms |

The reports are generated outside the repository under `/private/tmp` so they
do not become machine-specific release claims. A reviewed reference report can
be committed once the corpus and runner are selected.

## Findings

### F1 — the old benchmark measured the wrong engine

`bench:ingest` measures the retired native bulk/oracle path and a small fixture.
It cannot explain production observation-host behavior. The new
`bench:observation` opens the real sole-owner host, drives real adapters and
source drivers, distinguishes ready time from convergence, and fails if the
expected canonical message count is incomplete.

### F2 — readiness was not convergence

The coordinator bounds one reconcile pass at 4,096 records. Startup could
publish ready after the first bounded pass and then wait through retry delay
before processing the remaining object. The supervisor now represents
`backlog_remaining`, immediately drains known bounded work, and does not
publish ready until startup is quiescent. Transient and incomplete failures
retain backoff.

### F3 — transaction granularity dominated the initial path

The common append driver already allowed 1,024 records or 8 MiB, but the
coordinator split commits at 64 records. Restoring the 1,024-record cap cut the
16k run from 26.86 seconds to 10.55 seconds while preserving bounded memory,
cursor atomicity, and retry identity. The fact batch capacity is 8,192 facts.

### F4 — same-generation appends performed replacement work

Canonical, usage, evidence, delegation, artifact, and workflow projections
looked for rows whose generation differed on every ordinary append. Composite
`(source_object_id, source_generation)` indexes now exist where replacement
uses that predicate, and the commit context explicitly identifies a generation
replacement so same-generation commits skip retraction work.

### F5 — shared dispatch made unrelated projectors scan growing history

The symbolized writer profile showed transcript commits spending roughly half
their samples in team, task, presence, memory, settings, tool-result, artifact,
workflow, and session-index maintenance despite having no facts of those
kinds. Several cleanup statements then searched all `fact_records` owned by
the transcript object to delete zero rows.

Each replace-document projector now returns early only when both conditions
hold: the incoming batch has no matching fact and the source object owns no
prior assertion of that kind. Empty deletion snapshots and generation rewrites
therefore retain their semantics. Transcript-driven joins for session indexes
and persisted tool results still run when their changed keys require them, but
they no longer claim ownership of unrelated sidecars.

### F6 — row-level avoidable work compounded

The writer now keeps a prepared-statement cache, prefetches existing message
and usage identities once per batch, avoids empty dependency deletes, skips
child-table replacement for genuinely new messages, accumulates usage totals
once per affected session, and coalesces repeated session/run projection facts
inside a transaction. The fact ledger remains complete.

### F7 — SQLite defaults were inappropriate for the database size

The sole writer now uses a bounded 128 MiB cache, 256 MiB mmap ceiling,
memory-backed temporary storage, a 32 MiB WAL autocheckpoint target, and a 64
MiB journal-size limit. Readers use a bounded 32 MiB cache, the same mmap
ceiling, query-only mode, and statement caches. The writer asks SQLite's
`PRAGMA optimize` to refresh bounded planner statistics when a long-lived
connection opens.

### F8 — physical amplification is now the main bottleneck

At 131,072 synthetic records the database contains 589,824 facts, or 4.5 facts
per source record. The 781 MiB file consists of about 426 MiB of table B-trees,
352 MiB of secondary/automatic B-trees, and 3 MiB of canonical FTS storage.
Secondary B-trees are therefore about 45% of the file. The largest objects are
`fact_records`, `canonical_messages`, their identity/activity indexes,
canonical content blocks and indexes, `change_log`, usage contributions, and
run evidence.

`fact_records` also repeats source object, generation, cursors, hash,
observation time, and commit metadata for every emitted fact. Its average
entity key is about 107 bytes even though HashOnly payload bodies are correctly
omitted. Rust made decoding and control flow cheaper; it cannot make 10+
durable B-tree mutations per logical record free.

### F9 — change-log retention must be batch-aware

Retaining at least 1,024 commits was reasonable for tiny interactive commits
but could pin gigabytes after widening backfill commits. The minimum resumable
window is now 128 commits; age and byte limits still protect low-volume live
use. A future bootstrap epoch should publish snapshot/reset semantics rather
than retaining one public change per historical row.

### F10 — engine metrics are below the RFC requirement

The external benchmark now measures the production path, but the engine still
needs internal histograms for source read, decode, fact construction,
projection families, SQLite step/commit/checkpoint time, rows changed, WAL
bytes, writer queue delay, and reader age. Without those metrics, regressions
still require stack sampling to localize.

### F11 — two SQLite implementations in one process are unsafe instrumentation

The first long-lived live benchmark opened `node:sqlite` after every native
commit to read counters. The Node runtime and Rust module each statically or
dynamically provide a distinct SQLite implementation. On macOS the benchmark
reproducibly terminated with `SIGBUS` in the native writer when the second
implementation closed a connection and invalidated the process-scoped POSIX
locking/shared-memory assumptions. The crash report showed a WAL shared-memory
mapping page-in past EOF.

The harness now queries per-sample counts through the native read pool and opens
`node:sqlite` only after the entire native host has disposed. This is both less
intrusive and architecturally correct. SQLite explicitly identifies multiple
copies linked into one application as a corruption hazard; the benchmark is not
allowed to violate a rule that production correctly enforces.

## Controlled spikes and decisions

| Spike                                                                                               | Result                                                                                                                                       | Decision                                                                                                                  |
| --------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| Generation-aware cleanup plus aligned indexes                                                       | 16k: 30.24 -> 26.86 s before batching                                                                                                        | Landed; required for rewrite correctness at scale                                                                         |
| 64 -> 1,024 record commits                                                                          | 16k: 26.86 -> 10.55 s                                                                                                                        | Landed; matches existing 8 MiB bound                                                                                      |
| Statement reuse, prefetch, coalescing, usage delta flush                                            | 16k: 10.55 -> 2.65 s                                                                                                                         | Landed                                                                                                                    |
| Unrelated-projector fast paths                                                                      | 64k: 24.44 -> 13.43 s; 131k: 89.09 -> 42.14 s                                                                                                | Landed                                                                                                                    |
| Defer selected query indexes and canonical FTS                                                      | 64k: 9.79 s ingest + 3.18 s rebuild = 12.98 s; 131k: 26.36 s + 5.89 s = 32.25 s                                                              | Promising only above a threshold; design writer-owned bootstrap mode                                                      |
| Rebuild deferred objects through a second SQLite connection while native readers/writer remain open | Isolated spike produced invalid cached-schema/root-page state                                                                                | Rejected; DDL/finalization must be sole-writer-owned with readers quiesced                                                |
| Raise WAL autocheckpoint from ~64 MiB to ~1 GiB                                                     | 64k regressed from 13.43 s to 18.77 s                                                                                                        | Rejected; close-time checkpoint burst outweighed reduced checkpoint frequency                                             |
| Tune live WAL checkpoint cadence                                                                    | ~64 MiB gave 64-record p99 143.9 ms; ~16 MiB gave p99 69.1 ms but 64k cold 18.45 s; ~32 MiB gave 100-sample p99 91.4 ms and 64k cold 16.66 s | Landed ~32 MiB as the measured balance; retain adaptive/background checkpointing as future work                           |
| Standalone Rust delta reducer for run state                                                         | 131k was neutral (41.78 vs 42.14 s) and smaller cases regressed                                                                              | Reverted; revisit only with schema-level, set-oriented arrangements                                                       |
| Open `node:sqlite` for metrics while the native owner remains live                                  | Reproducible macOS `SIGBUS` in the native writer after 18–19 commits                                                                         | Rejected; all live metrics and queries cross the native read pool, and offline SQLite inspection waits for owner disposal |

The bulk spike is deliberately not exposed as a production flag. Its lifecycle
is incomplete until crash recovery, reader quiescence, durable readiness, FTS
validation, and subscription reset behavior are implemented together.

## Verification completed

The first wave was validated across the native engine, SDK boundary, retained
cross-engine oracles, and the packaged playground topology:

- `cargo fmt --all -- --check`, workspace/all-feature `cargo check`, and
  workspace/all-target/all-feature Clippy with warnings denied passed;
- the complete Rust workspace test run passed all 513 tests, including schema
  rebuild compaction, cleanup query-plan selection, retention codecs,
  projection replacement, recovery, cancellation, and query-pool coverage;
- `pnpm typecheck`, `pnpm lint`, `pnpm format:check`, and `pnpm validate`
  passed, with zero architecture-boundary violations;
- SDK package tests passed 311 tests with seven intentional skips, and all 110
  CLI tests passed;
- the small, medium, Codex, and Grok observation differential suites matched
  their retained oracles exactly;
- all 12 query-conformance groups passed on the 3-project, 20-session,
  300-message fixture, including a 12 MiB response-boundary case;
- `pnpm build` rebuilt the native module, SDK, CLI, and playground; and
- a post-build Electron utility-process smoke completed the native handshake,
  observed exactly 3 projects, 20 sessions, and 300 messages, recovered after
  request cancellation, and kept each single fixture query below 3 ms. That
  one-run smoke is topology evidence, not a release latency percentile.

The cold-load and 100-sample live distributions in the baseline section are
the performance evidence. The remaining failed 4x scale-ratio gate is retained
as an explicit open condition rather than hidden by the passing correctness
matrix.

## Target architecture

### A. Interactive mode remains fully indexed

Live appends keep all constraints, writer-critical arrangements, query indexes,
and FTS triggers available. They use bounded low-latency commits and publish
changes immediately. No synchronous setting is weakened, and no query performs
repair.

### B. Large cold bootstrap becomes a durable engine state

For a new/rebuildable database whose estimated append input exceeds a measured
threshold (provisionally 64 MiB or 100,000 records), the owner may enter
`bootstrap_building` before query workers become available:

```text
acquire owner and writer connection
persist bootstrap epoch/state
retain UNIQUE/FK and writer-critical indexes
defer an approved list of query-only indexes and canonical FTS triggers
ingest bounded cursor/fact/projection transactions
rebuild indexes and FTS on the same writer connection
PRAGMA optimize
checkpoint under the reader-free bootstrap policy
validate counts, FTS coverage, foreign keys, and quick_check
atomically mark projections ready and bootstrap complete
start query workers and publish ready
```

The deferred list is explicit and query-plan-tested. Generation-retraction,
fact identity, usage-series, reducer, foreign-key, and uniqueness indexes are
never dropped merely because a load is large.

If the process crashes with the marker set, the next owner does not serve
queries. It verifies the schema and either resumes finalization or recreates
the rebuildable structures before clearing readiness. Subscribers receive a
post-bootstrap snapshot watermark/reset boundary rather than millions of
historical row notifications.

The 64k spike saved only 3%; the 131k spike saved 23%. Bootstrap mode must
therefore be size-gated and benchmarked on the real corpus rather than applied
to every startup.

### C. Normalize source-record provenance in the next schema

Schema v41 should store source-record identity/provenance once:

```text
source_records
  record_id INTEGER PRIMARY KEY
  source_instance_id / stream_id / object_id / generation
  cursor_start / cursor_end / payload_hash / observed_at / commit_seq
  UNIQUE(source_object_id, generation, cursor_start, cursor_end, payload_hash)

fact_records
  fact identity
  record_id -> source_records
  local ordinal / fact kind / entity key / optional retained payload
```

Derived rows should reference a narrow fact/record surrogate where it reduces
foreign-key and secondary-index width while preserving the stable external
fact identity. The migration remains a rebuild of the local derived cache, as
RFC 011 already allows. `WITHOUT ROWID` and integer-surrogate alternatives must
be benchmarked because random hash primary keys can trade space for insert
locality.

### D. Move projection application toward sets and affected keys

The next projection layer stages one commit's facts in bounded temporary or
in-memory tables and performs `INSERT ... SELECT`, keyed upserts, and reductions
once per affected entity. Fact-kind/source ownership is computed once and
routed only to relevant projectors. Reducer arrangements store the minimum
state needed to apply deltas; full scans remain explicit repair/rewrite paths.

This follows the useful part of modern incremental view maintenance: propagate
changed keys and weighted/delta contributions, not entire histories. It does
not require adopting a distributed dataflow runtime inside the desktop engine.

### E. Make checkpointing adaptive only after reader telemetry exists

The measured 32 MiB checkpoint target remains. Both the 16 MiB and 64 MiB
alternatives lost an important workload gate, and a much larger fixed threshold
was worse. Future control should observe WAL bytes, write rate, oldest reader
age, and shutdown/finalization state. A pinned reader is allowed to delay a
checkpoint, but the engine must expose it and demonstrate that WAL space is
reclaimed after the reader releases.

## Execution plan

### P0 — first optimization wave (implemented)

- production-path cold/warm/live benchmark and percentile output;
- truthful startup convergence across bounded backlog passes;
- 1,024-record/8 MiB bounded append commits;
- generation-gated replacement cleanup and composite indexes;
- long-lived statement caches and bounded SQLite cache/mmap/checkpoint policy;
- batch prefetch, no-op delete elimination, session/run coalescing, and
  per-session usage flush;
- source-ownership fast paths for unrelated replace-document projectors;
- batch-aware change-log resumable window;
- schema tests that require the generation-cleanup query plans to use their
  composite indexes.

### P1 — observability and accepted baselines

- expose commit-stage histograms and row/write counters from Rust;
- record WAL, checkpoint, RSS, queue delay, and oldest-reader metrics;
- run 100+ one-host live appends and burst tests;
- run cold/warm/rewrite/crash tests on Claude, Codex, and Grok fixtures;
- select a redacted representative large corpus and capture both legacy-bulk
  and observation-host reports on the reference runner;
- add query latency with concurrent ingest and pinned-reader WAL recovery.

Exit: the RFC p50/p99 and apples-to-apples bulk comparison are measured rather
than inferred.

### P2 — writer-owned bulk bootstrap

- add durable bootstrap epoch/state and projection readiness;
- do not start query workers until finalization completes;
- classify and query-plan-test writer-critical versus rebuildable structures;
- drop/rebuild only through the writer connection;
- rebuild canonical FTS, run `PRAGMA optimize`, checkpoint, and validate;
- implement crash/restart at every lifecycle boundary;
- compact bootstrap notification semantics to a snapshot watermark/reset.

Exit: >=100k real-corpus loads improve materially, <=64k loads do not regress,
and no partial index/FTS state can be served.

### P3 — normalized provenance schema

- spike record/fact key layouts and `WITHOUT ROWID` candidates;
- land `source_records` plus narrow fact references behind a schema rebuild;
- remove proven redundant indexes only after `EXPLAIN QUERY PLAN` and query
  benchmark coverage;
- track DB+WAL bytes per source record and per fact.

Exit: lower storage/write amplification with byte-for-byte query and replay
parity.

### P4 — set-oriented projection batches

- stage typed facts and affected keys once per transaction;
- replace repeated row loops with bounded set operations where measurements
  show a win;
- persist reducer arrangements that make ordinary work proportional to deltas;
- preserve full reducers for audit, generation replacement, and recovery.

Exit: `T(64k) / T(16k) <= 5.25`, then tighten toward linear as the real corpus
confirms it.

### P5 — release acceptance

- full Rust, SDK, IPC, playground, parity, corruption/recovery, and package
  gates;
- reference cold/live/burst/query-concurrency reports reviewed by a maintainer;
- update the Phase 10 closure ledger without reintroducing any legacy owner.

## Research basis

The plan uses primary sources and separates applicable principles from systems
that are too large for this embedded engine:

- SQLite's [transaction FAQ](https://www.sqlite.org/faq.html) explains why
  grouping writes in transactions dominates per-statement micro-optimization.
- SQLite's [query planner guide](https://www.sqlite.org/queryplanner.html),
  [`EXPLAIN QUERY PLAN`](https://sqlite.org/eqp.html), and
  [`PRAGMA optimize` guidance](https://www.sqlite.org/lang_analyze.html) support
  aligned multi-column indexes, plan verification, and bounded statistics
  maintenance.
- SQLite's [WAL documentation](https://www2.sqlite.org/wal.html),
  [PRAGMA reference](https://www.sqlite.org/pragma.html), and
  [mmap guidance](https://www.sqlite.org/mmap.html) support measured checkpoint,
  durability, cache, and mapping policies rather than assuming larger values
  are always faster.
- SQLite's [corruption guidance](https://www.sqlite.org/howtocorrupt.html)
  explains why multiple linked copies of SQLite cannot safely coordinate their
  process-global POSIX-lock bookkeeping. This directly governs Node/native test
  instrumentation as well as production ownership.
- SQLite FTS5's [rebuild command](https://www.sqlite.org/fts5.html) provides the
  correct primitive for a writer-owned deferred FTS lifecycle.
- PostgreSQL's [bulk population guidance](https://www.postgresql.org/docs/17/populate.html)
  supports building secondary indexes after a large load when the lifecycle
  can keep the database unavailable until finalization.
- Materialize's [arrangements](https://materialize.com/docs/get-started/arrangements/)
  and [view model](https://materialize.com/docs/concepts/views/),
  [DBSP](https://arxiv.org/abs/2203.16684),
  [Differential Dataflow](https://www.cidrdb.org/cidr2013/Papers/CIDR13_Paper111.pdf),
  and [Timely Dataflow](https://research.google/pubs/incremental-iterative-data-processing-with-timely-dataflow/)
  motivate maintaining reusable keyed state and propagating deltas. Spaghetti
  adopts those principles in bounded SQLite transactions; it does not import a
  distributed streaming architecture without evidence.

## Non-negotiable guardrails

- no `synchronous=OFF` for the production live engine;
- no unbounded transaction or unbounded in-memory batch;
- no second SQLite implementation or direct connection to the database while
  the native owner is live;
- no readiness before bootstrap/recovery/index/FTS convergence;
- no dropping writer-critical or correctness indexes for headline throughput;
- no query-time repair or second TypeScript database authority;
- no optimization accepted from a tiny fixture or a different durable output.
