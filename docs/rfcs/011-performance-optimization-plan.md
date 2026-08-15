# RFC 011 performance optimization design and execution plan

Status: original implementation accepted on 2026-08-13; corpus-shaped follow-up
optimization and frozen-corpus rerun accepted on 2026-08-14; evidence-led
ingest/finalization architecture revision accepted on 2026-08-15

This record turns the RFC 010/011 performance requirements into a reproducible
baseline, documents the production-path findings and rejected spikes, and
records the accepted storage/bootstrap decisions and remaining evidence-gated
opportunities. It does not weaken the RFC's atomic cursor/fact/projection/outbox
boundary, sole-writer rule, query-only read pool, or readiness contract.

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

The first-wave curve was still superlinear: an 8x increase from 16,384 to
131,072 records took about 18.8x as long. The subsequent measured pass aligned
the run-state indexes with the reducer's actual ordering and removed or
narrowed secondary indexes that had no distinct production query consumer.
The pre-batching lifecycle build's same-host medians were 1.591 seconds at
16,384 records and 8.299 seconds at 65,536 records, a 5.217x ratio that passed
the <= 5.25 synthetic scale gate and stayed within the 10% instrumentation
budget. A 131,072-record continuation completed in 21.22 seconds, versus the
earlier 48.48-second checkpoint-balanced baseline.

The subsequent P3/P4 audit found that the generic fact ledger retained a
second semantic entity-key copy with no production query consumer, and that
fact, canonical-message, and new content-block rows were still issued one at a
time. The accepted implementation omits only that redundant ledger copy and
uses bounded multi-row upserts while retaining every fact identity and source
provenance field. Five-run same-host medians are now 1.176 seconds at 16,384
records and 6.140 seconds at 65,536 records, a passing 5.220x ratio. That is
about 26% faster at both sizes than the pre-batching lifecycle build; database
size falls from 94.5/375.4 MiB to 86.4/343.0 MiB.

The first truthful frozen-corpus bootstrap completed 3.86 GB of source in
634.37 seconds, with ready time equal to convergence time, zero source retries,
1,112,311 decoded records, 1,072,944 canonical messages, 5,138,709 facts, and
69,041 commits. The earlier 1,075.56-second run was not a valid baseline: it
returned ready at 565.51 seconds, repaired work afterward, and still omitted
29,850 session records. The P3/P4 production revision preserves the complete
output and zero-retry result at 574.80 seconds, 9.4% faster, while reducing the
durable database from 9,808.2 MiB to 9,214.1 MiB, 6.1% smaller.

The 2026-08-15 investigation stopped using final database size as a proxy for
latency and instrumented each physical ingest/finalization phase directly. On
the current immutable 3,706.5 MiB corpus, accepted statement reuse,
message-owned activity evidence, deferred artifact reduction, and a clean
bootstrap/recovery validation split reduced a fully converged run to 121.85
seconds. It retained 1,122,456 decoded native records, 1,082,909 canonical
messages, 2,604,162 emitted facts, 35,007 commits, zero retries, all mandatory
foreign-key/FTS readiness audits, and a zero-frame final WAL. The durable
ledger contains 1,512,372 rows because 1,091,790 message facts now own their
identical native-activity observation instead of writing a second provenance
row. Schema v44 forces stale caches through this rebuild.

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
   synthetic scale gate is accepted. The first-wave 6.46 ratio failed; the
   bootstrap-lifecycle 5.217 ratio and the stronger five-run set-write 5.220
   ratio pass.
4. Live latency is evaluated over at least 100 appends in one long-lived host;
   p50, p95, and p99 are reported. Recreating a database per sample is invalid.
5. The real-corpus gate compares production revisions on identical selected
   files, retention policy, durable outputs, and cold filesystem state. A
   retired engine with different durability or output semantics is diagnostic
   only; the tiny-fixture benchmark is not a substitute.
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
| Run-state reducer indexes, 3-run median      |  16,000 |        1.74 s |     9,218 |      17 |    104.0 MiB |
| Run-state reducer indexes, 3-run median      |  64,000 |       12.13 s |     5,276 |      64 |    413.9 MiB |
| Final aligned/pruned indexes, 3-run median   |  16,384 |       1.555 s |    10,539 |      17 |     94.5 MiB |
| Final aligned/pruned indexes, 3-run median   |  65,536 |       8.098 s |     8,093 |      65 |    375.5 MiB |
| + writer-owned checkpoints, 3-run median     |  16,384 |       1.559 s |    10,512 |      17 |     94.5 MiB |
| + writer-owned checkpoints, 3-run median     |  65,536 |       8.007 s |     8,185 |      65 |    375.5 MiB |
| + writer-owned checkpoints, continuation     | 131,072 |       20.36 s |     6,438 |     129 |    751.2 MiB |
| + bounded source telemetry, 3-run median     |  16,384 |       1.662 s |     9,858 |      17 |     94.5 MiB |
| + bounded source telemetry, 3-run median     |  65,536 |       8.597 s |     7,623 |      65 |    375.5 MiB |
| + bounded source telemetry, continuation     | 131,072 |       21.22 s |     6,176 |     129 |    751.2 MiB |
| Final bootstrap lifecycle, 3-run median      |  16,384 |       1.591 s |    10,301 |      17 |     94.5 MiB |
| Final bootstrap lifecycle, 3-run median      |  65,536 |       8.299 s |     7,897 |      65 |    375.4 MiB |
| + compact ledger/set writes, 5-run median    |  16,384 |       1.176 s |    13,931 |      17 |     86.4 MiB |
| + compact ledger/set writes, 5-run median    |  65,536 |       6.140 s |    10,674 |      65 |    343.0 MiB |

The earlier final-lifecycle reports measured process-level peak RSS at 334.4
MiB, 618.9 MiB, and 664.4 MiB for 16k, 64k, and 131k respectively. The latest
five-run reports measured 301.1 MiB at 16k and 556.0 MiB at 64k. The sampler
includes the Node harness and corpus generation, so this is not
native-allocation-only telemetry. The bounded multi-row values remain inside
the existing 1,024-record/8 MiB transaction envelope; they do not create an
unbounded staging area.

The first-wave checkpoint-policy live results used one long-lived host and
native query-pool validation after every sample:

| Append size | Samples |     p50 |     p95 |     p99 |      Max |
| ----------- | ------: | ------: | ------: | ------: | -------: |
| 1 record    |     100 |  6.0 ms |  6.5 ms |  8.2 ms |   8.5 ms |
| 64 records  |     100 | 19.1 ms | 85.5 ms | 91.4 ms | 100.5 ms |

After native instrumentation and the final aligned/pruned index pass, the same
long-lived-host protocol produced:

| Append size | Samples |    p50 |     p95 |     p99 |     Max |
| ----------- | ------: | -----: | ------: | ------: | ------: |
| 1 record    |     100 | 2.2 ms |  2.5 ms |  3.2 ms |  3.2 ms |
| 64 records  |     100 | 9.1 ms | 33.9 ms | 38.4 ms | 42.1 ms |

The final bootstrap-lifecycle build repeated the 100-sample one-record gate at
p50 1.3 ms and p99 2.1 ms; all samples were converged when refresh returned.

