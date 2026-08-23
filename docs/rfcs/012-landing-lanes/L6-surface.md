# Lane L6 — napi/SDK surface: one source of truth for every type that crosses the boundary

Worktree: `/Volumes/SamsungRed/spaghetti-rfc012/worktrees/land-l6-surface`, branch `land/l6-surface`.
Read `COMMON.md` first. Base: `main` ≥ `6d91eef`. L5 (adapters) is in flight and owns
`adapter/`, `claude|codex|grok|factory/`, `runtime_semantic_reducer.rs`, `agent-support/`.

## Outcome

Every type that crosses Rust→TypeScript has exactly one definition (Rust) and is
generated (napi-rs `index.d.ts` for `#[napi]` items, ts-rs for serde types). No
hand-written mirror, no second parser. The playground/CLI/SDK public API keeps
its shapes (the whole-response equality guard in
`packages/sdk/src/__tests__/observation-host.test.ts` must keep passing).

## Work (in order)

1. **Delete the hand-written mirror `packages/sdk/src/native.ts`** (≈2,100
   lines re-declaring the napi-generated `SpaghettiEngine*`/`Engine*` DTOs).
   Import the generated types from `@vibecook/spaghetti-sdk-native`
   (`crates/spaghetti-napi/index.d.ts`) instead; keep only the addon loader and
   genuinely hand-written glue (e.g. the legacy-addon guard), in a small file.
   Every `client/*`, `observation-*.ts`, `api.ts`, `observe-session.ts` import
   is updated; typecheck/tests/lint/build/check:sdk-package green.
2. **Collapse the `Engine*` DTO mirror layer in `crates/spaghetti-napi/src/napi_engine.rs`**
   (≈141 `#[napi(object)]` structs + ≈106 `impl From<>` that re-express engine
   types field-for-field, e.g. `FactFamilyCoverageItem` → `EngineFactFamilyCoverageItem`
   with `u64→f64`). Where napi-rs can expose the engine type directly, do so;
   where a type is serde-shaped (nested enums/unions), return it as a JSON
   string or `serde_json::Value` with a ts-rs type (L1 measured JSON string
   2.3–2.6× faster than napi object marshalling for large batches — measure
   for the hot paths you touch). Preserve every public method's JS-visible
   shape unless it is provably unused (grep packages/ and apps/), and list any
   removal. Target: `napi_engine.rs` ≤ 3,000 lines with no field-for-field
   duplicate of an engine struct.
3. **Parity tests → generated-type tests; delete the TS parsers.** Replace the
   remaining Rust-parser-vs-TS-parser parity tests (`contracts/__tests__/*`
   using `parseRfc012aV1Json`/`parseRfc012c*V1Json`) with tests that assert
   real Rust output conforms to the generated types and the semantic rules
   consumers rely on (see `packages/sdk/src/__tests__/vibefield.test.ts` for
   the pattern). Then delete `contracts/rfc012a.ts`, `rfc012c.ts`,
   `rfc012-semantic-json.ts` and their tests, the `parseRfc012*V1Json` napi
   helpers in `semantic_contract_napi.rs` (and `semantic_contract.rs` if it
   becomes dead; `lib.rs` gates it `#[cfg(not(test))]` today), and the
   `portable_relatives` remnant in `scripts/architecture/check_rfc011_boundaries.py`.
   Nothing in `agent-support/` or `adapter/` (L5's) — if a fixture there is only
   consumed by a deleted test, list it for L5 rather than deleting it.
4. **Re-gate or delete the silently-skipping SDK suite** "Grok native cold
   ingest smoke" (gated on `loadLegacyNativeAddon()` requiring `ingest`/`liveIngestBatch`
   exports that the default addon no longer has). If it only has meaning under
   `--features legacy-oracle`, make it run there (CI doesn't build that
   feature; say so) or delete it with its fixture. No suite may skip silently
   in the default run.
5. Zero clippy warnings in your files; `pnpm generate:types` idempotent; CI
   "Generated files are current" step still covers every generated location.

## Ownership / conflicts

You own `napi_engine.rs`, `napi_catalog.rs` (DTO layer only), `semantic_contract*.rs`,
`packages/sdk/src/native.ts`, `client/*`, `api.ts`, `observation-*.ts`, `contracts/`,
`index.ts` barrel (allowlist only shrinks). L5 may add `#[ts(export)]` derives in
`adapter/facts.rs` and regenerate types — expect trivial conflicts in
`packages/sdk/src/generated/index.ts`; regenerate after merging main. L7 (perf)
owns `engine/*` internals; you touch `engine/*` only for `#[napi(object)]`/ts-rs
derives on types you expose, and list them.
