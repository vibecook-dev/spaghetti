# RFC 012 implementation and validation program

- **Status:** Active non-normative roadmap; Phase 0A/0B evidence and umbrella/
  RFC 012A ratification complete; foundation implementation in progress
- **Created:** 2026-08-15
- **Umbrella:** [RFC 012](./012-evidence-backed-adapters-and-progressive-readiness.md)
- **Child contracts:** [012A](./012a-agent-adaptation-and-engine-boundaries.md),
  [012B](./012b-catalog-readiness-and-progressive-startup.md),
  [012C](./012c-runtime-semantics-and-usage-v2.md), and
  [012D](./012d-session-scoped-observation.md)
- **Evidence:** [Phase 0 catalog census](./012-phase-0-census-2026-08-15.md),
  [Phase 0B runtime census](./012-runtime-observation-census-2026-08-15.md),
  and [cold-start profile](./011-playground-cold-start-profile-2026-08-15.md)
- **Downstream requirements:**
  [VibeField aggregation and contribution needs](../petition/vibefield-needs.md)

## 1. Role of this document

This plan sequences implementation, experiments, migration, and rollout. It is
not a semantic authority. If it conflicts with RFC 012 or an owning child RFC,
implementation stops and the owning RFC is amended before work continues.

Each work package has exactly one owning child contract. Cross-cutting packages
integrate already owned semantics; they do not redefine them.

Statuses used here are:

```text
Not started
In progress
Blocked
Gate met
Rolled out
```

## 2. Program outcome

Ship:

1. an evidence-backed adapter/support workflow and dependency-constrained
   common engine;
2. a complete/degraded, snapshot-consistent catalog before history and FTS;
3. qualified response-level usage and typed runtime facts;
4. a database-free scoped observer for one session actor tree;
5. a feature-flagged Chopsticks migration with exact epoch replacement; and
6. reproducible performance/correctness gates and rollback.

The durable product sequence is:

```text
process starts
  -> last complete catalog served or bounded cold catalog built
  -> projects and sessions visible
  -> selected session promoted explicitly
  -> history, usage, capabilities, and artifacts hydrate
  -> complete FTS becomes available
  -> background convergence and maintenance finish
```

The downstream runtime sequence is:

```text
known native session selected/launched
  -> store-free observer attaches before root creation when needed
  -> root and existing descendants bootstrap through the shared decoder
  -> bootstrap barrier establishes consumer baseline
  -> hooks trigger bounded poll hints
  -> future actors and sidecars join the declared scope
  -> typed runtime revisions feed Godview
  -> overflow invalidates one epoch
  -> resync atomically replaces it from a complete correction snapshot
```

## 3. Dependency and delivery order

The critical path is:

```text
Phase 0 evidence (complete)
        |
        v
RFC 011 delta ledger + compatibility fixtures
        |
        v
012A logical boundary + adapter support foundation
        |
        +----------------------+
        |                      |
        v                      v
012B catalog/readiness    012C runtime semantics/usage-v2
        |                      |
        |                      v
        |                012D scoped observer
        |                      |
        +-----------+----------+
                    v
        A4 new-agent cross-topology proof
                    |
                    v
          host/UI/Chopsticks integration
                    |
                    v
          search/finalization + promotion
```

012B and 012C may proceed in parallel after the RFC 012A base-model and adapter
seams stabilize. The RFC 012D source-lifecycle skeleton may be prototyped in
parallel, but its public typed event contract cannot be frozen before RFC 012C.
The A4 new-agent proof runs only after the initial catalog/query and scoped
observer seams exist; A1/A2, not A4, are the foundation prerequisite for B/C/D.

Physical crate extraction follows proven logical boundaries. It is not a
prerequisite for early vertical slices.

## 4. Program status

| Work package                             | Owner               | Status      | Exit evidence                                                  |
| ---------------------------------------- | ------------------- | ----------- | -------------------------------------------------------------- |
| E0. Phase 0A/0B evidence                 | Umbrella            | Gate met    | catalog, diagnostic, topology, usage census reports/tests      |
| X0. RFC 011 delta/compatibility gate     | Umbrella            | In progress | retained/amended/superseded contract and migration fixtures    |
| A1. Logical dependency/model seam        | 012A                | In progress | architecture checks and contract fixtures                      |
| A2. ADS/scope/support tooling            | 012A                | In progress | schemas, sanitizer, ledger checker, access tracer              |
| A3. Current-agent support candidates     | 012A                | In progress | Claude/Codex/Grok ADS and candidate entries                    |
| A4. New-agent adaptation proof           | 012A/umbrella       | Not started | fourth adapter without common-runtime/query/observer change    |
| B1. Catalog identity/readiness contracts | 012B                | In progress | Rust/N-API/TS fixtures and transition table tests              |
| B2. Bounded catalog source compositions  | 012B                | In progress | three adapter catalog identity digest parity                   |
| B3. Durable catalog/query snapshots      | 012B                | Not started | atomic pack plus snapshot pagination conformance               |
| B4. Progressive host and UX              | 012B                | Not started | cold/warm UI topology and migration tests                      |
| B5. Catalog performance calibration      | 012B                | Not started | reproducible gate-amendment report                             |
| C1. Runtime semantic contracts           | 012C                | In progress | actor/usage/state/interaction serialization fixtures           |
| C2. Usage-v2 shadow projection           | 012C                | In progress | frozen/private corpus plus native affiliation parity           |
| C3. Durable usage migration              | 012C                | In progress | transactional switch and rollback tests                        |
| C4. Runtime semantic downstream suite    | 012C                | Not started | typed consumers plus durable/live merge without native parsing |
| D1. Store-free observer kernel           | 012D                | In progress | attach/bootstrap/poll/close, no SQLite/global scan             |
| D2. Claude scope composition             | 012D                | Not started | root/current/future actor and sidecar conformance              |
| D3. Control lane and epoch replacement   | 012D                | In progress | overflow/disappearance/duplicate/fairness matrix               |
| D4. SDK and Chopsticks migration         | 012D                | Not started | feature-flagged shadow comparison and rollback                 |
| D5. Observer performance calibration     | 012D                | Not started | reproducible latency/memory/access report                      |
| X1. Search/finalization separation       | 012B integration    | Not started | complete-only FTS and maintenance experiment                   |
| X2. Diagnostic disposition/aggregation   | 012A implementation | Not started | bounded rows with count/provenance parity                      |
| X3. Physical extraction                  | Implementation      | Not started | workspace boundaries mirror dependency checks                  |
| X4. Default promotion/drift lane         | Umbrella            | Not started | child gates, telemetry, rollback, promoted support releases    |

## 5. Completed evidence work (E0)

### 5.1 Tooling landed

- `scripts/catalog_census/`: independent bounded native catalog census with an
  optional read-only SQLite identity oracle;
- `scripts/diagnostic_census/`: diagnostic aggregation feasibility census;
- `scripts/runtime_observation_census/`: response-group, actor-topology, and
  typed-runtime evidence census; and
- deterministic focused tests for the catalog and runtime census logic.

Reports contain aggregate counts and identity digests rather than native paths,
IDs, prompts, titles, questions, or payloads.

### 5.2 Findings that constrain implementation

1. A 64 KiB maximum first logical head record covers current Codex identity
   discovery; 16 KiB does not. The Rust primitive must return a complete bounded
   logical record rather than truncate arbitrary bytes.
2. Claude catalog identity is the union of top-level transcripts, nested
   parent-session membership, and session-index entries.
3. Grok session-directory membership is sufficient for identity; summary is
   replaceable display metadata.
4. Phase 0 discovered all 176 projects and 1,414 sessions at 0.832% of selected
   primary transcript bytes.
5. The selected runtime corpus contains 342,861 usage rows but only 149,077
   response groups, proving response-revision semantics are necessary.
6. Current no-SQLite tail and database host are not interchangeable; RFC 012D
   must provide a store-free replacement before compatibility removal.
7. Runtime sources include standard children, workflow children, team members,
   tasks/plans, interactions, modes, and artifact references that require
   declared scope relationships.

### 5.3 Follow-on instrumentation

Before numeric gates are ratified, Rust reports must add:

- logical records and bytes by source/tier/adapter;
- scope relation and opened-object access traces;
- decoder and reducer time/allocation by fact family;
- writer queue, commit, WAL, checkpoint, and pack-readiness milestones;
- query latency during background ingest/migration;
- observer semantic/control queue high-water marks;
- scope epochs, overflow/resync, and cancellation state;
- event-ID and clean-bootstrap-versus-resync state digests; and
- product shell, first page, complete catalog, selected hydration, search, and
  full-convergence timestamps.

## 6. RFC 012A workstream

### A1. Logical boundary and base model

Implement internal contracts for:

- RFC 012A well-formed `QualifiedValue`, base entity/source-record/fact/revision
  IDs, stable source-instance and external entity references,
  `SemanticRevisionRef`, common `SourceCoveragePoint`/`SourceCoverageSet`,
  provenance, `SourceRecord`, `FactBatch`, and mapping dispositions;
- tier/view/topology compositionality and explicit per-stream overlap strategy;
- logical subsystem ownership markers and architecture tests;
- deterministic decoder emission ordering; and
- versioned decoder state.

Gate:

- Rust/N-API/TypeScript contract fixtures serialize semantic equivalents;
- invalid qualified values and incompatible contract majors are rejected;
- external entity references remain stable across restart/registration reorder,
  never use database handles, and cannot retarget tombstoned identities;
- durable and scoped forms expose identical semantic revision references, and
  the common Rust/client coverage comparator agrees on
  equal/dominating/behind/incomparable outcomes and rejects incompatible
  clocks/generations;
- head/prefix plus continuation and full-only ingest produce equal ordered
  record/fact/decoder-state/reducer digests;
- architecture checks reject every forbidden dependency edge;
- existing source tests pass without store/query access in adapters; and
- no physical extraction is required to claim the logical gate.

Current landing status (2026-08-16):

- implemented the parallel Rust RFC 012A v1 semantic model for qualified
  values, canonical source/entity/record/fact/revision keys, external entity
  references, native identity claims, semantic revision references, coverage
  points/sets, and the conservative coverage comparator;
- froze one Rust-produced fixture consumed by portable TypeScript validation
  and comparison tests;
- added an architecture ratchet preventing the base semantic module from
  importing source, adapter, store, query, delivery, N-API, or concrete-agent
  layers;
- bound a topology-neutral semantic decode context from the ADS identity
  version plus stable source-instance/stream/object/framing inputs; it derives
  append record identity without batch ordinal and snapshot-row identity with
  its stable row ordinal, while excluding catalog IDs, observation time,
  startup phase, and delivery order;
- added a parallel `FactSemanticRevision` envelope and explicit native-keyed
  and record-derived emission APIs. Legacy `FactBatch::push` deliberately emits
  no canonical reference, duplicate canonical revisions fail before ordinal
  mutation, dependency-derived facts can supply an explicit semantic revision
  rather than pretending the primary record owns the change, and the shared
  durable/scoped decode boundary supplies the same context;
- preserved explicit semantic identities in the durable fact transaction beside
  the RFC 011 storage key: schema v45 stores the complete nullable
  source-record/fact/revision triple, rejects partial or non-32-byte triples,
  rejects duplicate non-null revision identities, leaves legacy rows null, and
  removes the identities with their owning generation; transaction tests prove
  a uniqueness failure cannot advance the source cursor;
- added schema v48's normalized durable coverage storage. Common writer-owned
  administrative transitions atomically replace bounded coverage-set metadata,
  points, explicit absences/deletions, and errors beside their projection owner;
  content digests make equal replacements true no-ops, and the stored scope is
  bound to the adapter, canonical source instance, verified source-declaration
  digest, and explicit support-release ID. Restart and quarantine-gap tests
  prove that the representation survives and does not churn on an unchanged
  scan; and
- retained A1 as `In progress`: usage-v2 is the first built-in family on the
  canonical seam, while bounded public coverage query exposure, the remaining
  fact-family migrations, full semantic reduction, tier/view compositionality,
  N-API fixture parity, and full-only versus composed reducer digests remain.

