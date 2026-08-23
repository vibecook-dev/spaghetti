# RFC 012 landing plan

- **Status:** Active. Supersedes [012-implementation-plan.md](./012-implementation-plan.md),
  [012-implementation-deduplication-plan.md](./012-implementation-deduplication-plan.md),
  [012-parallel-work-handoff.md](./012-parallel-work-handoff.md), and the
  wave I/III runbooks as the program's execution authority.
- **Written:** 2026-08-23
- **Base:** `d478679` (local `main`, 536 commits ahead of `origin/main` = `7d24381`)
- **Semantic authorities (unchanged):** [RFC 012](./012-evidence-backed-adapters-and-progressive-readiness.md)
  (ratified) and [RFC 012A](./012a-agent-adaptation-and-engine-boundaries.md) (ratified);
  [012B](./012b-catalog-readiness-and-progressive-startup.md),
  [012C](./012c-runtime-semantics-and-usage-v2.md),
  [012D](./012d-session-scoped-observation.md) as draft semantic contracts whose
  *exact API/wire shapes are provisional* (`ProposedApi`) and are redesigned here.
- **Owner/integrator:** James (with one integrator session); lanes are Opus agents.

## 1. Why this plan exists

Assessment on 2026-08-23 (3 independent agents + direct measurement):

| Fact | Value |
| --- | --- |
| Rust LOC 2026-08-14 → 2026-08-22 | 97k → 316k (+218k, 0 files deleted, 88:1 add/delete) |
| User-visible change (`apps/playground`, `packages/cli`) | 0 files |
| Real consumers of RFC 012 output | none (Chopsticks pins SDK 0.5.16 and imports `watchSessionTranscript`; VibeField has no spaghetti dependency) |
| CI | none since 2026-08-11; 536 commits unpushed |
| Dead code | crate-wide `#![allow(dead_code)]`; 501 dead items when the lint is forced |
| Shape | one 40,662-line file; 30 files > 2k lines; 11 event families hand-written 5×; 18k lines of hand-written TS validators; 3 usage paths; 5 readiness surfaces; 2 catalog paths |
| Observer | 8 of 11 families hard-error at the wire; no test tails a real file |
| Process | 4,344-line plan edited 240×; 22% of commits docs-only |

The RFC's ideas are right and evidence-backed. The implementation grew by
accretion under a gate-driven multi-lane loop with no consumer, no CI, and no
deletion pressure. This plan keeps the semantics, replaces the implementation
with a lean one, wires it to the real consumers, and restores discipline.

## 2. Decision

1. RFC 012 semantics stand: one decode spine; adapters own native
   interpretation only; catalog membership is a first-class fact; readiness is
   a vector; queries are pure; one database authority; a store-free scoped
   observer sharing decoder/reducers with the durable host; response-level
   usage; deterministic identity (`ExternalEntityRef`, `SemanticRevisionRef`);
   epochs with full-replacement resync.
2. The implementation is **replaced, not extended**. Every lane deletes the
   path it replaces in the same PR series. No retained/shadow/legacy path
   survives the landing except through an explicit, dated compatibility window
   with a removal PR already scheduled.
3. Exact wire/API shapes in 012B/C/D are provisional and are redesigned for
   size and generated types. Semantic contracts (identity, ordering, epochs,
   reducer laws, fail-closed behavior) are preserved and tested.
4. Work is organized around **consumers**, not gates. A lane is done when a
   named consumer (playground, CLI, Chopsticks, VibeField) can use the result.
5. The multi-lane gate loop, the plan-as-ledger, and the dedup plan are
   retired. Status lives in §8 of this file and in PRs; nothing else.

## 3. Consumers and the landing surface

The landing surface is the *complete* list of capabilities RFC 012 must
deliver. Nothing else is in scope until these ship.

### 3.1 Chopsticks (replaces `watchSessionTranscript`)

`observeSession(request)` — store-free observer for one Claude session tree:

