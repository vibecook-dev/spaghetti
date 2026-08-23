# RFC 012 implementation de-duplication plan

- **Status:** SUPERSEDED on 2026-08-23 by
  [012-landing-plan.md](./012-landing-plan.md). Kept for history; do not
  update. The RFC documents remain the semantic authorities.
- **Written:** 2026-08-22
- **Baseline:** `d478679`
- **Related roadmap:**
  [RFC 012 implementation plan](./012-implementation-plan.md)
- **Semantic authorities:**
  [RFC 012](./012-evidence-backed-adapters-and-progressive-readiness.md),
  [RFC 012A](./012a-agent-adaptation-and-engine-boundaries.md),
  [RFC 012B](./012b-catalog-readiness-and-progressive-startup.md),
  [RFC 012C](./012c-runtime-semantics-and-usage-v2.md), and
  [RFC 012D](./012d-session-scoped-observation.md)

## Contents

1. Decision and urgency
2. Scope, non-goals, and protected invariants
3. Target implementation shape
4. Nine-phase execution plan
5. Pull-request and validation structure
6. Success metrics, stop conditions, and effort

## 1. Decision

RFC 012's architectural boundaries remain sound. This plan removes repeated
implementation templates without flattening those boundaries or changing RFC
semantics.

The immediate priority is to stop the multiplication cost of adding another
portable runtime family. No new message, plan, task, tool, content-block,
native-marker, user-input, or effective-state wire specialist should land
until the shared wire primitives and scoped revision-family kernel described
here are in place.

This is not a rewrite. It is a sequence of behavior-preserving extractions with
fixture, parity, and rollback gates at each step.

## 2. Why this work is needed now

At the baseline revision:

| Measure | Current value | Interpretation |
| --- | ---: | --- |
| `crates/spaghetti-napi/src` | 315,610 lines / 196 Rust files | Large crate, including extensive inline tests |
| `scoped_observation.rs` | 40,662 lines | Primary production composition hotspot |
| Complete RFC 012D Rust area | approximately 75,045 lines | Includes inline and extracted tests |
| RFC 012D excluding separate `tests.rs` files | approximately 65,845 lines | Footprint measure, not production-only |
| Approximate RFC 012D production code | approximately 51,900 lines | Real domain plus repeated family/wire mechanics |
| `adapter/registry.rs` | 17,800 lines | Approximately 98% tests; an organization issue |
| Internal runtime families | 11 | All are represented in the scoped core |
| Portable runtime-family specialists | 3 | Usage-v2, actor-run, and actor-affiliation |

The main scaling problem is not the absolute line count. It is that one new
portable family currently tends to require another copy of:

1. scoped event and projection-state shapes;
2. replacement entity, snapshot, digest, and validation code;
3. state binding and identity validation;
4. admission/reduction/retraction match arms;
5. resync snapshot iteration and offset bookkeeping;
6. durable-parity test reducers;
7. a large Rust wire specialist with locally copied codec helpers; and
8. a TypeScript envelope parser with another copy of the same JSON primitives.

If this template remains, completing the remaining portable families will make
the implementation materially harder to review and change. Extracting the
kernel first converts future work back into domain work: a reducer law, a
family descriptor, an explicit event variant, and a specialist wire body.

## 3. Scope

This plan covers four implementation layers, in descending priority:

1. scoped-observer family mechanics and replacement traversal;
2. same-language Rust and TypeScript wire primitives;
3. catalog producer/probe support shells; and
4. catalog-contract and engine cursor/SQLite glue.

The expected safe net reduction is approximately 7,000 to 11,000 production
lines. This is an estimate, not an acceptance criterion. Added generic support,
characterization tests, and compatibility adapters will consume part of the
raw duplicated-line total.

Success is measured primarily by removing copy sites and lowering the number
of kernel edits needed for the next family, not by maximizing deleted lines.

## 4. Non-goals

This plan does not:

