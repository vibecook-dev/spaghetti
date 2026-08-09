# RFC 008 Phase 0 — Contract Freeze and Baseline

**Companion to:** [RFC 008](./008-rust-ingest-production-readiness.md) · [survey](./008-readiness-survey.md)
**Captured:** 2026-08-08 · **Commit:** `a328312`
**Status:** Phase 0 exit gate met. No production behavior changed.

Phase 0 produces evidence, not fixes. Everything here is a measurement, a frozen
contract, or a representation that nothing yet publishes.

---

## 1. Frozen contracts

### 1.1 Native ingest error wire shape

Approved as `FrozenNativeIngestError` / `FrozenNativeIngestErrorReport` in
`packages/sdk/src/native.ts`. Declared, deliberately **not produced** — Phase 2
implements it.

```ts
type NativeIngestErrorSeverity = 'record-skip' | 'project-fatal' | 'source';

interface FrozenNativeIngestError {
  slug?: string;      // absent for `source` severity
  path: string;       // always present
  severity: NativeIngestErrorSeverity;
  message: string;
}

interface FrozenNativeIngestErrorReport {
  errors: FrozenNativeIngestError[];  // first N, for display
  errorCount: number;                 // uncapped total
  errorsTruncated: boolean;
}
```

**The change that matters is `slug` becoming optional.** Today's shape is
`{ slug: string, message: string }` with `slug` required, so a failure occurring
*before* a project slug exists cannot be expressed at all. That is why such
failures are currently swallowed rather than reported — `claude/project_parser.rs`
documents the swallow. A required slug also invites inventing a fake one, which
the RFC explicitly forbids.

`path` becomes mandatory in exchange, so every surfaced error can name a file
even when it cannot name a project.

### 1.2 Ingest-contract marker

`packages/sdk/src/data/ingest-contract.ts`, keyed
`(source_id, 'rust-ingest-contract')` in the existing `source_materializations`
table. **No schema change was needed** — that table already has the right shape
and primary key.

`RUST_INGEST_CONTRACT_VERSION = 1`.

Representation only. Nothing calls `markSourceContractCurrent`, and no source is
treated as repaired; a test asserts `ingest-service` does not publish the marker,
because publishing before Phase 1's success-last ordering exists would make a
failed repair look complete and never retry.

---

## 2. Cross-engine baseline

Regenerate any snapshot with:

```bash
tsx scripts/ingest-diff.ts --source=<claude|codex|grok> --snapshot-json docs/rfcs/008-baseline/<name>.json
```

Committed under [`008-baseline/`](./008-baseline/). Row *contents* are hashed
rather than dumped — the point is to detect that something changed and where,
not to review 35k rows in a diff. Verified reproducible: regenerating all four
produces byte-identical files.

| Fixture | Source | Diffs |
| --- | --- | --- |
| `small` | Claude | 0 |
| `medium` | Claude | 0 |
| `small-grok` | Grok | 0 |
| `small-codex` | Codex | **6 — enumerated below** |

### Known divergence: Codex token estimation

Not normalized away, per Phase 0's instruction to enumerate rather than
reconcile. Six differences, all one cause:

| Column | TS | Rust | Where |
| --- | --- | --- | --- |
| `sessions.tokens_estimated` | `1` | `0` | fixtures `02`, `03`, `04` |
| `messages.input_tokens` | non-zero | `0` | turns in those sessions |

Cause is documented in `crates/spaghetti-napi/src/codex/reader.rs`: the tiktoken
estimate for missing `token_count` is TS-only. The affected sessions are exactly
the three with un-attributed turns — no `token_count`, total-only, and mixed
coverage.

**Decision P4 owns this.** Phase 3A chooses between a turn-aware port, a
session-level fallback, and dropping estimation; this baseline is the input to
that choice, which is why `pnpm test:ingest-diff:codex` is deliberately not in
CI yet.

### Known divergence: fingerprint coverage

The `fingerprints` count differs by engine and always has — on the `small`
Claude fixture, TS records **16** and Rust **38**.