The bounded set-write build repeated the gate twice. The accepted repeat was
p50 1.5 ms, p95 1.7 ms, p99 3.0 ms, and max 30.8 ms. The isolated maximum is
the same periodic writer-owned checkpoint band observed in earlier burst
tests; the distribution remains far below the RFC's 100 ms p99 budget.

A 65,536-record warm reopen measured 7.7 ms median readiness and 103.2 ms
median full discovery convergence. It read zero source records and made zero
SQLite row changes.

The 64-record distribution shows a periodic 31–42 ms band when the next write
queues behind a completed writer-owned checkpoint, but remains within the
RFC's 100 ms p99 gate. A four-worker, ten-reader live-refresh run measured
reader p95 at 3.157 ms idle and 3.804 ms during ingest (1.20x), with the refresh
committing in 11.031 ms.

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
memory-backed temporary storage, a 32 MiB writer-owned WAL checkpoint target,
and a 64 MiB journal-size limit. SQLite's implicit autocheckpoint hook is
disabled so it cannot hide checkpoint work inside commit timing or compete
with the owner policy. Readers use a bounded 32 MiB cache, the same mmap
ceiling, query-only mode, and statement caches. The writer asks SQLite's
`PRAGMA optimize` to refresh bounded planner statistics when a long-lived
connection opens.

### F8 — physical amplification is now the main bottleneck

The pre-pruning physical audit at 131,072 synthetic records contained 589,824
facts, or 4.5 facts per source record. Its 781 MiB file consisted of about 426
MiB of table B-trees, 352 MiB of secondary/automatic B-trees, and 3 MiB of
canonical FTS storage. Secondary B-trees were therefore about 45% of the file.
The largest objects were `fact_records`, `canonical_messages`, their
identity/activity indexes, canonical content blocks and indexes, `change_log`,
usage contributions, and run evidence. The final audited index set reduces the
same 131k database to 751 MiB without changing its 589,824-fact output.

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

### F10 — bounded native telemetry now localizes the remaining curve

The owner exposes owner-lifetime fixed-bucket histograms and counters through
the existing native `getStats` query. The write lane reports queue delay, disk
reserve, commit stages, individual projection families, successful/failed
commits, facts, public changes, and SQLite rows changed. The read lane reports
queue depth/high-water, rejections, queue wait, execution latency, and oldest
active reader age. The source lane reports common-driver reads, failures,
genuine retries, bounded continuations, records/payload bytes, decode outcomes,
facts, quarantines, total decode time, the adapter call, and fact identity/
provenance construction. It exposes at most 128 adapter/stream/driver
dimensions plus one fixed overflow lane. The same snapshot includes physical
DB, WAL, and shared-memory bytes. Recording retains no per-request samples and
opens no instrumentation connection.

The production-path benchmark persists this snapshot and also samples peak
process RSS. At 65,536 records, source reads took 109.8 ms, complete decode
662.3 ms, and fact construction 107.3 ms (the last is a measured subset of the
adapter call). History/fact projection took 3,271.1 ms and SQLite commit took
1,473.4 ms. This directly rules out source I/O and JSON decoding as the main
bottleneck and keeps storage amplification and set-oriented projection as the
next targets. Remaining RFC-level observability gaps are cancellation and
subscriber-lag counters and native allocation/RSS accounting.

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

### F12 — every secondary index needs a demonstrated consumer

The schema accumulated indexes that were superseded by a wider prefix, used no
production SQL path, or covered columns that the source-statistics query did
not need. They consumed space and forced an additional B-tree mutation for
every matching historical row.

The final pass replaces the wide source-instance/fact covering index with a
compact source-instance index and removes the superseded run-order,
message-session-order, content-block-run, and usage-session indexes. The
remaining usage-session-time index covers both usage query shapes. Production
queries have `EXPLAIN QUERY PLAN` assertions for the new run-state ordering,
source statistics, and usage paths, and all 12 end-to-end query-conformance
groups pass. Existing schema-v40 databases receive the same repair through
idempotent `DROP INDEX IF EXISTS`/`CREATE INDEX IF NOT EXISTS` initialization;
they do not need a semantic schema-version bump.

### F13 — checkpoint ownership must be explicit and nonblocking

SQLite's autocheckpoint callback ran inside `COMMIT`, so the engine could see
neither checkpoint count and duration nor the reader that prevented progress.
It also left performance attribution ambiguous: the earlier 64k report charged
3,335 ms to SQLite commit even though much of that time was checkpoint work.

The writer now disables the implicit hook, checks physical WAL size after a
successful commit, and schedules a controlled checkpoint at 32 MiB. The
checkpoint runs on the sole writer lane after the durable receipt is returned.
It first performs a nonblocking PASSIVE copy, then attempts TRUNCATE with a zero
busy timeout; a reader can delay reclamation but never stalls behind a five
second busy handler. Incomplete checkpoints retry on subsequent commits no
more frequently than every 50 ms, and shutdown performs one final attempt after
query workers close.

Fixed telemetry reports attempts, completions, failures, reader-blocked
attempts/time, last log/checkpointed/remaining frames, and latency. A real
query-pool read transaction test pins a WAL generation, observes incomplete
progress and positive oldest-reader age, releases it, then proves a second
checkpoint reaches zero remaining frames and a zero-byte WAL. On cold 16k/64k
runs the policy completed 4/4 and 25/25 checkpoints respectively; the 131k
continuation completed 57/57. It preserves the scale gate and improves live
64-record p99 from 38.7 to 37.5 ms in the measured run.

### F14 — retry acknowledgement must match the scheduled scope

The frozen corpus contains 39 session transcript objects larger than the
coordinator's 4,096-record reconcile budget. Retry targets were correctly
object-specific, but `reconcile_declared_object` acquired an instance-scoped
observation lease. Completing the first object therefore acknowledged every
sibling object whose dirty sequence predated that lease. A later full scan
could rediscover some omitted ranges, which made repeated refreshes appear to
converge while still leaving output incomplete.

The lifecycle now has an explicit object reconcile scope. Success acknowledges
only that exact `(adapter, instance, stream, object)` target; failure and
further bounded backlog requeue the same object. A deterministic supervisor
test uses 17 sibling files, each with 4,097 records, and requires every durable
cursor to reach EOF across more than one 16-pass wake budget. The benchmark's
32-object/131,104-record release gate produces all 131,104 messages before
readiness with zero retries. The first lifecycle build took 16.45 seconds; the
set-write build takes 11.84 seconds.

### F15 — query bootstrap requires an observation pause barrier

Query index and FTS finalization took about 37 seconds on the frozen corpus.
During that interval the live supervisor's periodic polling backstop could
start another full reconcile. SQLite remained safe because the writer
serialized commands, but the host could start readers and return while that
reconcile was still active. The benchmark readiness assertion caught this as
`reconciling` with a pending full repair instead of silently timing the repair.

Bootstrap completion now drains and pauses every native supervisor before the
sole writer changes journal mode or rebuilds query structures. Watchers remain
registered and continue admitting bounded dirty state while paused. After
finalization, each worker resumes, drains all changes admitted during the
barrier, and resets its polling deadline before query workers start. A focused
test proves that a source mutation admitted while paused is invisible during
finalization and durable before resume completes.

