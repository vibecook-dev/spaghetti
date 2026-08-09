# RFC 008 Phase 3 — Codex Token Estimation Exit Gate

**Companion to:** [RFC 008](./008-rust-ingest-production-readiness.md) · [Phase 3A decision](./008-phase-3a-attribution.md)
**Captured:** 2026-08-09
**Status:** Phase 3 exit gate met. `pnpm test:ingest-diff:codex` is at zero diffs and now runs in CI.

Phase 3A characterized the attribution model and chose a policy. Phase 3B
implemented it. The decision and its rationale live in
[`008-phase-3a-attribution.md`](./008-phase-3a-attribution.md); this records
what shipped.

---

## 1. What was implemented

**Policy 2 — session-level fallback, narrowed to completed turns.**

Estimate a session only when **no official usage appears anywhere in it** and
**at least one assistant message exists**. Both conditions are required, and
each rules out a distinct failure:

- Without the first, a session would mix measured and estimated numbers and its
  total would stop meaning anything.
- Without the second, the estimator reports usage for work that never happened
  — an aborted session, a turn still in flight, a question never answered.

An assistant message is the marker for "a turn completed" because it is the
model's reply. A tool call is not: the turn is still running and the official
count arrives with the reply.

### Rust

- `codex/estimate_tokens.rs` — `tiktoken-rs` with `o200k_base`, a port of the
  TS estimator. `user`/`developer` text counts as input, `assistant` text as
  output, everything else contributes nothing. Falls back to `chars / 4` if the
  encoder is unavailable, matching TS: an ingest must not abort over an
  estimate it was never going to be exact about.
- `codex/reader.rs` — tracks whether any official value was applied and whether
  an assistant reply was seen, then emits estimates at session end.
- `IngestEvent::MessageTokens` — updates one message's token columns and
  nothing else.

**Why a new event.** The alternative is re-sending the whole `Message`, which
means holding every message's raw JSON for the length of a session on the
chance an estimate is needed at the end. On a large session that doubles peak
memory for a case that arises roughly once in eighteen sessions. Only
`(index, type, text)` is buffered instead, and the text is already truncated to
the same 2,000 UTF-16 units the TS side stores — which is also what makes the
two engines estimate from identical input.

### TypeScript

`sources/codex/ingest-hooks.ts` gains the completed-turn guard. This is a
deliberate behaviour change, not just a port target: the previous behaviour
estimated three of the ten fixture sessions where no turn had completed.

---

## 2. Verification

| Fixture | Estimated? | Why |
| --- | --- | --- |
| `01`, `03`, `04`, `07`, `08`, `09` | no | official usage present |
| `02` no `token_count` | **yes** | complete conversation, Codex emitted nothing |
| `05` aborted | no | no turn exists |
| `06` mid-turn | no | reply not yet generated |
| `10` never answered | no | model never responded |

`pnpm test:ingest-diff:codex`: **zero diffs**, 10 sessions / 39 messages. Small,
medium, and grok stay at zero. 228 Rust tests, 466 SDK, 113 package.

Four Rust tests pin the guard directly rather than leaving it to the harness —
a completed turn is estimated, official usage suppresses estimation entirely, a
session with no assistant reply is not estimated, and a turn still in flight is
not estimated. One more pins the `o200k_base` token counts, because if the
encoding drifts the cross-engine diff disagrees on every estimated session and
the failure is far more legible here.

---

## 3. Exit gate

| Gate | Status |
| --- | --- |
| The chosen fixture matrix passes | ✅ zero diffs across all ten sessions |
| Empty/internal-only sessions do not cause warm loops | ✅ cold → warm → warm reaches `projects_processed = 0`; the fixture set includes the aborted, mid-turn, and unanswered sessions |
| No official usage is overwritten or double-counted | ✅ estimation is suppressed entirely when any official value is present, so the two never meet |
| The UI never labels estimates as API truth | ✅ `tokens_estimated` drives the `~` prefix in `formatTokenUsage`, used by the CLI list/detail views and the playground; project rollups mark `~` if any session under them is estimated |
| RFC 009 handoff records the outcome | ✅ §4 |

---

## 4. Handoff to RFC 009

**Estimation was narrowed, not ported wholesale and not dropped.**

RFC 009 removes the TypeScript bulk engine. When it deletes the TS Codex
estimator, the Rust implementation in `codex/estimate_tokens.rs` is the
replacement and must stay — including the completed-turn guard, which is the
part that is easy to lose in a straight deletion because it lives in
`ingest-hooks.ts` rather than in the estimator itself.

Behaviour to preserve, in one line: *a Codex session shows estimated tokens
only when it holds a completed turn and Codex itself reported nothing.*

Two things RFC 009 does **not** inherit as open questions:

- **Turn-aware estimation** was considered and rejected on evidence, not
  effort. Its benefit lands only on mixed sessions, of which a survey of 18
  real sessions found none. Reopening it needs new evidence — real sessions
  that are partially attributed — not a fresh opinion.
- **The attribution-shape difference** is known and deliberate: official counts
  put the whole prompt's input on the assistant, while estimates put input on
  user rows and output on assistant rows. The two never occur in the same
  session, which is what makes it tolerable. Any future work that lets them mix
  has to resolve it first.