The repository-wide native-surface validator also discovered current Claude
drift that predates this model slice: `bridge-session` records now include
`ownerAccountUuid`/`ownerOrganizationUuid`, an active-session document includes
`nameSince`, and `settings.json` includes an `autoMode` policy object. No native
values were copied into RFC evidence. A3 classifies the owner UUIDs as sensitive
`native-only` bridge correlation metadata, `nameSince` as an opaque
`native-only` timestamp-like field, and `autoMode` as sensitive `native-only`
configuration rather than effective runtime-mode evidence. The native
TypeScript/Rust shapes accept them, but common identities, FTS, logs, telemetry,
runtime events, activity, ordering, presence, and effective-mode semantics do
not. Synthetic shape fixtures and positive/native-only projection tests back
those decisions; numeric or configuration shape alone is explicitly not
treated as transition evidence.

### A2. ADS, declarations, and support tooling

Build:

- support-ledger and ADS schemas;
- claim-addressable evidence manifest;
- deterministic fixture sanitizer and prohibited-field scanner;
- source declaration and restricted `ScopeProgram` validators;
- access telemetry and bound accounting;
- support-release/runtime version classifier;
- public contract-version compatibility selector;
- external-reference/semantic-reference/coverage contract-version checks; and
- machine-readable conformance manifest/checker.

The checker rejects unrestricted discovery functions, unbounded relation
fan-out, absent evidence, duplicate semantic ownership, and unclassified native
families.

Current landing status (2026-08-17):

- added strict v1 JSON Schemas for ADS, source declarations, restricted scope
  programs, evidence manifests, conformance manifests, and support-release
  entries under `agent-support/schemas/`;
- added a dependency-free repository checker that resolves claim references,
  verifies SHA-256 bindings, rejects path escape and unbounded recursive
  declarations, detects duplicate semantic ownership/unclassified families,
  scans every fixture, and enforces stronger promotion-only invariants;
- added a deterministic JSON/JSONL sanitizer that preserves structural shape
  and referential equality without committing hashes of native values. The v2
  fixture contract places numeric identifiers and timestamps in reserved,
  parser-safe sentinel ranges; the prohibited-field scanner validates those
  numbers as well as paths, string identifiers, text, secrets, and common
  credential forms, so non-string native identity/time values cannot bypass the
  repository gate;
- added executable exact/range/unverified/incompatible classification,
  pre-access public contract selection, and relation-level access budget
  accounting with overflow tests;
- moved runtime support classification and contract selection into an
  agent-neutral Rust authority, made catalog/durable/scoped access depend on a
  private non-serializable authorization, and made support authorization run
  before negotiation so candidates cannot inspect the offered typed surface;
- froze one shared support/selection fixture executed by Rust, the independent
  Python tooling, and the portable TypeScript SDK, including opaque exact
  versions, forward-catalog degradation, candidate denial, preference order,
  and incompatible rejection;
- added an architecture ratchet preventing support selection from depending on
  source, topology, persistence, N-API, or concrete adapters;
- added bounded in-memory support-package verification for the ledger plus all
  five referenced documents, including canonical confined paths, SHA-256
  bindings, adapter identity, and conformance release identity; bound every
  built-in adapter manifest to its package/decoder and ADS/source/scope
  digests; and added a strict registry path that admits only matching promoted
  packages while keeping the current zero-promotion N-API host on an explicit
  non-authorizing legacy path;
- added a strict Rust scope-program parser to support-package verification,
  made built-in manifests compile their referenced declarations, and made
  strict registration and typed authorization reject a compiled declaration
  that differs from the selected verified package even when its binding fields
  otherwise match;
- made repository validation discover candidate, promoted, and retired bundle
  directories instead of silently ignoring future promoted releases, and
  added tests that compare all three compiled adapter manifests with their
  current digest-bound candidate packages;
- implemented the agent-neutral Rust access budget with pre-access worst-case
  reservation, per-parent fan-out, total object/byte/row/depth enforcement,
  conservative failed/abandoned accounting, hashed object tokens, and bounded
  phase-tagged traces; wired adapter dependency object reads, parameterized
  queries, listings, and consistency revalidation through it; exposed aggregate
  counters on reconcile results; and added a ratchet preventing adapters from
  minting tokens or reserving native access;
- added the common `ScopeAccessPlan` compiler: one selected program now creates
  exact per-relation budgets; callers cannot substitute declaration roots or
  locators; named identity and relation/operation mismatches fail before source
  access; phases share one pass budget; and candidate Grok declarations execute
  as a non-authorizing conformance fixture;
- made scoped authorization require a negotiated observation contract and
  embed the exact promoted scope declaration plus release/declaration digests;
  only a program selected from that opaque Rust authorization can construct an
  `AuthorizedScopeAccessPlan`, while durable-only, unknown-program, candidate,
  and missing-observation paths fail closed;
- added an authorization-bound v1 access report with bounded relation traces,
  no native locators/values, a canonical SHA-256 digest, mutation verification,
  and a shared portable fixture verified independently by Rust, Python, and
  TypeScript;
- added a crate-private database-free scoped composition root that owns the
  Rust authorization and exact known-object grants, permits one bounded pass at
  a time, reserves before an actually confined read, supports attach before
  object creation, emits the frozen path/content-free report, and fails closed
  after idempotent close; plus an architecture ratchet preventing store,
  N-API, concrete-adapter, or premature public-host dependencies;
- extended the common append driver with an enforced physical-read ceiling and
  exact framing/continuity-anchor byte accounting, then added store-free root
  append state that owns its checkpoint/generation, retains partial suffixes,
  blocks bootstrap completion until bounded batches drain, distinguishes cold
  bootstrap and live post-attach creation from correction, and carries an
  explicit reset-before-items descriptor on truncate/replacement replay;
  checkpoint advancement now requires explicit ordered-lane admission, while
  discard leaves the cursor unchanged for deterministic replay and a pending
  observation blocks both another read and bootstrap completion;
- extracted adapter invocation, panic containment, disposition validation, raw
  retention, and decoder-state extraction from the durable coordinator into a
  shared store-agnostic decode runtime; the durable coordinator and provisional
  scoped host now call the same boundary, guarded by an architecture ratchet;
- added a scoped append decode transaction that stages ordered record facts and
  next decoder state under one lifetime-bound decoder/object-context/retention/
  finite-output configuration, commits decoder state and source checkpoint
  together only after admission of the matching unforgeable decoded-batch
  receipt, rejects a raw read or receipt from another observation, resets
  decoder state on a source generation change, and leaves both unchanged on
  retry, failure, or discard; retained evidence obeys the selected raw policy,
  and undeclared decoder dependency access fails closed;
- added a bounded internal scoped-admission lane with independent decoded-data,
  retained-native-byte, and reset-control limits; admission is all-or-nothing,
  returns the unchanged decoded batch on backpressure, queues reset before
  replay data, and commits source cursor plus decoder state only after the
  complete unit is resident. Its internal lane ordinal is not an RFC 012D
  `observer_sequence`;
- kept that provisional lane fact-based rather than inventing delivery events:
  `FactBatch` now supports a parallel explicit canonical revision while
  retaining the RFC 011 store-oriented `FactId`, but legacy emissions receive
  no synthesized fallback. Public semantic projection remains gated on
  built-in family adoption, durable preservation, and reducer ownership;
- integrated contract and tooling checks into `pnpm validate`.

A2 remains `In progress`: the internal scoped composition now owns the
authorized plan lifecycle, executes its first common confined primitive, and
reuses the store-agnostic decode boundary, but no catalog, durable, or public
scoped-observer host owns the full strict lifecycle; the access-report IPC
retrieval shape and trusted native-probe/grant request are not yet frozen;
adapter registrations must move from the explicit legacy path to the strict
promoted catalog after the first support release is promoted; and the remaining
scope primitives and public N-API/IPC boundary still require executable
conformance rather than portable classification alone.

### A3. Current-agent candidates

For Claude, Codex, and Grok:

1. pin/artifact-identify current targets;
2. write ADS object/identity/join/lifecycle claims;
3. declare durable, catalog, and scoped source compositions;
4. declare per-stream overlap strategy and safe decoder-state boundary;
5. create sanitized deterministic fixtures and adversarial transitions;
6. classify exact/range/unverified/incompatible runtime behavior;
7. map every family to one disposition; and
8. produce candidate support-ledger entries.

Claude additionally resolves every RFC 012D scope relation and every RFC 012C
runtime capability claimed for Chopsticks.

Current landing status (2026-08-16):

- added non-selectable Claude, Codex, and Grok candidate directories containing
  ADS, current bounded durable source declarations, partial restricted scope
  programs, claim-addressable evidence, conformance manifests, and digest-bound
  support-ledger entries;
- represented every currently declared stream with one native-family
  disposition, overlap strategy, safe decoder-state boundary, and finite
  object/record bounds;
- recorded unsupported catalog and scoped topologies explicitly instead of
  presenting existing durable adapters as full RFC 012 support; and
- classified Claude `ownerAccountUuid`/`ownerOrganizationUuid` as sensitive
  `native-only` bridge correlation metadata, `nameSince` as opaque
  `native-only` timestamp-like metadata, and the structured `autoMode` policy
  as sensitive `native-only` configuration rather than effective runtime mode;
  added agent-native TypeScript/Rust acceptance and preservation where policy
  allows, while conformance tests prove that none becomes common identity, FTS,
  logs, runtime semantics, activity, ordering, or effective-mode state. All
  three signatures remain explicit in the support ledger as `classified`, not
  silently treated as resolved semantic mappings.

A3 remains `In progress`: no artifact version is pinned; Codex/Grok still need
independent sanitized transition corpora; Claude needs the complete executable
RFC 012D relation set and RFC 012C semantic fixtures; catalog/scoped stream
compositions are not implemented; and identity, compositionality,
cross-topology, performance, and human sanitizer-review gates remain planned.

### A4. New-agent proof

Adapt one additional agent after A1/A2 and the initial B/C/D public semantic
seams stabilize. Record:

- common files/modules changed;
- new source/scope primitive pressure;
- fixture and ADS authoring time;
- unsupported/degraded capabilities;
- performance and access bounds; and
- any public-query/observer changes requested.

Gate: no source-runtime, observer lifecycle, store/query, or concrete-agent
switch modification is required. A missing reusable primitive triggers a
reviewed common-component proposal rather than a private adapter workaround.
This is an umbrella promotion gate, not a prerequisite for implementing the
first B/C/D vertical slices.

## 7. RFC 012B workstream

### B1. Contract fixtures

Add semantic fixtures for:

- catalog assertions and qualified display values;
- stable project/session identity and explicit identity relations;
- catalog representative-to-concrete-base-session attach handoff;
- native session/project association assertions, locator claims, conflicts,
  retraction, and policy-bound disclosure;
- persisted external-reference resolution across live, tombstoned, superseded,
  and unknown states;
- reducer precedence/conflicts/evidence retraction;
- coverage-plan identity; readiness state, epoch/attempt, reason, source
  coverage, and last-complete snapshot;
- `CatalogSnapshotId` and cursor query binding;
- catalog query-pack version selection/incompatible rejection; and
- hydration commands/receipts separate from queries.

Test every RFC 012B readiness transition, including required-source-set and
support-release changes, before implementing product startup.

Current landing status (2026-08-17): a crate-private, library-first RFC 012B
catalog contract module now normalizes coverage-plan identity across scope,
required/optional source role, stable source identity, support release,
catalog declaration, and access policy. It freezes `CatalogSnapshotId`, query
fingerprint, and keyset-cursor bindings, and implements an executable
epoch/attempt readiness machine covering pending, building, partial, ready,
degraded, and error states; current retry evidence; terminal degradation and
recovery; ordinary refresh; plan/contract/source-generation lineage changes;
and independently safe prior snapshots. Checked construction and resume reject
false-ready state, wrong plans/packs, incomplete required coverage,
duplicate/unplanned sources, and cursor reuse across snapshot, query, or sort.
A Rust-produced v1 fixture and architecture ratchet keep the draft module
private and free of storage, source, vendor, and N-API dependencies.

The second library-first slice defines crate-private project/session membership
assertions, native association and policy-bound locator evidence, explicit
alias/same/replacement relations, and a relation-proven
representative-to-base-session attach handoff. Deterministic field and
association reduction applies concrete-value precedence while retaining
competitors and equal-authority conflicts. Complete confirmed-deletion or
replacement evidence retracts only its owning source generation; immutable key
history prevents all four evidence domains from retargeting after retraction;
and canonical multi-owner tombstones require complete, commit-ordered evidence
and an explicitly newer generation for revival. Live external-reference
resolution now returns its typed reduced row, including session association
coverage, while tombstoned, superseded, and unknown results retain canonical
semantic provenance. Frozen Rust evidence fixtures and the nested catalog
architecture ratchet cover these contracts.

