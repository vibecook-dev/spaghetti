# Lane L7 — performance: durable ingest throughput first, then the §6 report

Worktree: `/Volumes/SamsungRed/spaghetti-rfc012/worktrees/land-l7-perf`, branch `land/l7-perf`.
Read `COMMON.md` first. Base: `main` ≥ `6d91eef`.

## Why this lane exists

Every schema bump (v64 now) drops and rebuilds the database from files. The
2026-08-15 profile (`docs/rfcs/011-playground-cold-start-profile-2026-08-15.md`)
ingested **1,969,824 records in 206.9 s (~9.5k rec/s)** on the production-shaped
corpus. Today: L3 measured **656 rec/s** on a 100 MB slice (release build); L4
measured **213,582 messages in 38.7 min (~92/s)** on the full real corpus with
three adapters, extrapolating to **~3 hours** to convergence. Catalog-first
(122 ms) makes the app usable meanwhile, but history and search lag for hours.
This is a 15–100× regression accumulated during the 012 period and it is the
top performance item of the landing.

## Work (in order)

1. **Reproduce** with a fixed, reproducible harness on the production-shaped
   corpus the 08-15 profile used (or the closest equivalent you can build from
   the real corpus under `~/.claude`, read-only, numbers only): records/s,
   wall time to history-complete, DB size, peak RSS. Record the exact
   commands (extend `scripts/bench-observation.ts` / the Bench gate workflow
   rather than adding a one-off). Establish the before number on current
   `main`.
2. **Profile and fix the durable ingest pipeline** (owned: `engine/coordinator.rs`,
   `engine/writer.rs`, `engine/commit.rs`, `engine/projection.rs`,
   `engine/query_pool.rs`, `engine/supervisor.rs`, `engine/startup.rs`,
   `engine/catalog/*`, `core/schema.rs` indexes/pragmas, `source/*` drivers).
   Known suspects: `MAX_APPEND_RECORDS_PER_RECONCILE = 4,096` per object per
   pass with a pass loop that revisits every object (`coordinator.rs`);
   per-record `fact_records`/provenance/coverage bookkeeping writes; commit
   granularity and WAL checkpoint cadence; per-commit projection work that
   could be batched; FTS finalization strategy (see §X1 in the superseded
   plan — evaluate deferred one-shot vs incremental with numbers); catalog
   rescans during ingest; the `usage_v2` projection cost (L3: ingest +2.7% on a
   slice — fine); readiness polling (L4 fixed a COUNT→EXISTS 1.7 s case —
   look for siblings). Keep semantics: same fact/revision digests, same query
   results (the whole-response equality guard `packages/sdk/src/__tests__/observation-host.test.ts`
   and `scripts/bench-queries.ts --mode conformance` must pass), crash safety
   (a crash may delay convergence but never exposes partial work as complete).
   **Do not touch the adapter decoders** — L5 owns Claude decode perf (it is
   64% of observer bootstrap; if your profile shows decode dominating durable
   ingest too, quantify it and hand the number to the integrator).
3. **Target**: restore ≥ 08-15 throughput on the same corpus class (≥ 9k rec/s
   release build), or explain the unavoidable residual with numbers. Report
   before/after on the identical corpus and commands.
4. **Then the §6 report** — `docs/rfcs/012-landing-perf-report.md` (≤ 150 lines,
   reproducible commands, numbers only): catalog cold/warm (L4: 122/8 ms),
   playground time-to-library (L4b: 491 ms), observer steady-state p95 (L1b:
   8.3 ms @ 674 objects) and bootstrap (635 ms @ 43.7 MB, decode-bound),
   usage query p95 (L3: getStats −24%, getUsage +0.7 ms), ingest throughput
   before/after (you), full-rebuild wall time on the real corpus before/after,
   DB size, RSS. Mark which §6 budgets are met/missed. Bench gate workflow
   stays green.

## Rules specific to this lane

No semantic changes hidden as perf; every optimization has a behavioral test
or a digest/equality guard proving it; no new `#[allow]`; zero clippy warnings
in your files; perf claims come with the command that reproduces them.
