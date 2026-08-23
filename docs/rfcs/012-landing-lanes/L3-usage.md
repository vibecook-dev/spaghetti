# Lane L3 — usage-v2 becomes the one usage path (RFC 012C), wired to CLI/playground

Worktree: `/Volumes/SamsungRed/spaghetti-rfc012/worktrees/land-l3-usage`, branch `land/l3-usage`.
Read `COMMON.md` first.

## Consumer and outcome

`spag stats` (`packages/cli/src/commands/stats.ts` → `api.getStats()`) and the
playground usage views still use the RFC 011 additive usage (per-JSONL-row
contributions). The Phase 0B census proved that is ~2.3× over-counted
(342,861 usage rows vs 149,077 response groups; repeats and evolving
counters). RFC 012C's response-level snapshot semantics are implemented but
never selected (`engine/runtime_usage_totals_query.rs` falls back to legacy;
`query_pack_selections` is never written at runtime). Outcome: response-level
usage is **the** usage everywhere; legacy usage and the selection/shadow
machinery are deleted; CLI and playground show corrected numbers; one oracle
proves them.

## Facts

- Three usage paths: `usage_contributions`/`usage_totals` (011),
  `usage_v2_qualification_specs`/`usage_v2_response_contributions` +
  `runtime_actor_runs_v2`/`runtime_actor_affiliations_v2` (012C), plus
  `token_activity_daily`. Six napi usage methods (`napi_engine.rs` ~4835–4944).
  `engine/runtime_usage_query.rs:1` calls itself a "shadow query pack";
  selection lives in `runtime_usage_totals_query.rs` (`LEGACY_USAGE_QUERY_ID`,
  `RUNTIME_USAGE_V2_QUERY_ID`, `SELECTED_RUNTIME_USAGE_QUERY_ID`).
- `engine/projection.rs` has usage-v1 ledger code (~@3702–4203) and
  `apply_usage_v2_facts` (@3287). `engine/runtime_semantic_merge/`,
  `runtime_semantic_projection.rs`, `semantic_contract*.rs` (3.9k),
  `unknown_evidence_reducer.rs` are part of the 012C stack; keep only what the
  single usage path and the observer's reducers need (the reducer laws
  themselves live in `runtime_semantic_reducer.rs`, which L1 also uses — touch
  it minimally and list edits).
- Independent oracle: `scripts/usage_v2_oracle/` (keep; it is the proof).
  `scripts/usage-v2-compatibility-window.ts` and
  `scripts/usage-v2-private-parity.ts` are one-shot experiments (delete with
  their `typecheck:scripts`/`experiment:*` entries, or move to
  `scripts/archive/`).
- Schema is drop-and-rebuild (`core/schema.rs` `SCHEMA_VERSION = 62`); a bump
  rebuilds the DB from files, so migration of old rows is not required — but
  the rebuild cost on the real corpus must be acceptable (state it).

## Work

1. Make response-level usage the single projection: one canonical usage table
   (or the existing v2 tables if they are already the minimal shape) keyed by
   provider response id / session, with `INSERT … ON CONFLICT DO UPDATE`
   semantics for revisions, and `QualifiedValue` quality on the
   aggregates. Delete `usage_contributions`/`usage_totals` and their
   projection/query code, `query_pack_selections`, the selected/shadow query
   packs, and 5 of the 6 napi usage methods. Keep `getStats()`'s *shape*
   (CLI/playground depend on it) but back it by the new path; if a field has
   no honest v2 meaning, mark it and say so in the report.
2. `spag stats` and the playground usage views read the corrected numbers;
   show value quality where the UI already has a slot (don't redesign the UI).
3. Prove it: the oracle in `scripts/usage_v2_oracle` against the engine output
   on a fixture corpus (in-repo) and on the real corpus (report aggregate
   deltas only: legacy vs corrected totals, no native content).
4. Performance: `getStats` p95 no worse than legacy on the real corpus; usage
   projection throughput during ingest no worse than 011; report numbers from
   `scripts/bench-queries.ts`/`bench-observation.ts` or a focused timing.
5. Delete hand-written TS usage contracts you make obsolete
   (`packages/sdk/src/contracts/rfc012c.ts` parts, `runtime/usage-v2-live-merge.ts`
   if nothing consumes it) — coordinate: L2 leaves them to you because your
   callers import them.
6. Zero `dead_code` warnings in your modules; file caps; tests behavioral
   (real SQLite, real decode of fixture JSONL).

## Out of scope

Observer (L1), catalog/readiness (L4), SDK barrel/codegen pipeline (L2) — but
you may adopt `ts-rs` derives on any new public type per the convention.
