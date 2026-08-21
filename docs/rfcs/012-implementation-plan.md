# RFC 012 implementation and validation program

- **Status:** Active non-normative roadmap. C3's durable usage-v2 migration and
  A4's new-adapter proof have met their package gates. The public native host
  now verifies every compiled built-in support package, but all current-agent
  releases remain candidates and therefore grant no typed RFC 012 authority.
  The retained RFC 011 compatibility path cannot publish promoted RFC 012
  coverage, replay it, or select its query pack. Catalog, remaining runtime
  families, the public scoped-observer transport, complete performance reports,
  and default-on rollout are still in progress. Child-RFC 012B/C/D ratification
  remains a later product decision.
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

| Work package                             | Owner               | Status      | Exit evidence                                                                           |
| ---------------------------------------- | ------------------- | ----------- | --------------------------------------------------------------------------------------- |
| E0. Phase 0A/0B evidence                 | Umbrella            | Gate met    | catalog, diagnostic, topology, usage census reports/tests                               |
| X0. RFC 011 delta/compatibility gate     | Umbrella            | Gate met    | retained/amended/superseded contract and migration fixtures                             |
| A1. Logical dependency/model seam        | 012A                | In progress | architecture checks and partial contract-family fixtures                                |
| A2. ADS/scope/support tooling            | 012A                | In progress | internal strict authority; remaining public catalog/scoped lifecycle                    |
| A3. Current-agent support candidates     | 012A                | In progress | Claude 2.1.223 review-blocked candidate; Codex/Grok remain candidate                    |
| A4. New-agent adaptation proof           | 012A/umbrella       | Gate met    | fourth adapter without common-runtime/query/observer change                             |
| B1. Catalog identity/readiness contracts | 012B                | In progress | contract fixtures complete; authorized public transport open                            |
| B2. Bounded catalog source compositions  | 012B                | In progress | candidate conformance; runtime producers incomplete                                     |
| B3. Durable catalog/query snapshots      | 012B                | In progress | private pack/pagination; public policy/query lifecycle open                             |
| B4. Progressive host and UX              | 012B                | In progress | partial host readiness; selected hydration and UI flow open                             |
| B5. Catalog performance calibration      | 012B                | In progress | partial experiment; complete report and ratification open                               |
| C1. Runtime semantic contracts           | 012C                | In progress | broad fixture set; remaining families/reducer parity open                               |
| C2. Usage-v2 shadow projection           | 012C                | In progress | private parity; complete coverage/topology gate open                                    |
| C3. Durable usage migration              | 012C                | Gate met    | transactional switch, rollback, and compatibility-window proof                          |
| C4. Runtime semantic downstream suite    | 012C                | In progress | usage merge landed; all-family reducer/digest suite open                                |
| D1. Store-free observer kernel           | 012D                | In progress | substantial internal kernel; public/multi-object surface open                           |
| D2. Claude scope composition             | 012D                | In progress | bounded internal directory membership; runtime promotion open                           |
| D3. Control lane and epoch replacement   | 012D                | In progress | shared pool; user-input/message/task/effective-state/plan/tool replacement; no Gate met |
| D4. SDK and Chopsticks migration         | 012D                | In progress | injected-source shadow only; public observer transport open                             |
| D5. Observer performance calibration     | 012D                | In progress | partial timings; memory/access/slow-consumer report open                                |
| X1. Search/finalization separation       | 012B integration    | In progress | strategy harness landed; required report fields open                                    |
| X2. Diagnostic disposition/aggregation   | 012A implementation | In progress | bounded machinery landed; retained reduction/parity report open                         |
| X3. Physical extraction                  | Implementation      | In progress | coverage encoding extracted; remaining boundaries still logical                         |
| X4. Default promotion/drift lane         | Umbrella            | In progress | Claude candidate quarantined; independent review/telemetry open                         |

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

Current landing status (2026-08-18):

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
  scan;
- hardened the RFC 012A coverage wire boundary before further catalog or
  observer exposure. Aggregate and public Rust leaf DTOs plus portable
  TypeScript now reject unknown fields at every nested coverage shape, explicit
  `null` for optional evidence, nonplain portable objects, zero or non-
  JavaScript-safe generations, unsafe orders/timestamps, noncanonical or
  oversized reasons/identifiers, non-machine error codes, duplicate errors,
  and object-scoped errors without their stream coordinate. Coverage point,
  absence, and error collections share the existing engine limits and are
  rejected before unbounded traversal;
- added JSON-string N-API consumers for the committed RFC 012A and RFC 012C
  semantic fixtures. Rust and portable TypeScript now reject the same unknown
  nested fields, explicit qualified nulls, noncanonical integer lexemes,
  unpaired UTF-16, oversized leaves/envelopes, and drifted source/revision/
  provenance identity. The TypeScript runtime fixture consumes caller-held
  Rust-derived identity coordinates rather than attempting to synthesize
  BLAKE3 identities, and a differential mutation matrix exercises both native
  helpers plus the portable parsers; and
- retained A1 as `In progress`: usage-v2 is the first built-in family on the
  canonical seam, while the remaining fact-family migrations, complete
  retraction/replacement fixtures, full semantic reduction, tier/view
  compositionality, scoped/durable end-to-end parity, and full-only versus
  composed reducer digests remain.

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

The strict access-request boundary is now also frozen without widening source
authority. A private, non-serializable request can be minted only from one
`TypedAccessAuthorization`; it binds the classify-time native probe, exact
release/declaration/program/selection coordinates, an opaque nonzero host
policy digest, and the complete path-free KnownObject grant set selected from
the promoted scope program. A separate private retrieval request binds one
exact access-report digest. Portable Rust, TypeScript, and Python projections
are bounded, deny unknown fields, recompute the same SHA-256 digests, and cannot
mint either authority or reserve native I/O. Candidate and capability-
restricted packages still cannot authorize the operation.

A2 remains `In progress`: the internal scoped composition now owns the
authorized plan lifecycle, executes its first common confined primitive, and
reuses the store-agnostic decode boundary. The native host verifies the
built-in catalog and performs a bounded Claude native probe before durable
source access. Candidate declarations are compared with every runtime stream
in conformance tests, but no current release can mint typed authority, publish
promoted coverage, replay it, or select its query pack. Catalog and public
scoped-observer hosts still do not own the full strict lifecycle, and no public
N-API/IPC access-report retrieval surface exists. The promotion-
safety capability gap is now closed across Rust, Python, and portable
TypeScript: verified release descriptors retain the digest-bound capability
topology and level declarations; exact/range classification permits a broad
catalog, durable, or scoped operation only when that topology is nonempty and
all of its declarations are `supported`; degraded, unsupported, absent,
duplicate, and malformed declarations fail closed; and forward-catalog access
requires supported catalog capability evidence. A shared fixture proves that
a promoted but capability-restricted release cannot mint broader typed access.
The remaining scope primitives and public N-API/IPC boundary still require
executable conformance rather than portable classification alone.

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

Wave I (`692f78a`, 2026-08-21) rewrote the Claude identity fixture into a
sanitizer-v2 declared-input identity matrix and rebound `claude-identity-rules`
without flipping it off `degraded`. Decoder-executed `identity-determinism`
stays `planned`. Compiled ADS/source/scope digests were not touched.

A3 remains `In progress`: Claude Code 2.1.223 has a narrower durable candidate
with bounded marker probing and digest-bound stream declarations, but the
native distributable digest, independent sanitizer approval, complete
conformance report, section 12 performance report, and compatible-release
telemetry remain open. It is not runtime-selectable. Catalog and scoped
capabilities remain unsupported. Codex and Grok also remain candidates and
still need production-authorizing composition and complete independent
transition evidence.

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
query surface should land until engine transport can bind a caller-authorized
policy view to the frozen access-policy digest and the retained-snapshot lane
has public transport, retention, and expiration authority. B2 is independently
`In progress` as described below. B3 now includes source-neutral plan
registration, the initial build lineage, one atomic immutable initial Library
publication, a crate-private WITHHELD-only retained-page reader, and durable
ordinary-refresh start plus atomic successor publication that keeps the prior
snapshot readable; scheduling and hydration execution remain later integration
work.

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

Current landing status (2026-08-18): B2 remains `In progress`. The first
crate-private, adapter-neutral v1 slice freezes Claude, Codex, and Grok catalog
composition contracts and one Rust-produced conformance fixture. Composition
identity binds explicit sanitized decoder/disposition overlap axes and a
planned/unbound versus exact declaration/support-release promotion state;
planned values cannot authorize execution. Membership publication requires
complete, canonical, positive-generation evidence from every admitting
authority, while metadata-only evidence cannot fabricate a member. Bounded
directory-membership, replace-document, delimited-head, and delimited-prefix
primitives retain the exact RFC 012A overlap strategy and safe decoder-state
boundary. Initial head/prefix planning starts only at record zero; every later
window consumes the opaque continuation issued by the immediately preceding
step, bound to composition, component, window specification, frozen record
layout, next ordinal, and chain sequence. The conformance trace proves equal
ordered record, disposition, fact/revision, semantic payload, qualified
provenance, and final decoder-state digests against full-only decoding. The
64 KiB head value remains candidate fixture evidence rather than a ratified
global bound. Still open are promoted real source declarations/support
releases, actual common-runtime and vendor composition, independent Phase 0
catalog identity plus final hydrated-identity oracle parity, source
access/coverage integration, and calibrated performance evidence.

The second B2 slice (`df3b6b0`) freezes a Codex-only candidate conformance
oracle without changing that promotion state. A test-only runner uses the real
common directory snapshot and bounded append-delimited driver to interpret the
first complete `session_meta` record through the same normalization seam as the
existing durable decoder. Its privacy-reduced fixture pins one project and ten
sessions against the independent Phase 0 census and full durable decode, plus
registration-invariant RFC 012A source-record, canonical entity, planned
membership-fact, and record-owned revision identities. Executable edge cases
separate the 64 KiB framing prefix, 65,535-byte pre-LF record payload, 4 KiB
checkpoint-anchor reread, and 69,632-byte maximum physical driver read per
object; internal, malformed, and oversized first records fail closed. The
exact `rollout-sessions` declaration remains durable-only and `full_only`, the
digest-bound support release remains `Candidate`, catalog capability remains
unsupported, and catalog access authorization is rejected. This is candidate
evidence only: Codex's legacy payload identities still depend on numeric source
registration and do not yet carry semantic revisions, so cross-topology payload
parity, promoted declarations/support, and runtime catalog execution remain
open alongside the Claude identity oracle.

The third B2 slice (`08f4cec`) freezes a Grok-only candidate conformance
oracle without promoting or executing the planned composition. A test-only
runner uses the real common directory-snapshot and replace-document drivers
with the exact current candidate declaration: four admitting sidecars at
100,000 entries/depth 8 and bounded 1 MiB summaries. Complete membership is
authoritative before optional summary enrichment; chat-only, malformed-summary,
and oversized-summary members remain visible, while updates-only and
unknown-only directories remain explicitly non-admitting pending a declared
policy decision. Summary/path session and project identity drift fails closed
and requires explicit relation evidence. The privacy-reduced Rust fixture
matches the independent Phase 0 census and durable summary decoder at three
projects/four sessions, accounts every physically read summary byte, and pins
registration-invariant source-record, entity, planned membership/association/
metadata fact, and revision identities. The exact digest-bound support release
remains `Candidate`, catalog capability remains unsupported, and catalog
authorization is rejected; the materially different Grok adapter-neutral
composition remains planned/unbound. Claude three-source identity and final-
hydration parity, promoted declarations/support, source-access/coverage
integration, runtime catalog execution, and calibrated performance evidence
remain open.

The fourth B2 slice (`62a7540`) completes the candidate-only three-adapter
identity gate with Claude's index, top-level transcript, and nested-parent
membership union plus bounded transcript-head fallback. Complete positive-
generation membership authorities are re-scanned unchanged after enrichment;
metadata cannot fabricate a member, blank/malformed/oversized heads remain
explicitly unavailable, and path-based association identity cannot be
retargeted by conflicting `cwd`/project evidence. Every association basis is
retained through an opaque occurrence-bound reference. The privacy-reduced
fixture matches the independent Phase 0 census and full durable identities at
three projects/twenty sessions, including synthetic index-only, top-only,
nested-only, overlap, and registration-invariance cases. The common append
driver also stops immediately when a committed record exactly fills its batch,
so the candidate evidence distinguishes a 64 KiB logical record, one bounded
64 KiB framing read-ahead, a 4 KiB checkpoint anchor, and the conservative
132 KiB physical ceiling. Current support remains `Candidate`, catalog remains
unsupported, and the planned composition remains unbound; checkpoint restore
and scan bounds are now aligned by the later closure below, while nested-parent
UUID enforcement remains undeclared and the head ceiling remains fixture
evidence rather than ratified policy.

The fifth B2 slice (`ee331b7`) closes only the factual Claude decoder-axis
drift: parent/subagent candidate declarations and the planned transcript-head
component now name the exact compiled `claude-session-record` and
`claude-subagent-record` decoders. The candidate package, compiled adapter
binding, and conformance tests carry the recomputed source-declaration digest,
and a stale declaration document fails verification. No topology, admission,
overlap, bound, capability, or status changed: the release remains Candidate,
catalog authorization still rejects, and runtime composition/promotion remain
blocked on the open B2 evidence. A2's capability-to-operation binding is now
fail-closed before any release can be promoted.

The sixth B2 slice (`d36b589`) closes the common directory-checkpoint restart
mismatch without promoting a source. Checkpoint decoding now requires the
active `DirectorySnapshotConfig`, rejects entry counts above its exact
`max_entries`, validates canonical binary path-key framing, and rejects members
deeper than its current `max_depth` before traversal. Coordinator resume uses
the selected stream configuration and leaves the durable checkpoint unchanged
when a declaration narrows. Executable coverage proves that a valid
100,001-entry checkpoint fails at 100,000 and restores at the Claude candidate's
250,000 bound; this is serialization/restart evidence, not a ratified global
performance limit. The Claude fixture drops only that resolved blocker.
Nested-parent UUID policy, Grok updates-only admission, promoted runtime
composition, source-access/coverage integration, and catalog publication remain
open.

The seventh B2 slice (`a913fcb`) adds privacy-safe evidence for the two open
admission choices without changing catalog output. The independent census now
counts Claude UUID-shaped versus opaque nested parents and the nested-only
identity delta, plus Grok directories admitted by the current four-sidecar
candidate policy, current-policy members with/without updates, and updates-only
members admitted only by the broader census. Explicit-zero counters and
synthetic matrices keep the result deterministic and contain no project,
session, or path values. The checked-in corpus reports no nested-only or
updates-only delta, so neither absence is treated as policy evidence; a private
census rerun and review are required before changing either declaration or
runtime admission rule.

