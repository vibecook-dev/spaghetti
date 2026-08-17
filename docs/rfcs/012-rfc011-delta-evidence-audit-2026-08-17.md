# RFC 012 / RFC 011 delta evidence audit — 2026-08-17

- Audit base: `3373fc4` (`fix(rfc-012): close contract review gaps`)
- Executable ledger: `scripts/architecture/rfc012-rfc011-delta.json`
- Planned evidence entries at audit start: **12**
- Scope: classify the exact planned claim in each ledger row; existing
  `implemented` entries are context, not part of the count below.

## X0-RECONCILIATION-ORDER — retained

- **Planned claim:** A1 coverage-comparison fixtures prove common ordering
  semantics.
- **Classification:** `implemented-and-executable`
- **Evidence:**
  - `crates/spaghetti-napi/src/adapter/semantic.rs` —
    `coverage_comparison_is_driver_ordered_and_generation_safe` proves equal,
    ahead, behind, and generation-reset/incomparable outcomes from common
    driver positions.
  - `crates/spaghetti-napi/fixtures/contracts/rfc012a-v1.json` and
    `packages/sdk/src/contracts/__tests__/rfc012a.test.ts` carry the same
    comparison cases across the portable contract boundary.
  - `crates/spaghetti-napi/src/source/scheduler.rs` —
    `same_object_and_generation_is_serial` and
    `duplicate_work_coalesces_and_escalates_priority` retain per-object source
    order while treating notifications as coalescible hints.
  - `crates/spaghetti-napi/src/adapter/registry.rs` —
    `scoped_decode_coverage_advances_only_after_the_matching_offer_boundary`
    binds scoped coverage to admitted/offered source order rather than an
    observer clock.
- **Reproduction:**
  `cargo test -p spaghetti-napi adapter::semantic::tests::coverage_comparison_is_driver_ordered_and_generation_safe -- --exact`;
  `cargo test -p spaghetti-napi source::scheduler::tests::same_object_and_generation_is_serial -- --exact`;
  `cargo test -p spaghetti-napi adapter::registry::tests::scoped_decode_coverage_advances_only_after_the_matching_offer_boundary -- --exact`;
  `pnpm --filter @vibecook/spaghetti-sdk test`.
- **Semantic gap:** This proves the common comparison law and each topology's
  ordering boundary. It does not prove whole-decoder semantic parity; that is
  the separate `X0-DECODE-TOPOLOGY` gap.
- **Recommended owner:** RFC 012A should promote this planned entry to the
  cited executable composition without broadening it into decoder parity.

## X0-ADAPTER-BOUNDARY — strengthened

- **Planned claim:** A2 declaration checks reject adapter-private mechanics.
- **Classification:** `implemented-and-executable`
- **Evidence:** `scripts/architecture/check_rfc011_boundaries.py` executes the
  `rfc012_adapter_access_authority_violations`,
  `rust_adapter_storage_boundary_violations`,
  `rfc012_decode_runtime_boundary_violations`, and
  `rfc012_adapter_support_binding_gaps` ratchets. Together they reject private
  storage/access authority, topology-dependent shared decoding, and built-in
  adapters without compiled support declarations.
- **Reproduction:** `python3 scripts/architecture/check_rfc011_boundaries.py`.
- **Semantic gap:** This is a production dependency/authority rejection, not
  proof that every legacy discovery callback has already been classified. That
  inventory remains under `X0-DECLARED-ADAPTER-MECHANICS`.
- **Recommended owner:** RFC 012A should promote this entry and retain the
  architecture ratchet as its executable gate.

## X0-DECODE-TOPOLOGY — amended

- **Planned claim:** A1 and D4 compare durable and scoped semantic digests at
  equal coverage.
- **Classification:** `partial`
- **Evidence:**
  - `crates/spaghetti-napi/src/adapter/facts.rs` —
    `canonical_revision_ignores_catalog_ids_observation_time_and_append_batch_ordinal`
    and `object_scoped_native_identity_is_topology_stable_and_generation_local`
    prove topology-independent semantic revision identity.
  - `crates/spaghetti-napi/src/scoped_observation.rs` —
    `scoped_usage_replacement_snapshot_is_phase_independent_and_rejects_live`
    and `scoped_usage_replacement_snapshot_has_stable_order_and_digest` prove
    stable scoped replacement semantics.