The readiness resume validator additionally rejects future last-complete
epochs, empty or optional-only `Partial` state, false current-complete commits,
and integrity-failure snapshot-disposition mismatches. `IndependentlySafe` and
`Discarded` integrity outcomes are persisted explicitly rather than inferred
from the presence of an old snapshot.

The third library-first slice composes the existing RFC 012A contract-version
selection into explicit catalog query request, offer, and selected-contract
DTOs. It requires the catalog query pack, reports a typed incompatibility axis
for every base/family/query/observation/unknown-preservation mismatch, and
negotiates a hard bound for additive fields and future response variants. Rust
and portable TypeScript parse and round-trip the same Rust-produced fixture;
reject non-JSON, over-depth, over-node, oversized, reserved-key, and
non-JavaScript-safe payloads; and require responses and continuations to equal
the caller's exact negotiated selection. Continuations additionally bind the
selected pack, retained snapshot, query fingerprint, sort specification, and
cursor, with JavaScript-safe snapshot counters. The module remains free of
engine execution, snapshot retention, storage, hydration, and N-API exposure.

The fourth library-first slice keeps hydration separate from read-only queries
and defines idempotent selected-base-session commands bound to the exact
negotiated query selection, retained snapshot and coverage-plan source,
support release, catalog declaration, access policy, reducer-proven locator
authorization, and bounded fact-family/pass scope. The locator authorization
freezes canonical semantic provenance plus locator kind, basis, disclosure,
source generation, and the relation-proven representative-to-base handoff;
the displayed representative remains a valid target when it is itself the
selected disclosed base member. Stable request keys reject retargeting, while
equal work under distinct request keys coalesces exactly. Rust-derived
scheduling receipts bind accepted, already-satisfied, in-progress, retryable,
and terminal outcomes to attempt/prior lineage and, for in-progress work, the
exact accepted active command and receipt. Portable TypeScript validates the
Rust fixture against caller-held command, requested-scope, prior-receipt, and
active-schedule context, and shared portable contract-version parsing now
rejects values outside Rust's `u32` range. The contract carries no raw request
token, native identity, or locator and deliberately adds no scheduler, source
read, storage, cancellation, hydration execution, or N-API authority.

The fifth library-first slice freezes complete portable project/session page,
readiness, external-resolution, and snapshot-expiration contracts. Rust
constructs snapshot-frozen pages with exact caller-held selection, query, sort,
page-size, cursor, and coverage-plan bindings; canonical nonrepeating row
order; self-consistent known/unknown counts; selected and competing evidence
membership; policy-withheld native values; complete association coverage; and
bounded canonical provenance. Readiness coverage is bound to catalog
declaration plus access policy, rejects zero generations and noncanonical or
unbounded member evidence, and can retain an independently safe prior-plan
snapshot without presenting it as current-plan completeness. External
resolution preserves the requested identity across live, tombstoned,
superseded, unknown, and negotiated typed-unknown states. Snapshot expiration
is emitted only after the exact continuation and caller-held scope validate,
and requires a strictly newer snapshot in the same pack/scope lineage;
malformed or foreign cursors cannot be relabeled as expired. Portable
TypeScript independently parses the Rust fixture, rejects semantic
unknown-field drops and Rust/JavaScript numeric drift, and preserves only
bounded negotiated additive response data. The contracts remain free of
storage, engine queries, source reads, hydration execution, and N-API.

B1 remains `In progress` only at the public exposure gate: no catalog N-API
query surface should land until engine transport can bind an authorized
`CatalogPolicyView` to the frozen access-policy digest and B3 provides real
retained-snapshot/query execution. The first B2 adapter-neutral bounded
catalog-composition slice is now in progress. B3 durable transactional
persistence, outbox, and restart parity have not started; scheduling and
hydration execution remain later integration work.

### B2. Catalog source compositions

Implement common bounded head/prefix and catalog membership mechanics, then
compose:

- Claude index, top-level transcript membership, nested parent membership, and
  bounded transcript-head fallback;
- Codex bounded first `session_meta` record and internal-rollout exclusion; and
- Grok directory membership plus replaceable summary metadata.

Gate: Rust catalog identity digest equals the independent Phase 0 census and
the final hydrated identity oracle for every frozen fixture. Reads stay within
declared bounds. Head/prefix plus continuation equals full-only fact and final
projection digests under the RFC 012A overlap strategy. Project-association
digests retain every evidence basis/conflict and never infer VibeField grouping.

### B3. Durable pack, readiness, and pagination

Implement:

- catalog assertion/canonical/tombstone rows;
- transactionally materialized project/session counts and summaries;
- coverage-plan-bound readiness epoch and last-complete snapshot publication;
- durable invalidations/outbox;
- keyset pagination bound to snapshot and query fingerprint;
- snapshot retention/lease or explicit expiration;
- external catalog-reference resolution plus policy-filtered association and
  locator evidence; and
- coarse-grained project/session query APIs.

Crash-inject before/after every rows/snapshot/readiness/outbox boundary. Query
purity tests run against actual read lanes.

### B4. Progressive host and UX

Change host lifecycle so all source catalogs are registered before full history
work. Add:

- safe last-complete warm query path;
- boot-critical versus background migration classification;
- complete/degraded cold catalog boundary;
- writer-aware weighted scheduling;
- explicit selected-session hydration;
- renderer pagination and content/readiness states; and
- complete-only FTS availability.

Remove initial-library per-project usage and per-session task fan-out.

### B5. Performance calibration

Run cold/warm/catalog-query/selected-hydration experiments on frozen inputs.
Report every environment/evidence digest required by RFC 012B. Propose a child
RFC gate amendment that promotes measured values from experiment target to
ratified release ceiling. Until amended, semantic correctness gates block
release but provisional p95 values do not become accidental architecture law.

## 8. RFC 012C workstream

### C1. Semantic contracts and fixtures

Add agent-independent types and reducers for:

- common revision metadata, reducer classes, ownership/retraction, and complete
  replacement representations;
- mandatory RFC 012A semantic revision references on native-derived typed
  runtime revisions;
- aggregate-facing durable evidence pages with fixed-snapshot continuation,
  external subject references, projection status, and common source/family
  coverage;
- revision-granular and composite-row semantic-reference mappings;
- actor runs and independent team/workflow affiliations;
- messages/content blocks with correction and complete/partial replacement;
- qualified four-bucket usage response revisions;
- model, effort, session mode, and permission mode evidence;
- plans, tasks, tool calls/results, compaction, and progress;
- structured user-input request lifecycle and typed options; and
- capability/evidence quality plus unknown native evidence.

Fixture serialization must match across Rust, N-API, and TypeScript without
freezing language-specific layout prematurely. Every family fixture includes
entity/revision identity, explicit retraction, partial non-retraction, and its
complete replacement representation.

### C2. Usage-v2 shadow projection

Replace Claude row-UUID delta emission in the shadow path with response-keyed
snapshots. Implement deterministic fallback for missing message IDs,
qualified-bucket semantics, exact-repeat suppression, distinct deterministic
revision IDs for evolving corrections, downward correction, generation
retraction, actor attribution, and affiliation regrouping.

The independent oracle groups native evidence without adapter imports and
checks response, actor, session, affiliation-group, and aggregate values plus
quality/coverage.

Current landing status (2026-08-16):

- added a sanitized, Rust-derived `runtime.actor-run`,
  `runtime.actor-affiliation`, and `runtime.usage-v2` v1 fixture consumed by the
  portable SDK. Rust constructs every opaque identity and semantic revision;
  TypeScript independently validates the same wire values without SQLite,
  N-API, native paths, or Node-only `Buffer`. The contract preserves and
  validates affiliation effective time and usage source time, exercises
  browser-like parsing plus malformed timestamp cases, and remains explicitly
  a value-contract slice rather than the complete C1 observer/reducer exit
  gate;
- added the agent-neutral `runtime.usage-v2` response fact with canonical
  session/actor keys, object-and-generation-scoped response identity, explicit
  native-ID versus source-record fallback, four independently qualified
  buckets, optional qualified model/effort evidence, and mandatory RFC 012A
  semantic revision identity;
- introduced usage-v2 in Claude decoder contract 17, retained that response
  identity unchanged in contract 18, and moved to contract 19 when permanent
  diagnostics gained capability-scoped coverage consequences. Contract 19
  forces existing contract-18 databases through replay while preserving the
  same response identity and dual-emitting the canonical response fact beside
  the unchanged legacy row-delta fact. Non-empty
  `message.id` is primary, `requestId` is correlation metadata only, exact zero
  survives, absent buckets remain `Unknown/Missing`, and a malformed usage
  object emits a bounded diagnostic without dropping the message or replacing
  the last valid shadow contribution;
- added schema v46's private usage shadow reducer: qualification metadata is interned,
  one latest row is stored per canonical response fact, source order rejects a
  stale overwrite, downward revisions replace rather than subtract, and
  generation replacement retracts the old response namespace in the source
  transaction;
- proved topology-independent response identity and exercised exact repeats,
  evolving/upward/downward counters, missing buckets, absent and reused
  `requestId`, actor/session attribution, legacy/v2 numeric divergence, and
  generation reset while keeping the legacy query unchanged; and
- added an adapter/SDK/database-independent Python oracle, a sanitized frozen
  root-and-child source-record fixture, and a digest-bound report. The real
  parent/subagent decoders and durable reducer match it at response, actor,
  session, and aggregate scope, including qualified unknown coverage, exact
  zero, framed fallback identity, malformed non-erasure, request-ID reuse, and
  generation cleanup; and
- added schema v47's topology-neutral `runtime.actor-run` and
  `runtime.actor-affiliation` projections. Root/child transcripts now emit one
  canonical actor declaration, workflow journals emit replaceable workflow
  affiliation evidence using the identical child key, and the common reducer
  handles `Present`, `Removed`, `Unknown`, late arrival, orthogonal team plus
  workflow dimensions, and generation retraction without copying or reburning
  a response;
- added the versioned `getRuntimeUsageV2` shadow query across Rust, N-API, and
  the transport-neutral SDK. It validates legacy project/session membership,
  returns canonical session/actor/revision references, pages one response
  revision per contribution with independently qualified buckets, reports
  actor context and all current affiliation revisions, supports actor and
  present team/workflow filters without copying contributions, and binds
  continuation to one commit watermark or fails an expired cursor;
- added writer-owned projection-version transitions for `runtime.usage-v2`.
  Commits from only the transcript streams that declare this family atomically
  set the pack to `Pending`; a separate zero-fact administrative transaction on
  the same durable commit clock establishes `Ready` or `Unavailable` after the
  bounded reconciliation drains. Equal transitions are true no-ops, unrelated
  sidecar commits cannot churn the pack clock, and the public query returns the
  readiness row from the same SQLite snapshot as responses and aggregates;
- made quarantine gaps fail closed: record quarantine on a provider stream
  establishes sticky `Unavailable` readiness, and a later append or no-op scan
  cannot fabricate recovered coverage. Clearing that state is deliberately
  reserved for the explicit replay/revalidation path still required by C3;
- added schema v48's normalized durable RFC 012A source/fact-family coverage.
  The common administrative writer atomically replaces one bounded set of
  points, explicit absences/deletions, and errors with the readiness barrier;
  it persists its support-release and verified declaration binding, uses a
  deterministic content digest for no-op suppression, survives restart, moves
  with append progress, and retains a stable replay-required gap after
  quarantine;
- added the common coordinator's source-instance-scoped fact-family replay
  lifecycle for usage-v2. It writes a durable `Pending` marker before reading,
  freezes normalized coverage as the per-object generation baseline, selects
  providers by capability declaration, forces untouched append/document/
  presence/database objects into a replacement generation, and retracts the
  old generation atomically with the first replay slice. Bounded work keeps
  the baseline and resumes after restart from replacement-generation cursors;
  only a complete post-drain barrier replaces coverage and becomes `Ready`,
  while a new quarantine returns to replay-required `Unavailable`; and