The eighth B2 slice (`6b1cfe4`) closes the missing authorization-to-composition
execution seam without promoting or reading a source. The selected
`TypedAccessAuthorization` now retains the verified source-declaration digest,
and only a borrowed, field-private `CatalogDiscovery` authorization with an
exact selected query pack can enter composition execution. The executable
wrapper verifies and retains the exact adapter, support release ID and digest,
source-declaration digest, and negotiated contract selection; a planned/unbound
composition and every drift axis fail closed. Digest-only promoted-binding
values remain useful for static composition construction and fixtures but can
no longer authorize execution. Source access, coverage publication, policy-view
binding, persistence, query execution, and built-in promotion remain open.

The ninth B2 slice (`9ec352f`) adds a crate-private, complete-only Library
coverage assembly seam without reading or promoting a source. Exact/range
catalog authorization, the complete negotiated selection, adapter and source
instance, support release, source declaration, access policy, and normalized
composition are bound together before assembly. Every composition component
must provide an explicit complete enumeration and unordered snapshot revision,
including an empty metadata-only component; canonical live/absence lineage and
primitive-specific positions then produce one RFC 012A coverage set and a
separate aggregate restart revision. Catalog membership, native coverage
membership, and component-completion revisions remain distinct identities.
Forward-recognized authorization, incomplete/error evidence, replay under a
different selection or policy, and planned/unbound compositions fail closed.
This remains a no-I/O contract seam: real source producers, access-plan proof,
forward degraded coverage, partitioning beyond the portable 250,000-point cap,
persistence, query publication, and N-API exposure remain open.

Wave I (`ef4536f`, 2026-08-21) compiles the Claude catalog coverage producer as
crate-private runtime and replaces the raw `Path` argument with
`CatalogBoundSourceAccess` on `CatalogExecutableComposition`. Missing roots
fail closed before filesystem reads. The built-in Candidate still cannot
authorize `CatalogDiscovery`. Synthetic conformance constructors stay
`#[cfg(test)]`. Codex/Grok producers, persistence, and public catalog N-API
remain open.

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

Current landing status (2026-08-18): B3 is `In progress`. The first durable
slice (`dc7e9aa`) registers one immutable, bounded Library coverage plan and
advances only the initial `Absent -> Pending -> Building` readiness lineage on
the existing RFC 011 commit clock. Schema v50 reserves source-neutral commits
for the two completed zero-fact catalog administration transitions; the typed
writer transaction atomically owns the commit row, plan, CAS-bound build state,
and privacy-safe `catalog.readiness.changed` outbox entry, and publishes an
in-memory commit notification only after SQLite durability. Equal lost-ack
replays are no-ops, stale or foreign expectations fail without mutation, and
restart reparses the bounded plan, recomputes its ID/content digest, validates
commit ownership, and reconstructs the B1 readiness machine. Crash injection
proves rollback at every precommit seam and durable idempotency after commit.
This slice itself cannot accept or synthesize coverage, reducer rows, complete
or degraded readiness, snapshot IDs, last-complete state, retention,
pagination, or public query authority. The later third slice now supplies the
initial atomic publication, the fourth slice supplies the first private
retained-page reader, and the fifth slice supplies checked retained identity
resolution.

The second bounded B3 slice (`e65a9fd`) adds a store-free, contract-only
initial-publication assembly envelope. It consumes exact complete Library
source assemblies projected from authorized B2 composition, the frozen initial
durable `Building` expectation, explicit source-member-to-live-session
bindings, and one canonical bounded reducer freeze. Construction requires every
required source, accepts only declared complete optional sources, binds the full
negotiated selection plus plan, declaration, access-policy, coverage,
membership, and component-completion revisions, rejects uncovered evidence and
unevidenced relation, locator, or association endpoints, and converges a shared
cross-source member identity on one base session. Coverage vectors and reducer
state are canonicalized before deriving the privacy-safe publication digest;
early aggregate preflight plus grouped row materialization and indexed member
validation keep the freeze bounded without quadratic lookup. A complete empty
required source composes honestly with a nonempty required source. The envelope
is non-serializable and store-free: it does not itself write SQLite, publish a
Ready snapshot/readiness/outbox transition, read native sources, expose N-API
or query authority, or implement retention/pagination. The following slice
consumes this checked envelope for initial durability.

The third bounded B3 slice (`51b0025`) consumes the checked initial Library
publication envelope under an exact `Building` commit compare-and-swap and
writes one immutable schema-v51 snapshot header, canonical typed private
frames, `Ready` build/readiness state, and one privacy-safe readiness outbox
entry in a single SQLite transaction. Durable preparation enforces entry and
aggregate byte ceilings before SQL with bounded streaming JSON encoding;
restart reconstructs the exact plan, selection, and source coverage and
validates commit ownership, closed vocabularies, header/frame coordinates,
nonzero revisions, reducer-key linkage, canonical ordering, counts, and
content digests before exposing `Ready`. Crash injection covers every write
boundary and lost acknowledgements, while a second connection proves
uncommitted rows remain invisible. This remains initial `Library` publication
only: no refresh/degraded transition, retention/expiration, query-page
execution, hydration execution, source reads, or public N-API authority.
At that landing, retained-snapshot read lanes and pagination remained the next
B3 gates.

The fourth bounded B3 slice (`c0ea685`) adds the first checked, crate-private
retained-page execution without exposing a public catalog API. Ready restart
now strictly decodes canonical bounded project/session frames and requires the
exact current RFC 012A model/reference versions even for a source-free
publication. Its non-serializable read authority shares one authenticated
snapshot-header identity plus bounded per-row key, length, and payload-digest
commitments, so another connection, a missing row, or a canonical same-key row
substitution cannot escape the restart-validated publication. The only v1
query is engine-derived `All` filtering with opaque entity-key ascending order;
its keyset continuation binds the exact snapshot, full negotiated selection,
query fingerprint, sort version, page size, and final row. SQLite reads use the
existing snapshot/kind/key primary-key range with `page_size + 1`, preflight
payload lengths before allocation, enforce aggregate page bytes, and remain
query-only while unrelated commits advance. Projection accepts only a
WITHHELD policy token bound to the exact Library plan and selection, so native
and policy-sensitive values cannot be disclosed. An absent snapshot remains an
internal not-retained error rather than a fabricated `SnapshotExpired` claim.
At that landing, public N-API/SDK exposure, caller-authorized local policy
views, richer filters/sorts, external-resolution execution, refresh, snapshot
retention, and expiration authority remained open.

The fifth bounded B3 slice (`fa1eb64`) extends Ready restart to strictly decode
and reconstruct every source, member-binding, reducer, tombstone, project, and
session frame; reproduce the exact reducer, source coverage, and publication
revisions; and retain a privacy-minimal lifecycle index. Its crate-private
WITHHELD-only resolver validates the exact caller-held snapshot, full
negotiated selection, policy binding, and restart-authenticated publication
identity before returning a committed live row or a tombstoned, superseded, or
typed-unknown result. Tombstone and replacement provenance is canonical and
bounded, semantic drift cannot be hidden behind coordinated frame-digest
rewrites, and a file-backed close/reopen test proves Tombstoned,
Superseded-to-exact-target, and replacement-Live resolution from durable state.
This slice adds no schema writes, public N-API/SDK surface, local-sensitive
policy view, refresh/degraded lineage, retention/expiration authority, or
richer filter/sort vocabulary. Those remain the next exposure and lifecycle
gates.

The sixth bounded B3 slice (`773985b`) adds schema-v52 and crate-private
ordinary Library refresh start from an exact restart-authenticated plain-Ready
publication. One source-neutral zero-fact administrative commit atomically
retains the immutable current snapshot, advances durable readiness with
`refreshing_from_snapshot`, and writes a privacy-safe v3 invalidation under an
exact Ready/publication compare-and-swap. Restart reconstructs the refreshing
Ready lineage and continues to issue only a snapshot-frozen WITHHELD read
authority for the retained publication. Crash seams, separate-connection
isolation, lost-ack replay, source-free operation, and forged or foreign
lineage rejection are executable. This slice adds no replacement snapshot,
refresh completion, degraded state, retention/expiration authority, source
reads, or public N-API/SDK catalog surface. Atomic refresh publication is the
next prerequisite for a genuine newer snapshot and later retirement evidence.

The seventh bounded B3 slice (`c603039`) adds schema-v53 and atomically
publishes an ordinary-refresh successor under the exact active-refresh and
restart-authenticated predecessor compare-and-swap. The successor carries a
distinct durable-v2 domain, exact predecessor publication/content/reducer and
cumulative member-history commitments, canonical complete source assemblies,
and a monotonic reducer continuation. Existing member references cannot
retarget, live facts cannot be rewritten at the same observation commit or
removed without exact retraction, and tombstones can disappear only through a
valid newer-generation live revival. One transaction writes the newer
immutable snapshot and typed frames, advances `Ready`, clears the refresh
marker, and emits a privacy-safe v4 invalidation; crash seams, separate-reader
isolation, and lost-ack replay remain executable. The predecessor snapshot and
already-issued authority continue serving their exact keyset pages while the
new authority serves the successor, and file-backed restart authenticates only
the current snapshot after validating the complete linear predecessor chain.
An internal eight-refresh ceiling bounds retained lineage traversal and refuses
a ninth refresh without mutation; orphan snapshots fail restart. This slice
itself added no retirement/deletion lease, `SnapshotExpired`, degraded/error
refresh, native source execution, caller-authorized local policy view, public
N-API/SDK catalog surface, or richer filters/sorts.

The eighth bounded B3 slice (`c6369e7`) adds schema-v54 and append-only logical
query-retirement evidence without deleting immutable snapshot headers or
frames. A typed source-neutral zero-fact command can retire only the exact
oldest query-retained non-current ancestor under a restart-authenticated plain
Ready expectation; stale, foreign, current, skipped-prefix, digest-drifted, or
refreshing lineages fail without mutation. Retirement rows bind the retired
and exact then-current successor publication/content commitments to their
administrative owner and form one bounded canonical oldest prefix. Restart
reconstructs and validates that prefix, including the exact successor that
existed before each retirement commit, while retained historical authorities
are loaded on demand from the authenticated ancestry. Page reads validate the
complete request and continuation before consulting retirement, then classify
retention and read rows in one SQLite read transaction. Valid retired
continuations return the frozen typed `SnapshotExpired` response naming the
current same-plan successor; malformed or foreign cursors remain invalid and
cannot be relabeled as expired. Crash seams, lost-ack replay, separate-reader
isolation, refresh-versus-retirement races, stale-authority rejection,
append-only triggers, and unchanged physical snapshot/frame counts are
executable. The eight-refresh ceiling remains the durable-lineage bound. This
slice adds no physical compaction or automatic retention policy, retired
external-reference resolution, degraded/error refresh, caller-authorized
local policy view, richer filters/sorts, or public N-API/SDK catalog surface.

The ninth bounded B3 slice (`b9a7f39`) adds schema-v55 and the first durable
active-refresh integrity failure without weakening the last independently-safe
publication. Only an exact restart-authenticated ordinary refresh can publish
the checked lowercase-ASCII machine reason and fixed `IndependentlySafe`
disposition. One source-neutral zero-fact transaction appends immutable failure
evidence, clears the refresh marker, advances durable readiness to `Error`, and
emits a privacy-safe v5 invalidation under the exact plan, selection, epoch,
attempt, refresh-start commit, retained snapshot, and publication/content
digests. Restart treats the change log as prunable notification rather than
authority: it authenticates the failure ledger and both administrative commit
owners, reconstructs the retained publication and logical-retirement prefix,
then replays Ready, refresh start, and safe failure through the readiness
machine. Snapshot-frozen WITHHELD pages and live resolution remain available;
a valid retired continuation still yields its exact `SnapshotExpired` result.
Crash seams, separate-reader isolation, lost-ack replay, success-versus-failure
CAS races, source-free operation, strict Rust/TypeScript reason-code parity,
and coordinated corruption negatives are executable. Discarded/no-snapshot
errors, retry/degraded/partial lineage, physical compaction, caller-authorized
local policy, richer queries, and public N-API/SDK catalog transport remain
open.

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

Current landing status (2026-08-21): the sole-owner host can admit query
workers before complete-only FTS finishes and exposes source-neutral readiness
and search state. Deferred bootstrap is an explicit caller choice rather than
an SDK filesystem-size heuristic. This does not yet provide the authorized
public catalog pages, last-complete warm policy view, selected-session hydration
command, renderer pagination/content states, or the complete cold/warm UX
matrix. B4 remains `In progress`.

### B5. Performance calibration

Run cold/warm/catalog-query/selected-hydration experiments on frozen inputs.
Report every environment/evidence digest required by RFC 012B. Propose a child
RFC gate amendment that promotes measured values from experiment target to
ratified release ceiling. Until amended, semantic correctness gates block
release but provisional p95 values do not become accidental architecture law.

The current FTS strategy fixture is diagnostic-only. It records a small subset
of elapsed/WAL/query timing values, but not the complete benchmark-report
contract in section 12, selected-hydration measurements, repetitions, memory,
or a reviewed gate amendment. B5 remains `In progress`.

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

Current C1 landing status (2026-08-21): actor-run, actor-affiliation, usage-v2,
effective-state, user-input-request, interaction, message, task, plan, and tool
v1 fixtures now have strict Rust, JSON-string N-API, and portable TypeScript
consumers. Effective-state currently proves the independent Model dimension;
interaction covers Pending | Resolved | Failed | Cancelled plus complete
retract and partial non-retraction. The fixture breadth is substantial, but it
does not close C1: remaining model/effort/mode dimensions, content/progress and
capability families, complete family-by-family replacement/retraction cases,
full semantic reduction, tier/view compositionality, and durable/scoped reducer
digest equality remain open.

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

Wave I (`c50cfd2`, 2026-08-21) joins already-selected `runtime.actor-run@1`,
`runtime.actor-affiliation@1`, and `runtime.usage-v2@1` identities across
durable reducer rows, Claude-decoded `getRuntimeUsageV2` pages, and selected
scoped envelopes, including A→B usage correction, affiliation present→removed,
generation reset, and partial coverage that cannot prove absence. The
digest-bound report is
[`runtime-selected-family-durable-scoped-parity-v1.json`](../../agent-support/claude-code/candidate-2026-08-15/reports/runtime-selected-family-durable-scoped-parity-v1.json)
(`sha256:2dc73693fbaab8e5e6f56840545f3f1897a43280fe8bd5327ead62cfa40aad2f`).
Fixture-adapter facts still do not bind `getRuntimeUsageV2` membership;
durable actor-run/affiliation coverage stays `not_materialized`; Candidate
`cross-topology-parity` stays `planned`. C2 remains `In progress`.

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

C3 is `Gate met`: private parity, source-scoped selection, its crash boundaries,
rollback, and the bounded non-mixing aggregate vector are closed.
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

The representative external compatibility-window gate is now closed by the
privacy-reduced report
[`usage-v2-compatibility-window-v1.json`](../../agent-support/claude-code/candidate-2026-08-15/reports/usage-v2-compatibility-window-v1.json)
(`sha256:3f4eaa7c8144fe078c9df71ca6cb72a3b0c9a9e5390bcecb7433b455e1087a83`).
On a stable ephemeral clone, an independent census and durable ingest matched
exactly across 153,525 responses, 5,141 actors, 911 usage sessions, all four
token totals, 5,289 complete Ready coverage points, and zero final foreign-key
violations. One 101-project window plus an order-independence probe remained on
the proven unselected `legacy.usage@1` default; all four comparisons were
`legacy_higher`, an expected semantic difference rather than an oracle failure.
`getUsage`/`getUsageActivity` remain explicitly legacy, and this C3 gate does
not promote the Claude capability. Effective model/effort/mode, interaction
lifecycle, remaining family/scoped parity, and support-package promotion remain
owned by C1/C2/A3/D and are still open.

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

