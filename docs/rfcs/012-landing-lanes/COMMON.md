# Common lane brief — RFC 012 landing (read first)

You are one lane of the RFC 012 landing described in
`docs/rfcs/012-landing-plan.md` (read it fully; it is short). The plan's §2
decision, §3 landing surface, §4 budgets, and §5 rules are binding.

## Environment

- Your worktree: `/Volumes/SamsungRed/spaghetti-rfc012/worktrees/land-<lane>`
  on branch `land/<lane>`, branched from `main` at `8753f28`. Work ONLY there.
  Never run cargo/pnpm in `/Users/jamesyong/Projects/project100/p008/spaghetti`
  (the integrator's checkout) and never touch other lanes' worktrees.
- `export CARGO_TARGET_DIR=/Volumes/SamsungRed/spaghetti-rfc012/build/land-<lane>/target`
  for every cargo command (first build ≈ 3–5 min; later incremental).
- `pnpm install` is done. A prebuilt `crates/spaghetti-napi/spaghetti.darwin-arm64.node`
  + `index.js` are seeded so SDK tests run; after you change Rust that the SDK
  exercises, rebuild with `cd crates/spaghetti-napi && pnpm build:debug`
  (debug napi build) before running SDK tests.
- Real Claude Code data exists under `~/.claude/projects` on this machine. You
  may read it for behavioral verification (read-only). Never copy native
  content, paths, prompts, or IDs into fixtures, tests, docs, or commit messages.
- Do NOT push. Do NOT merge. Do NOT edit `docs/rfcs/012-implementation-plan.md`,
  `012-implementation-deduplication-plan.md`, wave runbooks, or
  `012-landing-plan.md` §8 (integrator-only). Do not create new plan/status
  documents.

## Rules (from the landing plan §5 — non-negotiable)

1. Replace, don't extend: delete the path you replace in the same PR series.
   No retained/shadow/legacy/"compat" duplicates unless the brief names an
   explicit compatibility window.
2. Every new `pub` type/fn has a caller in the same commit series. Zero
   `dead_code` warnings in your modules when you finish (`cargo build` prints
   them now that the crate-level allow is gone; ignore other lanes' modules).
3. No hand-written TypeScript mirrors of native output. Types cross the
   boundary via generated bindings (napi-rs `index.d.ts` for `#[napi]` items;
   `ts-rs` exports for serde types — see L2 convention below).
4. Tests are behavioral: real temp files / real SQLite / real decode; frozen
   fixtures only as goldens. No digest-stability or struct-round-trip tests.
5. No production `.rs` file > 3,000 lines; inline `mod tests` > 500 lines go
   in a sibling `tests.rs`. `python3 scripts/code_shape/check_code_shape.py`
   must pass (it is a ratchet; it only forbids getting worse). Never edit
   `scripts/code_shape/baseline.json` except to *lower* numbers.
6. Shared files (`lib.rs`, `engine/mod.rs`, `napi_engine.rs`, `core/schema.rs`,
   `Cargo.toml`/`Cargo.lock`, `packages/sdk/src/index.ts`, `native.ts`,
   `client/*`) may be edited minimally; list every such edit in your report.
7. Keep RFC 012 *semantics* (identity, ordering, epochs, reducer laws,
   fail-closed decoding, one decode spine, no DB/query authority for
   adapters/observer). Exact wire/API shapes are provisional — simplify them.
8. Performance matters: no regressions on paths you touch; measure where the
   plan §6 gives a budget and report numbers.

## ts-rs convention (owned by L2, adopted by all lanes)

- Cargo: `ts-rs = { version = "12", features = ["serde-compat"] }` in
  `crates/spaghetti-napi/Cargo.toml` (if L2 has not landed it yet, add it
  yourself with this exact line; the integrator dedups at merge).
- Public serde types that cross to TS: `#[derive(serde::Serialize, serde::Deserialize, ts_rs::TS)]`
  `#[ts(export, export_to = "packages/sdk/src/generated/")]` relative to the
  repo root is what L2 configures via `TS_RS_EXPORT_DIR`; until then use
  `#[ts(export)]` and L2 will wire the export directory. Generated files are
  committed and checked in CI.
- Tagged unions: `#[serde(tag = "type", rename_all = "snake_case")]`.

## Working style

- Commit early and often on your branch (conventional commits:
  `feat(...)`, `fix(...)`, `refactor(...)`, `chore(...)`, `test(...)`); each
  commit compiles and its focused tests pass. End every commit message with
  `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- If you run low on context, commit WIP with a precise message of what is
  done/undone and report; do not leave uncommitted work.
- Before reporting: `cargo fmt --all -- --check`, full
  `cargo test -p spaghetti-napi`, `pnpm typecheck`, `pnpm test:packages`,
  `bash scripts/validate-all.sh`, `git diff --check`.

## Your final report (this is the PR description; send it as your final message)

1. Consumer served and what they can now do (one paragraph).
2. What was deleted (paths + LOC) and what was added (paths + LOC); prod/test
   LOC before → after for your owned modules.
3. Commands run with exact pass/fail counts; perf numbers if applicable.
4. Every edit to shared files.
5. Open issues / follow-ups, and anything the integrator must know to merge
   (expected conflicts with other lanes).