- change any RFC 012A/B/C/D semantic contract;
- ratify a child RFC or promote an agent support release;
- add a new portable runtime family;
- change durable query semantics, persistence authority, or observer topology;
- merge adapters with catalog producers;
- merge catalog evidence, page, query, publication, or hydration domains;
- replace specialist wire modules with one Serde enum;
- generate TypeScript parsers from Rust;
- introduce a runtime family-plugin registry or trait-object dispatch;
- merge usage-v2 with revisioned-entity families;
- include actor overlays in the usage digest;
- merge bootstrap and resync identity rules;
- merge completion and watermark queue rules;
- make a conformance oracle call production logic when independence is part of
  what the oracle is intended to prove; or
- perform broad SQL-schema or projector-framework redesign.

Any proposed change requiring one of those actions is a separate design change,
not part of this cleanup.

## 5. Invariants that every change must preserve

### 5.1 Architecture

1. Adapters decode native records and emit common facts. They do not own
   watchers, SQLite, durable queries, or portable delivery.
2. Catalog identity/readiness remains in `catalog_contract/` and
   `engine/catalog_*`, not in adapters.
3. RFC 012C reducers remain topology-neutral and are invoked by durable and
   scoped sinks.
4. The scoped observer remains store-free and cannot claim persistence,
   durable completeness, or query authority.
5. Factory remains without a catalog producer unless an owning RFC explicitly
   changes that decision.

### 5.2 Runtime-family semantics

RFC 012C declares five reducer classes:

| Reducer class | Current relevant families |
| --- | --- |
| `RevisionedEntity` | actor, affiliation, effective-state, and currently plan |
| `OwnedSetSnapshot` | currently task; plan/task may select a declared representation by support release |
| `CorrelatedLifecycle` | user-input request and tool call/result |
| `CurrentGenerationLog` | message, content block, and native marker |
| `UsageContribution` | usage-v2 |

The common kernel may own identity binding, storage shape, deterministic
iteration, capacity plumbing, and replacement mechanics. It must not erase
these reducer-class distinctions.

Usage-v2 remains outside the revisioned-family extraction. Actor-run and
actor-affiliation remain outside it because their root/child and affiliation
context affects admission, delivery, and replacement. Unknown native evidence
also remains separate because its bounded sample plus aggregate representation
is not one of the eight common shapes.

### 5.3 Wire behavior

1. Top-level received envelopes have no unbound `Deserialize` path.
2. Specialist modules retain explicit field sets and family validation.
3. Unknown fields, missing fields, invalid nulls, unsafe integers, bad opaque
   values, and incompatible versions continue to fail closed.
4. Additive unknown event variants are preserved only through the negotiated,
   bounded unknown-wire contract. They are never silently dropped.
5. Bounds are checked before retaining or allocating attacker-controlled data
   where a preflight bound is possible.
6. Existing canonical serialized JSON and stable IDs do not change.
7. Rust and TypeScript remain independent implementations consuming the same
   frozen fixtures.

### 5.4 Epoch and replacement behavior

1. Bootstrap and resync retain their distinct epoch and barrier identities.
2. Overflow remains sticky until a complete replacement succeeds.
3. A replacement remains complete per selected supported/degraded family.
4. Family order, event order, offsets, barrier sequence, digests, and
   disappearance semantics remain deterministic.
5. Clean bootstrap and completed resync at equal coverage retain equal family
   semantic digests.
6. A consumer never merges a correction snapshot into stale old-epoch state.

## 6. Target implementation shape

### 6.1 Wire primitives

Add an internal Rust module:

```text
crates/spaghetti-napi/src/scoped_observation/wire_codec.rs
```

It owns only context-free JSON/wire mechanics:

- canonical `v1:` base64url-no-padding encode/decode;
- decoded and encoded length preflight;
- fixed-length opaque decoding;
- positive JavaScript-safe integer validation;
- non-negative JavaScript-safe integer validation; and
- exact-object field inventory support.

It returns a small internal issue type. Each specialist maps that issue into
its existing family-specific contract error so public error ownership and
labels remain local.

Add an internal, unbarrelled TypeScript module:

```text
packages/sdk/src/contracts/rfc012-wire-json.ts
```

It owns the equivalent same-language primitives:

- `record` and `exactRecord`;
- safe and positive-safe integer validation;
- canonical `decodeV1Opaque` and fixed-length decoding; and
- bounded text/array helpers only when their accepted language is identical.

It must not export through the package barrel. Domain context types stay in
their current owning contracts.

### 6.2 Scoped-observer physical boundaries

