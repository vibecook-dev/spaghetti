# RFC 012 landing — performance report

- **Status:** measured 2026-08-23 on the landing branches. Numbers only.
- **Budgets:** [012-landing-plan.md](./012-landing-plan.md) §6.
- **Machine:** Apple Silicon, macOS 26.5, other landing lanes compiling
  concurrently. Every ingest number is one run of a release build.
- **Real-corpus numbers were taken at base `8467d71`** and could not be re-taken
  after merging current `main` — see §4.

## 1. Corpora

| Name | Contents | Records | Bytes |
| --- | --- | ---: | ---: |
| `claude-slice` | 75 whole project directories | 145,431 | 301 MB |
| `claude-full` | complete `projects/` + `todos/` + `file-history/` | 1,158,442 | 3.2 GB |

Frozen copies of the live Claude Code corpus taken before measuring, so a run
cannot race the agent writing to it. Which directories a slice holds does not
matter; using the same copy for before and after does.

## 2. Durable ingest throughput — the top landing item

One cold ingest per row, release build, the §6 command, `claude-slice`:

| | before (`8467d71`) | after | change |
| --- | ---: | ---: | ---: |
| Records/s | 70 | 11,653 | **166×** |
| History complete | 2,077.35 s | 12.48 s | −99.4% |
| Catalog visible | 677.8 ms | 219.9 ms | −68% |
| Search queryable | 2,077.35 s | 15.32 s | −99.3% |
| Writer `usage_projection` | 2,055,783 ms | 776 ms | **−99.96%** |
| SQLite row changes | 1,202,462 | 778,947 | −35% |
| Database | 804.8 MiB | 709.8 MiB | −12% |
| Peak RSS | 762 MiB | 1,975 MiB | +159% |

Both converge in 0 repair passes, run 52 vs 1 checkpoints, and produce
identical durable output: 144,853 canonical messages, 199,602 facts, 4,583
commits.

`claude-full`, after: **193.34 s to history complete, 202.03 s to search
queryable, 5,992 records/s**, catalog visible at 258.2 ms, 6,415 MiB database,
35,752 commits, 4,969 MiB peak RSS. Before is not reported for this corpus: L4
measured 213,582 messages in 38.7 min on it, extrapolating to ~3 h, and
reproducing that costs three machine-hours for what the slice already shows.

### What was wrong

**1. `intern_usage_v2_qualification` re-scanned every contribution
(`669c34d`).** The upsert used `ON CONFLICT(qualification_key) DO UPDATE SET
qualification_key = excluded.qualification_key`. Assigning the conflict key
makes SQLite treat the statement as a parent-key update, so with foreign keys
enforced it must prove no child still references the old value. Six columns of
`usage_v2_response_contributions` reference that key and none is indexed, so
every intern scanned the whole contributions table once per constraint — six
interns per usage fact, against a table that grows with the corpus. A `sample`
profile of the sole writer put **97.5% of writer time** in this one function,
all in `sqlite3BtreeNext`/`sqlite3BlobCompare`; writer telemetry put **99.0% of
wall time** in `usage_projection`. Assigning a non-key column leaves the parent
key unmodified, so no foreign-key enforcement is generated. Guard: the upsert
reports zero `SQLITE_STMTSTATUS_FULLSCAN_STEP` with 0, 64 and 4,096
contributions present (756 steps at 64 rows before), and a colliding spec still
updates nothing.

**2. Cold builds no longer entered query-bootstrap mode (`ed40a3b`).**
`465dd46` removed the preflight that measured source-root bytes to enable
`bootstrapQueryStructures` — correctly, since the host must not traverse source
roots to infer policy — and left the capability behind an opt-in flag no caller
sets. Every cold build since 2026-08-21, including the rebuild each schema bump
forces, therefore ran with full-text triggers and the deferred indexes live,
foreign keys enforced per row, 8-commit physical groups and a 32 MiB WAL
checkpoint target. The host need not measure anything: the engine already
refuses the request unless the database has no committed ingest commits, so the
option defaults to on. Worth 6,799 → 11,653 records/s on its own.

### What was not wrong