- **Reproduction:**
  `cargo test -p spaghetti-napi adapter::facts::tests::object_scoped_native_identity_is_topology_stable_and_generation_local -- --exact`;
  `cargo test -p spaghetti-napi scoped_observation::projection_tests::scoped_usage_replacement_snapshot_has_stable_order_and_digest -- --exact`.
- **Semantic gap:** No fixture feeds the same declared source records through
  both durable and scoped decode/projection paths and compares facts,
  provenance, final decoder state, family state, and coverage at one equal
  vector. D4 consumer parity is also absent.
- **Recommended owner:** A1/D4 should add a sanitized, multi-revision fixture
  and one differential harness over the real durable and scoped paths.

## X0-DURABLE-ATOMICITY — retained

- **Planned claim:** D3 tests observer bootstrap, reset, overflow, and
  replacement barriers.
- **Classification:** `implemented-and-executable`
- **Evidence:**
  - `crates/spaghetti-napi/src/adapter/registry.rs` —
    `scoped_append_kernel_keeps_cursor_partial_and_reset_state_without_a_store`,
    `scoped_bootstrap_barrier_is_ordered_idempotent_and_replay_stable`, and
    `scoped_whole_epoch_completion_swaps_source_coverage_and_reducer_state`.
  - `crates/spaghetti-napi/src/scoped_observation.rs` —
    `scoped_delivery_rejects_oversized_batch_without_partial_offer` and
    `scoped_reoverflow_discards_incomplete_stage_and_requires_a_fresh_epoch`.
- **Reproduction:**
  `cargo test -p spaghetti-napi adapter::registry::tests::scoped_append_kernel_keeps_cursor_partial_and_reset_state_without_a_store -- --exact`;
  `cargo test -p spaghetti-napi adapter::registry::tests::scoped_bootstrap_barrier_is_ordered_idempotent_and_replay_stable -- --exact`;
  `cargo test -p spaghetti-napi scoped_observation::projection_tests::scoped_delivery_rejects_oversized_batch_without_partial_offer -- --exact`;
  `cargo test -p spaghetti-napi adapter::registry::tests::scoped_whole_epoch_completion_swaps_source_coverage_and_reducer_state -- --exact`.
- **Semantic gap:** These are internal D3 composition tests; the public async
  observer facade remains a separate D1/D2/D4 deliverable.
- **Recommended owner:** RFC 012D should promote this entry while leaving the
  broader scoped-observer migration entry planned.

## X0-SCOPED-OBSERVATION — amended

- **Planned claim:** D1-D4 implement and migrate the store-free observer.
- **Classification:** `partial`
- **Evidence:** `crates/spaghetti-napi/src/scoped_observation.rs` and
  `crates/spaghetti-napi/src/adapter/registry.rs` contain executable tests for
  store-free authorization, exact access, decoding, projection, bounded
  delivery, bootstrap, resync, application acknowledgement, and close.
- **Reproduction:** `cargo test -p spaghetti-napi scoped_ --lib`.
- **Semantic gap:** There is no public async multiplexer/facade, watcher-before-
  scan orchestration, dynamic whole-session scope, or D4 Chopsticks shadow and
  rollback path. The internal module remains intentionally non-public.
- **Recommended owner:** D1/D2 should land the public lifecycle composition;
  D4 should then prove shadow migration before this entry is promoted.

## X0-DECLARED-ADAPTER-MECHANICS — superseded

- **Planned claim:** A2/A3 support manifests classify every production
  mechanic.
- **Classification:** `partial`
- **Evidence:** `scripts/agent_support/validate.py` validates bounded source
  declarations, restricted scope relations, one semantic owner per native
  disposition, and release/document digest bindings. The three candidate
  bundles pass `python3 scripts/agent_support/validate.py`, and
  `rfc012_adapter_support_binding_gaps` confirms each built-in adapter compiles
  its declared support package.
- **Reproduction:** `python3 scripts/agent_support/validate.py`;
  `python3 scripts/architecture/check_rfc011_boundaries.py`.
- **Semantic gap:** The repository still contains versioned legacy
  `discover`/stream callbacks, and no executable callback-to-declaration census
  proves every production mechanic is either mapped to an approved primitive
  or explicitly unsupported. All support releases remain candidates.