- typed events for **all 11 families**: message, content block, tool
  call/result, user-input request, plan, task, native marker, effective state
  (mode/model/effort), actor run, actor affiliation, usage-v2 — plus control
  events: bootstrap complete, reset/rewrite, overflow, resync complete, close;
- deterministic `event_id`, `scope_epoch`, `SemanticRevisionRef` on every
  semantic event; actor/run identity on every event;
- watch-before-scan, reset-before-replay, bounded queues, overflow → new epoch
  with full snapshot replacement (RFC 012D §10–§13 semantics);
- follows root transcript + subagent transcripts + declared sidecars, no global
  enumeration, no SQLite;
- one async-iterator style SDK API with generated types; no hand-written
  parsers.

### 3.2 VibeField Phase A (from `docs/petition/vibefield-needs.md` §7)

- stable `SessionRef`/`ProjectRef` (= RFC 012A `ExternalEntityRef`), native
  session id when provable, identity conflicts explicit;
- native project-association evidence with provenance;
- durable query watermark (`at_commit_seq`) and snapshot-consistent pagination;
- stable `SemanticRevisionRef` shared by durable queries and the observer;
- observer epoch + full-replacement resync.

### 3.3 Playground and CLI

- **catalog-first startup**: projects/sessions visible (complete or explicitly
  degraded) before history/usage/FTS converge; one readiness vector
  `{catalog, history, usage, capabilities, artifacts, search}` exposed to the
  UI and `spag doctor`;
- **corrected usage**: response-level (usage-v2) semantics are *the* usage in
  `spag stats`, `getStats`, and playground; legacy additive usage removed.

## 4. Target shape and budgets

| Area | Keep / replace | Budget (prod LOC, tests separate) |
| --- | --- | --- |
| `core/`, `source/` (incl. `append_delimited.rs`), `decode_runtime.rs` | keep | as is |
| `adapter/` + `claude/ codex/ grok/ factory/` | keep `AgentAdapter` + compiled-in `agent-support/`; collapse per-adapter triplicates; one `version` field; promote real releases | ≤ 15k |
| `engine/` durable | keep 011 core (writer/commit/query_pool/query packs); usage-v2 only; simple catalog (one table + readiness vector); delete 012B publication/epoch/lineage machinery and usage selection/shadow stacks | ≤ 45k |
| `observer/` (new, replaces `scoped_observation*`) | rebuild on `append_delimited` + `decode_record` + reducers; generic wire for 11 families | ≤ 6k (+ ≤ 4k behavioral tests) |
| `napi_engine.rs` | thin; no hand-mirrored DTO layer beyond what napi-rs needs | ≤ 3k |
| `packages/sdk` contracts | generated from Rust (ts-rs/schemars); curated barrel; no hand-written validators of native output | generated |
| Whole crate | — | ≤ 150k incl. tests by end of landing (from 316k) |

Hard limits (enforced by `scripts/validate-all.sh` from Wave 0): no crate-level
`allow(dead_code)`; no production `.rs` file > 3,000 lines; inline `mod tests`
> 500 lines moves to `tests.rs`; SDK barrel exports are an explicit allowlist.

## 5. Engineering rules (non-negotiable, every PR)

1. Targets `main`; CI green (`cargo test`, `pnpm typecheck`, `pnpm validate`,
   `pnpm test:packages`, `git diff --check`, format).
2. Deletes the path it replaces. A PR that adds > 500 net lines must ship a
   consumer-visible capability named in §3, or be a pure deletion/refactor.
3. Every new `pub` type/function has a caller in the same PR. Zero
   `dead_code` warnings in touched modules.
4. TS types are generated from Rust; no hand-written mirrors of native output.
   Fixture-based Rust↔TS parity is a *test*, not a second implementation.
5. Tests are behavioral (real temp files/JSONL for the observer; real SQLite
   for the engine; frozen fixtures only as goldens). Digest-stability tests do
   not count as coverage.
6. PR description states: consumer served, files deleted, prod/test LOC delta
   before→after, perf numbers when the path is on a budget (§6).
