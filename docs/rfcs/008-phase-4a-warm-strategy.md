# RFC 008 Phase 4A — Warm Strategy Decision

**Companion to:** [RFC 008](./008-rust-ingest-production-readiness.md) · [Phase 0 baseline](./008-phase-0-baseline.md)
**Captured:** 2026-08-09
**Status:** Decided. **Full-source clear-and-reingest is accepted. Per-project incremental is not needed.**

Decision **P1** deferred this: *"correct full-source clear-and-reingest first;
consider per-project incremental only after measurement."* This is the
measurement.

---

## 1. The comparison had to be built first

Phase 0 recorded that the comparison Phase 4 specifies — Rust full-source warm
against the **TS incremental** warm path — was unmeasured, because the harness
had no TS warm mode. `runTsOnce` cleaned the database on every iteration, so a
`--mode warm` TS run measured a cold start; `--mode warm` was gated to
`--only rust` for exactly that reason.

Both engines are now seeded once and measured warm, and `--scenario` states
what changed between runs. "Warm" alone conflates the fast path with a full
rebuild — numbers two orders of magnitude apart that answer different
questions.

---

## 2. Hardware and corpus

| | |
| --- | --- |
| CPU | 13th Gen Intel Core i9-13900K, 32 logical cores |
| Memory | 96 GiB |
| OS / Node | win32 x64 / v26.6.0 |
| Corpus | `generate-medium-fixture.mjs --scale 50` — 9 projects, 1,404 sessions, 44 MB |
| Runs | 5 measured, 1 warmup, engines back to back |

---

## 3. Results

Raw samples, median in bold.

### Unchanged — the fast path

| Engine | Median | Samples |
| --- | --- | --- |
| Rust | **60.1 ms** | 61.4, 59.3, 59.5, 61.1, 60.1 |
| TS | **426.5 ms** | 421.5, 410.5, 427.9, 426.5, 429.6 |

### Growth — one day of new messages (20 per run, rotating sessions)

| Engine | Median | Samples |
| --- | --- | --- |
| Rust | **2.61 s** | 2.66, 2.61, 2.59, 2.64, 2.60 |
| TS | **6.65 s** | 6.48, 6.62, 6.65, 6.75, 6.80 |

### Deletion — a removed session

| Engine | Median | Samples |
| --- | --- | --- |
| Rust | **2.59 s** | 2.61, 2.59, 2.61, 2.57, 2.56 |
| TS | **6.63 s** | 6.49, 6.63, 6.62, 6.78, 6.82 |

### Forced repair — stale contract version, fast path defeated

| Engine | Median | Samples |
| --- | --- | --- |
| Rust | **2.57 s** | 2.56, 2.57, 2.52, 2.59, 2.71 |
| TS | n/a | the ingest-contract marker is a Rust mechanism; TS has its own warm check |

---

## 4. Decision

The gate is: accept when the Rust median is **no worse than
`max(2 × TS median, 3 s)`**.

| Scenario | Threshold | Rust | Margin |
| --- | --- | --- | --- |
| Unchanged | max(0.85 s, 3 s) = **3 s** | 0.060 s | 50× under |
| Growth | max(13.3 s, 3 s) = **13.3 s** | 2.61 s | 5.1× under |
| Deletion | max(13.3 s, 3 s) = **13.3 s** | 2.59 s | 5.1× under |

**Accepted, with room to spare. Per-project incremental deletion/reinsert is
not implemented, and the Phase 1 and 2 matrices stay unchanged.**

### The result worth stating plainly

Rust's **full-source rebuild is 2.5× faster than TS's incremental path**. The
optimisation Phase 4 held in reserve would have been optimising something
already faster than the thing it is measured against.

That also reframes Phase 0's finding. Changed-warm costing a cold start
(2.61 s vs 2.67 s) reads like a problem in isolation; against the alternative
it is not one. The full-source path is simple, has no per-project incremental
state to get wrong, and is comfortably fast — and Phases 1 and 2 showed how
much correctness risk lives in exactly that kind of state.

### What would reopen this

Not a faster machine — the ratio half of the gate is self-normalising, since
both engines run back to back on the same hardware. It reopens if the corpus
grows enough that the **absolute** 3-second floor starts to bind on the
unchanged fast path, or if a real corpus is found where the Rust median exceeds
`2 × TS`. Neither is close: the fast path is 50× under its floor.

---

## 5. Reproducing

```bash
node scripts/generate-medium-fixture.mjs --out /tmp/large --scale 50
pnpm bench:ingest --fixture /tmp/large/.claude --mode warm --scenario unchanged --runs 5 --warmup 1
pnpm bench:ingest --fixture /tmp/large/.claude --mode warm --scenario growth    --runs 5 --warmup 1
pnpm bench:ingest --fixture /tmp/large/.claude --mode warm --scenario deletion  --runs 5 --warmup 1
pnpm bench:ingest --fixture /tmp/large/.claude --mode warm --scenario repair --only rust --runs 5 --warmup 1
```

Mutating scenarios copy the fixture first, so neither the committed fixture nor
a real `~/.claude` is modified.

**Performance work weakened no correctness test.** Nothing in the ingest path
changed for this phase — only the benchmark harness.