- **Recommended owner:** A2/A3 should add a callback/mechanic census keyed to
  source declaration and scope relation IDs; unmapped callbacks must fail the
  promotion gate.

## X0-USAGE-V2 — superseded

- **Planned claim:** C2/C3 define oracle parity, switch, and rollback fixtures.
- **Classification:** `implemented-and-executable`
- **Evidence:**
  - `scripts/usage_v2_oracle/test_oracle.py` checks the frozen independent
    response-level oracle.
  - `crates/spaghetti-napi/src/engine/projection.rs` —
    `usage_v2_projection_matches_independent_qualified_oracle` and
    `usage_v2_replaces_response_snapshots_without_changing_legacy_usage` prove
    parity, corrected response replacement, and an intact legacy shadow.
  - `crates/spaghetti-napi/src/engine/commit.rs` —
    `query_pack_selection_requires_ready_complete_coverage_and_rolls_back_explicitly`
    proves guarded selection and explicit rollback even when v2 is pending.
- **Reproduction:** `python3 scripts/usage_v2_oracle/test_oracle.py`;
  `cargo test -p spaghetti-napi engine::projection::tests::usage_v2_projection_matches_independent_qualified_oracle -- --exact`;
  `cargo test -p spaghetti-napi engine::projection::tests::usage_v2_replaces_response_snapshots_without_changing_legacy_usage -- --exact`;
  `cargo test -p spaghetti-napi engine::commit::tests::query_pack_selection_requires_ready_complete_coverage_and_rolls_back_explicitly -- --exact`.
- **Semantic gap:** Default selection is intentionally not enabled; promotion
  remains governed by support and integration gates. The planned claim only
  requires the parity/switch/rollback fixtures.
- **Recommended owner:** RFC 012C should promote this evidence without treating
  it as authorization for a default switch.

## X0-CATALOG-READINESS — refined

- **Planned claim:** B1-B4 cover transition, warm-state, and progressive-host
  behavior.
- **Classification:** `not-found`
- **Evidence:** Searches for `CatalogReadiness`, `coverage_plan`, catalog epoch
  transitions, and progressive catalog host tests found only the generic RFC
  011 projection-readiness states and catalog snapshot hydration. Those are
  explicitly the legacy behavior being refined, not the B1-B4 state machine.
- **Reproduction:**
  `rg -n "CatalogReadiness|coverage_plan|catalog readiness|progressive" crates/spaghetti-napi/src packages/sdk/src`.
- **Semantic gap:** The catalog-specific coverage-plan identity, epoch
  transition table, warm acceptance/rejection rules, snapshot query contract,
  and progressive host are absent.
- **Recommended owner:** RFC 012B must implement B1-B4; generic
  `ProjectionReadiness` tests must not be promoted as substitutes.

## X0-COMMON-FACTS — retained

- **Planned claim:** C1/C4 add versioned revision and reducer parity fixtures.
- **Classification:** `partial`
- **Evidence:**
  - `crates/spaghetti-napi/fixtures/contracts/rfc012c-runtime-v1.json`, Rust
    `runtime_contract_fixture` tests, and
    `packages/sdk/src/contracts/__tests__/rfc012c.test.ts` provide a shared,
    independently validated actor/affiliation/usage-v2 value fixture including
    A -> B -> A semantic revision identity.
  - `usage_v2_replaces_response_snapshots_without_changing_legacy_usage`
    proves versioned usage reduction can run beside legacy usage.
- **Reproduction:** `cargo test -p spaghetti-napi runtime_contract_fixture`;
  `pnpm --filter @vibecook/spaghetti-sdk test`;
  `cargo test -p spaghetti-napi engine::projection::tests::usage_v2_replaces_response_snapshots_without_changing_legacy_usage -- --exact`.
- **Semantic gap:** C4 consumer parity is absent, and the portable fixture
  covers actor-run, affiliation, and usage-v2 only—not every retained common
  message/content/run family named by the row.
- **Recommended owner:** C1 should add remaining family fixtures only when a
  versioned representation is introduced; C4 must compare portable consumer
  reduction before the ledger entry is promoted.

## X0-CROSS-TOPOLOGY-IDENTITY — strengthened

- **Planned claim:** Each durable fact family and scoped observer event
  migrates from legacy runtime-ID keys to canonical references.
