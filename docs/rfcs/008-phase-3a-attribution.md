# RFC 008 Phase 3A — Codex Token Attribution Model and Decision

**Companion to:** [RFC 008](./008-rust-ingest-production-readiness.md) · [Phase 0 baseline](./008-phase-0-baseline.md)
**Captured:** 2026-08-09 · **Fixtures:** `crates/spaghetti-napi/fixtures/small-codex`
**Status:** Phase 3A exit gate met. Policy chosen. No estimator code has landed.

Phase 3A separates the decision from the code because token estimation
previously accumulated speculative fixes without the storage model being
characterized first. This is the characterization.

---

## 0. The fixtures had to be rebuilt first

The traces below could not be produced against the Phase 0 fixtures. Six
sessions yielded **five messages between them, none of them a user or assistant
turn**, so there was nothing for `last_token_usage` to attribute onto.

Real Codex writes each turn twice: a canonical `response_item` with
`payload.type = "message"` and a `role`, plus a UI projection
`event_msg/{user_message,agent_message}`. The extractor keeps the first and
skips the second — otherwise every turn is indexed twice. The generator emitted
only the projection.

This also corrects the Phase 0 record, which read the six known diffs as
landing on sessions `02`, `03`, `04`, "the three sessions with un-attributed
turns". They were on `01`, `04`, `05` — the three with a `developer` preamble —
and the cause was TS estimating that preamble. `02` and `03` had zero rows and
could not differ. **None of the historical divergence was about turn
attribution.**

Fixture set is now 10 sessions / 39 messages; `01` carries both record forms so
the skip stays under test.

---

## 1. What covers what

The question the RFC asks is which message each official value covers, because
a record already covered must not then receive an estimate.

**Official `input_tokens` on an assistant covers the whole prompt** — the
developer preamble and every preceding user record in that turn. That is why
user rows sit at `0/0` in every officially-attributed session below, and it is
correct rather than missing data.

`output_tokens` covers only the assistant's own reply.

---

## 2. Per-fixture traces

Both engines, cold ingest. `in/out` is `input_tokens/output_tokens`.

### Officially attributed — engines already identical

| Fixture | Row | TS | Rust | Covered by |
| --- | --- | --- | --- | --- |
| `01` official-per-turn | developer | 0/0 | 0/0 | assistant #2's input |
| | user "Rename…" | 0/0 | 0/0 | assistant #2's input |
| | **assistant** | **120/40** | **120/40** | `last_token_usage` #1 |
| | user "Now add…" | 0/0 | 0/0 | assistant #4's input |
| | **assistant** | **90/55** | **90/55** | `last_token_usage` #2 |
| `03` total-only | **assistant** ×2 | **300/120**, **540/210** | same | cumulative `total_token_usage` fallback |
| `04` mixed-coverage | **assistant** #2 | **200/75** | same | `last_token_usage` |
| | assistant #4 | 0/0 | 0/0 | **nothing — uncovered** |
| `07` unattributed-then-official | assistant #1 | 0/0 | 0/0 | **nothing — uncovered** |
| | **assistant** #3 | **150/60** | same | `last_token_usage`, arriving late |
| `08` multiple counts, one assistant | **assistant** | **45/15** | same | **the last count wins** — not summed |
| `09` assistant-only tail | **assistant** | **60/25** | same | `last_token_usage`; no user record to cover |

`tokens_estimated = 0` on all of these in both engines.

**Two behaviours worth pinning.** `08` establishes last-write-wins for repeated
counts on one assistant — both engines already agree, so it is a contract, not
a coincidence. `07` establishes that a late official count covers only its own
turn and does not retroactively attribute the earlier one.

### Un-attributed — the entire divergence

| Fixture | TS | Rust | Is the estimate real usage? |
| --- | --- | --- | --- |
| `02` no `token_count` | user 10/0, assistant 0/11, user 2/0, assistant 0/3 · `est=1` | all 0/0 · `est=0` | **Yes.** A complete conversation Codex simply never counted. |
| `05` empty-internal | developer 11/0 · `est=1` | 0/0 · `est=0` | **No.** Session aborted; no turn exists. The model never ran. |
| `06` live-growth | user 7/0 · `est=1` | 0/0 · `est=0` | **No.** Mid-turn; the reply has not happened yet. |
| `10` user-only tail | user 5/0 · `est=1` | 0/0 · `est=0` | **No.** The model never answered. |

Three of the four estimate usage for work that never occurred.