- **`MAX_APPEND_RECORDS_PER_RECONCILE` (4,096) and the pass loop.** Both sides
  converge in **0 repair passes**: append backlog already routes as
  object-targeted work (`ReconcileRetryTarget` → `PendingObservationWork::
  Object`). The earlier "many passes" reading came from the benchmark driving
  convergence with `refresh`, which rescans the corpus every pass (`19817d7`).
- **Per-record fact/message writes** were 0.2% of writer samples each, and the
  remaining readiness counts are over the catalog tables with `EXISTS`
  per-session predicates — no sibling of the case L4 fixed remains.

### Residual

At 5,992 records/s `claude-full` is below the 2026-08-15 Claude-only rate
(1,123,368 records in 115.6 s = 9,717 records/s) while the slice is above it.
Its largest line item is **source read: 72.8 s for 3,212.7 MiB (44 MB/s)**
against 32.5 s for a comparable payload on 08-15 — this copy lives on an
external USB SSD and holds 22k small file-history blobs, where 08-15 read from
the internal disk. **Decode is 41.1 s, adapter 40.6 s of it** — L5's decode
budget, not a durable-ingest cost. Writer stages total ~102 s (canonical 32.0,
SQLite commit 26.1, prepare 18.5, runtime 17.5, usage 8.6); WAL hit 1,109 MiB.

## 3. Budgets and the rest of the §6 surface

| §6 budget | Result (lane) | Verdict |
| --- | --- | --- |
| Catalog visible warm < 1 s | 8 ms (L4) | **met** |
| Cold catalog on the production-shaped corpus < 10 s | 122 ms (L4), 258 ms on the 3.2 GB copy here | **met** |
| Observer attach + bootstrap of a 50 MB tree < 500 ms | 635 ms @ 43.7 MB, adapter 64% / io 22% / reduce 10% (L1b) | **missed** — decode-bound, L5 owns it |
| Observer steady-state append→consumer < 50 ms | 8.3 ms p95 @ 674 objects (L1b) | **met** |
| Usage-v2 query: no regression vs legacy `getStats` p95 | −24%; `getUsage` +0.7 ms (L3) | **met** |
| Bench gate workflow green | `bench:ingest` path untouched | **met** |

Also measured: playground time-to-library 491 ms (L4b). Ingest has no ratified
§6 ceiling; against the ~9.5k records/s reference the slice is above and the
full corpus below, for the reasons above.

## 4. Blocker on current `main`

`main` at `44f21eb` cannot ingest real Claude data. The configured startup
worker fails with `UNIQUE constraint failed:
fact_records.semantic_fact_revision_id` while decoding a transcript, which
reaches callers as "configured observation startup worker is unavailable". It
reproduces with `main`'s own native module and with this branch's, and not at
`8467d71` — the L5 adapter merge sits between them. One 15.8 MB, 7,007-record
transcript is enough, and several project directories in the 301 MB slice trip
it independently, so no substantial real corpus ingests today. §2's real-corpus
numbers were therefore taken at `8467d71`; post-merge ingest is confirmed on
the synthetic corpus instead (§6).

## 5. Trade-offs accepted

- **Peak RSS during a cold build: 762 → 1,975 MiB (slice), 4,969 MiB (full).**
  The cold builder raises the SQLite page cache to ~976 MiB and mmap to 2 GiB
  and lets the WAL reach its bootstrap target. Bounded, build-only, mostly
  file-backed, and the behaviour that shipped before 2026-08-21.
- **Search is unavailable during a cold build**, queryable 2.8 s after history
  on the slice and 8.7 s on the full corpus — which the readiness vector
  already reports (`search: pending`).
- **Public change-log rows are not written during a cold build.** Only reachable
  with zero committed ingest commits; the SDK subscription treats changes as
  bounded invalidations and re-reads snapshots. A crash mid-build leaves the
  marker set and the next writer open finalizes before admitting anyone.

## 6. Reproducing

The synthetic corpus needs no private data and runs post-merge:
```bash
cd crates/spaghetti-napi && pnpm build && cd ../..   # release
node --import tsx scripts/bench-observation.ts --records 65536 --objects 128 \
  --runs 2 --warmup 1 --report-json /tmp/after.json  # 14,921 records/s
cargo test -p spaghetti-napi --lib usage_v2_qualification
node --import tsx scripts/bench-queries.ts --mode conformance
```

Swap in `--fixture <frozen-corpus>` for §2's runs.