- **Classification:** `partial`
- **Evidence:** `object_scoped_native_identity_is_topology_stable_and_generation_local`,
  `scoped_source_controls_use_coverage_identity_and_stable_event_ids`, and the
  RFC 012C runtime contract fixture prove canonical identity for common native
  facts, scoped controls, and usage-v2 values.
- **Reproduction:**
  `cargo test -p spaghetti-napi adapter::facts::tests::object_scoped_native_identity_is_topology_stable_and_generation_local -- --exact`;
  `cargo test -p spaghetti-napi scoped_observation::projection_tests::scoped_source_controls_use_coverage_identity_and_stable_event_ids -- --exact`;
  `cargo test -p spaghetti-napi runtime_contract_fixture`.
- **Semantic gap:** There is no exhaustive fact-family/event inventory proving
  every durable family and future observer envelope has migrated; several
  scoped families and the public observer contract do not yet exist.
- **Recommended owner:** A1/A4 should add a family/event migration census and
  cross-topology tests as each family becomes observable.

## X0-FIXED-SNAPSHOT-PAGINATION — strengthened

- **Planned claim:** C1/C4 add fixed-snapshot and expiration fixtures.
- **Classification:** `implemented-and-executable`
- **Evidence:**
  - `usage_v2_replaces_response_snapshots_without_changing_legacy_usage`
    validates stable aggregate/page state and explicitly rejects a continuation
    after a generation-changing commit as `cursor expired`.
  - `canonical_search_cursor_is_scope_and_watermark_bound`,
    `orchestration_cursors_are_scope_and_watermark_bound`, and
    `timeline_keyset_cursor_is_scope_and_session_revision_bound` cover durable
    history scope binding and explicit expiration rather than silent drift.
- **Reproduction:**
  `cargo test -p spaghetti-napi engine::projection::tests::usage_v2_replaces_response_snapshots_without_changing_legacy_usage -- --exact`;
  `cargo test -p spaghetti-napi engine::search_query::tests::canonical_search_cursor_is_scope_and_watermark_bound -- --exact`;
  `cargo test -p spaghetti-napi engine::timeline_query::tests::timeline_keyset_cursor_is_scope_and_session_revision_bound -- --exact`.
- **Semantic gap:** Retained-snapshot pagination currently chooses explicit
  expiration when the relevant durable snapshot changes; it does not promise
  indefinite historical snapshot retention. That is allowed by the target
  behavior.
- **Recommended owner:** RFC 011/RFC 012C should promote this entry and keep
  explicit expiration in the public cursor contract.

## X0-DURABLE-LIVE-HANDOFF — refined

- **Planned claim:** C4/D4 exercise deduplication, overlay retirement,
  correction, and incomparable coverage.
- **Classification:** `partial`
- **Evidence:** Rust and TypeScript RFC 012A contract tests implement comparable
  coverage (`equal`, `dominates`, `behind`, `incomparable`); scoped usage tests
  cover exact-repeat suppression, corrections, reset retractions, and stable
  semantic revision references.
- **Reproduction:** `cargo test -p spaghetti-napi adapter::semantic::tests::coverage_comparison_is_driver_ordered_and_generation_safe -- --exact`;
  `pnpm --filter @vibecook/spaghetti-sdk test`;
  `cargo test -p spaghetti-napi scoped_observation::projection_tests::scoped_usage_projection_suppresses_exact_current_repeat_but_preserves_a_b_a -- --exact`.
- **Semantic gap:** No consumer composes an actual durable page and scoped
  stream, retires the overlay when durable coverage catches up, or rejects an
  incomparable merge. C4/D4 remain unimplemented.
- **Recommended owner:** C4/D4 should build the reference overlay reducer and
  differential fixtures before promotion.

## Reconciliation and proposed ledger patch

- Planned entries audited: **12**
- `implemented-and-executable`: **5**
- `partial`: **6**
- `not-found`: **1**
- Reconciliation: **12 = 5 + 6 + 1**

Recommended promotions, with no claim broadening:

1. `X0-RECONCILIATION-ORDER` — replace its planned item with the common
   comparison/scheduler/scoped-offer executable composition.
2. `X0-ADAPTER-BOUNDARY` — replace its planned item with the RFC 012 authority
   and support-binding architecture ratchets.