Current landing status (2026-08-21): Rust and portable TypeScript reference
consumers merge durable and scoped usage-v2 by `SemanticRevisionRef`, dedupe
delivery by occurrence-scoped event ID, and retire overlays only when both
coverage vectors are complete and durable coverage is equal to or ahead of the
observer. Partial, unavailable, incomparable, and observer-ahead evidence is
retained. The remaining runtime families, catalog/base-session equality,
snapshot expiration/reset matrices, multi-run identity, and all-family reduced
and replacement-state digest comparison are not implemented. C4 remains
`In progress`.

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

Current landing status (2026-08-20):

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
- commit `8840de2` adds bounded auxiliary `runtime.actor-run` and
  `runtime.actor-affiliation` state around the still-selected-only
  `runtime.usage-v2` reducer. Actor and affiliation revisions are pre-scanned
  before usage regardless of fact order, must keep exact source-object and
  generation ownership while advancing their append cursor, and have
  independent entity ceilings; each actor context also caps and canonically
  orders its semantic revision evidence. A usage occurrence receives an exact
  actor declaration when available and a dimension-local conservative
  team/workflow view: ambiguity in one dimension does not erase an independent
  value in the other, and removed affiliations stop grouping without erasing
  their revision evidence. Late context revisions do not redeliver unchanged
  usage or change usage event/semantic identity. Reset and deletion
  retractions carry the pre-mutation context before all three reducer maps are
  cleared, while failed replacement objects roll back their auxiliary state
  without disturbing siblings. Replacement replay may attach newer current
  context to the stable usage event, but the usage replacement digest
  deliberately excludes that overlay because actor/affiliation families are
  not yet selected replacement-integrity contributions. The existing portable
  usage wire continues to reject enriched context rather than silently widen
  its frozen shape; this is crate-private reducer and mapper state only;
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
  pre-access canonical root session/reference, a canonical RFC 012C actor-run
  reference using bounded evidence-backed state when available and the
  pre-existing canonical fallback otherwise, explicit actor attribution,
  conservative affiliation completeness, path-free source occurrence,
  native/observed time, evidence qualification, and native evidence
  disposition. Usage routes to its canonical actor; source lifecycle controls
  route through the root only as `ScopeFallback`, never as semantic root
  attribution. Typed usage must match the observer root session and carry its
  durable-equal semantic reference, while controls must not fabricate one.
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
  owner services a fresh epoch-2 poll. The structured handoff now also drives
  the exact current append bindings through a complete canonical replacement
  pass while it continues supervising the retained watcher. It waits for
  control capacity without draining application events, drains every bounded
  source batch, publishes and atomically swaps the complete epoch, clears the
  one-shot contract-replay flag, and re-pairs the same watcher with the fresh
  source owner. Portable poll behavior during that handoff remains open;
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
  leaves the ordered terminal control drainable before close. The retained
  handoff's automatic append replay/rebind path is fail-closed: transient
  access/decode, re-overflow, incomplete coverage, admission, projection, or
  delivery failure cannot swap partial state and instead terminalizes the
  observer through the ordered failure control. Remaining portable-host
  wiring, dynamic/non-append replacement composition, and policy calibration
  remain open;
- one pass is active at a time, a later pass receives fresh bounds, close is
  idempotent, and the frozen access report excludes paths, identity values, and
  content;
- a separate crate-private negotiation boundary now composes RFC 012A selection
  into an exact RFC 012D v1 observation profile before native access. Request,
  offer, and selected contracts bind the model major, external-entity and
  semantic-revision references, coverage, requested fact families, observation
  profile, envelope, event, and lifecycle versions; query-pack authority is
  rejected on both sides. All nine incompatibility axes produce the typed
  `IncompatibleObservationContract` result, while malformed shapes remain
  validation errors. Rust and portable TypeScript consume a received selection
  only when it exactly equals the caller-held request/offer result, including
  the fact-family set and preferred versions. The internal attachment request
  now carries those RFC 012D wrappers rather than a bare RFC 012A pair; its host
  negotiates first, requires the resulting base selection to equal the typed
  access authorization, retains that exact value for the eventual capabilities
  report, and uses it for root-reference and coverage assembly. The internal
  accessor is deliberately named `contract_selection()` rather than falsely
  presenting that selection alone as RFC 012D's per-family `capabilities()`
  result. An incompatible event contract therefore fails with its typed axis
  before registry authority or source-access construction. A Rust-produced
  fixture proves independent portable negotiation and strict bounded parsing.
  This slice does not freeze the event/envelope DTO union, claim unknown-event
  preservation, or expose native observer transport;
- the first exact per-family RFC 012D capabilities DTO now repeats the
  caller-held negotiated selection and reports `Supported`, `Degraded`, or
  `Unsupported` with support-release evidence, evidence quality, expected
  timing, expected completeness, and a closed canonical limitation set. Exact
  promoted support reports complete capability; range-backed support remains
  qualified and partial; and host-offered families outside this attachment's
  selection remain explicitly unavailable rather than silently disappearing.
  Construction additionally binds selected families to the scoped reducer's
  compiled implementation set, currently only `runtime.usage-v2` v1, so an
  offered but unimplemented family cannot be promoted into a support claim.
  Wire consumption requires the exact caller-held selection, host offer,
  compatibility class, and support-release identity, and rejects any selected
  reference, coverage, family, profile, envelope, event, or lifecycle version
  absent from that offer. The completeness field is
  explicitly an implementation expectation, never current source readiness;
  barriers and RFC 012A coverage retain that authority. A Rust-produced fixture
  independently exercises exact, range-degraded, and unselected-family parsing
  in portable TypeScript. The report is retained on the crate-private
  attachment; no N-API observer transport lands in this slice; and
- the internal consumer envelope mapper now retains that exact negotiated
  selection rather than reconstructing a global version. The envelope's
  `contract_version` comes from the selected envelope axis, the delivered event
  must match the independently selected event axis, and a typed usage revision
  is rejected unless `runtime.usage-v2` at its exact family version belongs to
  the attachment selection. The full selection is retained beside every mapped
  envelope for the forthcoming contextual wire parser. Lifecycle versioning
  remains a separate selected axis; this correction does not yet freeze or
  expose the portable event/envelope union; and
- the first contextual envelope wire slice now freezes only the implemented
  `runtime.usage-v2` v1 upsert/retraction projection. Rust serialization
  withholds native payload and locator data, while strict consumption requires
  the caller-held negotiated selection, exact root (including the root actor
  run), and a nonempty bounded set of authorized source coordinates. It
  rejects omitted canonical fields, unknown nested meaning, unsafe portable
  integers, cursor/range or generation drift, unsupported enrichment, and
  unbounded identity/runtime text; it also recomputes the semantic revision
  and delivery event identities from the received normalized value and source
  occurrence. Portable TypeScript independently validates the Rust-produced
  fixture's shape, bounds, operation/evidence coherence, and caller-held
  context. It deliberately does not claim to recompute the native BLAKE3
  identities: the crate-private Rust contextual parser remains the integrity
  boundary before this DTO may cross into portable code. Source and observer
  lifecycle controls, typed future event preservation, native transport, and
  public N-API iteration remain gated on the complete envelope union; and
- a second contextual wire slice freezes only `source_created`,
  `source_deleted`, `source_reset`, and typed `source_object_error` controls.
  Every value repeats the exact caller-held selection and root, resolves one
  member of a bounded canonical authorized-source set, routes only through the
  root actor's `SourceLifecycleControl` fallback, and keeps semantic revision,
  native time, locator, record, cursor, byte-range, and native payload fields
  explicitly absent. Created/deleted generations, reset successor lineage,
  and retryable/exhausted/non-retryable error state are checked against engine-
  control evidence, portable integer bounds, canonical append-position
  provenance, and the envelope epoch/source generation. Rust reconstructs the
  exact typed control and recomputes its event ID; portable TypeScript
  independently parses the Rust-produced fixture without claiming BLAKE3
  authority. Observer bootstrap/resync-completion barriers, semantic families,
  future typed-unknown preservation, native transport, and N-API exposure
  remain outside this deliberately incomplete union;
- a third contextual wire slice freezes `observer.resync_required`,
  `observer.resync_started`, and terminal `observer.failed` controls. Every
  control repeats the exact selected lifecycle/envelope/event contract and
  root-owned observer-control coordinate, carries no semantic/native record or
  source-occurrence evidence, and uses portable bounded epoch, sequence,
  discard, and observation counters. Consumption is stateful rather than
  shape-only: the caller must retain the current epoch, last contiguous
  sequence, replacement baseline digest, and phase; replacement start also
  requires the exact previously delivered invalidation and must advance one
  epoch in `FullSnapshot` mode. Rust reconstructs the typed control and
  recomputes its deterministic event ID, while portable TypeScript independently
  validates the Rust-produced fixture without claiming BLAKE3 authority.
  Diagnostic discard counts remain explicitly outside event identity. Initial
  bootstrap and resync-completion barrier DTOs, complete manifests/coverage,
  future typed unknowns, native transport, and N-API exposure remain gated;
  and
- a fourth lifecycle wire slice freezes the attachment-bound close completion
  proof behind the eventual `close() -> Future<void>` facade. One process-local
  opaque attachment reference and one deterministic idempotent close-request
  identity bind the exact caller-held selection and full resolved root. Rust
  accepts only the original in-process attachment authority and exact consumer
  drain, then emits a receipt only after the existing two-part barrier reports
  zero owned operations and watcher tasks plus a closed drain. Active counters,
  applied sequence, timestamps, locators, and payloads never become portable
  trust claims. Portable TypeScript independently requires the caller-held
  selection, root, attachment reference, and request identity and rejects
  silent nested fields or a merely `closing` state. This slice defines no
  public N-API method or transport owner;
- the internal replacement-family manifest now closes the selected-family
  empty-state ambiguity. The compiled reducer accepts exactly the negotiated
  `runtime.usage-v2@1` family, validates the frozen replacement snapshot and
  every RFC 012A coverage set, and always emits one family manifest—even when
  its entity count is zero. Missing, foreign, malformed, or version-drifted
  coverage therefore fails bootstrap/resync completion instead of turning an
  uncovered family into an apparently valid empty result. Complete and partial
  evidence still merge conservatively. This is an internal usage-v2 closure,
  not a complete multi-family or public barrier surface;
- exact known-object watermark assembly now carries the first RFC 012D
  `scope_coverage` proof. It binds the selected promoted scope-program digest,
  the program-declared `KnownObject` root, the exact complete set of declared
  `KnownObject` relations, opaque source coordinates, positive generations,
  present/absent/deleted state, and conservative completeness; validates
  one-for-one against the RFC 012A Decode set; and is carried and digested
  identically by bootstrap and resync barriers. A promoted program without a
  typed `KnownObject` root, an omitted relation, or a caller-swapped root flag
  fails before access. The path-free summary deliberately excludes source
  positions and cannot replace their cursor or membership authority. Dynamic
  relation discovery, non-append members, D-owned artifact/capability
  manifests, the portable barrier wire, and N-API transport remain open; and
- the exact-known-object `scope_coverage` proof now has a strict contextual
  portable projection without becoming a watermark or completion barrier.
  Rust retains the expected root, selected scope-program identifier and
  SHA-256 digest, declaration-derived root relation, complete canonical
  relation set, exact expected scope revision, and the one authoritative RFC
  012A Decode set; wire consumption reconstructs the typed summary, recomputes
  its BLAKE3 scope revision, and rejects revision/program/root/source/generation/state/completeness or
  relation-set drift. Portable TypeScript independently parses the frozen Rust
  fixture, requires that same caller-held context, cross-validates every
  present/absent/deleted relation one-for-one with Decode evidence, and rejects
  unsafe integers, unknown nested meaning, duplicate coordinates, and root
  reassignment. Source positions, membership revisions, and errors remain
  exclusively on the Decode set, so cursor movement does not masquerade as a
  scope-membership revision. The projection contains no native locator/path,
  family manifest, barrier sequence, root-presence shortcut, readiness, public
  observer method, or N-API authority. Dynamic/discovered membership and the
  complete artifact/capability/family manifest remain prerequisites for a
  portable bootstrap or resync-completion barrier; and
- the first bounded artifact wire slice freezes a path-free, attachment-bound
  request/result contract without granting native source access. A process-
  local command retains the exact attachment authority, negotiated selection,
  resolved root, artifact key and kind, expected generation, byte ceiling, and
  `metadata_only`, `hash_only`, or bounded `inline` disclosure policy. V1
  always withholds the native locator. Available results require complete,
  positive-generation provenance and policy-exact hash/content fields; Rust
  verifies canonical padded base64, exact size, and SHA-256 before producing
  or consuming inline content. Typed unavailable results distinguish scope,
  denial, absence, limit, generation, support, malformed, and unstable states
  without smuggling paths. Portable TypeScript independently enforces strict
  caller-held context, shape, number, identity, base64, and byte bounds against
  the Rust fixture, while Rust remains the SHA-256 integrity boundary. The
  architecture ratchet requires both the Rust attachment seam and portable
  export. No native locator mediator, file read, public observer method, or
  N-API transport lands in this slice;
- the attachment-level artifact policy gate (`5532844`) now makes the
  request's `artifact_access_policy` executable before any future native
  mediation. The trusted Rust composition root selects either disabled access
  or one immutable per-read byte ceiling plus a maximum disclosure class;
  command minting permits only monotonic
  `metadata_only` -> `hash_only` -> `inline` disclosure within that ceiling.
  Invalid policy bounds fail after contract negotiation but before support or
  source access, disabled attachments cannot mint even metadata requests, and
  process-local commands retain and revalidate the exact attachment policy.
  This is a caller ceiling rather than locator authority: it adds no artifact
  relationship resolution, file read, public request field, N-API/SDK method,
  or portable claim that access occurred; and
- a post-landing mediation audit keeps native artifact reads closed: the
  selected scope program can authorize an `ArtifactLocatorFromEvidence`
  relation, but the current private seam cannot yet prove that a requested
  portable artifact key was derived from that exact relation and evidence.
  Built-in Claude artifact metadata/content facts also still use legacy
  registration-bound entity keys without semantic revisions, and the scoped
  reducer intentionally ignores those non-usage facts. Locator resolution,
  canonical artifact fact adoption, evidence-to-key binding, and the native
  read mediator must land together before the policy ceiling can authorize an
  actual read; and