Keep `scoped_observation.rs` as the module root and move cohesive concerns into
the existing `scoped_observation/` directory. The exact split may adjust for
Rust privacy and cycle constraints, but the target ownership is:

```text
scoped_observation.rs                         module facade and re-exports
scoped_observation/contracts.rs               public/internal event and lifecycle types
scoped_observation/admission.rs               semantic/control lane admission
scoped_observation/projection.rs              fact preparation and per-scope projection
scoped_observation/revision_family.rs         common eight-family mechanics
scoped_observation/replacement.rs             snapshot creation, validation, digest, traversal
scoped_observation/epoch.rs                   overflow, staging, epoch swap, barriers
scoped_observation/tests.rs                   former root inline tests
adapter/registry/tests.rs                     former registry inline tests
```

The split is performed as move-only commits before generic behavior changes.
Symbols are not renamed during moves. The module root should become a readable
facade rather than another composition implementation.

### 6.3 Revision-family kernel

The kernel has three layers.

#### Layer A: common data mechanics

Provide generic internal shapes for the eight structurally common families:

```text
ScopedRevisionEvent<R>
ScopedRevisionProjectionState<R>
ScopedRevisionReplacementEntity<R>
ScopedRevisionReplacementSnapshot<E>
```

Existing family-specific names remain as type aliases or thin named wrappers
where required for diagnostics and public Rust API stability. Redacted `Debug`
behavior remains centralized and does not expose preserved values.

#### Layer B: reducer-class strategies

Implement explicit compile-time strategies for:

- current-generation log;
- correlated lifecycle;
- revisioned entity; and
- owned-set snapshot.

These strategies own replacement/retraction mechanics shared by members of the
same RFC 012C class. They call the existing family reducers; they do not
reimplement or genericize the family reduction laws.

Plan/task owned-set completeness checks remain explicit hooks. A support
release's selected replacement representation remains an associated constant
or typed parameter and cannot be inferred from the Rust revision type.

#### Layer C: sealed family descriptors

Use sealed, compile-time descriptors for the eight families. A descriptor may
declare:

- fact family and contract version;
- Rust revision type;
- reducer class/replacement representation;
- entity/fact key access;
- family-specific pre-validation hook;
- existing reducer function;
- family semantic-digest function;
- capacity/error mapping; and
- deterministic replacement order position.

The descriptor must not contain source access, serialization, dynamic
registration, or a trait object. An exhaustive compile-time list is acceptable;
a runtime plugin registry is not.

The following remain explicit and visibly exhaustive:

- `Fact` to family dispatch;
- `ScopedObservationEvent` variants;
- portable event-family dispatch; and
- RFC 012C reducer bodies.

The table generates or drives common state binding, replacement construction,
validity checking, and snapshot traversal. It does not generate wire
specialists or semantic law.

### 6.4 Shared fact-envelope header

Actor and usage envelopes share an 18-field top-level header. Reuse should
occur below the explicit specialist structs:

- one field-name inventory for the truly common header;
- one validated internal header value;
- one constructor from a scoped envelope; and
- one contextual parser from an already exact-checked object.

Do not use a broad `serde(flatten)` envelope that weakens exact-object or
required-field checks. Actor and usage event bodies, versions, and local error
types remain specialist-owned. Source and continuity may consume shared header
pieces but retain their Root-only, source-record, and engine-control rules.

Completion, close, watermark, capability, and replacement envelopes do not
join this abstraction merely because some fields have the same names.

### 6.5 Catalog support kit

Extract only production shell mechanics whose semantics are identical:

- source-driver error mapping;
- common identity-digest framing, if its bytes are contractually identical;
- bounded probe filesystem helpers;
- platform-ID normalization;
- deterministic sorted directory entry collection; and
- reusable conformance-harness setup/assertion utilities.

Claude, Codex, and Grok keep their native membership logic, topology, decoder,
and support evidence.

Claude's duplicated directory selectors require a role decision before
extraction:

- if `catalog_conformance.rs` is an independent oracle, keep its membership
  computation independent and share only low-level test infrastructure;
- if it is only a contract harness for the producer, share pure selectors;
- never make an independent oracle call the production producer and then
  compare production output with itself.

This role decision must be recorded in the extraction PR.

