# Ingest fixtures

Committed corpora for the cross-engine ingest-diff harness and for RFC 008.

Every fixture is generated deterministically — file mtimes are pinned, ids are
fixed, and no fixture contains captured user content. Regenerate rather than
hand-edit.

| Fixture | Source | Generator |
| --- | --- | --- |
| `small/` | Claude Code | `node scripts/generate-ingest-fixture.mjs --out crates/spaghetti-napi/fixtures/small` |
| `medium/` | Claude Code | `node scripts/generate-medium-fixture.mjs --out crates/spaghetti-napi/fixtures/medium` |
| `small-grok/` | Grok | `node scripts/generate-grok-fixture.mjs --out crates/spaghetti-napi/fixtures/small-grok` |
| `small-codex/` | Codex | `node scripts/generate-codex-fixture.mjs --out crates/spaghetti-napi/fixtures/small-codex` |

Run the harness with `pnpm test:ingest-diff`, `:medium`, `:grok`, or `:codex`.

`scripts/generate-medium-fixture.mjs` also takes `--scale N`; the bench gate
uses `--scale 50` (~35k messages / ~50 MB) to get a large corpus without
committing one.

---

## `small-codex/` — Codex token-attribution shapes

RFC 008 Phase 3A needs a fixture-backed trace per token-attribution behavior.
Each behavior gets its own session file so the trace can name it.

| Behavior | File |
| --- | --- |
| Official per-turn usage on every turn | `…-01-official-per-turn-…jsonl` |
| No `token_count` events at all | `…-02-no-token-count-…jsonl` |
| Cumulative `total_token_usage` only, plus an `info: null` event | `…-03-total-only-…jsonl` |
| Official first turn, un-attributed second turn | `…-04-mixed-coverage-…jsonl` |
| Internal records only — no user or assistant turn | `…-05-empty-internal-…jsonl` |
| Tail a warm run would grow (tool call, no reply, no `token_count` yet) | `…-06-live-growth-…jsonl` |
| Un-attributed first turn, official count arriving only later | `…-07-unattributed-then-official-…jsonl` |
| Several `token_count` events for one assistant turn | `…-08-multiple-counts-one-assistant-…jsonl` |
| Assistant turn with no preceding user record | `…-09-assistant-only-tail-…jsonl` |
| User turn the model never answered | `…-10-user-only-tail-…jsonl` |

### Turns must be `response_item`, not `event_msg`

Real Codex writes each turn **twice**: a canonical
`response_item` with `payload.type = "message"` and `role` in
`user | assistant | developer`, and a UI projection
`event_msg/{user_message,agent_message}`. The extractor keeps the first and
skips the second, or every turn would be indexed twice.

The first version of these fixtures emitted **only** the `event_msg` form, so
no fixture produced a user or assistant row at all — six sessions yielded five
messages between them, none of them turns. There was nothing for
`last_token_usage` to attribute onto, so the token-attribution fixtures tested
nothing. Fixture `01` now carries both forms for one turn, which keeps the skip
itself under test.

A survey of 18 real sessions on 2026-08-09 counted 747 `response_item` messages
against 570 `event_msg` projections, confirming both appear and that the
canonical form is the one to model.

### Why synthesized

A survey of 18 real sessions found only two of the shapes above — 17 with
`last + total`, one with no `token_count`. The rest had to be authored
regardless, and real sessions carry real prompts. Synthesizing costs no
coverage, needs no scrubbing, and lets each file name its own behavior.

`03` includes `token_count` with `info: null`, which real Codex emits when an
event carries only rate limits — easy to mistake for an absent event, and the
Rust reader treats it as "no usage".

### Known divergence — do not normalize

`pnpm test:ingest-diff:codex` reports **11 differences**, all one cause: the
tiktoken estimate for sessions with no official usage is TS-only.

Official attribution is already at parity. Every session carrying an official
`token_count` — `01`, `03`, `04`, `07`, `08`, `09` — produces byte-identical
token columns in both engines, including the cumulative-total fallback, the
partially-covered sessions, and last-write-wins when one assistant turn draws
several counts.

The divergence is confined to `02`, `05`, `06`, and `10`, the four sessions
with no official usage at all. RFC 008 Phase 3A owns the decision; see
`docs/rfcs/008-phase-3a-attribution.md` for the full per-message trace.
