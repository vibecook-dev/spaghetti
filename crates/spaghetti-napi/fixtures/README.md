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

RFC 008 Phase 0 requires a fixture per token-attribution behavior, and rejects
"covered by medium" as a mapping. Each behavior gets its own session file.

| Behavior | File |
| --- | --- |
| Official per-turn usage on every turn | `…-01-official-per-turn-…jsonl` |
| No `token_count` events at all | `…-02-no-token-count-…jsonl` |
| Cumulative `total_token_usage` only, plus an `info: null` event | `…-03-total-only-…jsonl` |
| Official first turn, un-attributed second turn | `…-04-mixed-coverage-…jsonl` |
| Internal records only — no user or assistant turn | `…-05-empty-internal-…jsonl` |
| Tail a warm run would grow (tool call, no reply, no `token_count` yet) | `…-06-live-growth-…jsonl` |

### Why synthesized

A survey of 18 real Codex sessions on 2026-08-08 found only two of the six
shapes — 17 sessions with `last + total`, one with no `token_count`. The other
four had to be authored regardless, and real sessions carry real prompts.
Synthesizing all six costs no coverage, needs no scrubbing, and lets each file
name its own behavior.

The envelope is modelled on real output: `{timestamp, type, payload}` with
`type` in `session_meta` | `response_item` | `event_msg` | `turn_context`.
`03` includes `token_count` with `info: null`, which real Codex emits when an
event carries only rate limits — easy to mistake for an absent event, and the
Rust reader treats it as "no usage".

### Known divergence — do not normalize

`pnpm test:ingest-diff:codex` currently reports **6 differences**, and that is
the expected result today:

- `sessions.tokens_estimated` — TS `1`, Rust `0`, on the three sessions with
  un-attributed turns (`02`, `03`, `04`)
- `messages.input_tokens` — TS non-zero, Rust `0`, on those sessions' turns

The cause is documented in `crates/spaghetti-napi/src/codex/reader.rs`: the
tiktoken estimate for missing `token_count` is TS-only. RFC 008 decision **P4**
owns whether to port it, narrow it, or drop it, and Phase 3A must settle that
before any estimator code lands.

**This is why `:codex` is not in CI.** RFC 008 Phase 0 says to enumerate known
TS/Rust differences rather than normalize them away; wiring a knowingly-red
check into CI would force exactly the premature normalization the RFC warns
against. Add it to CI as part of Phase 3B, once P4 is decided.