### F16 — normalizing source records alone does not reduce this schema

The 64k page audit attributed 80.7 MiB to `fact_records`, 13.5 MiB to its
identity B-tree, and 11.8 MiB to its two required ownership indexes. Raw fact
entity keys accounted for 31.5 MiB, but only 10.1 MiB after deduplication,
which made a dictionary or source-record table look attractive in isolation.

The actual `source_records` spike retained the stable external fact identity,
normalized the record provenance once, and added the minimum record/fact
indexes needed by ownership and retraction. Its tables and indexes occupied
89.91 MiB versus 89.88 MiB for the current fact/source structures. The narrow
record row was offset by a new fact-to-record key, uniqueness B-tree, and
ownership indexes. It would also add 65,536 inserts and joins to the measured
workload. The rewrite is therefore rejected, not deferred as an assumed win.
Any future surrogate design must benchmark the complete tables and indexes and
must narrow downstream foreign keys as well; normalizing one repeated prefix
is insufficient.

The audit did identify one safe physical redundancy. Canonical and assertion
projections retain their semantic entity keys and point to the generic ledger
by `fact_id`; no production SQL reads `fact_records.entity_key`. New writes now
store that nullable ledger column as `NULL` while preserving fact ID, kind,
source instance/stream/object/generation, cursor range, record hash, local
ordinal, observation time, retained payload policy, and commit sequence. An
entity-omission-only 64k spike was runtime-neutral and reduced the database
from 375.5 MiB to 342.9 MiB, 8.7%. Existing databases remain readable and
correct; the space is reclaimed on the next rebuild rather than by a risky
in-place rewrite.

### F17 — bounded multi-row writes reduce SQLite and FTS amplification

The remaining history path executed one SQL statement for every fact,
canonical message, and content block. The writer now emits multi-row fact
upserts in chunks of 512, canonical-message upserts in chunks of 256, and
new-message content-block inserts in chunks of 512. Full-size SQL shapes use
the connection's statement cache; variable tails are prepared without
polluting it. Owned parameter buffers are bounded by the existing
1,024-record/8 MiB transaction limits. Existing messages and duplicate message
keys retain the sequential delete/replacement path so last-write semantics do
not change.

Coalescing canonical-message upserts also lets FTS5 amortize trigger/segment
work. At 64k the production path now reports 724,910 SQLite row changes rather
than roughly 1.14 million and a 22.3 MiB WAL rather than roughly 43 MiB. The
five-run medians are 1.176/6.140 seconds at 16k/64k with a passing 5.220x scale
ratio, while exact Claude, Codex, and Grok differentials preserve cold, live,
reconcile, generation-replacement, and restart semantics. On the frozen 3.86
GB corpus, ready remains equal to convergence at 574.80 seconds with zero
retries, and the database is 6.1% smaller.

### F18 — the frozen corpus rejects oversized physical transactions

The next corpus-shaped profile reached 285.9 seconds for 1,118,236 records,
5,166,425 facts, 34,909 logical commits, 424 physical transactions, and
9,701,080 SQLite row changes. Source reads and decoding still overlapped the
writer and accounted for only 27.5 and 31.9 seconds. Physical transactions,
checkpoints, finalization, and projection writes remained the limiting path.

A controlled attempt to keep more objects resident, wait longer for group
formation, and admit up to 131,072 facts per physical transaction reduced the
physical transaction count from 424 to 164 but regressed wall time from 285.9
to 379.75 seconds. Physical transaction time rose to 201.8 seconds, SQLite
commit time to 84.2 seconds, checkpoints to 129.2 seconds, finalization to 58.9
seconds, and peak RSS to 5.55 GiB. This is direct evidence that fewer/larger
transactions are not intrinsically better for the production database and
that the single-object synthetic gate cannot choose corpus transaction policy.
The experiment was reverted in full.

### F19 — larger pages and exclusive bootstrap locking lost their gates

An 8 KiB page-size spike reduced a 131k/4,096-object database from 613.4 MiB to
589.3 MiB but slowed it from 14.88 to 15.68 seconds. The default 4 KiB page size
therefore remains selected. A bootstrap-only `locking_mode=EXCLUSIVE` /
`cache_spill=OFF` spike failed the query-bootstrap lifecycle test because the
writer retained a lock when the read pool configured its page cache. It was
also reverted; startup correctness is not traded for a speculative lock
optimization.

### F20 — Claude repeated object declarations on every transcript record

Claude differed from the Codex and Grok adapters by emitting a `SessionFact`
and `RunFact` on almost every transcript line, plus a `DelegationFact` on every
subagent line. Those are object declarations, not distinct per-message
observations. A fixed 67-byte, versioned decoder state now emits each
declaration once per object generation, while still emitting session
enrichment when cwd, branch, first prompt, or title metadata changes. Message,
usage, activity-evidence, raw-retention, cursor, and provenance behavior stays
unchanged. The Claude contract version is bumped so existing objects replay
from a safe generation boundary.

On the 131,072-record/4,096-object gate, facts fell from 589,824 to 335,872,
SQLite row changes from 1,081,773 to about 827,900, database size from 613.2 to
557.1 MiB, and peak RSS from 1,536 to about 1,419 MiB. The first controlled run
improved from 16.38 to 14.45 seconds; a subsequent three-run median was 13.16
seconds. The one-object continuation improved from about 18.9 to 12.41
seconds. A 100-sample one-record live run remained well inside the RFC gate at
p50 1.6 ms and p99 7.2 ms. Small and medium Claude differentials are exact
across cold, live, reconcile, generation replacement, and restart.

Making the declarations stateful exposed two correctness bugs that are now
fixed. An overflow split could previously commit decoder state from the next,
not-yet-committed record; the state/cursor boundary now uses the state before
that record. Parent and subagent transcripts could also race to own a shared
session row; the reducer now deterministically prefers the parent transcript
and uses stable object identity for peer ties.

### F21 — disk pressure invalidated the initial follow-up claim

The reference volume is 99% full with only about 14 GiB available. A retained
private benchmark directory occupies about 20 GiB, including two stale ~8.4
GiB derived databases. SQLite commit, checkpoint, and finalization timing at
that free-space level is not comparable to earlier runs, and a fresh complete
database plus WAL cannot be created with a safe reserve. The declaration-state
change therefore remained accepted on synthetic, live, and differential gates,
but its frozen-corpus wall-clock claim was held pending cleanup and an identical
rerun. F22 records that rerun after the stale artifacts were removed.

### F22 — measured critical path and deferred foreign-key enforcement

After removing stale build artifacts, the current 3,699.3 MiB fixture contains
1,101,389 JSONL records and produces 1,118,373 native records, 1,078,900
canonical messages, 2,595,192 facts, and 34,929 logical commits. A storage-free
ablation retained discovery, reads, decode, fact construction, and scheduling
but completed in 32.59 seconds. The unchanged production path took 211.33
seconds on the first cold run and 176.64 seconds after filesystem caches had
warmed, proving that at least 84.6% of the first run was persistence work and
also proving that one-run wall deltas are not sufficient evidence.