### 6.6 Catalog-contract and engine primitives

Add small helpers, not a generic engine framework:

- a bounded JSON/base64 cursor codec;
- a page-limit validator;
- `sqlite_u64` and SQLite error mapping in one engine support module;
- publication row binding shared by initial and refresh snapshot inserts; and
- a provenance-entry primitive with an explicit validation policy.

Per-query cursor structs, scope hashes, positions, ordering, and error labels
remain local. Named default/max page constants remain owned by each query
contract even when their current numeric values happen to match.

Provenance currently differs across evidence, hydration, and page domains on
zero IDs and canonical sort requirements. The first extraction must preserve
those policies explicitly, for example:

```text
ProvenancePolicy {
  require_nonzero_id,
  require_canonical_order,
  reject_duplicates,
  maximum_entries,
}
```

Changing the policies to one semantic rule requires a separate RFC-backed
decision after the current differences are characterized.

Replace-document projectors are not made generic during the first pass. Their
SQL, transaction order, ownership checks, conflict topics, and reducer calls
remain local. Only repeated leaf operations are extracted. A broader projector
abstraction is reconsidered only after two migrations demonstrate an identical
transaction state machine.

## 7. Execution plan

Each phase lands independently. A failed phase is reverted without requiring
later phases to restore behavior.

### Phase 0 — Freeze behavior and establish the baseline

**Objective:** make accidental semantic drift observable before moving code.

Deliverables:

1. Record exact file/production/test line counts and duplicate-site counts.
2. Add a shared wire-codec conformance fixture covering:
   - empty and valid opaque values;
   - wrong prefixes;
   - padded, non-url-safe, and non-canonical base64;
   - invalid trailing bits;
   - exact decoded-length boundaries;
   - preflight encoded-length boundaries;
   - zero, one, `Number.MAX_SAFE_INTEGER`, and max-plus-one integers; and
   - exact-object missing/unknown-field cases.
3. Make Rust and TypeScript consume the same positive and negative cases.
4. Freeze canonical JSON output for every existing RFC 012D fixture.
5. Add or confirm clean-bootstrap-versus-resync digest parity for every one of
   the eight common families.
6. Freeze current replacement family order, event order, offsets, retraction
   causes, and capacity failure behavior.
7. Add a family census asserting that every selected internal family has one
   reducer class and one replacement representation.

Exit gate:

- the fixture hashes and all existing focused tests are green;
- the new negative matrix agrees across Rust and TypeScript; and
- no production code has changed.

Estimated effort: 1–2 engineer-days.

### Phase 1 — Extract the Rust wire codec

**Objective:** establish one Rust implementation of context-free wire
primitives before moving family envelopes.

Sequence:

1. Add `wire_codec.rs` and unit-test it directly.
2. Migrate `actor_wire.rs` and `usage_wire.rs` first because they expose the
   known helper drift and share the future fact header.
3. Compare accepted/rejected cases and canonical output against Phase 0.
4. Migrate source and continuity.
5. Migrate completion, close, capability snapshot, replacement manifest,
   scope coverage, unknown evidence, artifact, artifact availability, and
   artifact-availability event modules.
6. Migrate watermark integer helpers where the accepted domain is identical.
7. Add a ratchet preventing new local definitions of the canonical opaque and
   safe-integer helpers in specialist modules.

Do not normalize family-specific maximum lengths or error labels while moving
them. Bounds are arguments owned by the specialist.

Exit gate:

- one Rust opaque implementation and one safe-integer implementation remain;
- all specialist contract tests pass unchanged;
- all frozen Rust JSON remains byte-equivalent after canonicalization; and
- top-level specialist values still lack unbound `Deserialize`.

Estimated effort: 2–3 engineer-days.

### Phase 2 — Extract the TypeScript wire JSON primitives

**Objective:** remove same-language SDK parser drift while preserving an
independent implementation from Rust.

Sequence:

1. Add the unbarrelled `rfc012-wire-json.ts` module and direct tests.
2. Implement preflight and post-decode bounds matching the Phase 0 matrix.
3. Migrate actor and usage first.
4. Migrate source and continuity.
5. Migrate the remaining modules only when their primitive has the same
   accepted language; leave family-specific validators local.