This is intended, not drift. RFC 003 gave the Rust port wider warm-start
coverage: the TS ingest fingerprints only session JSONL and
`sessions-index.json`, while Rust also covers subagent transcripts, tool
results, memory, todos, tasks, and file-history snapshots. RFC 008 Phase 1.4
widened it further — plans, `agent-*.meta.json` sidecars, workflow
`journal.jsonl`, and nested `subagents/workflows/<wf>/` transcripts, which the
parser read but discovery never saw.

The consequence is asymmetric in the safe direction: Rust notices changes TS
misses, so Rust re-ingests where TS would wrongly stay warm.

`source_files` is excluded from the diff harness's table inventory, so this gap
does **not** surface as a diff — the exclusion predates this work and its stated
reason ("the Rust ingest doesn't write it") is now stale, since Rust does write
it. The counts above are the record; a future phase that wants the harness to
compare fingerprints will have to decide whether to bring TS up to parity first.

---

## 3. Timings

Hardware is recorded in every bench report as of `20ec07c`. These numbers are
from one machine and are a baseline to compare against, not a threshold:

| | |
| --- | --- |
| CPU | 13th Gen Intel Core i9-13900K, 32 logical cores |
| Memory | 96 GiB |
| OS / Node | win32 x64 / v26.6.0 |

Corpora: `small` (176 KB), `medium` (618 KB), `large` — generated with
`node scripts/generate-medium-fixture.mjs --scale 50`, 9 projects / 1,404
sessions / 44 MB. The large corpus is generated rather than committed, so it
reproduces anywhere without shipping 44 MB.

### Cold start — median of 3, 1 warmup

| Corpus | Rust | TS | Rust advantage |
| --- | --- | --- | --- |
| small | 30 ms | 51 ms | 1.7× |
| medium | 46 ms | 101 ms | 2.2× |
| large | 2,647 ms | 5,851 ms | 2.2× |

### Warm start — large corpus, Rust

| Scenario | Median |
| --- | --- |
| Unchanged (fingerprints match) | **60 ms** |
| Changed (one session grown by 20 lines) | **2,500 ms** |
| *(cold, for reference)* | *2,531 ms* |

### What the warm numbers say

**Changed-warm costs a cold start.** 2,500 ms against 2,531 ms cold — the Rust
warm path clears and re-ingests the whole source on any change, so touching one
file in a 1,404-session corpus costs the same as rebuilding all of it. The
unchanged fast path is genuinely fast (60 ms); there is simply nothing between
"nothing changed" and "everything again".

That is decision **P1** made concrete: *"correct full-source clear-and-reingest
first; consider per-project incremental only after measurement."* This is the
measurement.

Two things follow for Phase 4:

1. On this hardware the full-source path already lands **under the 3-second
   floor** of `max(2 × TS median, 3 s)`, so it may pass on the absolute floor
   alone without per-project incremental work. That would be the cheap outcome.
2. The comparison Phase 4 actually specifies — Rust full-source warm against the
   **TS incremental** warm path — is not measured here. The bench harness has no
   TS warm mode, and building the growth / deletion / forced-repair scenarios is
   explicitly Phase 4's job. The changed-warm number above came from a throwaway
   probe, not a committed harness; treat it as indicative.

---

## 4. Exit gate

| Gate | Status |
| --- | --- |
| Fixtures and baseline results committed | ✅ four snapshots, regenerable and verified reproducible |
| Known TS/Rust differences enumerated, not normalized | ✅ §2, six Codex differences with cause and owner |
| Contract version and error wire shape approved | ✅ §1 |
| No production behavior has changed | ✅ marker unpublished, error types unproduced, snapshots read-only |

Fixture coverage was closed in `20ec07c` — Codex fixtures did not previously
exist, and Phase 3 cannot start without them.

**Phase 1 may begin.** Its first task is the one the survey flagged as larger
than the RFC's wording implies: `source_files` is keyed by path alone in *both*
engines, so migrating to `(source_id, path)` touches six call sites plus the
live writer, not just a primary key.