New non-overlapping finalization telemetry explains 161.88 of the 176.64
seconds (91.6%): 105.43 seconds in physical write transactions, 24.68 seconds
in checkpoints, and 31.77 seconds in finalization excluding its two checkpoint
calls. Transaction time was dominated by canonical projection (32.08 seconds),
runtime projection (31.50 seconds), preparation (14.49 seconds), and SQLite
commit (22.97 seconds). Finalization spent 19.36 seconds validating foreign
keys, database structure, and FTS; deferred indexes took 6.07 seconds, FTS
rebuild 4.86 seconds, and planner optimization 1.44 seconds. Disabling all
checkpoints was rejected after it failed to become ready in 628 seconds and
grew a 17 GiB WAL.

The completed database is 7.55 GB from 3.17 GB of decoded payload. Offline
`dbstat` attributes 2.75 GB to `canonical_messages`, more than 1 GB to its
indexes, and about 849 MiB to `run_evidence` plus its observed-state effects.
All 1,087,781 run-evidence rows on this corpus are the same
`activity_observed/native_activity` assertion for only 5,178 runs; messages
already carry the same run, timestamp, fact identity, and provenance. A
runtime-stage ablation reduced wall time by 36.95 seconds and the database by
848.9 MiB, but the time result is only an upper bound because that diagnostic
also relaxed foreign keys. Replacing duplicate activity facts with
message-derived evidence remains the highest-value schema experiment, not an
accepted semantic change.

A clean A-B-A experiment then disabled immediate foreign-key enforcement only
during the reader-free cold build while retaining the exhaustive final audit.
Controls took 176.64 and 179.46 seconds; the treatment took 168.96 seconds,
9.09 seconds (5.1%) faster than the control mean with identical durable counts
and a passing 9.62-second final audit. Physical transaction work fell from
about 105 seconds to 92.56 seconds; checkpoints absorbed 5.31 seconds of that
gain. Fault injection proves an orphan still leaves the bootstrap marker set
and blocks readiness. Immediate enforcement is restored before live writers
or query readers are admitted. The production implementation then completed
the same corpus in 167.62 seconds with exact counts, zero retries, a passing
9.15-second foreign-key audit, 9/9 checkpoints, and a zero-byte final WAL.

### F23 — compact run-evidence reductions remove per-message projection amplification

The F22 runtime ablation was only an upper bound, so the follow-up preserved
every `RunEvidenceFact` and its durable `fact_records` provenance. It changed
only the query projection: `run_evidence` now retains one exact winning fact,
evidence count, and maximum activity time per
run/source-object/generation/kind/strength. Runtime evidence counts sum those
counts, decisive evidence still resolves to the original fact ID and native
metadata, and generation replacement deletes the old summaries before the
same bounded reducer runs. The observed-state foreign key cascades winner-ID
updates during live ingest; cold bootstrap still performs its exhaustive
foreign-key audit before readiness.

A focused regression proves cursor-ranked decisive evidence with out-of-order
timestamps, independent maximum activity, terminal precedence, generation
replacement, exact count preservation, and a clean `foreign_key_check`.
Runtime-query fixtures separately prove that one compact row reports all seven
represented evidence facts. The 361-test production-feature native suite, the
546-test all-feature native suite, and all 313 active SDK tests pass. Medium
Claude, Codex, and Grok remain exact across cold, live, reconcile, generation
replacement, and restart. The small Claude corpus retains only its previously
recorded `canonical_sessions` differential; its cold state is exact and the
mismatch is outside runtime evidence.

The production comparison froze one immutable 3,701.6 MiB corpus containing
1,102,826 JSONL records. A treatment-control-treatment sequence produced the
same 1,119,826 native records, 1,080,334 canonical messages, 2,598,391 facts,
34,950 commits, zero retries, and zero final WAL in every run. The control took
208.92 seconds; treatments took 177.42 and 165.56 seconds, averaging 171.49
seconds. That is 37.43 seconds (17.9%) below control. Both treatments bracket
the control, so the result does not depend on one favorable cache ordering.

The causal counters match the schema hypothesis. Dedicated run-state work
fell from 8.545 seconds to a 1.139-second treatment mean (-86.7%); total runtime
projection fell from 36.283 to 25.675 seconds. SQLite row changes fell from
6,708,798 to a 5,625,456 mean, almost exactly the eliminated per-message
projection writes. The database fell from 7,555,858,432 bytes to a
6,677,094,400-byte treatment mean (-878,764,032 bytes, 11.6%). Physical
transaction time fell from 117.14 to a 99.04-second mean, while foreign-key
validation fell from 11.03 to 7.75 seconds because it audits a much smaller
index graph. This accepts compact projection rows; eliminating the durable
activity facts themselves remains a separate experiment because they are the
audit ledger and stable public evidence identities.

### F24 — content-block identity normalization saves space but misses the wall gate

The next audit found that `canonical_message_content_blocks` repeated stable
message, session, and run identities averaging 154, 86, and 82 bytes on the
131,072-record fixture. Its table and indexes occupied 105.4 MiB. An offline
whole-schema spike replaced the message owner with an integer surrogate and
removed the unused run identity, reducing that projection to 28.1 MiB while
retaining a narrow covering session-facet index. A more aggressive 6.5 MiB
shape derived session scope through the message table, but it was rejected
before production testing: on one 131,072-message session, the two mandatory
facet queries regressed from about 14.3 ms combined to about 97.7 ms p95.

The query-safe shape passed 362 native tests, the TypeScript rebuild test, and
exact projection/replacement/FTS checks. Interleaved 131k controls and
treatments were effectively neutral, so the production decision used a new
immutable 3,704.8 MiB corpus with 1,104,445 JSONL records. Every run produced
1,121,471 native records, 1,081,951 canonical messages, 2,602,011 facts,
34,979 commits, zero retries, and zero final WAL. Treatment-control-treatment
took 177.26, 168.01, and 165.02 seconds. The 171.14-second treatment mean is
3.13 seconds (1.9%) slower than control, so the experiment is rejected.

The stage counters explain the miss. The treatment database mean was
6,290,843,648 bytes, 375.5 MiB (5.9%) below control, and finalization improved
by 4.33 seconds. However, physical transaction work increased by 3.40 seconds,
including 1.85 seconds more canonical projection and 1.70 seconds more SQLite
commit time; checkpoint work also increased by 2.61 seconds. Returning and
propagating generated surrogates cost more on this write path than the smaller
derived B-trees saved. Schema v44 and the surrogate projection were therefore
reverted in full. Future identity normalization must allocate narrow owners
without a per-message `RETURNING` path and must win wall time, not only storage.

### F25 — reusing fact identity also fails the wall gate

The follow-up removed the generated-surrogate mechanism from F24. Content
blocks instead reused each canonical message's already-known 32-byte fact ID,
so the writer needed neither `RETURNING` nor a new allocation. The schema still
removed the unused run identity and kept the direct, narrow session-facet
indexes. Existing-message lookup retained the previous fact owner so duplicate
messages, cross-source corrections, and generation replacement could remove
the exact superseded blocks. All 362 native tests, the TypeScript schema
rebuild test, correction/retraction regressions, and the facet query-plan test
passed.