7. No edits to superseded plans; no status ledgers. Child RFC amendments are
   ≤ 300-line semantic contracts: decisions, interfaces, acceptance tests.
8. Shared files (`lib.rs`, `engine/mod.rs`, `napi_engine.rs`, `core/schema.rs`,
   `Cargo.toml`, `packages/sdk/src/index.ts`, `native.ts`, `client/*`) may be
   edited minimally by any lane; every such edit is listed in the PR and the
   integrator resolves overlaps.

## 6. Performance budgets (measured, not ratified ceilings)

- Catalog visible (warm, last-complete catalog): < 1 s after engine open;
  cold catalog on the production-shaped corpus (1.97M records): < 10 s to a
  complete/degraded library; history/FTS continue in background.
- Observer: attach + bootstrap of a 50 MB session tree < 500 ms; steady-state
  event latency from append to consumer < 50 ms; bounded memory per scope.
- Usage-v2 query: no regression vs. legacy `getStats` p95.
- Bench gate workflow stays green; `scripts/bench-observation.ts` and
  `scripts/bench-queries.ts` are the instruments.

## 7. Waves and lanes

### Wave 0 — integrator (2026-08-23)

- Freeze base `d478679`; local `main` fast-forwarded to it.
- Remove `#![allow(dead_code)]`; publish dead-item list to lanes.
- Land this plan; mark superseded docs; add size/barrel ratchets to
  `validate-all.sh`.
- Create lane worktrees on `/Volumes/SamsungRed/spaghetti-rfc012/worktrees/land-*`.
- Push `main` + backup branch `archive/rfc012-program-2026-08-22` once the
  owner confirms (first CI run since 08-11).

### Wave 1 — four parallel lanes (disjoint ownership)

| Lane | Consumer served | Owns (exclusive) | Deletes |
| --- | --- | --- | --- |
| **L1 observer** | Chopsticks, VibeField §3.2 epoch/refs | `crates/spaghetti-napi/src/observer/` (new), `scoped_observation*.rs`, `scoped_observation/`, `observation_contract*`, `scoped_observation_napi.rs`, `scoped_observation_transport.rs`, `fixtures/contracts/rfc012d-*`, `runtime_semantic_reducer.rs` (reducers are shared; changes minimal and listed) | the old scoped-observation tree (~74k) |
| **L2 sdk-api** | all | `packages/sdk/src/contracts/`, `scoped-observation.ts`, `observation-shadow.ts`, `sources/claude-code/live/session-observation-shadow.ts`, `index.ts` barrel, `native.ts` (observer section), codegen tooling | hand-written rfc012 TS validators (~18k) |
| **L3 usage** | playground, CLI | `engine/runtime_usage*.rs`, `engine/usage_query.rs`, `engine/runtime_semantic_merge/`, `engine/runtime_semantic_projection.rs`, `semantic_contract*.rs`, `unknown_evidence_reducer.rs`, `packages/cli/src/commands/stats.ts`, playground usage views, usage schema tables, `scripts/usage_v2_oracle` | legacy usage path, query-pack selection, shadow stacks |
| **L4 catalog** | playground, CLI, VibeField §3.2 refs | `catalog_contract*`, `engine/catalog_*`, `engine/catalog_query/`, `engine/progressive_startup.rs`, `*/catalog_runtime.rs`, `*/catalog_conformance.rs`, `source/catalog_composition.rs`, `observation-host.ts`, `packages/cli/src/commands/{projects,sessions,doctor}.ts`, playground library screens, catalog schema tables | 012B publication/epoch/lineage machinery (~40k) |