6. Add an import/static ratchet preventing new local `decodeV1Opaque`,
   `safeInteger`, and `exactRecord` implementations in RFC 012D contracts.

Exit gate:

- one TypeScript implementation remains for each shared primitive;
- every frozen Rust fixture is still accepted by the matching TypeScript
  parser;
- every shared negative fixture is rejected with the expected issue class;
- SDK typecheck, tests, build, and package checks pass; and
- the helper is not part of the public SDK barrel.

Estimated effort: 2–3 engineer-days.

### Phase 3 — Perform no-logic physical splits

**Objective:** make the family extraction reviewable and lower merge/conflict
risk without changing behavior.

Sequence:

1. Move inline `adapter/registry.rs` tests to `adapter/registry/tests.rs`.
2. Move inline scoped-observer tests to `scoped_observation/tests.rs`.
3. Move contract/type definitions, preserving names and visibility.
4. Move admission/projection code.
5. Move replacement/resync code.
6. Leave a small module facade with explicit submodule declarations and
   re-exports.

Rules:

- one concern move per commit;
- no renaming, generic conversion, or error cleanup in a move commit;
- use the compiler to reveal privacy edges, then make the smallest visibility
  change needed; and
- review with moved-code detection enabled.

Structural target, not a semantic gate:

- module root below roughly 8,000 lines;
- no new concern file above roughly 12,000 lines; and
- tests no longer obscure production-file measurements.

Exit gate:

- zero intended logic diff;
- fixture hashes unchanged;
- focused and full Rust tests pass; and
- `git diff --check` is clean.

Estimated effort: 2–4 engineer-days.

### Phase 4 — Collapse reducer identity wrappers

**Objective:** remove the lowest-risk RFC 012C clone before changing observer
state mechanics.

Add one private identity helper in `runtime_semantic_reducer.rs` that performs:

1. revision validation;
2. semantic entity/revision reference comparison; and
3. `FactRevisionId` derivation and binding verification.

Family wrappers become thin calls. Plan/task owned-set completeness checks run
before the shared identity helper and remain explicit. Actual reducer and merge
bodies remain unchanged.

Prefer a private generic function or sealed internal trait. Do not add a public
runtime revision interface solely for this refactor.

Exit gate:

- all positive identity fixtures still pass;
- every mismatched entity, revision, fact ID, or owned-set case still fails;
- durable reducer output digests are unchanged; and
- no reducer-law body changed.

Estimated effort: 1–2 engineer-days.

### Phase 5 — Extract the scoped revision-family kernel

**Objective:** remove the highest-value multiplier before another portable
family is added.

This phase is deliberately split into small migrations.

#### 5.1 Common shapes

Introduce generic event, projection-state, replacement-entity, and
replacement-snapshot shapes. Convert existing family structs to aliases or
thin wrappers without changing code paths.

Gate: compile/tests only; no behavior uses generic dispatch yet.

#### 5.2 Common state binding

Replace the repeated `scoped_*_state` bodies with one identity/context binder.
Family-specific validation hooks execute explicitly before or inside the
binder.

Migrate by reducer class:

1. message and content block as the current-generation-log pilot;
2. native marker;
3. user-input and tool as correlated lifecycle;
4. plan and effective-state as current revisioned entities; and
5. task as owned-set snapshot.

Gate after every group: focused reducer, admission, reset, and replacement
tests plus unchanged semantic digests.

#### 5.3 Replacement construction and validation

Move common snapshot collection, redacted debug, identity verification, and
snapshot validity checks into the kernel. Keep representation-class-specific
absence/retraction rules explicit.

Gate: malformed snapshot negatives remain family-addressable and no incomplete
coverage can prove absence.

#### 5.4 Admission and reduction delegation

Keep the exhaustive `Fact` match, but make each common family arm delegate to
the appropriate class strategy. The visible match must still show every
implemented family and reject unsupported versions explicitly.

Gate: validate → reduce → retract/upsert behavior and capacity failures remain
identical for each family.

#### 5.5 Replacement traversal

Replace the manually accumulated `offer_snapshot_next` family offsets with one
compile-time ordered traversal over the selected family descriptors.

The order is frozen from Phase 0. The traversal must retain:

