# Lane L5 — adapters (RFC 012A/012C): emit the missing fact families, promote real releases, decode perf

Worktree: `/Volumes/SamsungRed/spaghetti-rfc012/worktrees/land-l5-adapters`, branch `land/l5-adapters`.
Read `COMMON.md` first. Base: `main` ≥ `fcaadb6` (L1, L1b, L2, L3 merged; L4 catalog still in flight).

## Consumer and outcome

Chopsticks now has `observeSession()` (L2) over the rebuilt observer (L1), and all
11 RFC 012C families cross the wire — but the **Claude decoder emits only 3 of
them** (actor-run, actor-affiliation, usage-v2). Message, content-block, tool,
effective-state, user-input-request, task, plan, and native-marker have reducer
laws (`runtime_semantic_reducer.rs`), contract fixtures, and observer support,
and **zero emitters**. Chopsticks cannot drop `watchSessionTranscript` until
messages, content blocks, tool calls/results, and effective state (mode/model/
effort) arrive as typed events. This lane is the critical path for Wave 3.

## Priorities (in order; report after 1–3 if context runs low, then continue)

1. **Emit the 8 families from the Claude adapter** (`claude/adapter.rs` +
   siblings; adapters emit facts only — RFC 012A). Source them from the
   transcript records (user/assistant messages and their content blocks; tool
   use/result pairs as correlated lifecycle; permission-mode/model/effort
   changes as effective state; local-command / system-reminder / hook markers
   as native markers; user-input requests) and from declared sidecars for
   task/plan where the ADS declares them. Follow the reducer classes exactly
   (current-generation log for message/content-block/native-marker;
   correlated lifecycle for tool and user-input; revisioned entity for
   effective-state/plan; owned-set snapshot for task). Deterministic fact
   identity per RFC 012A. Behavioral tests on fixture JSONL (in
   `crates/spaghetti-napi/fixtures/` and `agent-support/claude-code/*/fixtures`)
   asserting reduced state per family; plus one end-to-end test through
   `observeSession` (packages/sdk/src/__tests__/) showing a message, a tool
   call+result, and a mode change arriving as typed events for a real
   `.claude`-shaped tree. Keep `decode_record` as the single spine — both the
   durable path and the observer get these families for free; verify the
   durable projections/queries don't regress (full `cargo test`).
2. **Type `SemanticEvent.value`**: add `ts_rs::TS` derives to the fact/revision
   types in `adapter/facts.rs` (and what they reference) so the generated
   `ObserverEvent` union types the payload per family; run `pnpm generate:types`;
   the SDK `isSemanticEvent` narrowing (L2) should then give typed values.
   Update the Chopsticks README section accordingly (remove the "value is
   unknown" caveat).
3. **Decoder performance** (shared with durable ingest): L1 profiled a single
   88.5 MB transcript at 875 ms = adapter decode 560 ms (64%) + io 189 + reduce
   91; target ≤ 10 ms/MB end-to-end (≈ 500 ms @ 50 MB). Profile the Claude
   decode path (sonic-rs parse, allocation, per-record fact construction) and
   remove the waste without changing semantics; prove no fact/revision digest
   changes on the committed fixtures; report before/after on the same file.
4. **Restore the scope relation set**: `agent-support/claude-code/candidate-2026-08-21/scope-programs.json`
   declares only `root-transcript`; the 08-15 candidate had child/workflow/
   team/sidecar relations. Restore them (evidence-backed, with the ADS and
   fixtures) so `observer/scope.rs`'s hard-coded locator templates can be
   replaced by evaluating the declared `ScopeProgram` (L1 will do the evaluator
   once the declarations exist — coordinate by leaving the observer untouched
   and telling the integrator when the declarations land).
5. **Grok exact usage**: read per-response `params.update.usage.{inputTokens,
   outputTokens,cachedReadTokens,cacheCreationTokens}` from Grok `updates.jsonl`
   so Grok usage-v2 facts are `Exact` per response instead of one
   session-scoped `Estimated` revision; oracle/fixture proof; preserve the
   TS `distributeGrokSessionTokens` behavior and tests untouched.
6. **Support releases**: one `version` field replacing candidate/promoted;
   promote the real Claude/Codex/Grok releases with the evidence the bundles
   already carry (`scripts/agent_support/validate.py` must pass); delete
   `agent-support/claude-code/candidate-2026-08-15` and its 27 references;
   keep `fixture-agent` as a test fixture only (not a "promoted" release);
   collapse per-adapter `support_probe`/conformance-shell triplicates into
   shared helpers — **but do not touch `*/catalog_runtime.rs` or
   `*/catalog_conformance.rs`** (L4 is deleting/replacing them).

## Ownership and conflicts

You own `adapter/`, `claude/` (except `catalog_runtime.rs`/`catalog_conformance.rs`),
`codex/`, `grok/`, `factory/`, `agent-support/`, `scripts/agent_support/`,
`decode_runtime.rs`, and minimal listed edits to `runtime_semantic_reducer.rs`.
L4 (catalog) is still in flight and may touch `claude/adapter.rs` for
discovery hooks and `core/schema.rs`: keep your `claude/adapter.rs` edits in
the decode/emit paths, merge `main` before finishing, and list conflicts. Do
not edit `observer/` (L1's; ask via the integrator), `engine/` usage/catalog
code, or the SDK barrel beyond generated types + README.