Lane briefs live in `docs/rfcs/012-landing-lanes/<lane>.md` (written by the
integrator; short). Each lane produces a PR series on `land/<lane>`; the
integrator merges into `main` in the order L2 → L3 → L4 → L1 (L1 last because
it is the largest and depends on L2's codegen).

### Wave 2 — after Wave 1 merges

- **L5 adapters/012A** (now the critical path for Chopsticks): the Claude
  decoder emits only 3 of the 11 RFC 012C fact families (actor-run,
  actor-affiliation, usage-v2); message, content-block, tool, effective-state,
  user-input-request, task, plan, and native-marker have reducers, fixtures and
  observer wire but **no emitter**. L5 makes the Claude adapter emit them
  (RFC 012A: adapters emit facts only), with `ts_rs::TS` on `adapter/facts.rs`
  so `SemanticEvent.value` is typed; then collapse per-adapter triplicates;
  one `version` field replacing candidate/promoted; promote real
  Claude/Codex/Grok support releases; read Grok's per-response usage from
  `updates.jsonl` (`params.update.usage.*`) so Grok usage becomes exact
  instead of session-estimated; delete `candidate-2026-08-15`; restore
  the 08-15 scope relation set so `observer/scope.rs` evaluates the declared
  `ScopeProgram` instead of resolving locators in Rust; keep fixture-agent as
  a test fixture only.
- **L6 napi/SDK surface**: collapse `Engine*` mirror DTOs to what napi-rs
  needs; delete the hand-written mirror of `index.d.ts` in
  `packages/sdk/src/native.ts` (import the generated `@vibecook/spaghetti-sdk-native`
  types instead); replace the remaining Rust-parser-vs-TS-parser parity tests
  with Rust-output-vs-generated-type tests and then delete
  `contracts/rfc012a.ts`, `rfc012c.ts`, `rfc012-semantic-json.ts` (~5.4k lines;
  parity is a test, a second parser is not); readiness vector as the only
  status surface.
- **L7 perf** (top item: durable ingest throughput). The 2026-08-15 profile
  ingested 1,969,824 records in 207 s (~9.5k rec/s); today a full rebuild of
  the real corpus runs at ~100–650 rec/s (L3: 656 rec/s on a slice; L4: 213k
  messages in 38.7 min, ~3 h extrapolated) — a 15–100× regression accumulated
  during the 012 period (candidates: per-object `MAX_APPEND_RECORDS_PER_RECONCILE`
  = 4,096 per pass, fact_records writes, coverage/provenance bookkeeping, FTS
  finalization, commit granularity). Profile, fix, and prove on the
  production-shaped corpus; every schema bump costs users a full rebuild. Then:
  Claude decode ≤ 10 ms/MB (L5 item 3), observer budgets, catalog budgets
  (met), usage query p95; publish one report.
- **L8 docs**: trim 012B/C/D to ≤ 300-line semantic contracts matching what
  shipped; archive censuses/runbooks under `docs/rfcs/archive/`; README and
  CHANGELOG updated.

### Wave 3 — release and downstream

- Release 0.8.0 (usage correction, catalog-first, `observeSession`).
- Chopsticks PR: bump SDK, replace `watchSessionTranscript` with
  `observeSession`; keep the old tail one release as fallback.
- VibeField: integration note listing the §3.2 surface with examples.

## 8. Status (integrator-only; updated at wave boundaries)

| Item | State | Evidence |
| --- | --- | --- |
| Wave 0 | done 2026-08-23 (local `main` `3db39a7`) | plan + lane briefs landed; `allow(dead_code)` removed (`a08c013`); code-shape ratchet in `validate-all.sh` (`8753f28`); lane worktrees `land-l1..l4` on the SSD; push of `main` + archive branch awaiting owner go-ahead |
| L1 observer | **merged** `90e6bec`/`4a19e28` + L1b `0fd6ba3` (2026-08-23) | `observer/` 3,625 prod / 1,418 test LOC (budget 6k/4k); 97,486 lines deleted (scoped_observation tree, rfc012d fixtures/contracts, observation_contract; `adapter/registry.rs` 17,800→992); all 11 families on the wire; 25 behavioral file tests + Node smoke test; adapter-neutral; L1b: watcher-directed dirty set + stat pre-check sweep — append→consumer p95 40.2 → 8.3 ms at 674 objects (0.2 ms at 1 object), object opens during bootstrap 21,254 → 1,428; fixed `bootstrap_complete` firing after 64×1,024 records with a 66k-record both-directions regression test; root bootstrap 635 ms @ 43.7 MB is decoder-bound (adapter 64%, io 22%, reduce 10%) → Wave 2 L5; JSON-string transport 2.3–2.6× faster than napi object marshalling; Rust 916/916, SDK 433/0, CLI 110/0, validate-all 9/9 |
| L2 sdk-api | Phase A merged `a0bc677`; **Phase B merged `ff1e2da`** (2026-08-23); final AbortSignal option pending | Phase A: ts-rs pipeline + `pnpm generate:types` + CI diff; 13,694 lines of hand-written contracts/shims deleted; barrel 38 → 17 explicit exports (allowlist); VibeField Phase A refs generated + tested on real engine output; `watchSessionTranscript` restored to the barrel (missing at base). Phase B: `observeSession(request, options)` async-iterator SDK API over the native observer (single consumer, one-batch buffering, close-on-exit), 7 behavioral tests on real `.claude`-shaped trees, Chopsticks README section; found the observer `bootstrap_complete`/65,536-record bug (routed to L1). SDK 431/0, CLI 110/0, validate-all 9/9 |
| L3 usage | **merged** `71c7268` + `d6fed37` (2026-08-23) | response-level usage is the only usage path: Codex/Grok adapters now emit response-level facts (Codex legacy double-counted cache-read inside input and reasoning inside output; Grok delta 0); legacy `usage_contributions`/`usage_totals`, `query_pack_selections`, shadow/selected packs, `Fact::Usage`, 5 napi methods, usage experiments deleted (+2,157/−12,688); `spag stats` + playground show corrected totals with value quality; oracle exact on in-repo fixture (119 responses) and real slice (5,238 responses); Claude full corpus 78.52B → 36.88B tokens (2.129×, 362,043 rows → 158,118 responses); getStats p95 −24%, getUsage p95 +0.7 ms (accepted); SCHEMA_VERSION 63 (rebuild ≈29 min on 2.9 GB corpus); Rust 912/912, SDK 429/0, CLI 110/0, validate-all 9/9 |
| L4 catalog | **merged** `d92c5b7` (2026-08-23) | catalog-first startup: discovery pass → `projects`/`sessions` catalog rows with `catalog_state`/degraded/external refs, one `Readiness` vector (ts-rs) consumed by the host, `spag projects/sessions/doctor`, playground indicator; real corpus: catalog listable **122 ms cold / 8 ms warm** (budget 10 s / 1 s); 66,777 lines of 012B machinery deleted (+9,896/−77,467; Rust 218,013 → 153,278, SDK → 61,880; schema tables 108 → 100, SCHEMA_VERSION 64); whole-response equality guard for listProjects/listSessions/getOverview/search/listMemoryDocuments vs a `3db39a7` baseline; three bugs fixed (1.7 s COUNT→EXISTS readiness, degraded_reason CHECK abort, CLI projectId); Rust 666/666, SDK 399/0, CLI 117/0, validate-all 9/9. Follow-up: playground library list onto the catalog path (2 IPC channels). Found: full rebuild of the real corpus extrapolates to ~3 h → L7 |
| Wave 2 | in progress: **L5 adapters** (`land/l5-adapters`) — owns the last 109 clippy warnings; L6/L7/L8 not started. Done: **L1c** `df5a459`, **L1d** `2f83d8b`, **L4b** `a36454f` (playground library in 491 ms), playground tests in CI `5c16423`, **L0 hygiene** `d8e1033` (141 → 0 clippy warnings in source/engine/core/napi, −3.5k LOC, 21 subject-less tests removed, invariants documented; `cargo check --features legacy-oracle` kept clean) | Rust crate 315,610 → **149,896** LOC; SDK 81,349 → 61,977; ratchet baseline 4/19/17; Rust 645/645 (2 ignored), SDK 399/0, CLI 117/0, playground 35/0, validate-all 9/9. Open: SDK "Grok native cold ingest smoke" suite silently skips (legacy addon gate) → L6/L7; `--features legacy-oracle` is not in CI |
| Wave 3 | not started | — |