- exposed normalized fact-family coverage through the generic bounded
  `getFactFamilyCoverage` contract across Rust, N-API, and the
  transport-neutral SDK. The query resolves an opaque project/session scope to
  its source instance, pages points, explicit absences/deletions, and errors in
  deterministic order at one commit watermark, expires stale or cross-scope
  cursors, distinguishes `not_materialized`, and returns only versioned opaque
  common references rather than native paths or object keys; and
- exposed `replayFactFamily` through N-API and the sole-owner observation host
  with a compare-and-set authorization copied from one materialized coverage
  set. The command resolves project/session scope before source discovery,
  rechecks the source/digest/commit after acquiring the instance lease, and
  makes the writer compare that same authorization in the transaction that
  marks replay `Pending`. Stale/cross-scope tokens, wrong adapters, wrong host
  roots, and writer-side mismatches create no commit. The host injects only its
  configured roots; the transport-neutral query/IPC client stays read-only;
  and
- made coverage construction corpus-bounded without changing its identity:
  membership length is counted before a second canonical streaming digest
  pass, small inputs remain digest-compatible, and inputs above the previous
  64 KiB serialization ceiling no longer require one aggregate allocation.
  Targeted object recovery now closes the same instance-wide provider barrier
  as a full scan, while capability-scoped diagnostics keep unrelated retained
  projection loss auditable without contaminating usage-v2 coverage;
- passed the private native corpus-scale gate on a stable ephemeral clone. The
  independent census and durable reducer matched exactly across 149,369
  responses, 5,044 actors, 854 sessions, root/child partition, model and
  fallback counts, all four token totals, and zero unknown buckets. All 5,182
  declared transcript objects produced complete coverage and Ready v1; six
  retained `claude_typed_projection_loss` diagnostics correctly remained
  audit evidence outside usage-v2 coverage. The aggregate-only report is
  [`usage-v2-private-parity-v1.json`](../../agent-support/claude-code/candidate-2026-08-15/reports/usage-v2-private-parity-v1.json)
  (`sha256:2d84af3dd9bcfb91e727b8d0e067679b1637e61b0a343957a09b8f42c303176e`);
- added decoder contract 20's native team-to-actor correlation. Team configs
  affiliate the root through `leadSessionId` plus an exactly-one lead-member
  match; zero or multiple matches fail closed as retained unknown evidence;
  child sidecars affiliate the path-owned actor through native `teamName` plus
  member `name`. Both derive the same canonical team/member keys without
  filename guessing. Replace-document edits and deletion retract only the
  sidecar-owned grouping edge and never copy, burn, or retract actor usage. An
  aggregate-only native census found 26/26 valid configs with exactly one lead
  member, 20 unique current child joins, and zero ambiguous joins; sanitized
  root/child fixtures plus an end-to-end query test cover late arrival,
  filtering, update, and deletion;
- added decoder contract 21's complete value-derived usage revision key. The
  common fact layer suppresses equal revisions inside one batch, the durable
  ledger suppresses an already-retained usage revision across commits, and the
  reducer independently recomputes the key before accepting it. Exact repeats
  emit no `runtime.usage-v2.changed` entry, changed normalized snapshots emit
  one upsert with the new RFC 012A semantic reference, and live/correction
  generation reset emits explicit response deletes before replay. Query
  bootstrap coalesces the historical response stream into the final
  readiness/coverage baseline instead of writing one public usage-v2 change
  per native revision; a focused test proves that exact post-barrier repeats
  remain silent and the first changed live revision is delivered once. Exact
  equal revisions decoded into separate batches merge idempotently, while an
  unequal value under that revision fails closed. A non-consecutive
  `A -> B -> A` value reversion is delivered as a second ordered transition
  while reusing `A`'s valid semantic ledger row. A stable-clone independent
  census found 965 exact complete semantic repeats, 135,981 counter-equal
  metadata corrections, and 262 non-consecutive semantic reversions among
  344,160 usage rows; its aggregate-only report is
  [`usage-v2-semantic-revision-census-v1.json`](../../agent-support/claude-code/candidate-2026-08-15/reports/usage-v2-semantic-revision-census-v1.json)
  (`sha256:4dee1d89f0f5a474458cbe257f3607b28e0911bf38d7f8c26dadfe83550edf9d`).
- added decoder contract 22's RFC 012C root actor identity correction before
  promotion. One common derivation now binds the final base session key, the
  `Root` role, and either a support-declared native run discriminator or the
  stable singleton-root discriminator. Scoped pre-attach and durable
  `FactBatch` derivation are equal; Claude root actor declarations, root usage,
  child parent references, and root team affiliations all use that key while
  child identity remains unchanged. The decoder-contract bump forces a
  generation replay rather than mixing the superseded candidate-only key with
  corrected state. The Rust-produced portable fixture freezes the root/child/
  usage identity consequences and the TypeScript parser consumes the same
  bytes. A release-artifact-bound contract-22 run then passed exact durable
  parity, zero final foreign-key violations, and complete Ready coverage for
  150,757 responses across 5,201 transcript objects in 358.25 seconds. The
  report is
  [`usage-v2-semantic-revision-parity-v1.json`](../../agent-support/claude-code/candidate-2026-08-15/reports/usage-v2-semantic-revision-parity-v1.json)
  (`sha256:3f3e0606e1dd228771f0778c9299a9cdbd62fbb11b555352a36d693bdcf7ad76`);
  and
- kept the candidate capability `unsupported`: portable remaining runtime
  family serialization, the remaining scoped-observer family/envelope mapping,
  and the representative external compatibility telemetry window are not yet
  complete. The crate-private usage-v2 mapping described under D1 closes only
  the first common observer reducer slice; it is not the public observer
  contract.

### C3. Durable migration

Build usage-v2 beside legacy usage. Keep v2 readiness non-ready during replay,
compare against the independent oracle, then switch the versioned query in one
transaction. Retain the legacy projection during the compatibility window and
test crash/restart at each migration boundary.

Current landing status (2026-08-16): the versioned v2 detail query, durable
readiness, coverage/replay, and source-scoped selection control plane are
implemented. `getRuntimeUsageV2` reports `shadow`, `selected`, or
`not_materialized`; it still does not alter `getUsage` or `getUsageActivity`.
A valid legacy project/session scope with no canonical v2 session mapping
returns `not_materialized` and qualified-unknown aggregate coverage rather than
silently falling back to row-additive usage. Selection, response pages,
readiness, and aggregates share one SQLite snapshot; cursors are scoped to all
filters and expire on a newer commit. Tests cover pagination, actor filtering,
present-affiliation regrouping/removal, reset expiration, and selection state.

`projectionReadiness` is now independently versioned and writer-owned. A
provider-stream data transaction publishes `Pending` with its rows; the
post-drain barrier publishes `Ready` or `Unavailable` without touching source
cursors. The transition has a durable commit sequence, is visible in the same
query snapshot, survives restart, and does not move for settings/presence/task
streams that do not provide usage-v2. Provider quarantine is sticky rather
than becoming ready after its cursor has skipped the failed record.

Schema v48 now stores the corresponding RFC 012A fact-family coverage as
normalized, bounded sets/points/absences/errors. The post-drain administrative
transaction replaces coverage and readiness atomically; equal content does not
advance the commit clock. Coverage identifies the support release and verified
source declaration, survives restart, records append-cursor progress, and
keeps a quarantined interval explicitly unavailable until replay.

The first explicit recovery path is also implemented in the common
coordinator. `FactFamilyReplayRequest` is scoped to one already-declared source
instance and currently accepts usage-v2 v1. The coordinator durably marks the
attempt before provider reads, retains the old coverage set as its crash-safe
generation baseline, and resumes bounded append work after restart without
resetting an object that already entered the replacement generation. Tests
prove ordinary repair cannot clear a gap, explicit replay does, old-generation
usage is retracted without duplicates, and a five-record replay interrupted at
a test four-record pass bound completes after engine restart through the same
path whose production bound is 4,096.

The bounded public coverage inspection surface is now implemented as
`getFactFamilyCoverage` contract v1 across Rust, N-API, and the
transport-neutral client. It returns normalized set metadata plus a
deterministically ordered point/absence/error union under one commit watermark;
its full-scope cursor expires after a newer commit. The public DTO contains
only opaque common references and cannot disclose native paths or object keys.
Focused Rust tests cover complete and unavailable sets, pagination, stale
cursor rejection, restart stability, and reference privacy; the persistent SDK
test exercises the native boundary.

The public recovery command is also implemented. Low-level N-API
`replayFactFamily` requires explicit configured roots and a current coverage
authorization; `ObservationHost.replayFactFamily` removes the roots parameter
and injects only the selected adapter's configured roots. The authorization is
the source-instance reference, content digest, and coverage commit sequence
from one materialized coverage set. It is checked during scope resolution,
again against the leased replay baseline, and atomically by the writer before
`Pending`. Focused Rust and SDK tests cover stale tokens, unconfigured/wrong
roots, writer rollback, successful replacement, and post-success token expiry.
The transport-neutral query/IPC protocol deliberately remains read-only.

The readiness/coverage administrative commit now has deterministic fault
seams before its transaction, after commit-row allocation, after readiness
writes, after coverage replacement, immediately before SQLite commit, and
after durability but before acknowledgement. Tests prove every precommit
failure leaks neither half and retries to one shared commit sequence; an
after-commit error survives database reopen and an equal retry is a no-op.

Schema v49 and query-selection contract v1 now add the source-instance-scoped
control plane. An absent row is the immutable `legacy.usage@1` default at epoch
zero. Promotion compare-and-sets that tuple and, in the same writer
transaction, revalidates Ready v1 projection state plus complete matching
coverage from one barrier commit. Selection transactions are isolated from
readiness/coverage changes, use the normal commit clock, and preserve
`legacy.usage@1` as the rollback target. Stale or failed guards write nothing;
identical retry after acknowledgement loss is a no-op success.

`selectRuntimeUsageQuery` is exposed by low-level N-API and the sole-owner
`ObservationHost`, while the transport-neutral query/IPC client stays
read-only. `getRuntimeUsageV2` returns the selection from the same snapshot as
rows/readiness and reports `selected` only for an explicit v2 selection.
Explicit rollback compare-and-sets back to the retained legacy target without
requiring v2 to remain healthy and never deletes either projection. Focused
writer tests cover every precommit seam and post-commit acknowledgement loss;
SDK tests cover the implicit default, promotion, stale rejection, rollback,
idempotent retry, host forwarding, and the read-only client boundary.

C3 remains `In progress`: private parity, source-scoped selection, its crash
boundaries, rollback, and the bounded non-mixing aggregate vector are closed.
`getRuntimeUsageTotals` now validates one to 128 non-overlapping canonical
scopes, negotiates every contributing source under one read snapshot, returns
a typed non-result for mixed/unready selection, and exposes exactly one labeled
legacy or usage-v2 aggregate arm after resolution. Native-boundary tests cover
two source instances, deterministic request reordering, one-sided and complete
promotion, explicit legacy compatibility, and a selected v2 vector becoming
unready without fallback.

The read-only compatibility sampler, bounded owner-lifetime telemetry, and
unhealthy-v2 rollback drill are now implemented. The sampler compares both
labeled arms under one snapshot, classifies each bucket as equal,
legacy-higher, v2-higher, or incomparable, and never treats expected semantic
divergence as oracle failure. It retains only fixed counters/delta summaries
and commit bounds. The two-source drill proves partial rollback is visibly
mixed, complete rollback restores legacy, and v2 shadow rows survive.

The remaining C3 gate is collection and review of a representative external
compatibility-window report. `getUsage`/`getUsageActivity` remain explicitly
legacy; new composite/default consumers migrate to `getRuntimeUsageTotals`,
and this implementation evidence alone is not a support-promotion claim.

### C4. Downstream semantic suite

Before RFC 012D cutover, exercise common semantic reducers for:

- per-actor qualified usage and unknown context handling;
- response-observed versus configured model/effort;
- native mode transitions;
- plan/task/tool/progress lifecycles;
- question open/resolved/failed/cancelled with typed display fields; and
- late affiliations without duplicate usage.

Exercise a reference downstream aggregator for:

- durable query plus scoped-observer state reconciliation by
  `SemanticRevisionRef`, while ordered observer delivery deduplicates by the
  occurrence-scoped `event_id` so `A -> B -> A` is not lost;
