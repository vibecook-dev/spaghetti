# RFC 012 Wave III execution runbook

- **Status:** Active remaining-gate wave after skeptic rejection of the
  `c51c072` all-Gate-met stamp
- **Written:** 2026-08-21
- **Assignment base:** the integrator commit that lands this file; announced as
  `RFC012 WAVE III BASE <sha>`
- **Does not reuse:** stale SSD worktrees `a1-c1`, `a2`, `a3`, `b2`, `c2`,
  `c3`, `w1-*`, `w2-*`

Normative semantics remain RFC 012 / 012A–D and
[012-implementation-plan.md](./012-implementation-plan.md). This file is
operational only. Do not flip a package to Gate met from a lane. The
integrator updates section 4 only after Exit evidence is honestly met.

Honest remaining packages: **X0, B4, B5, C4, D2, D4, D5, X1, X2, X3, X4**.

Already Gate met and frozen this wave: E0, A1–A4, B1–B3, C1–C3, D1, D3.

## 1. Slots

| Role | Worktree | Branch | Packages |
| --- | --- | --- | --- |
| Integrator | `/Volumes/SamsungRed/spaghetti-rfc012/worktrees/root` | `work/rfc012-integration` | status table, delta ledger, merges |
| Lane B4 | `/Volumes/SamsungRed/spaghetti-rfc012/worktrees/w3-b4` | `work/w3-b4` | B4 host lifecycle |
| Lane C4 | `/Volumes/SamsungRed/spaghetti-rfc012/worktrees/w3-c4` | `work/w3-c4` | C4 engine/SDK merge consumer |
| Lane D2 | `/Volumes/SamsungRed/spaghetti-rfc012/worktrees/w3-d2` | `work/w3-d2` | D2 Claude composition |
| Lane D4 | `/Volumes/SamsungRed/spaghetti-rfc012/worktrees/w3-d4` | `work/w3-d4` | D4 observer epoch shadow |
| Lane X3 | `/Volumes/SamsungRed/spaghetti-rfc012/worktrees/w3-x3` | `work/w3-x3` | X3 physical crate |
| Lane EXP | `/Volumes/SamsungRed/spaghetti-rfc012/worktrees/w3-exp` | `work/w3-exp` | B5, D5, X1, X2 experiments |

```bash
# Per-lane cargo dir. Do not share one target dir across live lanes.
export CARGO_TARGET_DIR=/Volumes/SamsungRed/spaghetti-rfc012/build/w3-<lane>/target
```

Do not `cargo clean`. Do not prune other worktrees. Do not copy `draft/`.
Never run cargo, pnpm, or rust-analyzer in
`/Users/jamesyong/Projects/project100/p008/spaghetti`.

## 2. Checkpoints

1. Read-only freeze: objective, exact paths, invariants, negatives, validation.
2. Compile + focused tests, still unstaged.
3. Final unstaged diff. Wait for `STAGE <lane>` before any `git add`.
4. Commit on the lane branch only after `STAGE`. Integrator fetches and merges.

## 3. Universal lane rules

1. One owner, one worktree, one branch, one frozen path set.
2. Stay Candidate / unsupported for current agents. Never promote Claude,
   Codex, Grok, or Factory. Fixture-agent promotion does not satisfy X4.
3. Never `git add -A`, never touch another worktree, never edit
   `docs/rfcs/012-implementation-plan.md` or this runbook.
4. Native paths, IDs, prompts, content, and secrets never enter fixtures,
   Debug, logs, reports, or portable DTOs.
5. Authority is non-serializable and evidence-backed.
6. Preflight bounds before retaining attacker-sized input.
7. Report exact test pass counts. Run `git diff --check` before every handoff.
8. Stop and report rather than invent policy, authority, or path expansion.
9. Shared files (`Cargo.toml`, `Cargo.lock`, `index.d.ts`, crate `mod.rs`
   barrels, `napi_engine.rs` public surface) need prior integrator approval
   even for a one-line compile fix, except the frozen path set below.

## 4. Path ownership