- the contextual completion-envelope slice (`dc0fc41`) now freezes ordered
  `observer.bootstrap_complete` and `observer.resync_complete` delivery without
  exposing an observer transport. The non-Serde Rust consumer context is minted
  from the authorized attachment immediately before dequeue and binds the exact
  selection, resolved root, observer-control source, sequence, epoch, phase,
  observed time, queue boundary, barrier lineage, replacement manifest,
  capability/support-release evidence, RFC 012A source and scope coverage,
  canonical explicit errors, and artifact-availability snapshot. Context
  construction failure leaves the offered barrier queued. Rust reconstructs
  both typed barriers and recomputes their BLAKE3 snapshot and event identities;
  portable TypeScript consumes only the Rust-issued context and independently
  enforces the strict nested shapes, bounds, and opaque-identity equality. The
  barrier admission path was tightened with the wire slice: every negotiated
  fact family now requires matching coverage/completeness even when empty, and
  every coverage set must name the resolved root and the capability report's
  one selected support release, so an invalid barrier cannot be accepted and
  later wedge the consumer. A privacy-reduced Rust fixture covers both clean
  bootstrap and resync at equal state. This adds no N-API method, portable
  observer owner, unknown-event preservation, native source access, or task/
  artifact discovery authority; and
- the contextual poll-watermark slice (`c28bdef`) now freezes one completed
  poll's offered boundary without turning request generation or a native clock
  into semantic progress. A process-local attachment authority is retained by
  every captured watermark, and only the exact owning host can mint its non-
  Serde consumer context. That context binds the negotiated selection and
  support release, full resolved root, exactly one Decode set plus exactly one
  set for every selected fact family, declared scope coverage, canonical
  explicit errors, artifact-availability revision, epoch, offered sequence,
  and queue state. Only `Bootstrap` and `Valid` continuity can cross the wire;
  resync-required, resyncing, and failed states remain on the control lane.
  The portable queue law requires the offered/delivered difference to equal
  the retained semantic plus source-control item counts, while retained native
  bytes remain accounting rather than sequence. Serialize-only Rust and strict
  TypeScript parsers reject cross-attachment context, selection/root/support,
  coverage, nested semantic-state, queue, bound, or unknown-field drift against
  one privacy-reduced fixture. This adds no source-access authority, request-
  generation field, unified event union, N-API method, iterator owner, native
  payload/locator disclosure, or public observer transport; and
- the contextual poll-completion slice (`5854c5f`) now carries that frozen
  watermark through the real request-local poll runtime rather than leaving
  wire construction as a detached contract test. After a ticket resolves, the
  owning host re-resolves the exact ticket before minting one non-Serde,
  attachment-bound completed-poll value containing the strict context and
  watermark wire. Coalesced pre-reservation calls retain the same core and
  exact portable values; a call admitted after reservation remains pending for
  its follow-up pass, and an unchanged follow-up may serialize the same
  semantic watermark without importing its distinct flow-control generation.
  Foreign tickets fail before context minting, while cancellation and terminal
  failure remain typed non-watermark results. The async handle exercises this
  same contextual path, and its redacted result exposes no request generation,
  support/program digest, native locator, payload, or clock. The prior raw-core
  poll substrate remains internal for source-owner orchestration. This adds no
  unified event union, unknown-event preservation, N-API/SDK observer method,
  public iterator owner, or source-access authority; and
- the contextual continuity-control slice (`b88c902`) now mints one non-Serde,
  attachment-bound consumer context from the real ordered delivery lane before
  dequeue for `observer.resync_required`, `observer.resync_started`, and
  `observer.failed`. Required and started controls bind the exact completed
  baseline plus delivered invalidation lineage; terminal failure retains the
  completed bootstrap/replacement baseline when one exists, while a failure
  before the first completed bootstrap serializes an explicit `null` instead
  of fabricating snapshot authority. Rust and portable TypeScript enforce the
  same phase/baseline law, exact selection/root/control source, epoch,
  contiguous watermark, and strict nested control shapes. Context construction
  failure leaves the queued control untouched, attachment identity remains
  process-local and redacted, and completion barriers continue through their
  separate contextual contract. This adds no unified event union,
  typed-unknown preservation, public iterator/N-API method, native source
  access, or portable attachment authority; and
- the known-envelope dispatch slice (`612e680`) now projects every currently
  implemented ordered event through its existing strict usage, actor, source,
  artifact-availability, completion, or continuity contract before dequeue.
  The non-Serde, non-cloneable wrapper retains the exact process-local
  attachment authority, redacts both wire and context values from diagnostics,
  and binds common-family values to the selected contracts, resolved root, and
  exact delivered source. Families with specialist consumer authority require
  exactly that context; missing, stray, unselected-enrichment, or mapped-
  delivery drift fails while the offered event remains queued. Exhaustive Rust
  dispatch makes a newly added internal event variant a compile-time decision.
  This is deliberately a known-event multiplexer rather than the complete
  portable event union: no bounded `unknown_wire_event` carrier, typed-unknown
  negotiation, TypeScript union, N-API iterator, native source access, or
  portable attachment authority is added; and
- the portable known-envelope slice (`1bda3ee`) now freezes a strict outer v1
  discriminator and dispatches those same six implemented families through
  their existing contextual TypeScript parsers. The Rust wrapper remains
  non-Serde and attachment-bound, exposing only an owned authority-free value;
  the shared privacy-reduced fixture binds the canonical family spelling and
  one source-lifecycle example. Unknown outer fields, family/context/event
  drift, and unrecognized family discriminators fail closed. This is still not
  the additive event-union contract: `unknown_wire_event` negotiation and
  bounded preservation, native/N-API delivery, iterator ownership, and
  portable attachment authority remain absent; and
- the typed-unknown event-union slice (`765dda4`) now adds a separate v1
  sidecar negotiation bound to the exact already-negotiated observation
  selection. Selection requires type-tag, encoded-value, and envelope-
  provenance preservation, takes the smaller requested/offered byte ceiling,
  and caps it at 64 KiB. The non-Serde Rust carrier and strict TypeScript mirror
  preserve an uninterpreted canonical type tag, bounded JSON value, and exact
  source/sequence/epoch/generation/phase provenance without allowing an
  unknown tag to shadow any current known event. Depth, node, object-key,
  JavaScript-safe-integer, Unicode, and exact encoded-byte checks are
  executable, including bounded output-array allocation before cloning. A
  complete portable outer union routes known branches through their existing
  specialist parsers and admits `unknown_wire_event` only with the exact
  caller-held sidecar selection and authorized source context. The runtime
  still has no internal unknown-event variant and does not retain the sidecar,
  emit unknown events, or expose native/N-API iterator transport; and
- the attachment-binding slice (`89a0d3b`) now accepts that optional sidecar
  request/offer as one trusted Rust-host input, negotiates it immediately after
  the base observation contract and before artifact policy, support selection,
  grant validation, or source authority, and retains the exact result on the
  non-cloneable attachment host. Explicit absence preserves known-only
  operation; incompatible preservation fails before an invalid adapter or
  artifact policy can be consulted. This does not add an internal unknown-event
  producer, accept caller-created carrier values, read a source, or expose a
  native/public transport; and
- the consumer-binding slice (`b9d8824`) shares that immutable optional
  selection from the attachment into its sole bounded event drain, preserving
  exact selected and explicit-absent modes without copying retained payload
  state. A real dequeued known event now exposes the complete portable outer-
  union value directly while retaining its narrower specialist envelope and
  application receipt. The runtime still produces only known branches; the
  sidecar cannot manufacture an unknown event, widen source authority, or act
  as transport ownership; and
- the attachment-bound unknown-delivery slice (`71afae4`) adds the first
  crate-private trusted producer without widening source or transport
  authority. Preparation consumes one opaque producer occurrence and binds its
  canonical type tag, bounded JSON value, optional semantic revision, exact
  declared relation/source generation, active scope epoch, attachment, and the
  identical negotiated sidecar before sequencing. The drain revalidates those
  process-local authorities, charges the exact preserved JSON bytes, and
  returns the same non-cloneable offer on every backpressure or lifecycle
  failure so retry cannot renumber or silently discard it. Dequeue emits the
  already-frozen `unknown_wire_event` outer branch, while specialist known-
  family access becomes explicitly optional and preserved values remain
  redacted from diagnostics. The same slice removes native path material from
  `ScopedKnownObjectGrant` diagnostics. No concrete adapter produces this
  event yet, and no N-API/SDK iterator, public attachment authority, native
  source read, or semantic reinterpretation is added; and
- the runtime-bound contextual-close slice (`5f494a6`) now prepares one exact
  attachment/root/selection-bound close command before the async runtime opens
  its sole consumer drain and retains that command with the shared runtime
  owner. Raw barrier close, contextual close, cloneable-handle close, and
  drop-triggered cancellation all close that same drain through the retained
  binding. Repeated and concurrent callers share one lifecycle barrier and
  stable request identity; a strict receipt is available only after watcher
  and native operations have acknowledged cancellation and the drain has
  closed. The existing public shape is unchanged: the receipt remains
  crate-private, `close()` has no N-API/SDK transport yet, and this adds no
  source access, dynamic scope, unknown-event producer, or portable attachment
  authority; and
- the declared decoder-dependency slice (`a7cfcb6`) now resolves an adapter's
  object dependency only when its access root plus canonical object key names
  one unambiguous `KnownObject` grant in the exact current scoped pass. The
  complete relation set and every established object access identity are
  checked before the first native read, and dependency reads plus canonical
  revalidation consume the same declaration-sized pass ledger. Stable,
  missing, and oversized objects use the same common dependency-revision
  domains as durable decode. Every dependency actually used during decoding
  is re-read before decoder state is staged; change or instability is a
  transient whole-batch failure, so discard/retry advances neither the primary
  cursor nor decoder state. Live polling and automatic replacement replay use
  this mediator, while direct/manual decode remains dependency-denied.
  Omitted, duplicate, ambiguous, foreign-root, escaping, over-bound, and
  cross-relation-forged requests fail without exposing native paths or values.
  Parameterized database queries and object listings remain denied, and this
  adds no public N-API/SDK surface, concrete adapter dependency, dynamic scope,
  or reinterpretation of RFC 012D unknown-wire events; and
- the explicit-resync future slice (`cf0bfa4`) now gives the existing
  crate-private async attachment handle one clock-owned `resync()` operation.
  A consumer can mint only `ExplicitConsumerRequest`; concurrent calls join
  the exact sticky required or already-started lineage instead of advancing
  another epoch. Resolution observes the engine-offered replacement barrier,
  while the sole ordered event drain still owns delivery and application of
  `observer.resync_required`, `observer.resync_started`, and
  `observer.resync_complete`. A later re-overflow may satisfy the original
  waiter only through its strictly newer completed epoch. Bootstrap-incomplete
  requests do not mutate continuity, and terminal observer failure remains
  distinct from close cancellation. Integrated owner-pair tests exercise all
  of those boundaries without consuming the application receipt. This adds no
  N-API/SDK method, caller clock or reason input, dynamic scope, source policy,
  decoder-defined query/listing authority, or replacement-algorithm change;
  and
- the consumer-applied readiness slice (`eb68a6a`) now gives that same async
  attachment a retained `ready_applied()` boundary distinct from engine
  `ready()`. Its completion is constructed with the sole event drain and
  resolves only after the exact `observer.bootstrap_complete` application
  receipt advances consumer state. Merely offering or delivering the barrier,
  applying an earlier envelope, or presenting a foreign, stale, or mismatched
  receipt leaves it pending. Failure and close wake unresolved callers with
  distinct outcomes, while neither can revoke an already-applied historical
  bootstrap boundary. The waiter observes application state only: it consumes
  no envelope and invokes no consumer reducer. This remains crate-private and
  adds no SDK helper, N-API transport, source access, policy, dynamic scope, or
  resync-application claim; and
- the consumer-applied resync slice (`62e6a0b`) now adds the corresponding
  `resync_applied()` boundary without weakening `resync()`'s engine-offered
  meaning. Both operations issue or join the same exact explicit replacement
  lineage, but the applied form resolves only after the sole ordered event
  drain acknowledges the matching `observer.resync_complete` application
  receipt. A previously applied epoch cannot satisfy a new invalidation, while
  a re-overflowed later epoch can. Failure and close remain distinct terminal
  outcomes. If a completion envelope was already delivered before a later
  observer-failure control, its earlier receipt may still establish that
  historical applied boundary in total delivery order; an undelivered or
  foreign barrier cannot. The retained state is attachment-owned, non-Serde,
  and O(1), and the waiter neither consumes events nor invokes a reducer. This
  remains crate-private and adds no N-API/SDK method, native source access,
  policy authority, dynamic scope, or replacement-algorithm change; and
- the three-observer isolation checkpoint (`cb339a8`) now runs three
  simultaneous async owner pairs for distinct canonical root sessions on one
  executor. One observer deliberately leaves its source-control delivery
  undrained and enters a watcher-overflow replacement handoff; both healthy
  siblings still complete exact-scope polls, retain `Valid` continuity,
  deliver and acknowledge their own sequence-2 controls, and close only their
  own watcher owners. Root identity, queue state, poll completion, application
  receipts, resync state, close barriers, and backend lifetimes remain
  attachment-local. This closes the executable state-isolation case only: it
  does not ratify p99 latency, global scheduling policy, or calibrated
  starvation bounds; and
- the native-rescan continuity checkpoint (`d58ea42`) now maps a live
  `notify` rescan signal to sticky attachment-local `WatcherOverflow`
  invalidation instead of an ordinary poll request. Bootstrap-time rescan
  remains a coalesced watcher-before-scan reconciliation hint. The retained
  watcher owns the live loss across supervision-future recreation and backend
  recovery, then clears it only after the exact resync control is accepted.
  If another signal arrives while a replacement-start control is offered but
  not delivered, the watcher snapshots delivery-capacity generation, retries
  before waiting, and drives a strictly newer replacement after dequeue
  without losing the wakeup. Integrated coverage proves unchanged poll demand,
  the exact overflow reason, fresh-epoch replay, later polling, and orderly
  close. This adds no public API, dynamic scope, policy value, or performance
  claim; and
- the disappearance-replacement checkpoint (`d57e7c3`) now exercises the
  automatic owner path from an applied, present epoch-1 root through live
  watcher overflow to a complete absent epoch-2 snapshot. The replacement
  barrier carries `root_present = false`, complete per-family zero-entity
  manifests, explicit absent source coverage with no retained point or error,
  and a matching absent scope relation; the rebound owner then serves a valid
  absent-root poll and closes normally. This proves the D-owned static-root
  disappearance case without cross-epoch tombstones. Dynamic membership,
  semantic families not yet selected, and public consumer state replacement
  remain open; and
- the architecture checker forbids store/query/N-API/concrete-adapter imports
  and premature native public export from the provisional composition and
  negotiation roots, while keeping the portable negotiation graph contract-only.