- catalog/durable/observer equality for the concrete base-session reference;
- direct durable absorption and complete-coverage overlay retirement;
- partial/unavailable/incomparable coverage retaining or marking overlay state
  stale;
- reset/retraction and snapshot expiration during pagination; and
- several process-lifetime runs correlating to one native session without
  rewriting Spaghetti actor identity.

At the same RFC 012A source/family coverage vector, durable and observation
reducers must produce equal per-family semantic and replacement-state digests.

No consumer may parse native payloads for supported typed behavior.

## 9. RFC 012D workstream

### D1. Store-free observer kernel

Implement `ObservationProjectionSink` and the observer facade over the same
source/decoder registry used by the durable host. It must:

- instantiate no store/migration/query/outbox/global host;
- negotiate a compatible observation contract before source access;
- perform only the bounded native version probe before selecting an
  exact/range-supported observer support release;
- accept one explicit scoped access grant;
- accept/validate a persisted external session reference when supplied and
  derive final root session/run identity before installing watches;
- use watch-before-scan and bounded reconciliation;
- hold only bounded scope/cursor/correlation/reducer state;
- expose event drain, poll, bootstrap/resync barriers, bounded artifact reads,
  and idempotent close;
- expose RFC 012A source/family coverage from poll and barriers; and
- instrument every access for no-global-scan conformance.

Current landing status (2026-08-17):

- the crate-private composition root performs strict support/contract/program
  selection before validating exact grants and exposes no spoofable N-API
  artifact-probe request;
- after support/contract selection and before any scoped source access, the
  composition root now derives one stable canonical source-instance, base
  session, external-session-reference, and root actor/run identity from
  redacted pre-attach inputs. A supplied expected session key and/or persisted
  external reference must equal that derivation or attachment fails with
  `InvalidRootIdentity`; the root transcript and its containing directory may
  still be absent. The run derivation includes that base session and the `Root`
  role; an explicit support-declared native run discriminator changes only the
  run identity, not the base session identity;
- every scoped append object must carry the selected root's adapter and source
  instance before it can reserve read budget, decode, or contribute coverage.
  The internal watermark core carries the resolved root identity and every
  assembled RFC 012A coverage scope now names the same root session key;
- the agent-neutral `FactBatch` root-actor helper uses that exact derivation.
  Claude decoder contract 22 adopts it for root actor declarations, root usage,
  child parentage, and root team affiliations, while child keys remain stable;
  its contract bump provides the durable replay boundary and an artifact-bound
  full-corpus experiment proves response/actor/session/bucket parity, complete
  usage-v2 coverage, and zero final foreign-key violations;
- one host-approved known object can be absent at attachment, created later,
  and read through the common symlink-safe confined file primitive only after
  a declaration-sized reservation. After the initial snapshot, transitions in
  either direction between present and missing become explicit bounded common
  controls rather than implicit object-state changes. Object presence is an
  explicit `Unknown`/`Missing`/`Present` state: an unstable initial read retains
  `Unknown`, so the next stable cold-bootstrap batch cannot fabricate
  `source.created` without a prior stable absence;
- the same granted root can run through the common append driver under a hard
  physical-byte ceiling; its in-memory kernel retains cursor/generation and
  partial-record state, prevents an early bootstrap barrier while more bounded
  batches remain, and classifies true generation changes as correction with a
  reset-before-items descriptor; its cursor advances only after explicit
  admission, and discarded/unacknowledged batches cannot silently skip source
  records;
- complete append records pass through the same store-agnostic decoder boundary
  as durable ingestion; scoped facts and decoder state are staged in memory,
  raw evidence is policy-bounded, and source cursor plus decoder state advance
  atomically only after admission of the matching decoded-batch receipt; a raw
  read or mismatched receipt cannot advance either, retry/failure/discard
  advances neither, generation reset clears prior decoder state, and undeclared
  decoder dependency access fails closed;
- a bounded internal admission lane independently limits decoded-event weight,
  actual retained-native bytes, and source-presence/reset controls; it admits
  lifecycle controls before replay data, rejects the whole unit on pressure
  while returning it for retry, and only then commits the paired source cursor
  and decoder state. Recreation admits `source.created`, then `source.reset`,
  then corrected data. The lane ordinal is internal ordering, not public
  `observer_sequence` or semantic identity. Its production drain projects the
  front frame into a side-effect-free plan: semantic validation or delivery
  capacity failure restores the exact control/data frame and leaves reducer
  state, byte/event accounting, and offer sequencing intact for retry. Only an
  all-or-nothing delivery offer commits the prepared reducer mutation and
  releases the admitted frame. Raw popping and projection-only consumption are
  compiled for conformance tests only;
- the scoped decoder binding carries the same topology-neutral semantic context
  as durable decode; canonical fixture emissions replay to equal
  `FactRevisionId`/`SemanticRevisionRef` values even when numeric catalog IDs,
  observation times, and append batch ordinals differ. It now also derives the
  same opaque source-instance, coverage-stream, and coverage-object coordinates
  as the durable coverage path and carries them through admitted controls,
  projected controls, and typed usage provenance without exposing a native
  path;
- the first bounded `ObservationProjectionSink` family is wired for
  `runtime.usage-v2`. It independently validates each complete value-derived
  revision, rejects a whole decoded record before reducer mutation on invalid
  evidence, retains at most the configured number of response entities, and
  duplicates no retained native bytes. Equal current revisions are silent;
  corrections emit one deterministic event ID derived from the semantic
  reference plus canonical source occurrence, so `A -> B -> A` reuses `A`'s
  semantic reference but delivers the second `A` under a distinct event ID.
  A generation reset reaches the projection sink first, then deterministically
  retracts every old-generation response before corrected replay. Source
  deletion likewise emits its lifecycle control before deterministic
  deletion-owned retractions, so disappearance cannot leave usage state stale;
- the usage-v2 reducer can materialize a family-versioned bootstrap or
  correction replacement snapshot containing exactly one latest revision per
  retained response. Canonical fact ordering makes output independent of
  admission order, and its versioned semantic digest covers semantic IDs,
  value-derived revisions, stable source occurrence provenance, and current
  entity count while excluding delivery phase, observation time, and local
  runtime IDs. Empty replacement is explicit and source deletion removes the
  entity before the snapshot is built;
- a second bounded, post-reducer delivery lane keeps semantic events and source
  lifecycle controls in independent capacity domains while retaining one
  deterministic total offer order. Projected batches enter it all-or-nothing,
  semantic saturation does not consume source-control capacity, and a reset
  offered with its usage retractions is drained reset-first. The offered
  boundary assigns one monotonic epoch-1 `observer_sequence` across both
  capacity domains; dequeue is the distinct delivered boundary, and neither
  implies consumer application. Queue state reports `offered_through_sequence`
  plus both lane counts without treating a semantic/control backlog as applied
  state. The integrated
  admission-to-projection-to-delivery transaction proves saturation retry
  without reducer or sequence drift, permits an exact-repeat empty batch to
  retire while delivery is full, and keeps reset plus all retractions
  indivisible;
- an immutable post-delivery mapper now turns those internal values into the
  first sanitized RFC 012D envelope shape. Every mapped value carries the
  pre-access canonical root session/reference, a complete RFC 012C actor-run
  reference, explicit actor attribution, affiliation completeness, path-free
  source occurrence, native/observed time, evidence qualification, and native
  evidence disposition. Usage routes to its canonical actor; source lifecycle
  controls route through the root only as `ScopeFallback`, never as semantic
  root attribution. Typed usage must match the observer root session and carry
  its durable-equal semantic reference, while controls must not fabricate one.
  Attachment-local object tokens and admission ordinals are stripped. A
  qualified native session claim is accepted only when its external entity
  reference equals the already-derived root. Lifecycle-owned retractions carry
  the reset/delete observation time rather than the old response time;
- the first epoch-1 bootstrap barrier now enters that same ordered delivery
  boundary as a mandatory `observer.bootstrap_complete` control. It waits for
  the admission/coverage boundary to drain, remains deliverable through the
  dedicated control lane while the semantic lane is full, and captures its own
  barrier sequence plus post-offer queue state without claiming consumer
  application. Its versioned snapshot digest covers canonical root identity,
  root presence, source/family coverage, and explicit object errors while
  excluding observation time, queue state, and attachment-local sequencing.
  It now also carries the same per-family replacement manifest and
  replacement-snapshot digest used by resync: the first entry is usage-v2's
  latest-contribution representation, completeness, entity count, and semantic
  digest. The deterministic completion ID binds that full replacement digest,
  not only coverage, while still excluding observation time. Repeated
  ready-style calls return the retained barrier without redelivery, equivalent
  replay gets the same snapshot digest/event ID, failed preflight mutates
  nothing, and later Bootstrap-phase data is rejected after completion. A
  clean bootstrap and completed resync at equal common coverage now prove equal
  family manifests and replacement digests; subsequent invalidation/start
  controls retain that full digest as their semantic baseline;
- the delivery lane now tracks its distinct delivered-through boundary and an
  explicit `Bootstrap | Valid | ResyncRequired | Resyncing` continuity state.
  An explicit watcher/transport/consumer continuity-loss signal on a valid
  epoch clears only not-yet-delivered ordinary backlog across both capacity
  lanes, accounts
  what was superseded, and installs one root-bound sticky
  `observer.resync_required` as the next deliverable control. Its payload names
  the invalid epoch, last contiguous delivered sequence, bootstrap baseline
  digest, and reason; its deterministic ID excludes delivery speed, discarded
  counts, observation time, and sequence. Repeated signals cannot redeliver or
  replace the first control, cross-root reuse fails, all later ordinary offers
  return `ResyncRequired`, and invalidated epochs cannot publish a watermark or
  re-offer bootstrap completion. Semantic/control capacity pressure by itself
  remains retryable backpressure and never invokes this path;
- only after that sticky invalidation control is delivered may the same bound
  root begin a full-snapshot replacement. Starting replacement increments the
  scope epoch exactly once, retains the attachment-wide monotonic observer
  sequence, and installs `observer.resync_started` as the first new-epoch
  control before any snapshot value. The control ties old and new epochs to the
  delivered invalidation sequence, baseline digest, reason, and explicit
  `FullSnapshot` mode; its deterministic ID excludes observation time and
  attachment-local sequence. Repeated start calls return the same control,
  cross-root calls fail, and the new epoch accepts only `Correction`-phase
  traffic. Live and bootstrap traffic cannot leak into replacement. Atomic
  whole-scope staging/swap remains subsequent D3 work;
- replacement replay now reduces into a distinct empty, epoch-bound projection
  stage instead of the still-visible active reducer. The ordinary offered path
  rejects active-reducer mutation while continuity is `Resyncing`; the stage
  normalizes replay to `Correction`, consumes it without publishing transient
  revisions, and freezes only after its admission lane drains. Its bounded
  snapshot publisher emits the canonical stable-order usage replacement one
  latest revision per response, one retry-safe queue admission at a time, so a
  repeated/evolving native response cannot become duplicate replacement state.
  The old reducer remains byte-for-byte unchanged through publication, and
  total observer sequencing keeps `observer.resync_started` ahead of every
  snapshot entity;
- the isolated stage now completes its first family-qualified replacement
  protocol for `runtime.usage-v2`. The barrier manifest binds the declared
  family/version, latest-contribution representation, source-derived
  completeness, entity count, and semantic digest. A separate deterministic
  replacement-snapshot digest binds that manifest to root presence, common
  source coverage, membership, and explicit object errors while excluding
  epoch, observer sequence, delivery phase, and observation time. Completion
  is refused until the frozen snapshot is fully offered and the exact current
  coverage watermark is drained; a forged digest, changed state, or saturated
  control lane advances neither sequence nor reducer ownership. On successful
  `observer.resync_complete` offer, continuity becomes `Valid` and the staged
  projection replaces the active projection through an infallible swap.
  Repeated completion returns the retained barrier without redelivery, and a
  later invalidation uses its coverage snapshot as the new baseline. This is
  still an internal projection-level seam: complete multi-family manifests and
  atomic composition of replacement discovery, object/cursor/decoder state,
  and coverage membership remain subsequent D3 work;