A clean 131,072-message, one-session treatment-control-treatment gate rejected
the design before another multi-gigabyte run. Treatments took 9.21 and 9.20
seconds; the unchanged v43 control took 8.93 seconds. The 9.205-second
treatment mean is 0.279 seconds (3.1%) slower. Database size fell from 464.7
MiB to a 395.85 MiB treatment mean (-68.85 MiB, 14.8%), but canonical
projection increased from 1.794 to a 1.925-second mean and SQLite commit from
2.250 to a 2.653-second mean. Counts remained exact at 131,072 messages,
327,682 facts, and 128 commits with zero retries and zero final WAL.

The result isolates the failed premise: generated-ID return was not the only
reason F24 lost. On this write path, reducing content-block key width and final
database size does not necessarily reduce transaction time. The fact-ID schema
v44 was reverted in full. Further work must decompose canonical projection and
commit amplification directly instead of inferring their cost from final
B-tree size.

### F26 — direct decomposition replaces storage-shape intuition

The next pass added non-overlapping writer telemetry for history preparation,
fact storage, canonical-message writes, projection walking, content blocks,
delegation probes/assertions/reductions, and artifact
preparation/assertions/reductions/cleanup. A complete 3,706.5 MiB immutable
corpus run decoded 1,122,456 records into 2,604,162 facts and 1,082,909
messages across 35,007 commits with zero retries. Its 160.43-second wall time
contained 87.93 seconds of physical transactions, 33.01 seconds of bootstrap
finalization, 28.99 seconds of checkpointing, 25.03 seconds of canonical
projection, 22.71 seconds of SQLite commit, 21.86 seconds of runtime
projection, and 14.58 seconds of prepare work. Source read and decode took
about 25 and 29 seconds and overlapped the writer; they were substantial but
not the five-minute explanation.

History/fact work decomposed to 11.18 seconds of fact storage, 5.78 seconds of
canonical-message storage, 2.48 seconds of the projection walk, 1.59 seconds
of preparation, and 1.39 seconds of content-block writes. Delegation's initial
gate cost only 0.019 seconds; its 9.90-second total was real assertion and
correlation work, not an accidentally expensive no-op probe. Artifact work
was the more important architectural smell: 293,487 metadata assertions and
22,244 content assertions caused about 72,282 incremental reductions and
roughly 1.2 million cumulative metadata rows to be materialized while only
35,135 final artifact keys and 293,487 final metadata rows existed.

This decomposition also found stable artifact SQL being prepared repeatedly.
Switching those operations to the writer's bounded statement cache passed the
artifact correctness suite. Full-corpus treatment-control-treatment runs took
156.79, 153.24, and 144.75 seconds. Treatment mean was 150.77 seconds, 2.47
seconds (1.6%) faster than control; artifact time fell by 4.54 seconds and
physical transactions by 3.02 seconds. The change is accepted. The larger
reduction ablation was retained only as an upper bound until F28 supplied a
crash-safe implementation.

### F27 — a message can own its identical native-activity evidence

Every ordinary adapter message emitted both a durable `Message` fact and an
`ActivityObserved`/`NativeActivity` evidence fact with the same source record,
run, and qualified timestamp. The message itself proves that exact activity.
The common writer now aliases only an unambiguous one-message/one-evidence
pair: the message fact remains the durable provenance owner, while the compact
`run_evidence` row keeps the adapter's evidence kind, strength, native state,
source time, activity maximum, and evidence count. Standalone activity (for
example Codex token-count events), different timestamps, and ambiguous pairs
retain independent fact rows. Public evidence identities remain valid fact
IDs; the decisive fact kind is allowed to be `message`.

A focused regression proves paired ownership, standalone retention,
ambiguity fallback, projected evidence dimensions, and a clean foreign-key
audit. The 131,072-message treatment-control-treatment gate took 7.37, 7.71,
and 6.82 seconds; treatment mean was 8.0% faster, removed exactly 131,072
durable rows and row changes, and reduced the database by 30.4 MiB.

The production gate used one release binary and the isolated
`activity-evidence-ownership` control switch. Treatments took 135.77 and
132.66 seconds; control took 147.65 seconds. The 134.22-second treatment mean
is 13.44 seconds (9.1%) faster. It removed exactly 1,091,790 fact rows and
about 252 MiB while retaining 2,604,162 emitted facts, 1,082,909 messages,
35,007 commits, and zero retries. Fact storage fell from 11.09 to about 7.14
seconds, physical transactions by about 3.0 seconds, and SQLite row changes
from 5,635,926 to about 4,544,112. Schema v44 intentionally rebuilds v43
caches so this ownership rule applies uniformly.

### F28 — cold artifact state is reduced once at the readiness boundary

During reader-inaccessible bootstrap, artifact assertion tables are now the
sole durable source of truth and incremental canonical reduction is deferred.
Finalization collects each surviving artifact key and its own maximum
assertion commit sequence, clears the derived table, and reduces every final
key exactly once in one atomic transaction. Live ingest remains incremental.
The rebuild is idempotent: a crash before commit rolls it back; a crash after
commit but before readiness repeats the same clear/rebuild because the durable
bootstrap marker remains. Reader admission, foreign-key validation, and FTS
publication occur only afterward.

Unit and writer-lifecycle regressions prove that canonical rows remain absent
while queries are deliberately unavailable, the final resolved/captured state
and per-artifact provenance are exact, a second rebuild is identical, recovery
repeats safely, and foreign keys are clean. Production
treatment-control-treatment took 133.53, 132.85, and 128.50 seconds. Treatment
mean was 131.01 seconds, 1.84 seconds (1.4%) faster despite source-I/O noise.
The causal counters were stable: artifact projection fell by 4.77 seconds,
physical transactions by 3.73 seconds, incremental reduction disappeared,
and the complete 35,135-key rebuild cost only about 0.90 seconds. SQLite row
changes fell by about 148,700 and the final database by about 2 MiB. This
accepts the assertion-first/final-reduction architecture rather than the
semantically incomplete no-reduction ablation.

### F29 — clean bootstrap and crash recovery need different integrity gates

Finalization telemetry showed a serial 30.8-second tail: foreign-key audit
about 6.3 seconds, `quick_check` 5.6 seconds, FTS rebuild/integrity 7.6 seconds,
deferred indexes 5.7 seconds, checkpoints 3.5 seconds, and `optimize` 1.2
seconds. The same live writer had just created every page in an uninterrupted
fresh database and SQLite had reported every write, commit, checkpoint, and
DDL operation successful. Re-reading the complete file with `quick_check`
before the mandatory semantic audits duplicated work. A bootstrap recovered
from a durable marker is different: the prior process may have exited during
file mutation and must retain the structural scan.

Clean finalization therefore defers only `quick_check`; it still requires the
complete foreign-key audit (enforcement was disabled during cold writes), FTS
integrity, deferred structures, checkpoints, and readiness publication.
Startup recovery still runs all three integrity families. Tests assert the
normal phase set omits only `quick_check`, the recovery phase retains it, and
foreign-key fault injection still blocks readiness.