D1 remains `In progress`: multi-object discovery/cursor orchestration,
parameterized-query and object-listing decoder dependency composition, built-in
canonical fact-revision adoption beyond the current runtime families, scoped
reducers beyond usage-v2, coverage-complete durable query exposure, selected
and portable actor/affiliation replacement integrity, event variants outside
the current usage, actor, source-lifecycle, artifact-availability, continuity,
and completion specialist families, concrete adapter production and emission
of negotiated typed-unknown events, the public N-API/SDK iterator transport
for the current contextual watermark and event contracts over the internal
async lifecycle runtime,
portable dynamic/discovered scope coverage beyond the current exact
known-object relation/root summary, dynamic/discovered scope membership beyond
the attachment's current exact known-object grants and family coverage beyond
usage-v2,
complete multi-family replacement manifests, whole-scope discovery and source
state beyond the current exact append-object set, remaining portable-host
wiring and policy calibration, dynamic/non-append replacement composition,
native artifact-access mediation and the public artifact/close-method
transports,
the trusted native version-probe/identity-input drivers, and the complete
public request are not yet implemented. The internal offered and applied
boundaries are now transactional, but their contextual wire cannot become a
public watermark and consumer-ready helper until dynamic scope membership/
coverage, runtime-bound public close invocation, the native transport owner,
and the remaining negotiated lifecycle surface are defined. The usage-v2 sink
and delivery lane remain crate-private until the remaining lifecycle surface,
concrete unknown-event production, and runtime-bound negotiated portable
transport exist.

### D2. Claude scope composition

Compose the root plus existing/future standard children, workflow runs/journals
and children, referenced team/member/inbox objects, relevant tasks/plans,
presence, and policy-allowed artifact locators through RFC 012A scope
primitives.

Hooks remain in Chopsticks as root lifecycle and immediate-poll signals during
this package.

Current landing status (2026-08-21): D2 remains `In progress`. Decoder-executed
`claude_root_child_workflow_and_team_compose_typed_facts_not_unknown_records`
now also decodes the team-inbox sidecar to `Fact::TeamInboxSnapshot` rather
than `UnknownRecord`, and still proves typed root/child/workflow/team facts.
`rfc012_d2_observer_composes_claude_root_current_future_and_sidecar` attaches a
synthetic promoted fixture program with Claude-shaped root-transcript,
current-child, future-child, and team-inbox-sidecar grants. Future-child is missing at
bootstrap (attach-before-create), then root/current/sidecar appear, then the
future child is created later; each object keeps a distinct source identity.
`rfc012_d2_host_composes_child_directory_membership_before_bootstrap_completion`
now authorizes a Promoted fixture program that declares a KnownObject root plus
a `ChildDirectoryByNativeId` descendant relation, binds the declared
`descendant-stream`, scans membership, drains member reads, and records that
directory checkpoint as AccessAttempt evidence before bootstrap completion.
Bootstrap without membership remains `IncompleteScopePass`. Candidate support
still cannot carry a Promoted dynamic program. This is not Candidate-program
authorization or a Claude support status flip. No Claude release is promoted;
the candidate holds the full RFC 012D relation set but cannot authorize it.
The former `9f9749b` authorize-time rejection
of uncomposed observation primitives is replaced by bootstrap-complete-time
membership evidence for those relations. Evidence-derived artifact relations
remain on their separately bound availability contract. Sibling, referenced,
index, namespace, and parameterized-query composition, and Claude Candidate
directory promotion, remain later work.

Commit `8253e51` adds the first common representation needed behind that guard
without opening it. One complete bounded `DirectoryCheckpoint` may now be
retained as the membership-source observation for one declared dynamic
relation. It contributes one `Decode` point with `SnapshotRevision` and
`ExactSnapshot`; discovered children are not fabricated as additional declared
relations and must retain their own source coordinates when the D2 owner is
implemented. Admission is limited to a drained bootstrap/correction boundary,
shares the existing pre-retention coverage-object ceiling, and rejects zero
pass/generation values, live insertion, source/relation retargeting, and
known-object collisions. The convenience scope revision remains independent
of cursor/snapshot positions, while the bootstrap/replacement snapshot digest
binds the exact `SourceCoverageSet` and therefore changes when the directory
revision changes. The host now retains the full declared observation-relation
set for that future assembly, but the `9f9749b` rejection still blocks every
non-`KnownObject` observation primitive before source access. No Claude
directory read, child-object admission, watcher, live membership transition,
public transport, capability upgrade, or promotion is implemented by this
slice.

Commit `1ccb0dd` adds the matching pre-I/O locator authority. A
`ChildDirectoryByNativeId` reservation can render its declared template only
from the exact ordered identity values that minted that reservation's opaque
object token; substituted values, another primitive, path separators,
controls, non-UTF-8 bytes, absolute/drive paths, and `.`/`..` components fail
closed before a native root is joined. The result is only a confined relative
path. Future attachment root validation also requires the exact access-root set
for every observation relation, rather than deriving authority from the
known-object subset. The dynamic-relation promotion guard still executes first,
so this seam cannot open or enumerate a directory, construct a watcher, or
authorize the Candidate package.

Commit `a2bb82d` closes the common-driver fan-out prerequisite without opening
that locator. `DirectorySnapshotConfig` now carries a per-directory entry
ceiling in addition to its aggregate retained-entry ceiling. Each native entry
yielded by `read_dir` consumes that per-directory bound before selector logic,
full metadata, recursion, or checkpoint retention, including entries the
selector would ignore. The first excess entry aborts the scan instead of
returning a partial snapshot. Checkpoint restore also rejects a retained parent
whose children exceed the active per-directory bound, and every existing
catalog/coordinator declaration preserves its prior behavior by selecting an
explicit bound no broader than its aggregate ceiling. This is a driver safety
precondition only: no scoped relation invokes the driver, no entry receives a
child access token, and the dynamic-relation promotion guard remains closed.

Commit `91aa1d1` adds a descriptor-confined directory-snapshot path for the
currently evidenced POSIX platform without attaching it to RFC 012D. The
approved access root and rendered relative locator are opened component by
component with `openat`, directory entries and descendants are opened relative
to retained directory descriptors with no-follow semantics, and symlink or
unsupported entry kinds fail closed. The confined path counts the entire
enumerated set before selection, retains binary-safe relative identities, and
revalidates every traversed directory handle plus its descriptor-confined path
before returning a checkpoint; a membership mutation yields a transient retry,
not a complete partial snapshot. Platforms without that descriptor primitive
fail closed. The ordinary catalog driver remains behaviorally unchanged. No
scoped access pass calls this method yet, so this commit does not authorize a
listing, interpret a Claude selector, admit a child object, or relax `9f9749b`.

Commit `f343497` closes the same pre-I/O locator rule for `SiblingObject` and
`ReferencedObjectFromField`. A reservation for either primitive may render its
fixed locator only from the exact ordered identity values that minted the
reservation token; value substitution, another primitive, traversal,
separators, controls, non-UTF-8 bytes, and unconfined output fail with the same
privacy-safe contract error used by the other locator authorities. The result
is still only a relative path. There is no source-stream/decoder binding for
these relations yet, and neither this seam nor the confined directory driver
is called by an attachment.

Commit `dde701b` adds that source-selection authority to the common declaration
contract without attaching a source. An `observation_binding` names one exact
stream plus one exact pattern from the digest-bound source declaration;
`ChildDirectoryByNativeId` additionally carries a non-caller-controlled
selector relative to its confined rendered locator. The locator template and
selector must compose byte-for-byte to the declared source pattern, while a
referenced-object locator must compose directly to its pattern. Placeholders
must be unique declared identity inputs, and patterns are bounded canonical
star-only relative paths. Cross-document verification also requires the same
root, existing scoped topology, a complete object-revision or append-cursor
lifecycle, and source record/object bounds within the relation byte budget. A
Promoted dynamic relation cannot omit this binding; primitives whose execution
law is still undefined cannot declare one. The already-landed promotion guard
is exercised with an otherwise fully bound child relation and still rejects it
before access. No current Candidate document was rebound, no adapter invokes a
selector or source driver, no child receives an access token, and `9f9749b`
remains closed.

Commit `eabef8d` applies that declaration contract to the incomplete Claude
Candidate without making it executable. Eleven concrete dynamic or
related-object relations now name an exact existing scoped stream and source
pattern; the three directory relations additionally carry confined relative
selectors that compose byte-for-byte with their canonical identity-input
locator templates. The workflow-child selector is retained as a narrower,
non-widening pattern on the existing subagent stream, and the adapter
conformance test now derives runtime root, pattern, decoder, and topology
checks from the digest-bound declarations instead of a parallel selector
table. Source, scope, evidence, and conformance hashes are rebound, but the
release remains Candidate, the scope program remains incomplete, the scoped
capability remains unsupported, observation contract versions remain empty,
and `scope-access` remains planned. The conceptual task-artifact relation is
still unbound. No attachment invokes a source driver, no native source is
opened, no child access token or watcher is minted, and `9f9749b` remains
closed.

Commit `71c3f1e` adds the next authorization boundary behind that closed guard.
Only an `AuthorizedScopeAccessPlan` selected from typed scoped-support
authorization can mint a non-serializable observation-source reservation for
`ChildDirectoryByNativeId`, `SiblingObject`, or
`ReferencedObjectFromField`. The reservation retains the exact
declaration-owned stream, pattern, optional relative selector, access-root
label, confined rendered locator, opaque object token, and support-release,
source-declaration, and scope-program digests. Stream and digest coordinates
cannot be supplied by the caller; the confined locator and token are derived
once from the exact validated identity values in the bounded request. Unknown
and unsupported relations fail through one path-free error before touching the
access ledger, malformed identity locator material fails before a reservation
can escape, and Debug withholds identity, locator, and stream values. The
existing host test still proves that the same fully bound child relation is
rejected before attachment. This is only a pre-I/O common seam: it neither
joins the relative locator to a host root nor calls a driver, admits a
discovered child, installs a watcher, changes Candidate support, or relaxes
`9f9749b`.

Commit `9238ac2` prevents that reservation from degrading back into a stream
name plus a digest. Support-bundle verification now retains a closed,
non-serializable driver contract for every observation-bound scoped stream:
`AppendDelimited` carries its exact record/batch bounds, while
`ReplaceDocument` and `PresenceObject` carry their exact object bound. The
contract is retained inside typed access authorization and may enter an
authorized scope plan only when the selected program relation still names the
same verified stream and access-root label. The reservation therefore carries
the driver kind and bounds checked against the source document's scoped
topology, existing implementation state, lifecycle, safe decoder-state
boundary, relation budget, and source-declaration digest; later code cannot
reconstruct stronger source semantics from a string or digest alone.
Unrecognized driver kinds and lifecycle/boundary drift fail bundle
verification. This still selects no adapter decoder, joins no native root,
opens no source, and changes no Candidate capability or promotion guard.

Commit `b8f308e` binds that closed declaration contract to one actual adapter
runtime stream without opening the source. Digest verification now also
retains the declaration's exact ordered pattern set, decoder ID, fact
authority, and append per-batch record ceiling. A source reservation can be
consumed into a non-serializable runtime-stream reservation only when the
adapter manifest still carries the same support release, source declaration,
and scope-program binding; the supplied source instance has one unambiguous
declared access-root label; and exactly one returned `StreamSpec` matches the
declared stream, root, full include set, empty exclude set, decoder, authority,
common-driver bounds, consistency policy, and mirror-source deletion policy.
Missing or duplicate streams and any package, instance, selector, decoder,
authority, lifecycle, or bound drift fail through one path-free error and
consume the access reservation conservatively. The retained binding carries
the opaque source-instance key and its identity-contract version for later
topology-neutral source identity, but it still carries no approved native root
and invokes no listing, object driver, decoder, or watcher. The attachment does
not call this seam while the dynamic-relation guard is closed; Candidate
documents, capability status, and promotion state remain unchanged.

Commit `db947dc` joins that runtime-stream reservation to the exact native root
already approved by the active scoped attachment, still without opening it.
Each access pass retains the attachment-selected adapter, and the resulting
non-serializable, pass-borrowed reservation requires the supplied source
instance ID, identity-contract version, opaque stable key, derived canonical
source-instance key, access-root label, unique declared root, and native root
path to match the attachment and runtime binding exactly. The approved root
and confined relative locator remain separate values. Root substitution,
source-instance substitution, foreign canonical identity, and attachment close
before or during the join fail through stable path-free errors and consume any
already-minted common reservation conservatively; Debug exposes only presence
flags. This seam performs no source open, listing, read, driver execution,
decode, child admission, or watcher installation. Catalog-discovery provenance
for the source instance and evidence-owned identity inputs for later locator
execution remain open, the dynamic-relation guard stays closed, and Candidate
documents, capability status, and promotion state remain unchanged.

Commit `eddecc2` removes the remaining per-pass source-instance substitution
from that join. The trusted attachment request now supplies one exact
`SourceInstance`, and authorization retains it only when its nonzero runtime
ID, identity-contract version, raw stable discriminator, derived canonical
source-instance key, unique root-name set, and every root path equal the root
identity and host-approved access-root grants. Each later pass borrows that
same retained instance; the runtime-source reservation no longer accepts an
instance argument. Identity, version, root, duplicate-root, and path-shaped
drift fail through one stable non-disclosing error, and the attachment request's
Debug implementation exposes counts and presence flags rather than source keys
or native roots. This is still a trusted composition input rather than a wired
catalog-discovery result, and it performs no source open, listing, read, driver
execution, decode, child admission, or watcher installation. The dynamic
relation guard remains closed and Candidate support is unchanged.

Commit `2870468` compiles the first dynamic relation into an exact pre-I/O
directory-membership contract. The durable coordinator and scoped contract now
share one component-aware byte selector, where `*` stays within one component
and only a whole-component `**` recurses. Only an authorized
`ChildDirectoryByNativeId` `ObjectListing` reservation may produce this
non-serializable, pass-borrowed contract. Its `DirectorySnapshotConfig` comes
directly from the relation's exact maximum object, fan-out, and depth bounds;
its relative selector is the declaration-bound selector; and its path-free
membership identity binds the adapter, program, canonical source instance, and
opaque relation object token. Invalid primitive, operation, selector, or bound
material fails conservatively before native access. This commit still does not
open or enumerate a directory, read an object, mint a discovered-child access
reservation, account an enumerated entry, admit a member, or install a watcher.
Child access accounting and per-entry audit semantics therefore remain open,
the dynamic-relation guard stays closed, and Candidate support is unchanged.

Commit `6592ff1` closes that pre-I/O child-accounting contract without invoking
the directory driver. Because the listing root is already one accessed object
at depth one, the confined scan configuration now receives the remaining
`max_objects - 1` child capacity, fan-out capped by that remaining capacity,
and `max_depth - 1` recursion levels. Each future yielded entry must reserve a
domain-separated opaque token derived from the exact listing-root token and
binary-safe relative path identity before it can become membership evidence.
Its trace edge binds either the listing root or a previously completed
directory entry as parent; missing/file parents, duplicates, depth, fan-out,
aggregate object overflow, abandonment, and path-shaped or cross-pass
substitution fail closed. An in-memory root authority binds the exact budget
instance and reservation sequence, so another pass with equal visible
coordinates cannot spend it. The audit retains only child tokens and entry
kinds, never relative names or native paths. The confined scan still does not
call this contract, so recording every yielded entry before metadata/open,
completing the listing from an exact checkpoint, child content-read authority,
membership admission, and watcher installation remain open. The dynamic-
relation guard stays closed and Candidate support is unchanged.