- continuity loss during an incomplete replacement now invalidates that
  replacement instead of returning the former provisional `ResyncAlreadyActive`
  error. Once `observer.resync_started` itself has crossed the delivered
  boundary, re-overflow clears all not-yet-delivered correction snapshot state,
  emits a new sticky `observer.resync_required` for the current epoch, retains
  the last valid snapshot baseline, and leaves the old active reducer
  unchanged. The abandoned stage becomes unusable immediately and remains
  epoch-mismatched after the new invalidation is delivered and a monotonically
  fresh epoch starts. A signal received before `resync_started` delivery is
  rejected without mutation so the owning facade can retain it until the
  ordering dependency drains; whole-scope watcher/failure orchestration remains
  open;
- the append source component now has an isolated full-snapshot replacement
  primitive for cursor, partial-record, presence, decoder, and admission-token
  state. A replacement is lineage-bound to one active object and scope epoch,
  starts with no copied cursor or decoder state, preserves the exact authorized
  relation and source identity, and classifies every replay batch as
  `Correction`. Forking freezes the active object before another source access;
  a re-overflow may link it to only a strictly newer replacement epoch. It
  cannot be forked from a bootstrap-incomplete, pending, replacement, or
  retired object. Activation prevalidation requires the full bounded replay to
  drain and the active object's current epoch/token link to name that exact
  stage; an abandoned, superseded, or wrong-epoch stage leaves the active
  cursor/decoder state unchanged, while successful swap retires the old object
  and makes later access fail before reservation;
- the scoped host now composes those source components with their admission/
  offered-coverage lane and semantic reducer as one epoch-owned state. Binding
  epoch 1 re-verifies exact relation/source membership and both the source and
  family/digest content of the already-offered bootstrap barrier. Opening a
  replacement freezes every active append object, creates empty cursor/decoder,
  coverage, and reducer state, and tolerates a bounded old-epoch admission
  backlog that was invalidated before it could be offered. Completion checks
  every current epoch/token lineage, exact relation-to-source membership,
  drained replacement coverage, family manifest, and snapshot digest before
  offering `observer.resync_complete`; control pressure changes none of the
  three active components. After a successful offer, object state, the offered
  coverage lane, and reducers transfer without another fallible operation, and
  the invalid old admission backlog is dropped. Re-overflow retains the last
  valid active epoch even though the continuity chain advances through an
  incomplete epoch; a strictly newer stage supersedes the frozen link and the
  stale stage remains unusable. The conformance path covers attach-before-root,
  root creation during correction, unoffered old-epoch input, failed completion
  preflight, idempotent success, active watermark parity, and re-overflow;
- every admitted append observation now stages one bounded RFC 012A `Decode`
  coverage update using the same source-instance/stream/object coordinates and
  append-cursor representation as durable ingestion. Stable initial absence,
  later deletion, transient source failure, bounded backlog, driver quarantine,
  and decoder quarantine remain distinguishable through common point/absence,
  status, completeness, and explicit-error fields. The source cursor may commit
  at admission, but reportable coverage remains at its prior state until the
  observation's last lifecycle/data frame successfully crosses the atomic
  offered boundary; a rejected delivery offer cannot advance it. An event-free
  missing read or semantic no-op may advance coverage at the current observer
  sequence, and bounded bootstrap remains `Partial` until its final batch is
  offered. Retained coverage membership has its own explicit object bound and
  repeated no-event updates at one pending boundary coalesce by stable source
  identity;
- coverage membership now has one store-free canonical streaming encoder used
  by both durable usage-v2 coverage and scoped observation, preserving the
  existing usage-v2 byte contract while separating other domains. Once the
  admission lane is drained, a crate-private watermark core groups offered
  objects by common domain/source instance, derives declaration-bound
  `SourceCoverageSet`s, flattens explicit object errors, and pairs them with the
  exact scope epoch, offered-through sequence, and delivery queue state.
  `Decode` is implicit; a fact-family set exists only when the exact object
  declares it, contract negotiation selects the same version, and the common
  reducer implements it. Projection-pack coverage and adapter-private claims
  are rejected. The first such family is `runtime.usage-v2@1`;
- an append object now binds permanently, before native access, to the exact
  host-authorized known-object relation used by its first reconciliation.
  Rebinding fails without reserving or reading, and admission rejects a second
  semantic source object that claims an already-accounted relation. Bootstrap
  and resync watermark assembly require a one-to-one match between every exact
  known-object grant supplied at attachment and the admission lane's offered
  coverage members. A reconciled missing object contributes explicit absence;
  an authorized relation that was never reconciled cannot silently disappear
  from a completion barrier. Conformance tests cover omission, duplicate
  claims, failed rebinding, and no barrier/delivery mutation on failure;
- a crate-private single-consumer drain now owns its empty bounded delivery lane
  from construction, validates and maps the next envelope before dequeue,
  permits only one application-pending envelope, and advances a distinct applied
  boundary only after an exact attachment-bound receipt is acknowledged. One
  host can open that owner exactly once; invalid limits do not consume the slot,
  and closed hosts reject it. Foreign/mismatched receipts fail without mutation,
  while retrying the latest acknowledgement is a no-op. Consumer bootstrap and
  resync readiness become visible only after their completion envelopes are
  applied, while engine barriers remain offered-bound. Explicit sequence gaps
  created by invalidation are accepted only when the skipped backlog was never
  delivered. Raw dequeue and the standalone mapper are test-only seams, so
  production code cannot bypass this boundary inside the provisional kernel;
- the host and drain now share an unforgeable attachment authority in addition
  to canonical root identity, so simultaneous observers of the same native
  session cannot substitute one another's queue or readiness barrier. A
  crate-private poll coordinator gives every logical request a local ticket,
  coalesces all requests reserved by one bounded access pass, and leaves a
  request arriving during that pass for a conservative follow-up. Completion
  requires that the fresh pass ledger attempted every exact known-object
  relation, that each relation's offered coverage carries that same pass
  identity, and that the watermark is captured at the owning drain's offered
  boundary. A raw read cannot complete against coverage retained from an older
  pass. Incomplete, failed, or dropped passes acknowledge no ticket and are
  retryable; close cancels unresolved tickets, foreign tickets/leases/drains
  fail closed, and an unchanged follow-up advances no observer sequence. The
  retained attachment-bound bootstrap barrier is also exposed through an
  internal engine-readiness probe. Poll tickets and the engine-ready boundary
  now have wakeable completion handles: a failed/dropped pass remains pending,
  successful poll completion wakes with the exact shared offered watermark,
  successful bootstrap offer wakes with the retained attachment-owned barrier,
  and close wakes unresolved handles as cancelled without overwriting an
  already-ready result. Consumer-applied bootstrap readiness remains owned by
  the drain acknowledgement boundary. Request/lease generations remain local
  flow-control coordinates and do not enter semantic identity;
- the attachment-owned consumer drain now exposes a cloneable, lost-wakeup-
  safe event notification handle keyed by the monotonically offered observer
  sequence. Every non-empty semantic/control offer, direct continuity
  invalidation, and resync start wakes waiters with the newest offered boundary;
  a semantic no-op advances neither sequence nor wake state. Drain close or
  drop wakes waiters with an explicit closed state while preserving the last
  offered boundary. The async iterator bridge can therefore check the drain,
  snapshot under the same owner lock, and sleep without a polling loop or a
  check-then-wait race;
- event, poll, engine-readiness, watcher-cancellation, and close completion
  handles now also expose executor-friendly retained-state futures. They do
  not occupy blocking runtime workers, support multiple waiters where the
  underlying contract is cloneable, and resolve correctly when completion or
  cancellation races future construction and occurs before its first poll;
- an internal async lifecycle runtime now constructs the attachment's sole
  drain before bootstrap and splits it into one non-cloneable ordered event
  owner plus a cloneable attachment handle. The event owner checks the queue
  and snapshots wake state under the same short-held lock, releases that lock
  before awaiting, treats close as end-of-stream, and preserves explicit
  application acknowledgement. The handle permits `ready()`, request-local
  `poll()`, producer offers, watcher setup, and close to run concurrently with
  event delivery. The attachment lifecycle retains the drain's weak event
  notifier, so even a direct host close wakes a pending iterator; that iterator
  closes the drain before ending and therefore satisfies the consumer side of
  the existing watcher/operation barrier. The handle now also exposes a lost-
  wakeup-safe async source-owner reservation: watcher, audit, and logical poll
  requests wake one coalesced bounded access lease; a request arriving during
  that lease remains for a follow-up, and dropping an unfinished lease releases
  access serialization before making the same target runnable again. Close and
  terminal observer failure wake a parked driver explicitly. This facade
  remains crate-private; it introduces no N-API or incomplete portable request/
  envelope export;
- a shared attachment lifecycle now accounts active access passes, direct
  decodes, and consumer delivery/application calls. Idempotent close first
  rejects new work and cancels unresolved poll tickets, then waits on a
  barrier that remains incomplete until every registered operation exits and
  the exact consumer drain acknowledges cancellation. Drain close invalidates
  its pending application receipt and discards queued, never-applied envelopes;
  host drop requests cancellation without blocking. Foreign-drain close fails
  without closing either attachment, and the internal facade path closes its
  owned drain before waiting. The lifecycle substrate is synchronous and
  store-free. Watcher tasks now acquire one non-cloneable attachment-owned
  registration before starting: close makes its cancellation signal sticky,
  wakes tasks blocked on that signal, rejects later registration, and remains
  incomplete until every awakened watcher stops its callbacks and drops the
  registration. Host drop requests the same cancellation without blocking;
- the attachment now prepares a bounded watcher callback sink and lifecycle
  registration before backend installation, then requires explicit successful
  installation before the initial exact-scope access pass can start. Callbacks
  arriving during installation, initial scan, or reconciliation coalesce
  behind the bootstrap barrier; capacity pressure escalates to one full-source-
  instance reconcile instead of dropping a signal. Every initial/reconcile
  pass must attempt and offer coverage for each exact granted relation from
  that same pass. Dropped or incomplete initial passes retain captured hints,
  and abandoned reconciliation passes restore their bounded hint batch. The
  final empty-hint check, bootstrap-control offer, and transition to live share
  one ordering lock, so a racing callback either blocks the barrier or becomes
  a request-local live poll ticket after it. Producer offers also verify the
  attachment-owned consumer drain;
- the live exact-scope poll coordinator now has a concrete append-source pass
  executor. Before native mutation it validates the reserved lease, owning
  drain, attachment-bound active epoch, valid continuity, and exactly one
  redacted request per authorized relation. It visits relations in canonical
  order, performs bounded revalidation, decode, admission, and offer, and only
  then completes tickets with coverage produced by that same access pass.
  Decode, admission, access, and delivery failures acknowledge no ticket; the
  unfinished lease requeues for caller-owned classification and recovery. A
  retry first flushes any frame whose cursor was already committed to the
  admission lane, then takes a fresh exact pass; the
  bounded deletion-under-control-pressure conformance case proves that neither
  a committed cursor nor older offered coverage can acknowledge the poll. The
  active source/coverage/reducer epoch now carries the creating attachment's
  unforgeable authority, so a second observer of the same canonical root
  cannot substitute it;
- the async attachment handle exposes that pass executor without holding its
  consumer-drain mutex across native access or decode. Only preflight, bounded
  delivery offers, and final watermark publication enter the short-held lock;
  one source owner retains exclusive mutable epoch state while `poll()` and
  ordered event delivery remain concurrent. The attach-before-root, later
  creation path proves one shared offered watermark and matching lifecycle
  event through this bridge;
- the active epoch can now transfer into one non-cloneable, attachment-
  registered async source owner with an owned exact binding for every current
  append relation. Construction revalidates the authorized declaration,
  complete relation set, access bounds, and the opaque access object/parent/
  depth plus source/stream/object/media lineage permanently established by the
  first authorized bootstrap access. Failed construction returns the intact
  epoch and redacted bindings; native identity values and origin IDs never
  enter `Debug`. Each attempt creates only short-lived borrowed requests,
  refreshes observation time, reserves the coalesced poll pass, and retains
  exclusive source/coverage/reducer state. The owner holds a close-barrier
  operation for its whole lifetime and returns the intact epoch after
  cancellation or classified failure;