- resumable offsets;
- deterministic event IDs and sequence allocation;
- backpressure behavior;
- exact stop/resume position;
- per-family manifest count and digest; and
- atomic completion-barrier rules.

Gate: interruption at every family boundary and representative intra-family
offset resumes without duplicates, gaps, or reordering.

#### 5.6 Durable-parity test harness

Share test harness mechanics for applying family reducers, while keeping
family input fixtures and expected reduced state explicit. Do not make Rust and
TypeScript share parser implementation, and do not hide family laws behind a
single expected-output generator.

Phase 5 exit gate:

- the eight families use the common kernel mechanics;
- usage-v2 and actor/affiliation paths remain separate;
- four reducer/replacement classes remain explicit;
- no trait-object or runtime registration path exists;
- clean bootstrap/resync equality passes for all selected families;
- the public portable profile remains unchanged; and
- adding a hypothetical ninth common family would not require cloning state,
  replacement, admission, digest-validity, or traversal implementations.

Estimated effort: 7–12 engineer-days. This is the largest uncertainty and
should not be compressed into one review.

### Phase 6 — Extract the shared fact-envelope header

**Objective:** prevent actor/usage header copies from becoming the template for
future message/plan envelopes.

Sequence:

1. Freeze the current 18-field actor/usage inventories and contextual checks.
2. Add the internal validated header representation and constructor.
3. Migrate output construction while keeping explicit specialist structs.
4. Migrate input parsing after specialist exact-object validation.
5. Allow source/continuity to reuse only the portions with identical semantics.
6. Add an architecture/test gate requiring any future fact-family specialist
   to use the header primitive while declaring its own event body and version.

Exit gate:

- actor and usage fixture output is unchanged;
- actor root/affiliation and usage overlay rules remain distinct;
- source/continuity Root-only and engine-control rules remain intact; and
- no completion/close/watermark envelope is forced through the fact header.

Estimated effort: 2–4 engineer-days.

### Phase 7 — Extract catalog producer and probe support

**Objective:** remove cloned operational shells without abstracting native
catalog meaning.

Sequence:

1. Classify each duplicate as production shell, contract primitive,
   independent oracle logic, or test harness.
2. Extract probe error, platform, bounds, and sorted-directory helpers.
3. Extract source-driver error mapping.
4. Consolidate the identity digest only after byte-for-byte characterization.
5. Extract the smallest reusable `CatalogSourceRuntime` lifecycle helper.
6. Migrate Codex first as the simplest topology, then Grok, then Claude.
7. Resolve Claude selector duplication according to the oracle-role rule in
   section 6.5.
8. Share the eight-test conformance harness structure without sharing the
   expected native membership computation when it is meant to be independent.

Exit gate:

- all three candidate producers preserve identities, memberships, completeness,
  refresh behavior, and error mapping;
- no agent's topology is expressed as another agent's special case;
- Factory still has no producer; and
- no support status is promoted.

Estimated effort: 2–4 engineer-days.

### Phase 8 — Extract catalog-contract and engine leaf helpers

**Objective:** remove low-risk leaf duplication after the observer multiplier
is under control.

Land as separate PRs:

1. provenance primitive plus explicit per-domain policies;
2. generic bounded cursor serialization/deserialization;
3. shared page-limit validation with query-owned constants;
4. shared SQLite integer/error helpers;
5. shared publication row binding; and
6. repeated fact-record retraction/write leaf helpers where transaction order
   is demonstrably identical.

Do not combine initial and refresh publication CAS predicates. Do not move
query cursor semantic validation into the codec. Do not introduce a generic
replace-document projector in this phase.

Exit gate:

- query cursors preserve exact scope, ordering, size, and stale-cursor rules;
- SQLite overflow/error classification is unchanged;
- initial and refresh publication races retain separate tests;
- evidence/hydration/page provenance preserve their current policies; and
- projection transaction and conflict-topic behavior is unchanged.

Estimated effort: 3–6 engineer-days.

### Phase 9 — Ratchets, measurement, and final proof

**Objective:** ensure the duplication does not grow back and quantify the
result without turning line count into architecture law.

Deliverables:

1. Static checks reject new local copies of canonical Rust/TypeScript wire
   primitives.
2. A family census requires every common family to declare one reducer class,
   replacement representation, and deterministic replacement position.