3. `X0-DURABLE-ATOMICITY` — replace its planned item with the named D3
   bootstrap/reset/overflow/replacement tests.
4. `X0-USAGE-V2` — replace its planned item with the oracle, shadow parity,
   guarded selection, and explicit rollback tests.
5. `X0-FIXED-SNAPSHOT-PAGINATION` — replace its planned item with the usage-v2
   and history cursor scope/expiration tests.

Keep the other seven entries planned. In particular, do not equate internal
D3 barriers with the D4 migration, generic projection readiness with RFC 012B
catalog readiness, or identity primitives with full durable/scoped parity.

## Commands run during the audit

All commands below passed at the audit base:

```text
python3 scripts/architecture/check_rfc011_boundaries.py
cargo test -p spaghetti-napi adapter::semantic::tests::coverage_comparison_is_driver_ordered_and_generation_safe -- --exact
cargo test -p spaghetti-napi source::scheduler::tests::same_object_and_generation_is_serial -- --exact
cargo test -p spaghetti-napi adapter::registry::tests::scoped_decode_coverage_advances_only_after_the_matching_offer_boundary -- --exact
cargo test -p spaghetti-napi adapter::facts::tests::object_scoped_native_identity_is_topology_stable_and_generation_local -- --exact
cargo test -p spaghetti-napi adapter::registry::tests::scoped_append_kernel_keeps_cursor_partial_and_reset_state_without_a_store -- --exact
cargo test -p spaghetti-napi adapter::registry::tests::scoped_bootstrap_barrier_is_ordered_idempotent_and_replay_stable -- --exact
cargo test -p spaghetti-napi scoped_observation::projection_tests::scoped_delivery_rejects_oversized_batch_without_partial_offer -- --exact
cargo test -p spaghetti-napi adapter::registry::tests::scoped_whole_epoch_completion_swaps_source_coverage_and_reducer_state -- --exact
cargo test -p spaghetti-napi scoped_observation::projection_tests::scoped_usage_replacement_snapshot_has_stable_order_and_digest -- --exact
python3 scripts/usage_v2_oracle/test_oracle.py
cargo test -p spaghetti-napi engine::projection::tests::usage_v2_projection_matches_independent_qualified_oracle -- --exact
cargo test -p spaghetti-napi engine::projection::tests::usage_v2_replaces_response_snapshots_without_changing_legacy_usage -- --exact
cargo test -p spaghetti-napi engine::commit::tests::query_pack_selection_requires_ready_complete_coverage_and_rolls_back_explicitly -- --exact
cargo test -p spaghetti-napi engine::search_query::tests::canonical_search_cursor_is_scope_and_watermark_bound -- --exact
cargo test -p spaghetti-napi engine::timeline_query::tests::timeline_keyset_cursor_is_scope_and_session_revision_bound -- --exact
cargo test -p spaghetti-napi scoped_observation::projection_tests::scoped_source_controls_use_coverage_identity_and_stable_event_ids -- --exact
cargo test -p spaghetti-napi scoped_observation::projection_tests::scoped_usage_projection_suppresses_exact_current_repeat_but_preserves_a_b_a -- --exact
cargo test -p spaghetti-napi runtime_contract_fixture
pnpm --filter @vibecook/spaghetti-sdk test
```

P2 exit-gate validation also passed:

```text
python3 scripts/architecture/check_rfc012_delta.py
  RFC 012/RFC 011 compatibility ledger: ok (1 fully evidenced, 12 with planned evidence)
pnpm validate
  Validation suites: 8 passed, 0 failed, 0 skipped
git diff --check
  PASS
```

## Disposition/owner inconsistencies

- The `X0-RECONCILIATION-ORDER` planned claim is owned by A1 but its proof is a
  composition of common semantic, durable scheduler, and D3 scoped-offer tests;
  the ledger should cite that composition rather than the implementation plan.
- `X0-SCOPED-OBSERVATION` names D1-D4 as one evidence item even though D3 is
  independently executable and D4 is absent. Keep the aggregate row planned,
  while promoting D3 only under `X0-DURABLE-ATOMICITY`.
- `X0-CATALOG-READINESS` points to RFC 012B prose despite having no executable
  B1-B4 implementation. Generic RFC 011 readiness must not satisfy it.