| Lane | Owns | Must not touch |
| --- | --- | --- |
| B4 | `crates/spaghetti-napi/src/engine/mod.rs` host readiness; `crates/spaghetti-napi/src/engine/search_query.rs` complete-only gate if still needed; `crates/spaghetti-napi/src/engine/catalog_query/`; `packages/sdk/src/observation-host.ts`; `packages/sdk/src/contracts/rfc012b-pages.ts`; matching tests | usage merge, observer shadow, coverage crate, experiments, Claude decoder composition, promotion |
| C4 | `crates/spaghetti-napi/src/engine/runtime_semantic_merge.rs`; engine usage merge entry; SDK typed merge consumer under `packages/sdk/src/` without native JSON parsing | host lifecycle, search bootstrap, coverage crate, observer attach, experiments |
| D2 | Claude scoped composition tests/runtime for child, workflow, team, sidecar using the real Claude decoder, not `EmptyAdapter` | Factory/fixture-agent, promotion, catalog host, usage merge, experiments |
| D4 | `packages/sdk/src/sources/claude-code/live/session-observation-shadow.ts` and sibling live modules; epoch swap/rollback against the existing Rust scoped observer | observation-host.ts (B4), usage merge, catalog, coverage crate |
| X3 | `crates/spaghetti-coverage/`; `crates/spaghetti-architecture/`; `crates/spaghetti-napi/src/coverage_runtime.rs`; architecture checker JSON/Python; workspace/napi `Cargo.toml` + `Cargo.lock` if the coverage crate requires it | host, merge, observer SDK, experiments, adapter composition |
| EXP | `scripts/rfc012_experiments/` only | Rust engine, SDK, adapter, Cargo.toml |
| Integrator | Plan status, this runbook, delta ledger, `draft/`, merges | — |

## 5. Lane objectives

**B4.** Catalog-first host lifecycle. Last-complete/degraded catalog pages
remain readable while query bootstrap/FTS is incomplete. Search returns
`BootstrapInProgress` until complete. `progressiveStartupView` must be
consumed by the observation host (or an engine host helper it calls), not
only unit-tested in isolation. Do not ratify numeric p95 ceilings.

**C4.** `merge_durable_and_scoped_usage` must be reachable from
`SpaghettiEngineCore` / `runtime_usage_v2_cancellable` plus a typed SDK
consumer. Overlay join is `SemanticRevisionRef` / `event_id`. No native
payload parsing. Do not change `getUsage`.

**D2.** Prove Claude root/current/future actor plus sidecar composition
through the real Claude decoder and declared scope relations (child,
workflow, team, sidecar). `EmptyAdapter` + `Fact::UnknownRecord` is not
Exit evidence. Keep Candidate / unauthorized.

**D4.** Feature-flagged shadow must attach the real database-free observer
(or its typed event stream), compare against the transcript tail, and
atomically swap/roll back a scope epoch. Fixture-only
`rfc012dUsageEnvelopeShadowRecords` is not sufficient.

**X3.** `spaghetti-coverage` must compile, own membership encoding, and be
enforced by the architecture checker so `spaghetti-napi` cannot reimplement
the framed encoder. `spaghetti-architecture` must assert the coverage crate
exists as a workspace member. No store/N-API/adapter deps in the new crate.

**EXP.** Replace hardcoded timelines and `json.loads(fixture)` timings.
X1 must compare three complete-only FTS strategies against a frozen ingest
trace produced by real code. X2 must aggregate real `source_record_errors`
rows (in-repo sqlite fixture or census-compatible dump with no native
payloads). B5/D5 must time a real catalog retained-page operation and a real
observer attach/poll (subprocess to a focused test or a tiny in-repo
harness). Numeric values stay provisional.

**X0 / X4.** Integrator only. X4 stays In progress unless a current-agent
promoted bundle exists. Fixture-agent is not X4 Exit evidence.

## 6. Merge order

Integrator merges **X3 → C4 → B4 → D2 → D4 → EXP**, then reruns the
verification matrix, then updates section 4 only for packages whose Exit
evidence actually passed.