- that source owner now also waits on the attachment's retained offered-event
  signal while parked for poll work or a relation-local retry. A direct
  `observer.resync_required` therefore wakes it without another poll, and a
  resync racing lease reservation or a synchronous source pass is classified
  as typed continuity invalidation rather than a generic pass failure. The
  result records the owned and observed epochs/continuity and retains the exact
  invalidation control when it still names that owner. The stopped handoff
  preserves the intact old epoch, exact redacted relation bindings, and retry
  policy after releasing its close-barrier operation. Attachment-owned,
  lifecycle-counted resync entry points then reject rebinding that stale epoch,
  open and atomically complete the whole-scope replacement through the sole
  consumer lane, and permit the returned bindings/policy to bind a new owner
  only after the replacement epoch is valid. The integrated conformance path
  proves a parked epoch-1 owner performs no later access, epoch 1 cannot
  rebind, epoch 2 replaces source/coverage/reducer state, and its newly bound
  owner services a fresh epoch-2 poll. Automatic watcher-to-replacement replay
  orchestration and portable poll behavior during that handoff remain open;
- bounded delivery now has a separate retained producer-capacity generation.
  Dequeue, explicit epoch supersession, terminal failure, and drain close wake
  a parked owner without a check-then-sleep race. Semantic, retained-native,
  and source-control queue-full outcomes wait for that exact capacity owner and
  never manufacture continuity loss; batch/admission/configuration failures do
  not masquerade as recoverable pressure. Source/decode failures are retained
  per exact relation with stable redacted codes and provenance. Retryable
  failures use capped, cancellation-prioritized exponential delay and become
  typed exhaustion at the configured attempt ceiling; nonretryable and
  exhausted relations remain terminal locally without stopping healthy
  siblings. Every pass republishes their explicit error coverage, successful
  newer evidence clears only the matching relation state, and close cancels a
  parked retry without another access. The deletion-under-control-pressure and
  two-object async matrices prove no retry spin, committed-admission flush
  before fresh access, healthy-sibling progress, request-local poll completion,
  terminal isolation, and close acknowledgement;
- a concrete attachment-owned `notify` watcher now derives consolidated
  physical anchors no broader than the host-authorized access roots, rejects
  missing or filesystem-wide roots, and filters unrelated/access-only paths
  before they reach scope scheduling. Exact-object changes stay object-scoped;
  ancestor membership changes, empty/rescan events, and backend failures
  conservatively escalate to the source instance with their distinct dirty
  reasons. The coordinator and callback exist before backend construction and
  all anchor registration, so synchronous install callbacks are retained
  behind the initial scan barrier. Partial registration failure drops the
  backend and releases the retry slot. Normal shutdown drops the native backend
  before its non-cloneable watcher registration; unexpected owner drop closes
  the attachment rather than leaving an unwatched live observer. A retained
  async signal reports callback/audit/replacement generations, the successful
  backend installation generation, replacement-in-progress state, and
  backend/routing failure. The same owner can explicitly schedule one bounded
  full-instance audit and can replace a failed backend without releasing its
  attachment registration. Replacement drops the old callback owner first;
  failed construction or anchor registration leaves a retryable degraded
  state without advancing the backend generation, while success advances that
  generation and schedules mandatory full-instance recovery reconciliation.
  A validated async owner loop now schedules audits only after the configured
  quiet period, skips missed timer bursts, applies capped exponential backend-
  replacement delay for a finite attempt count, and gives cancellation
  priority at every wait. Successful replacement resumes auditing only after
  scheduling full-instance recovery; routing failure or exhausted replacement
  attempts deliver the retained terminal observer failure. The watcher then
  stops its backend/registration without implicitly closing the event drain,
  so the consumer can apply that failure before owning the independent close
  barrier. Default timing values remain provisional internal policy, not
  promoted performance gates;
- one non-spawning structured async owner now supervises that native watcher
  together with the exact current source epoch. Pairing requires the same
  attachment, a live watcher coordinator, and a valid current source owner;
  failed pairing returns both non-cloneable authorities intact. Cancellation
  stops and releases both halves, while watcher routing/recovery failure
  terminalizes the attachment and then waits for the source owner to observe
  the same failure. An unexpected source-owner failure likewise delivers one
  retained terminal observer failure before releasing the watcher. Intentional
  continuity invalidation is different: it stops only the stale source half
  and returns a structured handoff that still owns the native backend,
  callback authority, recovery policy, old epoch, exact redacted bindings, and
  source retry policy. Native callbacks remain routable during replacement and
  accumulate ordinary poll demand without reading the invalid epoch. The
  integrated conformance path carries that same watcher across a full epoch-2
  replacement, re-pairs it with the preserved source authority, services both
  handoff-time and fresh epoch-2 poll demand, and proves permanent backend
  failure stops the re-paired source, releases the backend exactly once, and
  leaves the ordered terminal control drainable before close. Automatic
  replacement replay/rebind orchestration, remaining portable-host wiring,
  and policy calibration remain open;
- one pass is active at a time, a later pass receives fresh bounds, close is
  idempotent, and the frozen access report excludes paths, identity values, and
  content; and
- the architecture checker forbids store/query/N-API/concrete-adapter imports
  and premature public export from this provisional composition root.

D1 remains `In progress`: multi-object discovery/cursor orchestration,
declared relation-backed decoder dependency access, built-in
canonical fact-revision adoption beyond the current runtime families, scoped
reducers beyond usage-v2,
coverage-complete durable query exposure, affiliation/actor enrichment and
envelope variants beyond the current usage/source-lifecycle families, the
public N-API/SDK iterator transport over the internal async lifecycle runtime
and complete scope coverage,
dynamic/discovered scope membership beyond the attachment's current exact
known-object grants and family coverage beyond usage-v2,
complete multi-family replacement manifests, whole-scope discovery and source
state beyond the current exact append-object set, automatic watcher/source-
owner replacement replay and rebind orchestration, remaining
portable-host wiring and policy calibration, plus re-overflow orchestration,
artifact mediation and the public portable close transport,
the trusted native version-probe/identity-input drivers, and the complete
public request are not yet implemented. The internal offered and applied
boundaries are now transactional, but they cannot become a public watermark and
consumer-ready helper until complete scope-membership/barrier coverage, portable
resync completion, portable watcher cancellation, and the negotiated lifecycle
surface are defined. The usage-v2 sink and delivery lane remain crate-private
until those envelope/lifecycle contracts and the negotiated portable surface
exist.

### D2. Claude scope composition

Compose the root plus existing/future standard children, workflow runs/journals
and children, referenced team/member/inbox objects, relevant tasks/plans,
presence, and policy-allowed artifact locators through RFC 012A scope
primitives.

Hooks remain in Chopsticks as root lifecycle and immediate-poll signals during
this package.

### D3. Identity, control, and resync

Implement:

- deterministic native-derived/control delivery IDs plus mandatory common
  semantic revision references for typed native-derived events;
- epoch-1 bootstrap, explicit admitted/delivered/applied boundaries, and safe
  event-drain/readiness ordering;
- semantic queue plus dedicated lifecycle/continuity control lane;
- partial-record and reset-before-correction behavior;
- per-observer isolated budgets and starvation-bounded scheduling;
- sticky continuity loss;
- common source/family coverage in poll/bootstrap/resync watermarks without
  commit/observer/native clock conflation;
- RFC 012C per-family replacement manifests/digests; and
- full-snapshot new-epoch staging plus atomic replacement barrier.

Gate compares clean-bootstrap and resync replacement digests per RFC 012C family
at the same RFC 012A coverage vector, including disappeared entities, explicit
unavailable coverage, and unchanged event IDs.

Current landing status (2026-08-17): D3 is `In progress`. Native-derived
usage-v2 upsert/retraction IDs are deterministic and include the selected event
and semantic-reference contract versions, typed semantic revision and stable
source occurrence. Source create/delete and reset controls now also have
mandatory versioned event IDs derived from the stable adapter/source/stream/
object coordinate and lifecycle revision; attachment-local object tokens,
admission/delivery ordinals, phase, and timing cannot perturb replay identity.
All projected variants expose one uniform event-ID, optional semantic-reference,
phase, and source-coordinate seam for the future envelope mapper. The bounded
usage reducer now produces the first
family-versioned replacement snapshot/digest with stable ordering, phase- and
observation-time-independent identity, empty-state removal, and equal
bootstrap/correction semantic digests and event IDs at equal state. Bootstrap
and resync barriers now use that same family manifest and full replacement
digest, with a parity test at equal common coverage. The internal
object-level `Decode` coverage checkpoint is also tied to that exact offered
transaction, so delivery pressure cannot overstate its cursor and semantic
no-ops can advance coverage without inventing a sequence. A drained watermark
core now emits common Decode and eligible usage-v2 `SourceCoverageSet`s at that
sequence, binds every set to the pre-access resolved root session, and carries
that root's canonical session/external-reference/root-run tuple. It also proves
one-to-one coverage of every exact known-object grant supplied to the current
attachment: unobserved relations and duplicate relation claims fail closed.
Exactly one grant must be tagged as the scope root. Bootstrap validates the
completed append-object set and derives root presence from that object; resync
derives it from the validated staged root. Neither barrier accepts a caller-
asserted presence bit, and bootstrap/source-state disagreement fails closed.
This is not yet proof of relationships or descendants that the future scope
orchestrator has not discovered and granted, nor is it the portable poll/
bootstrap/resync contract. The public N-API/SDK iterator transport, whole-scope
dynamic discovered-scope source/family sets, complete multi-family and D-owned
manifests, non-append source participants, and multi-observer scheduling/
starvation isolation remain unimplemented. Internally,
reducer mutation, admitted-frame release, bounded delivery admission, and
eligible coverage promotion now share one retry-safe offered transaction:
exact projected capacity is checked before mutation, queue pressure changes no
reducer, coverage, or sequence state, and reset/control plus semantic
retractions enter delivery as one ordered batch. Therefore the D3 and X0 gates
remain open for the still-missing public lifecycle rather than this internal
atomicity seam. Delivered internal values now carry the selected event-contract
version, epoch, observer sequence, mandatory event ID, optional semantic
revision, phase, stable source coordinate, and typed event. The immutable
mapper binds those values to the resolved root, emits RFC 012C actor and
unknown-affiliation context, strips internal ordinals, preserves source
occurrence and observed/native time, and distinguishes native records, common
reducer corrections, and engine controls. It rejects cross-root typed events
and mismatched native-session claims. This freezes the current
usage/source-lifecycle and initial resync envelope vocabulary but is not yet the
public multiplexer/facade, complete portable resync/scope manifest, complete
actor-affiliation reducer, or a portable consumer-ready helper. The internal
single-consumer drain now owns the bounded delivery lane from host construction,
distinguishes delivery from application with an exact attachment-bound receipt,
blocks a second dequeue while application is pending, rejects
cross-attachment/mismatched acknowledgements, tolerates only explicit
invalidation sequence gaps, and exposes engine versus consumer bootstrap/resync
readiness separately. An attachment authority now prevents a second observer,
even one with the same canonical root, from substituting its drain. The
internal poll coordinator coalesces requests present at pass reservation,
requires a fresh all-known-relation access ledger plus the owning drain's
offered watermark before completion, conservatively schedules requests that
arrive in flight for a follow-up pass, retries dropped/failed passes without
acknowledgement, and cancels unresolved tickets on close. An unchanged pass
does not advance observer sequence. Its attachment lifecycle counts active
passes, direct decodes, and consumer calls; close completes only after those
guards exit and the exact drain discards pending/queued delivery and
acknowledges cancellation. Epoch 1 now has an
ordered, snapshot-identified bootstrap-completion control and retained
idempotent barrier at the offered boundary; it remains internal until
multi-object scope orchestration and the portable drain/ready surface land. A
valid epoch can now
transition exactly once to a sticky root-bound `observer.resync_required`
state: the lane preserves the last delivered boundary, explicitly supersedes
undelivered backlog, prioritizes the control, rejects further ordinary offers,
and prevents invalid watermark publication. Once that invalidation control is
delivered, a root-bound, idempotent start advances exactly one epoch and offers
`observer.resync_started` first while preserving the attachment-wide observer
sequence. Its deterministic identity binds the old/new epochs, invalidation,
baseline digest, reason, and full-snapshot mode without timing or local
sequence; only correction traffic may follow in the replacement epoch. Family
snapshot staging is now isolated from the active reducer: replay is reduced
silently into an empty epoch-bound sink and its frozen usage snapshot publishes
only one latest correction per response through bounded retry-safe offers.
The first usage-v2 manifest and deterministic replacement digest are validated
against the exact common coverage watermark before ordered
`observer.resync_complete`; control pressure is retry-safe, and successful offer
atomically activates the staged projection. That activation now belongs to one
whole-epoch host transaction: exact append cursor/partial-line/presence/decoder
state and its offered-coverage lane are staged from empty beside the reducer,
then all three transfer only after the same successful completion-control
offer. Old admitted-but-unoffered input is superseded and dropped; completion
pressure preserves all active state; and re-overflow keeps the last valid epoch
while a newer stage supersedes the failed continuity epoch. Complete
multi-family and D-owned manifests, dynamic whole-scope discovery and
non-append participants, the public transport over the internal async applied
runtime, and watcher policy calibration/concrete source-pass and portable-host
integration remain open. The dedicated control lane now also carries one
deterministic, attachment-terminal `observer.failed` control before or after
bootstrap. The first cause wins idempotently, explicitly accounts for and
supersedes all undelivered semantic/source-control backlog and incomplete
resync controls, rejects later ordinary offers and epoch transitions, and
prevents failed-epoch watermarks. Pending poll and engine-ready waiters wake
with the same retained failure rather than hanging or claiming cancellation;
the ordered event drain still requires explicit application acknowledgement,
and close remains an independent resource barrier. The bounded native owner
now automatically connects audit, backend replacement, capped retry/backoff,
routing failure, and retry exhaustion to that control while preserving it for
delivery before close. The source owner now classifies source and decode
failure independently for every exact relation, publishes a typed
`source.object_error` control with redacted stable provenance, retains the
error in same-pass coverage, and schedules only genuinely transient relations
under bounded backoff. A failed relation cannot delay a healthy sibling;
retry exhaustion or a nonretryable outcome is terminal only for that relation,
while later successful evidence clears its retained failure. The conformance
matrix covers mixed healthy/retryable/terminal relations, deterministic retry
ceilings, no-spin idle behavior, current error coverage, and cancellation.
Continuity invalidation now independently wakes a parked source owner and
returns its exact epoch/binding/policy handoff before replacement starts; a
stale owner cannot read or bind into the next epoch, while a completed
whole-scope replacement can bind the preserved authority to a fresh owner and
service an epoch-local poll. A structured, non-detached supervisor now retains
the native watcher across that handoff, permits callbacks to queue demand while
the invalid epoch has no reader, re-pairs after replacement, and co-stops both
owners on cancellation or terminal watcher/source failure. Automatic
replacement replay and rebind orchestration remains later D3 composition
rather than an implicit background task.