### Estimates attribute differently from official data

Official counts put **input on the assistant**. The TS estimator puts **input
on the user row and output on the assistant** (`02` above). Session totals are
`SUM` over messages so the totals remain comparable, but per-message the two
shapes disagree. Any turn-aware work later must reconcile this deliberately.

### An estimated session that grows

Verified by appending the assistant reply and its `token_count` to `06` and
re-ingesting:

```
before:  user 7/0    (estimated)     tokens_estimated=1
after:   user 0/0, assistant 640/32  tokens_estimated=0
```

The estimate is **transient**. It is not merged with, or added to, the official
value — ingest is full clear-and-reingest (Phase 1, decision P1), so every run
recomputes from the file. There is no stale-estimate state to reconcile, which
removes the whole class of provenance-drift risk the RFC was guarding against.

---

## 3. Decision

**Policy 2 — session-level fallback, narrowed to completed turns.**

> Estimate a session only when it has **at least one completed user→assistant
> turn** and **no official usage at all**. Mixed sessions remain partially
> unattributed.

Rust ports this; TS is narrowed to match. The `01`–`10` diff goes to zero.

### Why not turn-aware (policy 1)

It is the most accurate option and it would additionally cover the uncovered
turns in `04` and `07`. It also needs per-message provenance — a schema
migration — plus turn reconstruction and covered-record tracking so a user row
already covered by an assistant's official input is not double-counted.

The benefit lands entirely on mixed sessions, and the 18-session survey found
**none**: 17 were fully attributed and 1 had no counts at all. Paying a schema
migration for a shape not yet observed in real data is the speculative fix this
phase exists to avoid. Revisit if mixed sessions turn up in practice.

### Why not drop (policy 3)

It is the cheapest and Rust already behaves this way. But `02` is a real
conversation whose tokens Codex genuinely never emitted, and roughly 1 in 18
real sessions looks like this. Showing nothing there loses information that a
clearly-labelled estimate conveys honestly.

### Why narrowed

The plain TS behaviour reports usage for sessions where the model never ran —
an aborted session (`05`), an unfinished turn (`06`), an unanswered question
(`10`). That is not an approximation of real usage; it is usage that does not
exist. The completed-turn guard is what separates "Codex did work and did not
count it" from "no work happened".

---

## 4. Exit-gate requirements

### Pending and retry behaviour

| Session state | Behaviour |
| --- | --- |
| Not yet ingested | No rows and no fingerprint; the next run reads it in full. |
| Official usage present | Attributed as §2; `tokens_estimated = 0`. |
| No official usage, ≥1 completed turn | Estimated; `tokens_estimated = 1`. |
| No official usage, no completed turn | Zero tokens; `tokens_estimated = 0`. |
| Mixed | Official kept, uncovered turns stay `0/0`, `tokens_estimated = 0`. |

**There is no pending state, and none is needed.** Ingest is full-source
clear-and-reingest, so every run recomputes token attribution from the file
rather than resuming partial work. This is why `sessions.tokens_estimated` is
never at risk of being overloaded as a pending marker, which the RFC forbids —
there is nothing to mark.

### Distinguishing "processed, nothing estimable" from "not processed"

By the presence of message rows, not by a flag:

- **Not processed** — no rows for the session, and no fingerprint for its file.
- **Processed, nothing estimable** — rows exist, tokens are `0`,
  `tokens_estimated = 0`. Fixtures `05`, `06`, `10`.

`tokens_estimated` keeps its single meaning: *this session contains estimated
values*. It never means "pending".

### Schema migration

**None required.** The policy needs only `sessions.tokens_estimated`, which
already exists with the right meaning. Policy 1 would have required a
per-message provenance column; that is part of what it costs and part of why it
was not chosen.

---

## 5. Exit gate

| Gate | Status |
| --- | --- |
| Every fixture has an agreed expected token table and rollup | ✅ §2, ten fixtures, both engines |
| Pending/retry behavior defined for new, official, estimated, mixed | ✅ §4 — no pending state exists, and why |
| Policy distinguishes "processed but nothing estimable" from "not processed" | ✅ §4, by row presence |
| Decision identifies any required schema migration | ✅ §4 — none |
| No estimator code has landed | ✅ this phase changed fixtures and documentation only |

**Phase 3B may begin:** port the estimator to Rust with `tiktoken-rs` behind
the completed-turn guard, narrow the TS estimator to match, and bring
`pnpm test:ingest-diff:codex` to zero so it can enter CI.
