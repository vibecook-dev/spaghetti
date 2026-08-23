# Lane L1 — store-free session observer (RFC 012D), rebuilt small

Worktree: `/Volumes/SamsungRed/spaghetti-rfc012/worktrees/land-l1-observer`, branch `land/l1-observer`.
Read `COMMON.md` first.

## Consumer and outcome

Chopsticks (`~/Projects/project100/p008/chopsticks/packages/adapter-claude/src/observation/transcript-observer.ts`)
today consumes `watchSessionTranscript` (raw `SessionMessage` tail, rewrite
flag) and projects it into its own events. VibeField Phase A needs observer
epochs + full-replacement resync and `SemanticRevisionRef`s that match the
durable side. Outcome of this lane: a native `SpaghettiSessionObserver` that
Chopsticks can switch to and that delivers **all 11 semantic families** plus
control events, built on the shared decode spine, in ≤ 6k production LOC with
behavioral tests against real files.

## What exists and what to keep

- Keep and reuse unchanged: `source/append_delimited.rs` (checkpointed append
  tail, truncation/rewrite transitions), `decode_runtime.rs::decode_record`
  (the one decode spine — durable and observer MUST call the same fn),
  `runtime_semantic_reducer.rs` (RFC 012C reducer laws; touch only if a bug
  blocks you, list the edit), the Claude adapter's scope/source declarations
  in `agent-support/claude-code/candidate-2026-08-21/` (`source-declarations.json`,
  `scope-programs.json`) and the `adapter/` support catalog that loads them.
- Replace: everything under `crates/spaghetti-napi/src/scoped_observation.rs`
  (40,662 lines), `scoped_observation/` (33,674), `observation_contract.rs`
  + `observation_contract/`, `scoped_observation_napi.rs`,
  `scoped_observation_transport.rs`, and `fixtures/contracts/rfc012d-*`. Study
  the old code only to harvest what is genuinely good (the async source owner
  loop, watcher install-before-scan, event-id derivation, epoch/barrier
  semantics); copy ideas, not the ceremony. Known defects of the old code: 8 of
  11 families return `PortableEventContractUnavailable` at
  `scoped_observation/event_wire.rs:181-191` and stall the queue; no test ever
  tails a real file; 27k production lines perform no I/O and call
  `decode_record` once.

## Design (target)

New module `crates/spaghetti-napi/src/observer/` (facade `mod.rs` + cohesive
files, none > 1,500 lines; tests in `observer/tests/` on real temp dirs):

1. **Scope**: one root session identified by adapter id + configured root +
   root transcript path/native session id. Follow only declared relations from
   the Claude scope program: root transcript, subagent/child transcripts, and
   declared sidecars (team/workflow/task/plan files as the ADS declares). No
   global enumeration, no SQLite, no query pool.
2. **Source owner**: install watcher (`notify`) before the bootstrap scan;
   reconcile on hint + bounded poll fallback; per-object `AppendCheckpoint`;
   truncation/rewrite → reset-before-replay with generation bump.
3. **Decode → facts → events**: `decode_record` → `FactBatch` → RFC 012C
   reducers → typed events. One Rust enum `ObserverEvent` (serde tagged union)
   with semantic variants for the 11 families (message, content_block,
   tool, user_input_request, plan, task, native_marker, effective_state,
   actor_run, actor_affiliation, usage_v2) and control variants
   (bootstrap_complete, reset, overflow, resync_complete, closed, error).
   Every semantic event carries `event_id` (deterministic), `scope_epoch`,
   `sequence`, actor/run identity, `SemanticRevisionRef`, and source
   position (object id, generation, byte range). Usage follows RFC 012C
   response-revision semantics (replace, never add; repeats add nothing).
4. **Delivery**: one bounded semantic queue + a small control lane that
   cannot be starved; `poll(max)` returns a batch; no per-event ack. Overflow
   → mark epoch invalid, emit `overflow`, build a complete replacement
   snapshot in a new epoch from reducer state, publish atomically with
   `resync_complete`. Clean bootstrap and completed resync at equal coverage
   produce equal family state (test it).
5. **N-API**: `#[napi] SpaghettiSessionObserver` with `open(requestJson|object)`,
   `poll(max) -> events`, `waitForEvents(timeoutMs)`, `status()`, `close()`.
   Events cross as JS objects (napi `serde-json` feature) or JSON strings —
   pick the faster one, measure, and state it. Types are `ts-rs` exported.
6. **Perf**: attach + bootstrap of a 50 MB session tree < 500 ms (measure on a
   real large session from `~/.claude/projects`, report the number, no
   content in the report); append→consumer latency < 50 ms; bounded memory.

## Behavioral tests required (real temp dirs, real JSONL the decoder accepts)

append; partial trailing line then completion; truncate mid-file; full rewrite;
rotate/replace; subagent transcript appears after bootstrap; sidecar appears;
close while events pending; overflow with small queue → resync equals clean
bootstrap; usage repeat rows add nothing and downward correction applies;
event ids deterministic across two identical runs. Plus one end-to-end test
through the napi class (Node) on a fixture session dir.

## Deletions (same lane, before you report)

Remove the old scoped-observation tree, its napi exports, transport, contract
modules, rfc012d fixtures, and any `lib.rs`/`napi_engine.rs` hooks into it.
`watchSessionTranscript` (legacy TS tail in
`packages/sdk/src/sources/claude-code/live/session-tail.ts`) stays untouched —
Chopsticks migrates in Wave 3. The SDK wrapper (`observeSession`) is L2's; you
provide the napi class, the generated types, and a Node smoke test.

## Out of scope

Durable engine, catalog, usage queries, adapters' decoders, the SDK barrel.