3. Architecture checks continue to forbid store/query dependencies in adapter
   and semantic layers.
4. Before/after reports record production/test line counts, compile time,
   focused test time, observer attach/bootstrap time, resync time, peak retained
   bytes, and representative queue high-water marks.
5. The report distinguishes removed production copies, relocated tests, and
   newly added generic/test support.
6. The implementation plan links this plan and records completion without
   changing child-RFC semantic status.

Provisional regression guard for the cleanup: a repeatable regression above
10% in observer attach/bootstrap, resync, or retained memory blocks completion
unless explained and explicitly accepted. This is an implementation guard, not
a ratified product performance ceiling.

Estimated effort: 2–3 engineer-days.

## 8. Pull-request and commit structure

Recommended review units:

| PR | Contents | Expected risk |
| --- | --- | --- |
| 0 | Characterization fixtures and baseline report | Low |
| 1A | Rust codec plus actor/usage migration | Low |
| 1B | Remaining Rust wire migration and ratchet | Low |
| 2A | TypeScript primitives plus actor/usage | Low |
| 2B | Remaining TypeScript migration and ratchet | Low |
| 3A | Registry/scoped test relocation | Low |
| 3B–3D | Scoped file splits by concern | Low, conflict-sensitive |
| 4 | Reducer identity helper | Low |
| 5A | Generic shapes and state binding | Medium |
| 5B | Log and lifecycle family strategies | Medium |
| 5C | Revisioned/owned-set strategies | Medium |
| 5D | Replacement traversal and parity harness | Medium-high |
| 6 | Shared fact-envelope header | Medium |
| 7A–7B | Probe helpers and catalog runtime shell | Low-medium |
| 8A–8C | Provenance, cursor/SQLite, publication helpers | Low-medium |
| 9 | Final ratchets and measurement report | Low |

Move-only and behavior-changing commits must not be mixed. Every PR identifies:

- exact old duplicate sites removed;
- invariants exercised;
- fixture hashes compared;
- focused and full tests run;
- any intentional error-text difference; and
- rollback commit boundary.

## 9. Validation matrix

### 9.1 Per-commit minimum

```bash
cargo fmt --all -- --check
cargo test -p spaghetti-napi <affected-module-filter>
git diff --check
```

### 9.2 Wire changes

```bash
cargo test -p spaghetti-napi scoped_observation
pnpm --filter @vibecook/spaghetti-sdk test
pnpm --filter @vibecook/spaghetti-sdk typecheck
pnpm --filter @vibecook/spaghetti-sdk build
pnpm check:sdk-package
```

Additionally compare every `crates/spaghetti-napi/fixtures/contracts/rfc012d-*`
fixture and run the shared negative codec matrix in both languages.

### 9.3 Family-kernel changes

```bash
cargo test -p spaghetti-napi runtime_semantic_reducer
cargo test -p spaghetti-napi scoped_observation
cargo test -p spaghetti-napi
cargo test -p spaghetti-architecture
cargo test -p spaghetti-coverage
```

Required behavioral matrices:

- insert, exact replay, higher/lower precedence, and explicit retract;
- reset, confirmed deletion, and temporary unavailability;
- complete versus partial owned-set replacement;
- actor context delayed/present/removed where applicable;
- capacity exactly at and one past each bound;
- overflow before, during, and after every family replacement boundary;
- failed resync and re-overflowed resync;
- cancelled attachment with outstanding delivery/application receipt;
- clean bootstrap versus completed resync digest equality; and
- durable versus scoped reducer equality at comparable coverage.

### 9.4 Catalog/engine changes

Run the full producer conformance suites for Claude, Codex, and Grok, catalog
contract fixtures in Rust and TypeScript, engine projection/query tests, and
the full `spaghetti-napi` suite. Cursor tests must include malformed base64,
oversize encoded/decoded values, wrong scope hashes, invalid positions, stale
cursors, and boundary page limits.

### 9.5 Final repository gate

```bash
cargo test --workspace
pnpm typecheck
pnpm validate
pnpm test:packages
pnpm format:check
git diff --check
```

If a repository command is known to require external native fixtures or a
machine-specific environment, record that limitation and run the closest
hermetic gate. Do not silently substitute a narrower test while reporting the
full gate as passed.