Commit `51c766b` connects that accounting contract to the common confined
directory driver's enumeration boundary. An entry-audit reservation is now
created immediately after the descriptor-backed directory stream yields a
name and before per-directory or aggregate bound checks, `statat`, selection,
or any no-follow child open. The reservation itself owns selection for the
verified kind; ignored files are therefore accounted rather than disappearing
behind the selector. Selected entries complete only after the no-follow child
descriptor and its metadata confirm the same kind, and no checkpoint state or
recursive work is retained until audit completion succeeds. Limit rejection,
disappearance, symlink/unsupported kind, open or metadata failure, and explicit
audit failure abandon the live reservation and take its conservative failure
path. A selector-only adapter preserves existing durable confined-scan
behavior, while the scoped directory-membership contract implements the same
borrowed interface with its exact root authority, opaque child tokens, parent
edges, and precompiled declaration selector. Tests exercise ignored names,
reserve-before-stat mutation, the first excess yielded name, symlink escape,
and the existing scoped parent/fan-out/depth/aggregate/abandonment invariants.
This still does not turn the enumerated checkpoint into admitted membership,
read discovered child contents, complete a whole listing, install a watcher,
or open the dynamic-relation guard. Candidate support and promotion state are
unchanged.

Commit `8d71360` lets that exact contract consume the audited driver result and
mint the first authorized scoped directory-listing evidence. Refresh authority
is non-serializable and bound to the exact in-memory attachment, relation and
path-free source coordinate, support-release/source-declaration/scope-program
digests, compiled selector and scan bounds, and listing-root token. A foreign
attachment or changed session coordinate is rejected before native I/O even
when its visible package coordinates are equal. The driver receives only the
attachment-approved native root plus the declaration-rendered relative
locator. Missing roots complete the root access as `Unavailable`; transient
mutation abandons the pass for retry; and driver or audit failure consumes the
root conservatively through stable errors that cannot echo native paths. A
successful snapshot is accepted only when every retained checkpoint entry
re-derives to one selected, kind-matching opaque audit token and the number of
retained entries equals the completed selected-token set. Only then does the
listing root complete as `Available`. The resulting listing hides paths in
Debug and must be consumed to construct complete dynamic-relation coverage;
the former production constructor that accepted an arbitrary
`DirectoryCheckpoint` plus caller-supplied source identity no longer exists,
and the architecture ratchet freezes that boundary. Tests cover initial scan,
same-attachment refresh, source and attachment substitution, missing roots,
path-free symlink failure, access-ledger outcomes, and listing-backed coverage
admission. The scoped host still rejects the uncomposed dynamic relation: no
discovered child is admitted or read, no live membership transition is
published, and no watcher is installed. Candidate support and promotion state
remain unchanged.

Commit `8c53831` binds selected children of that listing to their first bounded
content read. The common access seam seals the exact in-memory root authority
after a successful audited pass and remembers which opaque child coordinates
completed as declaration-selected files. Only an available
`ChildDirectoryByNativeId` stream whose verified driver is `ReplaceDocument`
can mint the resulting non-serializable read authority; its per-object byte
ceiling comes from that closed driver contract. Each selected child may reserve
exactly one `ObjectRead` with the original parent edge and depth, while ignored
files, directories, fabricated tokens, replay, another root authority, and an
unavailable listing remain non-authorizing. The scoped layer derives reads in
canonical checkpoint order without accepting a caller path or token. It
reconstructs the exact native relative name from the checkpoint's framed
binary key, rejects noncanonical/truncated/separator-smuggling keys, reserves
the full byte bound before I/O, and uses the descriptor-confined stable reader
without following symlinks. A returned file stamp must still equal the listing
identity, length, and modification time: disappearance, same-path replacement,
or instability invalidates the listing for retry, while a stable oversized
file is retained only as an explicit `Oversized` access outcome. Native driver
errors consume the reservation conservatively through one path-free error.
Dynamic-relation membership cannot enter admission until every selected read
has reached a stable or explicit oversized outcome; admission then destroys
the native root and read authority before retaining the listing proof. Tests
cover stable nested reads and exact accounting, unread-listing rejection,
same-path replacement, stable oversize, post-listing symlink substitution,
selector exclusion, one-shot replay, malformed binary keys, and non-UTF-8 key
round trips. This still invokes no declaration-owned decoder, constructs no
discovered-child source identity, publishes no live transition, installs no
watcher, and does not relax the dynamic-relation promotion guard or Candidate
status.

Commit `39cf02e` gives each completed selected read its exact topology-neutral
source coordinate without opening a decoder. The member identity binds the
same adapter, canonical source instance, declaration-owned stream ID, full
access-root-relative binary object key, and framing version used by durable
`FactSemanticContext`; it also retains the originating attachment, relation,
opaque child token, listing generation/revision, and entry generation/revision
behind a path-free Debug contract. Membership finalization re-derives every
source from that semantic context, requires one unique exact source per
completed selected child, and destroys the native root/read authority before
returning the set. The drained admission lane reserves both the membership
source and every selected child against its coverage-object ceiling, stopping
before retaining the first excess source. It rejects one child reserved by two
relations and a refreshed set that omits an already-known child, preserving the
ordered-retraction requirement for the future live owner. The existing append
admission path explicitly rejects these reserved document identities: a
matching declaration-owned child driver/decoder and lifecycle state must land
before any child can enter projection. Tests bind nested and root children to
the durable source keys and exact entry revisions, freeze path-free identity
Debug, and prove exact-set capacity, cross-relation exclusion, and omission
failure. No member decode, object state, live transition, watcher, public API,
promotion-guard relaxation, or Candidate status change is included.

Commit `d9d2b12` binds the path-free member identity to the exact native
decoder descriptor without invoking the adapter. Every selected child now
retains the already-verified runtime `StreamSpec` and a durable-style
`SourceObjectDescriptor` whose stream ID and binary object key match the
member's semantic context and whose full relative path was constructed only by
the authorized listing owner. One shared immutable stream contract is reused
across the bounded member set rather than reconstructed from caller strings.
Stable and explicit oversized outcomes carry the same non-serializable binding;
its path-bearing descriptor is visible only to the scoped owner (plus focused
test inspection), and all Debug surfaces disclose presence flags and bounded
counts rather than stream, relation, or native path material. Tests prove the
root and nested descriptor coordinates, exact verified runtime stream, and
oversized-path retention while freezing that redaction. No adapter bootstrap,
dependency access, decode, child lifecycle state, admission, watcher, public
API, promotion-guard relaxation, or Candidate status change is included.

Commit `a951d2d` removes caller-supplied adapter and source-instance inputs from
that future bootstrap join. The runtime-source/root binding now retains the
exact attachment-owned `Arc<dyn AgentAdapter>` and `Arc<SourceInstance>` that
were used to verify the authorized stream; adapter manifest identity is checked
alongside the existing numeric instance, identity-contract, stable-key,
canonical-key, root-label, and native-root equality before the binding can
escape. Each selected member receives those same immutable owners with its
runtime stream and descriptor, so stable and oversized outcomes cannot later
be bootstrapped against an equal-looking foreign adapter or reconstructed
source instance. Native roots and stable keys remain behind custom Debug
surfaces, and focused tests prove exact adapter pointer and source-instance
retention plus all prior substitution failures. This still does not invoke
adapter bootstrap or dependencies, decode a record, construct child lifecycle
state, admit projection, install a watcher, relax the promotion guard, or
change Candidate support.

Commit `4503e3b` invokes object-context bootstrap for the first time, but only
through a new explicit dependency-free adapter opt-in. The trait default fails
closed, Claude opts in by reusing its pure path-to-context parser, and the
common decode boundary contains adapter panics without retaining or returning
their payload. Before invoking that boundary, the scoped owner revalidates the
exact retained source instance, declaration-owned `ReplaceDocument` stream,
selector, descriptor stream/object coordinates, durable semantic source, and
framing version. Success consumes the member content into a nonconstructible
decode input that retains the exact adapter, source instance, stream,
descriptor, context, content revision, and bytes. An invalid binding or adapter
failure returns only a bounded decode-failure class while preserving the same
content for retry; custom Debug implementations expose counts and presence
flags rather than paths, native identifiers, context bytes, or payloads. Tests
prove default denial, Claude opt-in parity at every declared coordinate, panic
redaction, exact success binding, and path-free retry retention. No record has
yet entered the declaration-owned `ReplaceDocument` driver or decoder, no
child lifecycle state or projection admission exists, and watcher, public API,
promotion-guard, and Candidate status remain unchanged.

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

Current landing status (2026-08-21): D3 is `In progress`.
`rfc012_d3_durable_queries_progress_while_search_bootstrap_is_incomplete`
proves durable overview queries run while search is still
`BootstrapInProgress`.
`rfc012_d3_shared_pass_pool_serializes_catalog_like_work_and_observer_pass`
proves one `SharedSourcePassPool` permit serializes a catalog-labelled hold
with an observer source pass: the observer poll waits until that permit is
released.
`rfc012_d3_engine_catalog_query_workers_wait_for_shared_source_pass_pool`
proves engine catalog/query workers `blocking_acquire` that same caller-owned
pool: a held permit stalls `overview()` until release, and shutdown does not
acquire so disposal cannot deadlock behind a held permit. Task complete-snapshot
replacement already retracts missing items.
`rfc012d_user_input_request_replaces_one_lifecycle_entity_without_duplicates`
projects C1 `runtime.user-input-request` as a correlated-lifecycle family:
pending→resolved revises one entity, complete retract removes it, partial
retract does not, and bootstrap/correction replacement digests match at the
same current set. The replacement representation is
`correlated_lifecycle_current`.
`rfc012d_message_replaces_one_generation_log_without_duplicate_entities`
projects C1 `runtime.message` as `CurrentGenerationLog`: a correction revises
one message, a partial block list cannot drop a previously known key, a
complete block list retracts absent keys, complete retract removes the
entity, and bootstrap/correction replacement digests match at the same
current set.
`rfc012d_task_replaces_one_revisioned_entity_and_omits_from_complete_owned_set`
projects C1 `runtime.task` as revisioned-entity plus complete owned-set
snapshot: created→updated→completed revises one task, complete retract
removes it, and a complete owned-set listing only the peer native id retracts
the omitted current task while keeping the peer.
`rfc012d_effective_state_replaces_one_revisioned_entity_without_duplicates`
projects C1 `runtime.effective-state` as `RevisionedEntityCurrent`: configured
intent revises to response-observed evidence on one dimension entity, complete
retract removes it, partial retract does not, and bootstrap/correction
replacement digests match at the same current set.
`rfc012d_plan_replaces_one_revisioned_entity_and_omits_from_complete_owned_set`
projects C1 `runtime.plan` as `RevisionedEntityCurrent` plus complete owned-set
retract law: a partial step list cannot drop a previously known key, a complete
step snapshot replaces prior steps, complete retract removes the entity,
partial retract does not, and a complete owned-set listing only the peer
native id retracts the omitted current plan while keeping the peer.
Bootstrap/correction replacement digests match at the same current set.
`rfc012d_tool_replaces_correlated_lifecycle_without_rekeying_or_dropping_unmatched`
projects C1 `runtime.tool` as `CorrelatedLifecycleCurrent`: call and unmatched
result are separate entities, later correlation updates the relationship
without changing either fact identity, complete retract removes only the
named entity, and a partial upsert cannot drop a previously known
correlation. Bootstrap/correction replacement digests match at the same
current set. D3 remains In progress; this slice does not stamp Gate met. Native-derived
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
manifests, non-append source participants, and calibrated multi-observer
scheduling/starvation bounds remain unimplemented. Internally,
reducer mutation, admitted-frame release, bounded delivery admission, and
eligible coverage promotion now share one retry-safe offered transaction:
exact projected capacity is checked before mutation, queue pressure changes no
reducer, coverage, or sequence state, and reset/control plus semantic
retractions enter delivery as one ordered batch. Therefore the D3 and X0 gates
remain open for the still-missing public lifecycle rather than this internal
atomicity seam. Delivered internal values now carry the selected event-contract
version, epoch, observer sequence, mandatory event ID, optional semantic
revision, phase, stable source coordinate, and typed event. The immutable
mapper binds those values to the resolved root, emits the bounded current RFC
012C actor and conservative team/workflow context, strips internal ordinals,
preserves source occurrence and observed/native time, and distinguishes native
records, common reducer corrections, and engine controls. It rejects
cross-root typed events and mismatched native-session claims. This freezes the
current usage/source-lifecycle and initial resync envelope vocabulary but is
not yet the public multiplexer/facade, complete portable resync/scope manifest,
selected actor-affiliation replacement family, or a portable consumer-ready
helper. The internal single-consumer drain now owns the bounded delivery lane
from host construction,
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
while a newer stage supersedes the failed continuity epoch. Automatic-owner
coverage now also starts from a delivered present root and proves that overflow
replay against a disappeared file activates only the complete absent-root
scope/source snapshot before later polls resume. Complete multi-family and
D-owned manifests, dynamic whole-scope discovery and
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
delivery before close. A live native watcher rescan now enters that same
sticky continuity path rather than merely requesting an ordinary poll, remains
retained across backend recovery, and retries against the delivery-capacity
generation when a replacement-start control has not yet been delivered. The
source owner now classifies source and decode
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
owners on cancellation or terminal watcher/source failure. The next bounded
D3 slice (`101cb90`) closes automatic replay/rebind for that exact append-only
handoff without inventing a detached background task. The handoff supervises
the retained native watcher while it waits for delivered resync authority,
opens an empty epoch, visits exact relation bindings in canonical order,
replays every bounded batch to complete source coverage, publishes the staged
snapshot and completion barrier through existing capacity ownership, and only
then rebinds and re-pairs the new source owner. A one-shot contract replay is
applied only to the first batch and is cleared after replacement, preventing a
large object or subsequent live poll from restarting forever at offset zero.
The integrated test uses a five-record multi-batch replay, proves the rebound
owner retains generation one at the complete cursor, services handoff-time
poll demand, and later co-stops with the same watcher. The next D3 slice
(`db9ade6`) carries the stopped source owner's bounded retry policy into that
automatic replacement. Retryable source/decode failures publish ordered
`Correction`-phase `source.object_error` controls and partial coverage while
the retained watcher remains supervised; successful replay clears the staged
error and supersedes its coverage before the completion barrier. Terminal or
exhausted failure before any replacement position removes old object-owned
facts, publishes position-free `Unavailable` Decode coverage plus the typed
error, and lets healthy sibling relations finish the same full snapshot. The
next D3 slice (`f48dedd`) closes the corresponding partial-progress case with
an object-scoped in-memory transaction. Scheduled retries retain the exact
cursor and partial coverage; terminal or exhausted recovery prevalidates the
object, admission, and reducer authorities, then removes only that object's
offered coverage and object-token-owned facts while discarding its cursor and
decoder state. The typed error retains the last admitted position strictly as
diagnostic provenance, while current Decode coverage is position-free
`Unavailable`; the discarded marker is bound to that exact terminal error and
cannot become a resumable live cursor. Integrated retry-exhaustion and
sibling-ownership tests prove the incomplete prefix is absent from the frozen
replacement, an unaffected sibling remains projected, and the rebound epoch
continues to produce complete polls without failing the observer. The portable
Rust and TypeScript source-envelope parsers accept object errors only in
`Live` or `Correction`, never `Bootstrap`, and preserve the exact
retry/provenance contract. The following D3 slice (`8f396eb`) closes automatic
replacement re-overflow at every post-replay ownership boundary. An exact
persisted-state proof now binds the attempted epoch, declared root, invalidated
epoch, current bootstrap/resync baseline digest, and baseline scope epoch;
re-overflow during staging, source-owner binding, or final watcher/source
pairing discards the failed stage and retries on a strictly newer epoch instead
of publishing `observer.failed`. The handoff recovers each non-cloneable owner
only for its exact race error, retains queued poll demand and watcher
supervision, and later polls and closes normally. Deterministic boundary hooks
exercise both bind races and retry exhaustion without losing the last valid
epoch. Dynamic discovery, non-append source participants, the public host/SDK
transport, and calibrated watcher/replay policy remain open. The next D3 slice
(`c26819b`) closes the remaining watcher-budget reset across those ownership
transitions. One non-cloneable watcher now retains the exact recovery policy,
cumulative charged attempts, and absolute next-retry deadline for the active
backend-failure incident even when its supervision future loses to source
handoff or repeated re-overflow. Policy drift fails terminally instead of
extending or reshaping the frozen budget. Backend replacement commits success
under the same lock that checks callback and routing failure, so a callback
failure cannot be erased between precheck and generation advance; only a
genuinely installed and successfully reconciled backend resets the incident
for a later independent outage. The conformance matrix distinguishes the old
three-attempt reset from the bounded two-attempt outcome, verifies exact
deadline reuse and policy-drift rejection, and proves that a later independent
failure receives a fresh budget.