The same-binary production treatment-control-treatment sequence took 136.26,
136.76, and 121.85 seconds. The first treatment had anomalously slow source,
FTS, FK, and optimize scans, but treatment mean was still 129.06 seconds, 7.70
seconds (5.6%) below control. The isolated control scan cost 5.33 seconds; the
normalized closing treatment reduced finalization from 30.89 to 25.62 seconds.
All runs retained exact facts/messages/commits, zero retries, mandatory FK/FTS
audits, and zero final WAL. The clean/recovery split is accepted.

## Controlled spikes and decisions

| Spike                                                                                               | Result                                                                                                                                       | Decision                                                                                                                    |
| --------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| Generation-aware cleanup plus aligned indexes                                                       | 16k: 30.24 -> 26.86 s before batching                                                                                                        | Landed; required for rewrite correctness at scale                                                                           |
| 64 -> 1,024 record commits                                                                          | 16k: 26.86 -> 10.55 s                                                                                                                        | Landed; matches existing 8 MiB bound                                                                                        |
| Statement reuse, prefetch, coalescing, usage delta flush                                            | 16k: 10.55 -> 2.65 s                                                                                                                         | Landed                                                                                                                      |
| Unrelated-projector fast paths                                                                      | 64k: 24.44 -> 13.43 s; 131k: 89.09 -> 42.14 s                                                                                                | Landed                                                                                                                      |
| Defer selected query indexes and canonical FTS                                                      | 64k: 9.79 s ingest + 3.18 s rebuild = 12.98 s; 131k: 26.36 s + 5.89 s = 32.25 s                                                              | Promising only above a threshold; design writer-owned bootstrap mode                                                        |
| Rebuild deferred objects through a second SQLite connection while native readers/writer remain open | Isolated spike produced invalid cached-schema/root-page state                                                                                | Rejected; DDL/finalization must be sole-writer-owned with readers quiesced                                                  |
| Raise WAL autocheckpoint from ~64 MiB to ~1 GiB                                                     | 64k regressed from 13.43 s to 18.77 s                                                                                                        | Rejected; close-time checkpoint burst outweighed reduced checkpoint frequency                                               |
| Tune live WAL checkpoint cadence                                                                    | ~64 MiB gave 64-record p99 143.9 ms; ~16 MiB gave p99 69.1 ms but 64k cold 18.45 s; ~32 MiB gave 100-sample p99 91.4 ms and 64k cold 16.66 s | Landed ~32 MiB as the measured balance; retain adaptive/background checkpointing as future work                             |
| Standalone Rust delta reducer for run state                                                         | 131k was neutral (41.78 vs 42.14 s) and smaller cases regressed                                                                              | Reverted; revisit only with schema-level, set-oriented arrangements                                                         |
| Query-plan-aligned run-state reducer indexes                                                        | 64k: 13.62 -> 11.02 s in the controlled one-run comparison; reducer: 1,423.6 -> 386.8 ms; initial 16k/64k medians 1.74/12.13 s               | Landed; both reducer queries are `EXPLAIN QUERY PLAN` tested                                                                |
| Remove superseded indexes and narrow the source-statistics covering index                           | Final 16k/64k medians: 1.555/8.098 s (5.209x); DB: 94.5/375.5 MiB; 131k continuation: 20.08 s and 751.0 MiB; all 12 query groups pass        | Landed; same-version repair removes old shapes, while source-statistics and usage queries retain tested indexed plans       |
| Replace hidden autocheckpoint with writer-owned nonblocking checkpoint                              | 16k/64k: 1.559/8.007 s (5.138x), 4/4 and 25/25 complete; live p99: 2.5 ms/37.5 ms; pinned-reader recovery reaches a zero-byte WAL            | Landed; checkpoint progress and reader-blocked time are bounded telemetry, and shutdown retries after query-pool closure    |
| Add bounded source-pipeline and adapter/stream telemetry                                            | 16k/64k: 1.662/8.597 s (5.173x); 131k: 21.22 s; live p99: 3.2/38.4 ms; 64k read/decode totals: 109.8/662.3 ms                                | Landed; remains inside both regression gates and proves storage/projection, not source decoding, dominates                  |
| Open `node:sqlite` for metrics while the native owner remains live                                  | Reproducible macOS `SIGBUS` in the native writer after 18–19 commits                                                                         | Rejected; all live metrics and queries cross the native read pool, and offline SQLite inspection waits for owner disposal   |
| Give object retry work an instance-scoped lifecycle lease                                           | Frozen corpus silently lost sibling targets; even post-ready repair omitted 29,850 records                                                   | Fixed with exact object-scoped acknowledgement and 17/32-sibling regression gates                                           |
| Let supervisors poll during deferred-index/FTS finalization                                         | Readiness guard observed an active full reconcile after the writer cleared bootstrap                                                         | Fixed with native drain/pause/finalize/resume/drain barrier; watchers continue admitting changes                            |
| Durable bootstrap on the frozen 3.86 GB corpus                                                      | 634.37 s ready/converged, 1,112,311 records, 5,138,709 facts, 69,041 commits, zero retries, 9,808.2 MiB DB                                   | Accepted as the first complete-output large-corpus reference report                                                         |
| Normalize record provenance into a separate `source_records` table                                  | Complete 64k tables/indexes: 89.91 MiB normalized versus 89.88 MiB current; also adds one source-row insert and downstream joins per record  | Rejected; a future surrogate must narrow downstream identities and prove a net whole-schema win                             |
| Omit the unconsumed generic-ledger entity-key copy                                                  | Entity-only 64k spike was runtime-neutral; DB 375.5 -> 342.9 MiB (-8.7%); canonical/assertion keys and all source provenance remain          | Landed without a schema bump; existing rows remain compatible and reclaim space on rebuild                                  |
| Batch fact, canonical-message, and new content-block writes                                         | Five-run 16k/64k medians 1.176/6.140 s (5.220x); 64k row changes ~1.14M -> 724,910 and WAL ~43 -> 22.3 MiB                                   | Landed with 512/256/512-row bounds and sequential fallback for replacements/duplicates                                      |
| Repeat the optimized frozen-corpus gate                                                             | 574.80 s ready/converged, complete identical counts, zero retries, 9,214.1 MiB DB                                                            | Accepted; 9.4% faster and 6.1% smaller than the first truthful reference                                                    |
| Enlarge corpus physical groups and pipeline more objects                                            | Frozen corpus regressed 285.9 -> 379.75 s despite 424 -> 164 physical transactions; commit/checkpoint time and RSS rose sharply              | Rejected and reverted; transaction policy must be corpus-shaped, not selected by the single-object gate                     |
| Raise SQLite page size from 4 KiB to 8 KiB                                                          | 131k/4,096 objects: DB 613.4 -> 589.3 MiB, time 14.88 -> 15.68 s                                                                             | Rejected; smaller storage did not compensate for slower ingestion                                                           |
| Use exclusive locking and disable cache spill during bootstrap                                      | Native query-bootstrap lifecycle failed with `database is locked` while starting the read pool                                               | Rejected and reverted; normal locking remains mandatory at the readiness transition                                         |
| Emit Claude object declarations once per generation                                                 | 131k/4,096 objects: 589,824 -> 335,872 facts, 613.2 -> 557.1 MiB, 16.38 -> 14.45 s first-run; repeat median 13.16 s; live p99 7.2 ms         | Landed behind Claude contract v16 with bounded state, exact small/medium differentials, and deterministic parent ownership  |
| Defer cold-build FK enforcement to the exhaustive readiness audit                                   | Frozen-corpus A-B-A: 176.64 / 168.96 / 179.46 s; production: 167.62 s with exact counts, zero retries, and zero final WAL                    | Landed only for reader-free cold bootstrap; fault injection blocks readiness and live enforcement is restored               |
| Compact run evidence per source-generation/category while preserving counts and exact winners       | Frozen treatment/control/treatment: 177.42 / 208.92 / 165.56 s; mean treatment -17.9%, DB -838 MiB, run-state work -86.7%                    | Landed as schema v43; full fact provenance, runtime counts, decisive IDs, activity maxima, and replacement semantics remain |
| Normalize content-block message ownership through an integer surrogate                              | Frozen treatment/control/treatment: 177.26 / 168.01 / 165.02 s; mean treatment +1.9% despite DB -375.5 MiB                                   | Rejected and reverted; query-safe shape lost in physical transactions/checkpoints, while the smaller join shape hurt facets |
| Reuse message fact IDs as content-block owners without `RETURNING`                                  | 131k treatment/control/treatment: 9.21 / 8.93 / 9.20 s; mean treatment +3.1% despite DB -68.9 MiB                                            | Rejected and reverted; narrower B-trees still increased canonical projection and SQLite commit                              |
| Cache stable artifact projection statements                                                         | Frozen treatment/control/treatment: 156.79 / 153.24 / 144.75 s; mean treatment -1.6%, artifact work -4.54 s                                  | Landed; bounded writer cache retains correctness while removing repeated SQL compilation                                    |
| Let paired messages own identical native-activity evidence                                          | 131k mean -8.0%; frozen treatment/control/treatment: 135.77 / 147.65 / 132.66 s; mean -9.1%, DB about -252 MiB                               | Landed as schema v44; standalone/ambiguous evidence remains independent and projection semantics stay exact                 |
| Defer artifact reductions to one atomic bootstrap rebuild                                           | Frozen treatment/control/treatment: 133.53 / 132.85 / 128.50 s; mean -1.4%; artifact work -4.77 s; rebuild 0.90 s                           | Landed only while readers are unavailable; live reduction and crash-safe idempotent recovery remain                         |
| Defer structural `quick_check` only for an uninterrupted clean build                                 | Frozen treatment/control/treatment: 136.26 / 136.76 / 121.85 s; scan cost 5.33 s; normalized finalization -5.28 s                           | Landed; FK/FTS audits remain mandatory and marker-based recovery retains `quick_check`                                      |

