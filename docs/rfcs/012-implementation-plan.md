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
| B1. Catalog identity/readiness contracts | 012B                | Not started | Rust/N-API/TS fixtures and transition table tests              |
| B2. Bounded catalog source compositions  | 012B                | Not started | three adapter catalog identity digest parity                   |
| B3. Durable catalog/query snapshots      | 012B                | Not started | atomic pack plus snapshot pagination conformance               |
| B4. Progressive host and UX              | 012B                | Not started | cold/warm UI topology and migration tests                      |
| B5. Catalog performance calibration      | 012B                | Not started | reproducible gate-amendment report                             |
| C1. Runtime semantic contracts           | 012C                | Not started | actor/usage/state/interaction serialization fixtures           |
| C2. Usage-v2 shadow projection           | 012C                | Not started | independent qualified-bucket oracle parity                     |
| C3. Durable usage migration              | 012C                | Not started | transactional switch and rollback tests                        |
| C4. Runtime semantic downstream suite    | 012C                | Not started | typed consumers plus durable/live merge without native parsing |
| D1. Store-free observer kernel           | 012D                | In progress | attach/bootstrap/poll/close, no SQLite/global scan             |
| D2. Claude scope composition             | 012D                | Not started | root/current/future actor and sidecar conformance              |
| D3. Control lane and epoch replacement   | 012D                | Not started | overflow/disappearance/duplicate/fairness matrix               |
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

Current landing status (2026-08-15):

- implemented the parallel Rust RFC 012A v1 semantic model for qualified
  values, canonical source/entity/record/fact/revision keys, external entity
  references, native identity claims, semantic revision references, coverage
  points/sets, and the conservative coverage comparator;
- froze one Rust-produced fixture consumed by portable TypeScript validation
  and comparison tests;
- added an architecture ratchet preventing the base semantic module from
  importing source, adapter, store, query, delivery, N-API, or concrete-agent
  layers; and
- retained A1 as `In progress`: tier/view compositionality, adoption of
  canonical `FactRevisionId`/`SemanticRevisionRef` values by actual durable and
  scoped fact families, N-API fixture parity, and full-only versus composed
  reducer digests remain.

The repository-wide native-surface validator also discovered current Claude
drift that predates this model slice: `bridge-session` records now include
`ownerAccountUuid`/`ownerOrganizationUuid`, and an active-session document
includes `nameSince`. No native values were copied into RFC evidence. A3 must
classify their semantics, disclosure policy, and fixture representation before
the next Claude support release; merely adding permissive fields to make the
validator green would not satisfy RFC 012A.

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

Current landing status (2026-08-16):

- added strict v1 JSON Schemas for ADS, source declarations, restricted scope
  programs, evidence manifests, conformance manifests, and support-release
  entries under `agent-support/schemas/`;
- added a dependency-free repository checker that resolves claim references,
  verifies SHA-256 bindings, rejects path escape and unbounded recursive
  declarations, detects duplicate semantic ownership/unclassified families,
  scans every fixture, and enforces stronger promotion-only invariants;
- added a deterministic JSON/JSONL sanitizer that preserves structural shape
  and referential equality without committing hashes of native values, plus a
  prohibited-field scanner for paths, identifiers, text, secrets, and common
  credential forms;
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
  current `FactBatch` output still carries the RFC 011 store-oriented `FactId`,
  so public semantic projection remains gated on actual canonical
  `FactRevisionId`/`SemanticRevisionRef` adoption;
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

Current landing status (2026-08-15):

- added non-selectable Claude, Codex, and Grok candidate directories containing
  ADS, current bounded durable source declarations, partial restricted scope
  programs, claim-addressable evidence, conformance manifests, and digest-bound
  support-ledger entries;
- represented every currently declared stream with one native-family
  disposition, overlap strategy, safe decoder-state boundary, and finite
  object/record bounds;
- recorded unsupported catalog and scoped topologies explicitly instead of
  presenting existing durable adapters as full RFC 012 support; and
- registered Claude `ownerAccountUuid`/`ownerOrganizationUuid` and `nameSince`
  drift as open release-blocking signatures backed only by synthetic sanitized
  shapes. The runtime TypeScript/Rust unions were intentionally not widened.

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

### C3. Durable migration

Build usage-v2 beside legacy usage. Keep v2 readiness non-ready during replay,
compare against the independent oracle, then switch the versioned query in one
transaction. Retain the legacy projection during the compatibility window and
test crash/restart at each migration boundary.

### C4. Downstream semantic suite

Before RFC 012D cutover, exercise common semantic reducers for:

- per-actor qualified usage and unknown context handling;
- response-observed versus configured model/effort;
- native mode transitions;
- plan/task/tool/progress lifecycles;
- question open/resolved/failed/cancelled with typed display fields; and
- late affiliations without duplicate usage.

Exercise a reference downstream aggregator for:

- durable query plus scoped-observer deduplication by `SemanticRevisionRef`;
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

Current landing status (2026-08-16):

- the crate-private composition root performs strict support/contract/program
  selection before validating exact grants and exposes no spoofable N-API
  artifact-probe request;
- one host-approved known object can be absent at attachment, created later,
  and read through the common symlink-safe confined file primitive only after
  a declaration-sized reservation;
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
  actual retained-native bytes, and reset controls; it admits reset before
  replay data, rejects the whole unit on pressure while returning it for retry,
  and only then commits the paired source cursor and decoder state. The lane
  ordinal is internal ordering, not public `observer_sequence` or semantic
  identity;
- one pass is active at a time, a later pass receives fresh bounds, close is
  idempotent, and the frozen access report excludes paths, identity values, and
  content; and
- the architecture checker forbids store/query/N-API/concrete-adapter imports
  and premature public export from this provisional composition root.

D1 remains `In progress`: watcher-before-scan, multi-object discovery/cursor
orchestration, declared relation-backed decoder dependency access, canonical
fact-revision adoption plus semantic reduction/events, the public ordered
multiplexer and poll/readiness barriers, coverage, overflow/resync epochs,
artifact mediation, cancellation waiting, the trusted native version-probe
driver, and the complete public request are not yet implemented.

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
evidence item; therefore X0 remains `In progress` until the behavioral suites
named by the manifest land.

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

1. ratify the RFC 012 umbrella ownership/precedence and RFC 011 delta matrix;
2. implement X0 compatibility fixtures for every retained/amended/superseded
   RFC 011 contract;
3. review and ratify RFC 012A logical dependencies, qualified values, identity,
   external/semantic references, source coverage, tier compositionality, scope
   primitives, and conservative version policy;
4. implement A1/A2 contract fixtures and architecture/access checks;
5. build current-agent ADS/support candidates in A3;
6. ratify RFC 012B identity/readiness/pagination and RFC 012C qualified usage
   semantics in parallel;
7. implement bounded catalog compositions and common runtime facts;
8. implement the store-free RFC 012D kernel and Claude scope against those
   contracts;
9. build durable catalog/readiness and usage-v2 shadow projections;
10. run the A4 fourth-agent proof across catalog/query and scoped-observer
    seams;
11. run UI, Chopsticks, and reference durable/live aggregation shadow
    integrations before any default switch; and
12. calibrate numeric gates, amend the owning child RFCs, then promote.

No step may use a temporary renderer catalog, private adapter tail, arbitrary
scope search, second database, or duplicated native decoder to shorten the
critical path.