The next bounded D3 precursor (`24a86f0`) freezes the already-retained actor-run
and actor-affiliation reducers beside usage whenever a replacement stage is
prepared. Each private, redacted snapshot contains every current normalized
entity, exact semantic/source identity and path-free provenance, plus the
derived actor or `ActorAffiliationContext`; its versioned family digest is
canonical across fact insertion order and excludes attachment phase,
observation time, batch ordinals, object tokens, and numeric store IDs. Late
affiliation changes only the affiliation digest and never usage identity or
redelivery. Reset, confirmed deletion, object rollback, and bounded capacity
retain their exact ownership behavior, and a prepared stage rejects subsequent
reduction so usage, actor, and affiliation state freeze at one boundary. This
is deliberately non-authorizing: the selected family list, eligible coverage
domains, offered events, and barrier manifest remain exactly
`runtime.usage-v2`. Actor/affiliation incremental events, selected coverage,
portable envelopes, and a truthful complete multi-family replacement manifest
remain the next D3 gates.

The next bounded D3 slice (`24dfa17`) selects the crate-private
`runtime.actor-run` and `runtime.actor-affiliation` v1 families beside
`runtime.usage-v2` under the exact negotiated family set. Common reduction now
emits deterministic, provenance-bound actor and affiliation upserts, preserves
late affiliation without redelivering unchanged usage, and retracts dependent
families in affiliation-before-actor order on reset or confirmed deletion.
Bootstrap and replacement require matching per-family coverage and one
canonical complete manifest, while replacement offering remains retry-safe and
orders actor, affiliation, then usage. A synthetic promoted fixture proves the
exact three-family host selection and rejects a usage-only reducer before
watermark or readiness publication; all built-in support packages remain
Candidate and unauthorized. This slice adds no portable actor/affiliation DTO,
N-API or SDK exposure, vendor promotion, artifact resolution, persistence, or
public transport authority.

The next bounded D3 slice (`c9cf72a`) makes the existing scoped usage-v2
portable contract consume the actor and affiliation context selected by the
common host. Usage-only selection remains key-only compatible;
evidence-backed actor lineage and affiliation context cross the Rust and
TypeScript boundary only under their exact v1 family selections, with
canonical bounded text, exact root/native-session binding, and canonical
semantic-revision evidence. A selected family may still carry the explicit
key-only fallback when no declaration has arrived, while partial enrichment
cannot shed its parent or family authority. This adds no actor or affiliation
event wire, complete event union, N-API observer transport, support promotion,
or public authorization.

The next bounded D3 prerequisite (`a67d550`) makes `runtime.actor-run` and
`runtime.actor-affiliation` v1 use canonical value-derived semantic revision
keys. Equal normalized revisions under one stable fact identity now share one
durable/scoped join identity across source-record replay while source
occurrence remains separate provenance; every actor lineage, affiliation
state, qualification, and timestamp axis is digest-bound. The fact boundary
rejects weaker caller-supplied keys, scoped and durable reducers recompute the
identity, exact batch/database replay is idempotent, and generation or snapshot
replacement retains one current owner without fabricating a second occurrence.
The Rust/TypeScript RFC 012C fixture pins both keys and references. This adds no
actor event wire, N-API/SDK observer transport, source promotion, or public
authorization.

The next bounded D3 slice (`2196021`) freezes the selected actor-run and
actor-affiliation event envelope without claiming the complete observer
transport. A serialization-only Rust projection and mandatory contextual
consumer bind the exact negotiated selection, root/native-session identity,
actor and affiliation context, authorized source occurrence, append-reset or
deletion lineage, complete evidence, and withheld native-payload projection.
Rust recomputes both canonical semantic revision and occurrence event identity;
the exported TypeScript parser independently enforces every inspectable strict
shape, bound, family, context, occurrence, evidence, and privacy rule against
the same frozen fixture. Actor-run upserts and affiliation reset retractions
prove both selected families while unsupported usage, source-control, and
observer-lifecycle variants remain closed. This adds no N-API observer method,
complete portable event union, native source access, support promotion, or
public attachment authority.

The next bounded D3 prerequisite (`7d3431c`) freezes a contextual portable
replacement-family manifest for the selected actor-affiliation, actor-run, and
usage-v2 reducers without presenting it as a bootstrap or resync barrier. The
serialize-only Rust projection and strict TypeScript parser require the exact
caller-held negotiated selection, RFC 012A fact-family coverage, and
reducer-derived expected manifest. Family order, versions, replacement
representations, merged completeness, portable counts, and nonzero semantic
digests are fixed across bootstrap/correction phase and reject selection,
coverage, family, count, or digest replay. The frozen fixture carries only
opaque coverage coordinates and digests; no native payload, locator, root,
watermark, or source-access authority enters the manifest. Full barrier
publication remains gated on the complete D-owned artifact/capability state,
dynamic/discovered scope authority, and the complete event/control union; no
N-API observer transport or public attachment authority is added here.

The next bounded D3 prerequisite (`add1cb6`) freezes the attachment's
phase-independent observation-capability state without presenting static
support expectations as current source readiness. Rust validates the exact
caller-held selection, host offer, compatibility class, support-release
identity, selected-family implementation, and explicit unselected families,
then derives a domain-separated BLAKE3-256 semantic digest over the canonical
capability report. Exact-supported and range-supported reports therefore have
distinct snapshots, while replay is independent of bootstrap/correction phase
and JSON map insertion order. The exported TypeScript parser independently
enforces every inspectable capability/context/shape bound and compares the
Rust-derived digest to caller-held context without claiming portable BLAKE3
authority. No source coverage, current readiness, root, artifact state,
barrier sequence, native payload, source-access token, or observer transport
enters this snapshot. Full bootstrap/resync barrier publication remains gated
on artifact-availability revisions, dynamic/discovered scope authority, and
the complete event/control union; no N-API observer method or public attachment
authority is added here.

The next bounded D3 prerequisite (`9448ba3`) gives Claude file-history
metadata and content topology-neutral canonical session and artifact identities
plus canonical value-derived semantic revisions while retaining the RFC 011
durable keys in parallel. Named metadata and content derive the same artifact
key; numeric registration/source topology and observation time cannot perturb
canonical fact or revision identity, metadata entry order is canonical, and a
normalized metadata or content change revises the fact without rekeying it.
Legacy durable artifact projection and ingest parity remain unchanged. This
does not create artifact-availability state, evidence-derived locator
authority, native reads, a complete event/control union or barrier, support
promotion, or public transport.

The next bounded D3 prerequisite (`1968694`) makes canonical artifact metadata
a root-bound, lifecycle-owned scoped evidence state. Only facts committed
through the existing admission/offer transaction enter the bounded reducer;
reset, deletion, replacement rollback, and resync staging preserve exact
source ownership. A path-free canonical snapshot distinguishes
content-expected, explicit not-captured, and conflicting evidence and derives
topology-neutral revisions from canonical fact, revision, and source-record
identities while withholding native locator material. It does not reserve
`ArtifactLocatorFromEvidence`, construct or open a locator, claim content
availability, enter the portable artifact DTO or completion barrier, promote
support, or expose public transport.

The next bounded D3 prerequisite (`865e4c7`) removes arbitrary-key production
minting from the scoped artifact request seam. Only an exact active attachment
epoch with current, non-conflicting `ContentExpected` metadata may derive a
request; its opaque request identity privately binds the canonical artifact
and evidence revision. A borrowed validation proof keeps that epoch immutable
through request-context emission, so correction, reset, retraction, conflict,
or cross-attachment replay fails closed. The metadata artifact version remains
distinct from the optional caller-held native source-object generation check.
The frozen v1 wire shape and fixture do not change, and unbound request/result
construction is test-only. This still does not select an artifact relation or
kind-to-locator mapping, reserve `ArtifactLocatorFromEvidence`, construct a
locator, read native bytes, produce a production availability result, enter a
completion barrier, promote support, or expose public transport.

The next bounded D3 prerequisite (`a681aa7`) binds that current artifact
request to one exact promoted relation without performing native I/O. Trusted
attachment input maps each selected artifact kind to a unique
`ArtifactLocatorFromEvidence` declaration and supplies the exact absolute
host-approved roots used by its selected known-object and artifact relations;
known-object locators must use the same named root. The current evidence proof
privately supplies the agreed backup identity and version, while the exact,
complete native root-session claim supplies the session parameter. Callers
cannot substitute any of the three declared identity inputs. One common
access-pass reservation binds the attachment, active epoch, relation, root,
declaration locator identifier, request ceiling, and opaque object token; it
borrows both the validated epoch and pass and is conservatively abandoned if
dropped. Debug and access telemetry retain no native root, session, backup, or
tracked path. This slice deliberately does not interpret the declaration's
locator identifier, render a relative path, open a native object, complete the
reservation, produce artifact availability, enter a barrier, promote Claude's
incomplete Candidate scope program, or expose N-API/SDK transport. Executable
Claude locator parameters and a common confined read mediator remain open.

The next bounded D3 prerequisite (`1e8cd7c`) makes that common reservation
interpret only executable evidence-derived locator templates, still without
performing native I/O. Host authorization now rejects a selected artifact
relation unless its locator contains at least one unique placeholder named by
the declaration's identity inputs. After reservation, the common guard
re-derives the opaque object token from the exact already-bound session,
backup, and version inputs before rendering them into a UTF-8, confined
relative path capped at 4 KiB. Unknown, repeated, unmatched, or nested
placeholders; input substitution; separators, traversal, control bytes,
non-UTF-8 values, platform drive prefixes, and over-cap output all fail closed.
The borrowed proof privately retains the native root and rendered relative
path as separate values, while Debug and access telemetry remain path-free.
This slice does not join the root and path, resolve symlinks, open or read an
object, complete the access reservation, publish availability, enter a
barrier, promote support, or expose N-API/SDK transport. Claude's current
Candidate locator remains conceptual and therefore non-executable; declaring
and proving its exact file-history template remains a separate conformance
gate.

The next bounded D3 prerequisite (`827772c`) closes that Claude file-history
template gate without promoting or executing the scope program. The Candidate
declaration now names the exact
`file-history/{native-session-id}/{backup-name}` relative locator while keeping
the independently bound positive artifact version in the reservation identity.
Executable conformance renders both sanitized small-corpus file-history
objects through the common guard, matches the durable stream selector and
home root, and passes the resulting UUID/lowercase-hash/canonical-version paths
through Claude's real artifact bootstrap parser. The updated scope-document
SHA-256 is bound by both the support release and compiled adapter. The scope
program remains `incomplete`, the support release remains Candidate, scoped
observation remains unsupported, and the task-artifact locator remains
conceptual. This adds no root join, native read, availability result, barrier,
or public transport; a confined common read mediator remains a separate
promotion gate.

The next bounded D3 prerequisite (`9c74998`) closes the file-history
source-declaration/runtime decoder-ID drift without changing the compiled
decoder or any durable checkpoint identity. The Candidate source declaration
now names the existing `claude-file-history-blob` decoder for the exact
`file-history-blobs` stream, and the source-document SHA-256 is repinned in the
support release and compiled adapter. Executable conformance binds that digest
and compares the declared root, selector, decoder, and 1 MiB object ceiling to
the runtime `StreamSpec`. Candidate, scope, and capability statuses remain
unchanged. A confined common read mediator, task-artifact declaration, complete
scope evidence, and promotion remain open.

The next bounded D3 prerequisite (`386e025`) consumes an exact promoted
artifact-relation reservation through the common no-follow stable-file driver
and produces a crate-private confined capture. Missing, oversized, unstable,
and stable objects complete or conservatively fail the reserved access budget;
stable reads account their exact physical bytes. The validated content policy
is applied immediately: metadata-only retains neither bytes nor hash,
hash-only retains only SHA-256, and inline retains both. Native root/path,
identity, revision, hash, and content remain non-serializable and redacted
from Debug, while the capture continues to borrow the validated command and
access pass. It deliberately cannot construct the portable artifact outcome:
that outcome's generation is the native source-object generation, not the
metadata artifact version, and no durable generation authority is present at
this seam. Built-in Claude remains Candidate/incomplete and therefore cannot
execute the read. Generation mapping, portable availability publication,
completion-barrier integration, task-artifact declaration, promotion, and
public transport remain open.

The next bounded D3 prerequisite (`38ce442`) supplies that missing generation
authority inside the database-free attachment. A mutex-serialized, bounded
ledger keyed by the exact relation/session/backup/version access-object token
mirrors common `ReplaceDocument` presence lineage: content revision or native
file-identity replacement remains in one generation, while an observed delete
or recreation advances it. The ledger persists across access passes and shares
the artifact-evidence reducer's 4,096-object safety ceiling. Each stable or
known-missing observation derives a path-free opaque provenance reference from
the current metadata-evidence revision, source generation, and exact native
revision state. A stale caller-held expected generation produces
`changed_generation` before content enters the portable wire; matching reads
retain only the requested metadata/hash/inline disclosure, and attachment
close is rechecked before serialization. An unforgeable consumed-capture
witness prevents evidence validation alone from minting a result. This remains
crate-private and executes only from a genuinely promoted scoped authorization;
built-in Claude is still Candidate/incomplete. Source-driver errors, artifact
completion-barrier state, task-artifact declaration, support promotion, and
N-API/SDK observer transport remain open.