The bulk spike is deliberately not exposed as a production flag. Its lifecycle
is incomplete until crash recovery, reader quiescence, durable readiness, FTS
validation, and subscription reset behavior are implemented together.

## Verification completed

The first wave was validated across the native engine, SDK boundary, retained
cross-engine oracles, and the packaged playground topology:

- `cargo fmt --all -- --check`, workspace/all-feature `cargo check`, and
  workspace/all-target/all-feature Clippy with warnings denied passed;
- the latest complete Rust workspace/all-feature run passed all 531 tests,
  including the object-scope sibling-backlog and bootstrap pause/resume
  regressions, schema rebuild compaction, cleanup query-plan selection,
  retention codecs, batched fact/message/content replacement, recovery,
  cancellation, and query-pool coverage;
- `pnpm typecheck`, `pnpm lint`, `pnpm format:check`, and `pnpm validate`
  passed, with zero architecture-boundary violations;
- SDK package tests passed 312 tests with seven intentional skips, and all 110
  CLI tests passed;
- the Claude, Codex, and Grok observation differential suites are exact across
  cold, live, reconcile, generation-replacement, and restart;
- the packaged playground production build passed, the 32-sibling bootstrap
  gate retained all 131,104 messages with ready equal to converged, warm reopen
  rewrote zero rows, and the latest 100-sample live gate measured p50 1.5 ms /
  p99 3.0 ms;
- the frozen 3.86 GB gate completed with zero retries and ready equal to
  converged at 574.80 seconds with the exact complete durable counts; its
  report is retained outside the repository under the private reference-run
  directory;
- all 12 query-conformance groups passed on the 3-project, 20-session,
  300-message fixture, including a 12 MiB response-boundary case;
- `pnpm build` rebuilt the native module, SDK, CLI, and playground; and
- a post-build Electron utility-process smoke completed the native handshake,
  observed exactly 3 projects, 20 sessions, and 300 messages, recovered after
  100 request cancellations, and kept each single fixture query below 2.2 ms.
  That one-run smoke is topology evidence, not a release latency percentile.