## 10. Success metrics

The cleanup is complete when all of the following are true:

1. Rust has one canonical RFC 012D opaque codec and safe-integer primitive.
2. TypeScript has one canonical internal equivalent.
3. Specialist wire modules remain explicit and strict.
4. The eight common runtime families use shared state/replacement/admission
   mechanics grouped through their correct reducer classes.
5. Usage-v2, actor-run, actor-affiliation, and unknown evidence remain special.
6. Replacement traversal is deterministic without eleven manually accumulated
   family offset blocks.
7. The root observer module is a facade with cohesive submodules and separated
   tests.
8. Reducer identity validation has one shared implementation plus explicit
   family prechecks.
9. Catalog production shells share helpers without weakening independent
   conformance evidence.
10. Engine helpers are shared only below query/projection semantics.
11. All frozen output, stable IDs, digests, and acceptance/rejection matrices
    remain unchanged.
12. Adding the next common portable family requires:
    - its already-owned RFC 012C reducer law;
    - one compile-time family descriptor;
    - one explicit observation-event variant;
    - one specialist wire event body and frozen fixture; and
    - no cloned projection-state, replacement, state-binding, admission, or
      snapshot-offset implementation.

Target copy-site reductions:

| Copy site | Baseline | Target |
| --- | ---: | ---: |
| Rust opaque/safe-integer helper implementations in RFC 012D specialists | approximately 13 | 1 shared implementation |
| TypeScript RFC 012D JSON primitive implementations | approximately 16 files | 1 internal implementation plus local domain validators |
| Common `scoped_*_state` bodies | 8 | 1 binder plus explicit hooks |
| Common projection-state shapes | 8 | 1 generic shape plus names/aliases |
| Common replacement snapshot shells | 8 | 1 shell per required shape, behavior grouped by reducer class |
| Manual family blocks in replacement traversal | 11 total families | one ordered traversal, with usage/actor special handlers |
| Reducer identity wrappers | 8 common copies | 1 helper plus plan/task prechecks |

## 11. Stop conditions

Stop the relevant extraction and retain explicit code if any of these occur:

1. The abstraction needs runtime type erasure or dynamic registration.
2. A specialist envelope gains unbound top-level deserialization.
3. Exact-object validation or preflight bounds become weaker.
4. Family behavior is selected by string names at runtime.
5. A common function accumulates repeated family-name branching equivalent to
   the code it replaced.
6. A family reducer law moves into observer delivery code.
7. Usage digest begins to depend on actor/affiliation overlay context.
8. Bootstrap/resync or completion/watermark rules become one configurable
   boolean-heavy path.
9. A catalog conformance oracle loses intended independence.
10. Existing canonical JSON, event IDs, semantic references, replacement
    digests, or coverage comparisons change without an owning RFC amendment.
11. A phase cannot be reverted independently.

When a stop condition is reached, document the failed abstraction boundary and
keep the smallest already-proven helper extraction. Do not compensate by
building a broader framework.

## 12. Sequencing and effort

The critical path before another portable family is:

```text
Phase 0 behavior freeze
  -> Phase 1 Rust codec
  -> Phase 2 TypeScript primitives
  -> Phase 3 physical split
  -> Phase 4 reducer identity
  -> Phase 5 revision-family kernel
  -> Phase 6 shared fact header
  -> next portable family
```

Catalog and engine cleanup can follow after the multiplier is stopped. It is
useful, but it must not delay the family kernel.

Estimated serial effort for the complete plan is approximately 24–40
engineer-days, including characterization and review fixes. The family kernel
accounts for most uncertainty. The critical work required before another
portable family is approximately 15–26 engineer-days.

These are implementation estimates, not remaining RFC 012 delivery estimates.
RFC 012 still has separate support-promotion, real-agent composition,
downstream integration, performance-evidence, and rollout gates.

## 13. Final outcome

After this plan, RFC 012 remains a large implementation because its domain is
large: evidence-backed adapters, catalog readiness, topology-neutral reducers,
store-free observation, strict portable contracts, and recovery by complete
epoch replacement.

What changes is the multiplier. A new fact family adds its semantic law and
specialist portable representation without cloning the observer kernel and
same-language codec infrastructure again.