The next bounded D3 prerequisite (`a8175c2`) freezes completed native artifact
observations into attachment-owned, path-free availability revisions without
claiming ordered observer transport. Only a confined capture that successfully
constructs the strict artifact result may update the bounded reducer; a
dropped capture, contract failure, or attachment-close race cannot publish
state. The latest observation for one canonical artifact/kind pair binds the
exact current metadata selection, authorized relation, access-object token,
generation, opaque provenance, and stable size or missing/over-limit/unstable
state. Expected-generation checks and metadata/hash/inline disclosure choices
cannot perturb the underlying native availability revision, while a native
revision change does. Canonical snapshots filter observations whose metadata
evidence was corrected, retracted, or made conflicting instead of relabeling
them as missing, and Debug retains only bounded counts and redacted digests.
These revisions deliberately remain outside watermarks and bootstrap/resync
barriers until an ordered artifact-availability event binds them to the
observer sequence. Ambiguous source-driver failures are still internal rather
than guessed into portable unavailable reasons. Task-artifact declaration,
support promotion, and N-API/SDK observer transport also remain open.

The next bounded D3 prerequisite (`dae66ba`) binds that current canonical
artifact-availability snapshot into both bootstrap and resync completion
identity. Watermark capture freezes only observations whose metadata evidence
is still current, each barrier retains the exact validated snapshot, bootstrap
epoch binding rejects any post-barrier availability drift, and both coverage
and replacement completion digests change when an availability revision
changes. The completion aggregate advances to contract v2 while the existing
actor and usage replacement-family digest contract remains v1, avoiding an
unrelated semantic-family revision. The snapshot is still crate-private and
non-serializable: this slice does not invent an ordinary ordered
artifact-availability event, a portable source-generation/error mapping, or
an N-API/SDK observer surface. Dynamic artifact discovery, task-artifact
declaration, complete promoted scope evidence, and support promotion also
remain open.

The next bounded D3 prerequisite (`2d1ead4`) freezes a contextual portable
projection of the barrier-bound artifact-availability snapshot without
claiming an ordered event. Each canonical artifact/kind entry now retains its
explicit available, missing, over-limit, or unstable state beside the opaque
native-derived revision. The serialize-only Rust wire and independent
TypeScript parser enforce the 4,096-entry ceiling, portable integer and
identifier bounds, exact byte-order canonicalization, strict nested shapes,
and one caller-held contract-selection/root/entry/digest context. The frozen
fixture is produced by the real reducer rather than injected revisions and
contains neither relations, access-object tokens, locators, content, nor native
identifiers. This snapshot cannot assign an observer sequence, authorize a
source read, or establish a bootstrap/resync barrier by itself. A real ordered
artifact-availability event still requires an attachment-owned event/source
occurrence reservation; the complete barrier wire, dynamic artifact
discovery, task-artifact declaration, support promotion, and observer facade
remain open.

The next bounded D3 prerequisite (`18971a8`) binds an executable
evidence-derived artifact relation to one exact digest-bound scoped source
stream before an ordered event can exist. The Claude file-history stream now
declares scoped topology, and the relation repeats its exact
`ReplaceDocument` stream ID and 1 MiB object ceiling. Python and Rust bundle
verification reject unknown, wrong-root, non-scoped, unimplemented,
wrong-primitive/boundary/lifecycle, or digest-drifted packages. A selected
reservation enforces that per-object ceiling and derives the same
platform-neutral framed stream/object coverage coordinate used by durable
source discovery under the verified source-declaration digest. The proof
remains private and redacted and is consumed with the confined capture. It
still does not assign observer ordering or atomically pair queue admission
with availability-reducer mutation. Initial-missing/unstable source-generation
law, the ordered availability event and wire, task-artifact declaration,
promotion, and N-API/SDK observer transport remain open.

The next bounded D3 prerequisite (`89e2ead`) closes the attachment-local
ordering half of artifact availability without changing the frozen v1
snapshot wire. A generation-bound confined capture now retains the exact
verified source-declaration digest and common framed source coordinate, derives
an availability occurrence with a positive source generation, and prepares the
reducer change without mutation. The occurrence enters the attachment's one
semantic sequence before an infallible reducer commit; queue backpressure,
foreign or wrong-epoch drains, source-binding drift, attachment close, and
contract failure advance neither availability state nor observer sequence, and
the same bounded capture remains retryable with its original observation time.
Exact availability replay emits no duplicate sequence. Initial missing and
initial unstable observations use source generation 1 without manufacturing a
presence transition, so the first later present observation remains generation
1 in agreement with common `ReplaceDocument` coverage. The mapped internal
envelope is path-free, carries no locator, source record, cursor, native bytes,
or semantic fact revision, and its deterministic event ID binds the source
declaration, source coordinate/generation, root session, artifact key/kind,
and availability revision while excluding delivery time, phase, and sequence.
This remains a crate-private, non-serialized event seam: the portable ordered
artifact event wire, task-artifact discovery, complete promoted support,
N-API/SDK observer transport, and D4 migration remain open.

The next bounded D3 prerequisite (`dabce41`) freezes that ordered artifact-
availability event as a strict contextual Rust/TypeScript v1 envelope without
changing the existing availability-snapshot bytes or revision law. The
consumer drain now retains a redacted, non-Serde context minted while the
private source occurrence is still present. It binds the exact negotiation,
root, common source coordinate and generation, observer sequence and epoch,
observation time and phase, Rust-issued event ID, and availability entry. Rust
retains the verified source-declaration digest privately and recomputes the
event identity; the serialized portable context omits that digest and instead
requires the exact Rust-issued ID and reducer entry. Both parsers reuse the
strict source-envelope boundary for root actor, affiliation, source, null, and
engine-evidence validation, while requiring `CommonReducer` availability
evidence and rejecting entry/source-generation drift. The frozen fixture comes
from a real confined missing-object read through reducer preparation, ordered
offer, drain mapping, and contextual serialization. Neither event nor context
carries an artifact locator, source record, cursor, native bytes, semantic
revision, or source-declaration digest. This adds a portable contract and SDK
parser/export only: no N-API observer method, production transport, task-
artifact discovery, support promotion, or D4 migration is claimed.

The next bounded D3 prerequisite (`29ac300`) closes the remaining static
capability gap in the internal completion aggregate. Watermark capture now
freezes the attachment's exact negotiated `ObservationCapabilities`; both
bootstrap and resync barriers retain it, require its selected family/version
set to equal the canonical replacement manifest, and bind the existing
capability-snapshot semantic digest into the version-3 coverage and replacement
digests. Compatibility-class, support-release, selected-family, or canonical
order drift therefore changes or invalidates the completion identity before
queue mutation. Envelope mapping recomputes both stored barrier digests, and a
clean bootstrap/resync pair at equal coverage retains identical capability,
family, and replacement state. This remains a crate-private barrier contract:
the contextual portable completion-barrier wire, dynamic discovered scope,
non-append participants, executable task-artifact declaration, promoted
support, public observer facade, and D4 transport remain open.

The next bounded D3 scheduling slice (`76ca2ff`) closes duplicate live-watcher
demand and cooperative executor starvation without claiming calibrated global
pool policy. An attachment now retains its latest watcher poll ticket and
atomically coalesces repeated callbacks only while that generation remains
pending outside the reserved pass. A callback that arrives after pass
reservation always receives a strictly later generation, so a read already in
flight cannot falsely acknowledge the raced native change. After every
successful bounded exact-scope pass, the source owner yields once before it may
reserve another continuously pending generation. A current-thread two-observer
test keeps one scope runnable by requesting a follow-up from every active pass
and proves a sibling poll completes before the busy chain exhausts; the
watcher-before-scan test separately proves duplicate pre-reservation callbacks
share one completion while an in-flight callback is deferred to the next pass.
This establishes the attachment-local scheduling quantum only. Cross-workload
shared access/decode worker integration, durable/catalog workload fairness,
numeric latency ceilings, and a calibrated multi-observer performance report
remain open D3/D5 gates.

The follow-on D3 permit slice (`9402172`) adds a real caller-owned shared
capacity domain around bounded observer source passes. Multiple async runtimes
may now receive the same fair semaphore pool, while the compatibility `open`
path retains one attachment-local permit. A source owner reserves its logical
poll first, then must acquire shared capacity before observation time, native
access, or decode; attachment close cancels that wait, and continuity is
revalidated after acquisition before any read. Permit capacity rejects zero
and values above the runtime semaphore bound, cannot be resized by an attached
scope, and is released before delivery backpressure or the cooperative
post-pass yield. A one-permit two-observer matrix keeps one scope continuously
runnable while the sibling completes, and a separately held-permit case proves
that close cancels the queued owner with its poll unresolved, performs no
native pass, and leaks no permit. This closes shared capacity across current
scoped observers only. Durable live tails and catalog work do not yet enter
this pool, and numeric permit counts, latency/starvation ceilings, the retained
performance report, public host wiring, and D5 ratification remain open.

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

Current landing status (2026-08-21): the SDK has a feature-flagged shadow
wrapper beside `watchSessionTranscript`, but enabling it requires a
caller-injected `ObserverRecordSource`. It does not open or own the Rust scoped
observer, and it currently transports only the narrow typed usage shadow
record. There is no public N-API/IPC observer facade, lifecycle/error transport,
all-family normalization, or released Chopsticks integration. The legacy tail
therefore remains the only production source. D4 remains `In progress`.

### D5. Performance calibration

Measure attach, bootstrap barrier, hook/poll-to-admission,
admission-to-delivery under active demand, helper application through barrier,
queue/control high-water, overflow, resync barrier, close, access count,
retained bytes, and several simultaneous scopes including a deliberately slow
consumer. Propose a child gate amendment for any numeric release ceiling.

Current landing status (2026-08-21): `rfc012_d5_emit_observer_kernel_report`
times attach, poll, overflow/resync, and three-scope attach with `Instant`
inside the observer kernel and writes
`scripts/rfc012_experiments/fixtures/observer-kernel-report.json`. Numeric p95
ceilings stay unratified. The fixture omits admission-to-delivery under demand,
helper/barrier application, queue/control high-water, close, access count,
retained bytes, memory, repetitions/statistics, and a deliberately slow
consumer, and it does not satisfy section 12's report contract. D5 remains
`In progress`.

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

The executable ledger in
[`rfc012-rfc011-delta.json`](../../scripts/architecture/rfc012-rfc011-delta.json)
is complete: every contract is `implemented`, and
`check_rfc012_delta.py --require-complete` passes. X0 is `Gate met`.

### X1. Search and finalization separation

Compare on identical frozen inputs:

1. deferred one-shot FTS after history;
2. incremental FTS maintenance after catalog; and
3. bounded/chunked finalization with controlled reader quiescence.

Report history/search completion, query p99 during build, writer/WAL/checkpoint
behavior, RSS, total convergence, and crash recovery. Search remains unavailable
until complete and validated regardless of chosen strategy.

Current landing status (2026-08-21): `rfc012_x1_compare_fts_strategies_on_identical_claude_input`
runs deferred one-shot, eager/incremental-after-catalog, and crash-recovery
finalization on the same Claude transcript. Search stays `BootstrapInProgress`
until `completeQueryBootstrap` in every strategy. The report records elapsed
time, deferred WAL bytes, and query p99 during deferred bootstrap. RSS is not
sampled in-process. It also omits the full repetitions/environment/digest,
writer/checkpoint, convergence, and recovery fields required by section 12, so
X1 remains `In progress`.

### X2. Diagnostic disposition and aggregation

First reclassify known ignored records through RFC 012A dispositions. Then
aggregate genuine repeated diagnostics by source/reason/family while retaining
count, first/last provenance, sample identity, and bounded examples. Gate on
fact/query parity and measured row/database reduction.

Current landing status (2026-08-21): bounded aggregation helpers and a
read-only SQLite calibration/dump seam exist, but the generated diagnostic
database is ignored and there is no retained, digest-bound report proving
fact/query parity and measured row/database reduction on a frozen input. X2
remains `In progress`.

### X3. Physical extraction

Extract physical crates/modules only after logical dependency tests are stable.
Extraction must preserve public N-API/SDK behavior, source/decoder fixtures,
durable query digests, observer differential tests, and performance. No
physical move may introduce a forbidden dependency edge through feature flags.

Current landing status (2026-08-21): the store/transport-free coverage
membership encoder is consumed from `spaghetti-coverage`. The separate
`spaghetti-architecture` crate is currently a filesystem/manifest sentinel;
adapter, source/decode, semantic reducer, durable store/query, and observation
API boundaries still reside physically in `spaghetti-napi`. The full behavior
and digest-preserving extraction gate is not met. X3 remains `In progress`.

### X4. Promotion and drift

Promote current-agent support releases, run version/drift classification in
production, collect cold/warm/observer telemetry on maintainer-owned corpora,
and keep rollback flags through one compatible release cycle. Default-on
requires every owning child gate for that product path.

Audit correction (2026-08-21): the overnight Claude durable bundle was marked
promoted by a generator that also supplied its own sanitizer approval, while
the retained sanitizer input report still had `reviewer: null` and said review
was pending. Its `performance-v1.json` contained operation labels rather than
section 12 measurements, and `promotion-telemetry-v1.json` was a static
classification summary rather than compatible-release-cycle evidence. That
state violated RFC 012A section 9.8 and could not confer runtime authority. The
bundle is retained as `candidate-2026-08-21` with explicit blockers; the public
host verifies it but treats exact 2.1.223 as recognized-unverified. The old
mutating generator is replaced by a read-only preflight that requires external
sanitizer approval, a complete performance report, and compatible-cycle
telemetry. X4 remains `In progress` with zero promoted current-agent releases.

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

The next implementation sequence follows the remaining authority and semantic
gates rather than the prior wave labels:

1. finish C1/C2 family coverage and reducer laws, including the remaining
   effective-state dimensions, progress/content/capability facts, complete
   replacement/retraction fixtures, and equal durable/scoped reduced-state
   digests;
2. complete B1-B4 with real promoted catalog source compositions, an
   authorization-bound public catalog query/retention policy, selected-session
   hydration, and the cold/warm progressive-host matrix;
3. complete D1-D3 dynamic scope composition and publish one intentionally
   bounded native observer facade before treating D4's injected shadow source
   as a product migration;
4. replace the diagnostic B5/D5/X1/X2 timing fixtures with reproducible reports
   satisfying section 12, then review numeric ceilings instead of inferring
   them from single runs;
5. continue X3 only along dependency boundaries that preserve the existing
   public/digest matrices; and
6. collect real X4 cold/warm/drift telemetry through one compatible release
   cycle before any default-on decision.

Later product decisions, not implementation gates:

1. ratify draft children RFC 012B/C/D against the landed evidence;
2. consider default-on switches (`Rolled out`) after one compatible release
   cycle of telemetry and rollback;
3. amend RFC 012B/012D only if numeric p95 ceilings are actually ratified.

Publishing the bounded scoped-observer facade and wiring D4's
`observerSource` to it are implementation gates, not optional product
decisions.

No step may use a temporary renderer catalog, private adapter tail, arbitrary
scope search, second database, or duplicated native decoder to shorten the
critical path.