### D4. SDK and Chopsticks migration

Add a feature-flagged Chopsticks path beside `watchSessionTranscript`:

- map state by observer attachment/scope epoch;
- stage/swap resync rather than merge;
- preserve native hooks as poll hints;
- preserve Spaghetti semantic revision references and source coverage through
  Chopsticks normalization;
- establish bootstrap/correction usage baselines;
- exercise all RFC 012C fact families and artifacts; and
- fail observation independently from the agent process.

Shadow on sanitized/frozen scenarios. Do not remove the compatibility tail or
update the pinned SDK until a released Spaghetti observer passes the full
matrix.

### D5. Performance calibration

Measure attach, bootstrap barrier, hook/poll-to-admission,
admission-to-delivery under active demand, helper application through barrier,
queue/control high-water, overflow, resync barrier, close, access count,
retained bytes, and several simultaneous scopes including a deliberately slow
consumer. Propose a child gate amendment for any numeric release ceiling.

## 10. Integration packages

### X0. RFC 011 delta compatibility

Before A/B/C/D behavior replaces an RFC 011 contract, encode the umbrella delta
ledger as fixtures and architecture checks:

- retained durable ownership, transaction, outbox, query-purity, and per-object
  ordering cases remain unchanged;
- durable publish-after-commit and scoped pre-commit delivery are tested as
  topology-specific contracts;
- old unrestricted discovery/custom-producer paths fail the RFC 012A declaration
  checker;
- legacy `UsageFact`/quality/readiness representations have explicit conversion,
  shadow, or rejection behavior;
- RFC 011 aggregate-facing history/runtime pagination is bound to one snapshot,
  and `at_commit_seq` is not treated as a native/observer watermark; and
- rollback never interprets usage-v2 or scoped-observer state as the old
  contract silently.

Gate: every row in the umbrella section 4.4 ledger has a retained behavior test,
versioned migration fixture, or compile/architecture rejection proving the new
owner. An unclassified RFC 011 conflict blocks implementation.

The executable ledger and structural validator are implemented in
[`rfc012-rfc011-delta.json`](../../scripts/architecture/rfc012-rfc011-delta.json)
and
[`check_rfc012_delta.py`](../../scripts/architecture/check_rfc012_delta.py).
Normal validation rejects malformed or unclassified contracts. Release mode
uses `--require-complete` and additionally rejects every remaining `planned`
evidence item.

The [2026-08-17 evidence audit](./012-rfc011-delta-evidence-audit-2026-08-17.md)
reproduced all 12 previously planned entries and promoted five without
broadening their claims: common reconciliation ordering, adapter authority
boundaries, D3 scoped barrier atomicity, usage-v2 oracle/switch/rollback, and
fixed-snapshot expiration. The executable ledger now has six fully evidenced
contracts and seven with planned evidence. X0 remains `In progress`; durable/
scoped decoder parity, the public scoped migration, exhaustive declared-
mechanic classification, catalog readiness, full common-fact and identity
migration, and the durable/live consumer handoff retain concrete gaps.

### X1. Search and finalization separation

Compare on identical frozen inputs:

1. deferred one-shot FTS after history;
2. incremental FTS maintenance after catalog; and
3. bounded/chunked finalization with controlled reader quiescence.

Report history/search completion, query p99 during build, writer/WAL/checkpoint
behavior, RSS, total convergence, and crash recovery. Search remains unavailable
until complete and validated regardless of chosen strategy.

### X2. Diagnostic disposition and aggregation

First reclassify known ignored records through RFC 012A dispositions. Then
aggregate genuine repeated diagnostics by source/reason/family while retaining
count, first/last provenance, sample identity, and bounded examples. Gate on
fact/query parity and measured row/database reduction.

### X3. Physical extraction

Extract physical crates/modules only after logical dependency tests are stable.
Extraction must preserve public N-API/SDK behavior, source/decoder fixtures,
durable query digests, observer differential tests, and performance. No
physical move may introduce a forbidden dependency edge through feature flags.

### X4. Promotion and drift

Promote current-agent support releases, run version/drift classification in
production, collect cold/warm/observer telemetry on maintainer-owned corpora,
and keep rollback flags through one compatible release cycle. Default-on
requires every owning child gate for that product path.

## 11. Cross-workstream validation matrix

| Dimension         | Required cases                                                                                           |
| ----------------- | -------------------------------------------------------------------------------------------------------- |
| RFC 011 delta     | every retained/amended/superseded contract plus legacy migration/rollback                                |
| Versions          | exact/range/unverified/incompatible, decoder-state upgrade, compatible selection/rejection               |
| Base identity     | catalog/durable/scoped entity keys, attach-before-create root key, aliases remain base-stable            |
| External refs     | restart-stable source/entity refs, non-reuse, live/tombstone/supersession/unknown resolution             |
| Tier composition  | head/prefix + continuation versus full-only records, facts, decoder state, final projections             |
| Source state      | empty, small, many, large, active append, partial, replace, truncate, delete/recreate                    |
| Adapter mix       | each alone, reordered registration, all current, unavailable source, multiple roots                      |
| Catalog identity  | transcript/index/nested/summary-only, move, alias, conflict, evidence loss, tombstone                    |
| Project evidence  | index/cwd/directory/header/declared ancestor, conflicts, retraction, locator disclosure                  |
| Readiness         | every transition, coverage-plan change, epoch/attempt, degradation/recovery, prior-plan state            |
| Pagination        | concurrent commits between pages, stable keysets, changed filter, expiration                             |
| Usage             | repeat, evolving/downward snapshot, unknown/zero bucket, affiliation regroup, reset/retract              |
| Runtime semantics | every family identity/reducer/retraction/replacement, root/children, typed interactions                  |
| Topology parity   | identical fact and per-family reduced/replacement digests at the same source/family coverage vector      |
| Aggregation       | shared semantic refs, fixed snapshot, complete membership sets, coverage comparison, overlay retirement  |
| Observer          | attach-before-create, watch-before-scan, bootstrap/live/correction, deterministic IDs                    |
| Continuity        | semantic saturation, dedicated controls, disappeared entity, failed/re-overflowed resync                 |
| Fairness          | live flood, catalog/backfill load, three scoped observers with one slow consumer                         |
| Artifacts         | allowed, denied, missing, over-limit, changed generation, malformed, out-of-scope                        |
| Query             | purity, snapshot pagination, cancellation, invalidation, maintenance, search pending                     |
| Recovery          | crash before/after every commit/barrier/migration, dropped hint, source changes while down               |
| Security          | sanitizer, path/symlink escape, native-ID/locator disclosure, SQL/key bounds, root access, raw retention |

## 12. Benchmark report contract

Every performance report includes:

- source fixture and support-release digests;
- machine/OS/filesystem/storage/CPU/RAM and power state;
- cold/warm cache method, limitations, repetitions, and statistical summary;
- bytes and logical records by adapter/source/tier;
- tier-overlap strategy plus full-only/composed identity and state digests;
- external entity and semantic revision reference digests;
- facts, commits, changed rows, queue high-water, WAL, checkpoints;
- catalog coverage-plan/readiness snapshot and identity digests;
- observer access trace, watch count, object/generation coverage, semantic and
  control high-water, epochs, overflow/resync, cancellation;
- selected public contract versions, event-ID, and per-family replacement-state
  digests;
- durable/observer source-coverage vectors and comparison outcomes;
- usage response/actor/session/affiliation values and quality/coverage digests;
- shell, first page, complete catalog, selected hydration, search, and final
  convergence timestamps;
- query distributions during each background phase;
- RSS/database/WAL/diagnostic allocation; and
- every semantic contract and migration version.

Reports that change evidence, retention, durability, or diagnostic policy
between comparisons are diagnostic only and cannot accept an optimization or
promote a release ceiling.

## 13. Feature flags and rollback

Migration retains explicit choices:

```text
blocking full-search startup    current compatibility path
progressive catalog startup     RFC 012B path
legacy usage query              pre-usage-v2 path
usage-v2 query                  RFC 012C path
single-file transcript tail     downstream compatibility path
scoped session observer         RFC 012D path
```

Rollback switches a startup/query/observer selection, not database authority.
The usage migration retains legacy rows through its window. The observer path
is ephemeral. Progressive and blocking startup converge to the same accepted
durable state except for the explicitly versioned usage-v2 correction.

## 14. Immediate next work

The next execution order is:

1. close X0 with executable compatibility evidence for every remaining planned
   RFC 011 delta;
2. finish A1/A2/A3 promotion gates, including the remaining fact-family,
   tier/view, scope-composition, and current-agent evidence, without promoting
   any candidate early;
3. review and ratify RFC 012B identity/readiness/pagination and RFC 012C
   qualified runtime semantics against the implementation evidence already
   landed;
4. implement B1-B3 catalog identity, bounded compositions, durable readiness,
   and snapshot pagination while completing the remaining C1-C3 compatibility
   report and semantic-contract work;
5. build the portable async lifecycle facade over the landed watcher-before-
   scan coordinator while finishing D2 dynamic Claude scope and declared
   decoder dependencies;
6. complete D3 multi-family and D-owned replacement manifests, non-append
   participants, concrete watcher source-pass/portable-host wiring, policy
   calibration, and multi-observer fairness;
7. run A4 only after the initial catalog/query and public scoped-observer seams
   both exist, proving a fourth agent needs no common-engine API changes;
8. implement B4/C4/D4 UI, typed-consumer, Chopsticks, and reference
   durable/live reconciliation shadows with explicit rollback;
9. finish X1-X3 search/finalization, diagnostic aggregation, and physical
   extraction after the logical boundaries pass; and
10. calibrate B5/D5 numeric gates, amend the owning child RFCs, close X4 drift
    and promotion controls, then consider default switches.

No step may use a temporary renderer catalog, private adapter tail, arbitrary
scope search, second database, or duplicated native decoder to shorten the
critical path.
