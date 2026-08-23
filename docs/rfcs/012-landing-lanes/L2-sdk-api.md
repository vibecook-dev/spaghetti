# Lane L2 — SDK public API, generated types, barrel curation

Worktree: `/Volumes/SamsungRed/spaghetti-rfc012/worktrees/land-l2-sdk-api`, branch `land/l2-sdk-api`.
Read `COMMON.md` first.

## Consumer and outcome

Every consumer (playground, CLI, Chopsticks, VibeField) reaches Spaghetti
through `@vibecook/spaghetti-sdk`. Outcome: a small, deliberate public API
whose native-facing types are generated from Rust, with the 18k lines of
hand-written RFC 012 TypeScript validators gone and a codegen pipeline that
CI enforces. You also own the thin `observeSession` SDK wrapper once L1's
native class exists (second phase).

## Facts

- `packages/sdk/src/contracts/rfc012*.ts` = 18,226 lines / 25 files of
  hand-written parsers (2,243 guard lines, 1,050 `ContractValidationError`
  throws) re-validating what serde already enforces. No codegen exists.
- `packages/sdk/src/index.ts` gained 55 lines of `export *` from those
  contracts; SDK export symbols grew 868 → 1,448. No consumer imports them.
- `scoped-observation.ts`, `observation-shadow.ts`,
  `sources/claude-code/live/session-observation-shadow.ts` wrap the old native
  observer behind `rfc012dObserver: false`; nothing calls them. L1 is deleting
  the native side; delete these.
- Fixture-parity tests under `contracts/__tests__/` read Rust-owned fixtures in
  `crates/spaghetti-napi/fixtures/contracts/` — the one good idea: keep parity
  *tests* where they verify semantics consumers rely on (e.g.
  `ExternalEntityRef`/`SemanticRevisionRef` derivation if TS derives anything);
  drop tests that only pin a hand-written parser.
- Chopsticks imports exactly `watchSessionTranscript`, `SessionMessage`,
  `SessionTranscriptTail` (pinned SDK 0.5.16). Do not break those.

## Phase A (do now, independent of other lanes)

1. **Codegen pipeline**: add `ts-rs = { version = "12", features = ["serde-compat"] }`
   to `crates/spaghetti-napi/Cargo.toml`; configure export to
   `packages/sdk/src/generated/` (e.g. `TS_RS_EXPORT_DIR` in a small
   `scripts/generate-types.mjs` that runs `cargo test -p spaghetti-napi --lib export_bindings`,
   or ts-rs's `#[ts(export_to = ...)]`); add `pnpm generate:types`; make the
   existing CI "Generated files are current" step also diff
   `packages/sdk/src/generated`. Prove it on 2–3 existing serde types that
   cross the boundary today (e.g. the engine status/readiness structs or
   `ExternalEntityRef`) and use them from SDK code.
2. **Delete** `contracts/rfc012d-*.ts`, `rfc012c-unknown-evidence.ts`,
   `scoped-observation.ts`, `observation-shadow.ts`,
   `session-observation-shadow.ts`, their tests, and their barrel exports.
   For `rfc012a.ts`, `rfc012b*.ts`, `rfc012c.ts`: delete what has no
   non-test importer outside L3/L4-owned files; for pieces imported by
   `client/*`/`observation-host.ts`/`observation-service.ts` that L3 (usage) or
   L4 (catalog) are replacing, leave them and list them in your report — those
   lanes delete them with their callers. Never leave an `export *` of a
   contracts module in the barrel.
3. **Barrel**: replace `export *` with explicit named exports grouped by
   consumer (core client/API, legacy live tail, observer, VibeField refs).
   Write the allowlist down in `packages/sdk/README` or a top comment. The
   code-shape ratchet counts export statements — it may only go down.
4. **VibeField Phase A surface**: ensure `SessionRef`/`ProjectRef`
   (= `ExternalEntityRef`), `at_commit_seq` watermark on query results, and
   `SemanticRevisionRef` are exposed as *generated* types from the Rust
   definitions (coordinate names with what exists in `adapter/` /
   `catalog_contract` today; do not invent a second definition in TS).
5. Update `packages/sdk` tests accordingly; `pnpm typecheck`,
   `pnpm test:packages`, `pnpm --filter @vibecook/spaghetti-sdk build`,
   `pnpm check:sdk-package` green.

## Phase B (after L1 lands its napi class — the integrator will message you)

`observeSession(request): SessionObserver` — async-iterator style wrapper over
`SpaghettiSessionObserver` using generated `ObserverEvent` types; close on
iterator return; no hand-written parsing; a Node test against a fixture
session dir; a short usage example in the SDK README aimed at Chopsticks.

## Out of scope

Rust engine internals, catalog/usage query semantics (L3/L4), the legacy
`watchSessionTranscript` implementation.