The cold-load and 100-sample live distributions in the baseline section are
the performance evidence. The 4x synthetic scale-ratio, explicit checkpoint,
and pinned-reader recovery gates now pass, and a truthful large production
reference now exists. No apples-to-apples speed claim is made against the
retired legacy bulk path because its durability and durable outputs differ.

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
retain UNIQUE and writer-critical indexes; defer FK enforcement
defer an approved list of query-only indexes and canonical FTS triggers
ingest bounded cursor/fact/projection transactions
rebuild indexes and FTS on the same writer connection
PRAGMA optimize
checkpoint under the reader-free bootstrap policy
validate counts, FTS coverage, all foreign keys, and quick_check
atomically mark projections ready and bootstrap complete
start query workers and publish ready
```

The deferred list is explicit and query-plan-tested. Generation-retraction,
fact identity, usage-series, reducer, foreign-key-supporting, and uniqueness
indexes are never dropped merely because a load is large. Immediate foreign-key
enforcement is disabled only while the sole trusted writer owns a reader-free
cold build; the exhaustive final audit must pass before readiness, and live
enforcement is restored before the query pool starts.

If the process crashes with the marker set, the next owner does not serve
queries. It verifies the schema and either resumes finalization or recreates
the rebuildable structures before clearing readiness. Subscribers receive a
post-bootstrap snapshot watermark/reset boundary rather than millions of
historical row notifications.

The 64k spike saved only 3%; the 131k spike saved 23%. Bootstrap mode must
therefore be size-gated and benchmarked on the real corpus rather than applied
to every startup.

### C. Keep the current provenance schema until a surrogate proves a net win

The measured source-record normalization is not the next schema. A viable
future design would need to store source-record identity/provenance once:

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

Derived rows would also need to reference a narrow fact/record surrogate so
the saved prefix is not replaced by another wide foreign key and automatic
B-tree. The complete 64k spike measured 89.91 MiB normalized versus 89.88 MiB
current, so no schema version is reserved and no migration is scheduled. Any
successor must benchmark all tables, automatic/secondary indexes, insert work,
retraction plans, and query joins. `WITHOUT ROWID` and integer-surrogate
alternatives remain candidates because random hash primary keys can trade
space for insert locality, but they are not accepted architecture without that
whole-schema evidence.

### D. Move projection application toward sets and affected keys

The first accepted slice now applies bounded multi-row writes to the generic
fact ledger, canonical messages, and new content blocks. Further slices may
stage one commit's facts in bounded temporary or in-memory tables and perform
`INSERT ... SELECT`, keyed upserts, and reductions once per affected entity.
Fact-kind/source ownership should be computed once and routed only to relevant
projectors. Reducer arrangements should store the minimum state needed to
apply deltas; full scans remain explicit repair/rewrite paths.

This follows the useful part of modern incremental view maintenance: propagate
changed keys and weighted/delta contributions, not entire histories. It does
not require adopting a distributed dataflow runtime inside the desktop engine.
The earlier standalone run-state delta reducer was neutral at 131k and
regressed smaller cases, so complex reducers remain evidence-gated rather than
being rewritten merely to satisfy an architectural shape.

### E. Evolve the measured checkpoint controller carefully

The measured 32 MiB writer-owned checkpoint target remains. Both the 16 MiB and
64 MiB alternatives lost an important workload gate, and a much larger fixed
threshold was worse. The controller observes physical WAL bytes, explicitly
reports oldest-reader pressure alongside checkpoint progress, and finalizes
after the read pool closes. A pinned reader may delay a checkpoint but cannot
make the writer block; the recovery test demonstrates that WAL space is
reclaimed after release. Future adaptation may add write rate and shutdown/
bootstrap state, but only behind the same cold/live/concurrent gates.

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
- select a redacted representative large corpus, capture the production-host
  report, and admit a legacy comparator only if its durability and durable
  outputs are equivalent;
- add query latency with concurrent ingest and pinned-reader WAL recovery.

Exit: the RFC p50/p99 and production large-corpus baseline are measured rather
than inferred; a non-equivalent legacy number is explicitly rejected instead
of being presented as a comparison.

Progress on 2026-08-13:

- complete: bounded native write-stage/projector and query queue/execution
  histograms, row/change counters, physical DB/WAL bytes, and active-reader
  age;
- complete: production-path peak-RSS reporting, 100-sample one-record and
  64-record live distributions, and the ten-reader concurrent-refresh gate;
- complete: cold synthetic localization and query-plan-verified run-state and
  index-amplification fixes;
- complete: the fully instrumented synthetic 16k/64k scale gate at a 5.173x
  source-telemetry ratio and 5.217x final-lifecycle ratio, plus a 131k
  continuation and bounded process-RSS evidence;
- complete: explicit nonblocking checkpoint progress/timing and deterministic
  pinned-reader WAL recovery through the sole native owner;
- complete: bounded source read/decode/fact-construction metrics and capped
  adapter/stream/driver dimensions, with healthy bounded continuations kept
  distinct from retries;
- complete: a frozen 3.86 GB real-corpus production-host report with truthful
  readiness and complete object-scope convergence; the retired legacy-bulk
  number remains diagnostic only because its durability model and durable
  outputs are not equivalent.

### P2 — writer-owned bulk bootstrap (implemented)

- add durable bootstrap epoch/state and projection readiness;
- do not start query workers until finalization completes;
- classify and query-plan-test writer-critical versus rebuildable structures;
- drop/rebuild only through the writer connection;
- rebuild canonical FTS, run `PRAGMA optimize`, checkpoint, and validate;
- implement crash/restart at every lifecycle boundary;
- compact bootstrap notification semantics to a snapshot watermark/reset.

Exit: >=100k real-corpus loads improve materially, <=64k loads do not regress,
and no partial index/FTS state can be served.

Completed on 2026-08-13. The reviewed query-only index/FTS set is deferred only
for fresh inputs estimated above 48 MiB; uniqueness, foreign-key,
generation-retraction, reducer, and usage-series structures remain live. The
durable marker recovers before readers on restart, finalization stays on the
sole writer with file-backed temp sorting and FULL rollback durability, and
the supervisor pause barrier makes the readiness boundary observation-safe.
Controlled 131k results show no useful one-object benefit but a 10% win and
1.10 million fewer incremental row changes on a 4,096-object shape. The final
32-large-object regression and frozen-corpus gate both report ready equal to
converged.

### P3 — provenance storage decision (implemented)

- audited complete table/index bytes and repeated fact fields at 64k and on the
  frozen corpus;
- spiked a normalized `source_records` layout with the required identity,
  ownership, and retraction indexes;
- rejected the schema rewrite after it measured 89.91 MiB versus 89.88 MiB for
  the current structures;
- removed only the proven unconsumed generic-ledger entity-key copy while
  retaining semantic keys in canonical/assertion projections and preserving
  all fact/source provenance;
- retained the nullable column for existing-database compatibility rather than
  forcing an unmeasured migration.

Exit complete: 64k storage is 8.7% lower, the real database is 6.1% lower, and
query/replay parity is exact. A broader integer-surrogate design remains a new
measured proposal, not unfinished P3 work.

### P4 — bounded set-oriented projection batches (implemented)

- emit bounded multi-row upserts for fact rows and canonical messages;
- emit bounded multi-row inserts for cold/new content blocks while preserving
  sequential replacement and duplicate-key last-write behavior;
- cache only fixed full-chunk SQL shapes and keep all buffers inside the
  existing record/byte transaction bounds;
- preserve full reducers for audit, generation replacement, and recovery;
- defer more invasive reducer arrangements because the standalone run-state
  delta spike was neutral/regressive and the current real-corpus stage profile
  does not justify changing semantics without a dedicated proof.

Exit complete: five-run `T(64k) / T(16k)` is 5.220, real-corpus time improves
9.4%, one-record live p50/p99 remain 1.5/3.0 ms, and all adapter/query parity
gates pass.

### P5 — release acceptance (implementation complete)

- full Rust, SDK, IPC, playground, parity, corruption/recovery, and package
  gates pass;
- five-run cold, 100-sample live, warm-reopen, 32-object bootstrap, query
  conformance, Electron topology, and frozen-corpus reports are retained
  outside the repository;
- the Phase 10 closure ledger records the final ownership and performance
  evidence without reintroducing a legacy owner.

Local implementation acceptance is complete. Publication of private reports,
portable absolute thresholds, scale-50 policy, and the final release decision
remain maintainer-owned policy inputs rather than code gaps.

### P6 — production-corpus follow-up (implementation complete)

- complete: reject oversized physical groups on the complete frozen corpus;
- complete: reject 8 KiB pages and exclusive bootstrap locking on their
  correctness/performance gates;
- complete: remove repeated Claude session/run/delegation declarations with
  bounded durable decoder state;
- complete: make append overflow state/cursor commits atomic and shared Claude
  session ownership deterministic;
- complete: pass native tests, small/medium five-mode differentials, the
  131k/4,096-object gate, and the 100-sample live gate;
- complete: defer cold-build foreign-key enforcement only behind the exhaustive
  readiness audit and prove failure injection still blocks readers;
- complete: compact run-evidence projection rows while preserving the full
  ledger, public count/winner contract, replacement, and activity semantics;
- complete: freeze and run a treatment-control-treatment production comparison
  with identical counts, zero retries/WAL, and full stage/storage telemetry.

Exit: a complete, zero-retry frozen-corpus run has ready equal to convergence,
semantic parity, a safe disk reserve, and a material wall-clock improvement
over both the 285.9-second reference and its exact parent control.

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
