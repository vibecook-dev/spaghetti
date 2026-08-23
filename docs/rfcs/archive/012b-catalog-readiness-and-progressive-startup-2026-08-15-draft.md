# RFC 012B: Catalog, readiness, and progressive startup

> **Archived 2026-08-23.** The full 2026-08-15 draft, preserved as written
> apart from relative link paths. It is the design record, not the contract:
> what the code enforces is [RFC 012B](../012b-catalog-readiness-and-progressive-startup.md).

- **Status:** Draft child RFC; proposed semantic contract
- **Created:** 2026-08-15
- **Parent:** [RFC 012 umbrella](../012-evidence-backed-adapters-and-progressive-readiness.md)
- **Depends on:** [RFC 012A](../012a-agent-adaptation-and-engine-boundaries.md)
- **Program plan:** [RFC 012 implementation plan](./012-implementation-plan.md)
- **Evidence:** [cold-start profile](../011-playground-cold-start-profile-2026-08-15.md)
  and [Phase 0 catalog census](./012-phase-0-census-2026-08-15.md)
- **Owns:** catalog facts, identity reconciliation/canonical presentation,
  native project/session association and locator evidence, external catalog
  reference resolution, catalog reducers, coverage-plan/readiness transitions,
  snapshot-consistent catalog queries, startup tiers, warm migration policy,
  scheduling, and initial-library UX
- **Does not own:** adapter support/version policy, runtime usage semantics,
  scoped observer delivery, or physical crate layout

## 1. Summary

Spaghetti will make a complete, honest project/session catalog available before
full transcript, capability, usage, artifact, and full-text-search convergence.
Catalog membership is a first-class fact; it is not inferred by TypeScript and
does not pretend that metadata-only sessions have decoded transcripts.

Durable readiness becomes a versioned, epoch-scoped state machine. Catalog
pagination is bound to one complete snapshot so background ingestion cannot
produce duplicate, missing, or reordered rows across pages. On warm start, any
migration not required to safely read the last complete catalog snapshot runs
in the background.

## 2. Evidence

The 2026-08-15 production-shaped profile reached host readiness after 206.89
seconds, processing 1,969,824 native records into an approximately 8.25 GB
database. That boundary included complete history, full-text-search
finalization, integrity work, and checkpoints rather than only the information
needed to list projects and sessions.

The independent Phase 0 census found all 176 oracle projects and all 1,414
oracle catalog sessions while reading 0.832% of the selected primary-stream
bytes. Its Python feasibility run completed in 535.7 ms with 45.4 MiB peak RSS.
It also found 181 Claude index-only sessions, proving that discoverability and
transcript availability are different facts.

These measurements justify the architecture. They do not by themselves ratify
the provisional Rust performance targets in section 14.

## 3. Decisions

1. `library.catalog` is an independently versioned projection pack and the
   desktop interactive startup boundary.
2. Catalog facts represent evidence of discoverability and display metadata;
   complete history facts may contribute catalog evidence automatically, but a
   catalog fact never fabricates complete history.
3. Project/session identity is deterministic, adapter-declared, and
   provenance-bearing. Heuristic prompt/path/time similarity cannot merge
   entities.
4. Reducer precedence and evidence removal follow the laws in section 7.
5. Durable readiness has an epoch and explicit transition law. A new build may
   retain a separately identified last-complete snapshot.
6. Completeness is relative to an immutable configured-source coverage plan;
   changing that plan creates a new readiness lineage.
7. Every paginated catalog cursor is bound to a complete catalog snapshot and
   query fingerprint.
8. Queries are pure. Hydration and priority promotion use explicit writer-side
   commands.
9. Non-catalog-critical migrations cannot block safe warm presentation of the
   last complete catalog.
10. Search remains unavailable until its complete projection pack is validated;
    partial search cannot masquerade as complete search.
11. Numeric experiment targets and ratified release ceilings are different
    statuses and cannot be silently interchanged.
12. Session-to-project membership is provenance-bearing native association
    evidence. A reduced catalog relationship cannot become a VibeField project
    identity, cross-device merge, or contribution claim.
13. Public project/session rows expose persistable RFC 012A entity references,
    and reference lookup reports lifecycle state without silently retargeting a
    tombstoned or superseded key.

## 4. Contract maturity

| Element                                      | Classification           |
| -------------------------------------------- | ------------------------ |
| Catalog membership as a semantic fact        | Architecture invariant   |
| Catalog/transcript availability distinction  | Semantic contract        |
| Identity, precedence, and evidence-loss laws | Semantic contract        |
| Native project/session association evidence  | Semantic contract        |
| External catalog-reference resolution        | Semantic contract        |
| Coverage-plan identity and lifecycle         | Semantic contract        |
| Readiness state and transition model         | Semantic contract        |
| Snapshot-consistent pagination               | Semantic contract        |
| Exact Rust/N-API request structures          | Proposed API             |
| Scheduler weighting algorithm                | Implementation detail    |
| Current latency/RSS/byte thresholds          | Experiment targets       |
| Final numeric release ceilings               | Open pending gate report |

## 5. Catalog fact model

The semantic model contains assertion facts equivalent to:

```text
CatalogProjectFact {
  assertion_key
  source_instance_key
  project_key
  native_project_id?
  root_identity?
  display_path?
  display_name?
  native_time?
  availability
  provenance
}

CatalogSessionFact {
  assertion_key
  source_instance_key
  session_key
  native_session_id?
  title?
  first_user_summary?
  native_created_at?
  native_updated_at?
  native_message_count?
  transcript_locator?
  availability
  provenance
}

ProjectAssociationBasis =
  NativeProjectIndex
    | TranscriptCwd
    | SessionDirectory
    | RolloutHeader
    | DeclaredDerivedAncestor

SessionProjectAssociationFact {
  association_key
  session_key
  project_key
  basis: ProjectAssociationBasis
  locator_claim_key?
  authority
  quality: Exact | NativeClaimed | Derived | Estimated
  completeness: Complete | Partial | Unknown
  effective_at?
  provenance
}

NativeLocatorClaim {
  locator_claim_key
  subject_key: ProjectKey | SessionKey
  kind: Filesystem | NativeIndex | Repository | OpaqueNative
  locator: QualifiedValue<{
    native_value?
    canonical_local_path?
  }>
  disclosure: LocalSensitive | PolicyShareable
  basis: ProjectAssociationBasis
}
```

Optional display values use RFC 012A's common `QualifiedValue<T>` contract.
Catalog reducers preserve its authority, completeness, effective-time, and
provenance fields.

Absence is not encoded as an empty string, zero, epoch timestamp, or synthetic
title. Native counts remain `NativeClaimed` until transcript evidence verifies
them.

Availability is explicit:

```text
MetadataOnly
TranscriptDiscovered
Hydrating
HistoryReady
Unavailable(reason)
```

Availability describes content evidence, not projection-pack readiness.

An association fact asserts only that native evidence relates one base session
to one base project. `DeclaredDerivedAncestor` is permitted only when the ADS
defines and fixtures the bounded derivation; it is not permission to infer a
Git repository, compare path basenames, or group worktrees into a product
project. Locator claims remain separate so an association can be exposed while
its sensitive path/value is withheld.

## 6. Identity lifecycle

### 6.1 Project identity

Project identity uses, in order:

1. a stable native project ID declared by the adapter ADS;
2. another versioned native identity tuple proven stable by fixtures; or
3. a canonical root/object identity explicitly declared as a fallback.

If path/root identity is the only supported fallback, moving the project
creates a new project identity. The reducer cannot infer sameness from basename,
git remote, prompt content, or timestamps. A native alias/move record or an
explicit `IdentityRelationFact` is required to relate old and new identities.

### 6.2 Session identity

`session_key` is the RFC 012A base `SessionKey`; this RFC does not define a
second catalog-only key. When a native session ID exists, the support release
derives it from source instance, adapter identity namespace, entity kind, and
that native ID. Index, transcript, summary, durable ingestion, scoped
observation, and sidecar evidence for the same native ID therefore converge on
one base key.

When no stable native ID exists, the ADS defines a deterministic source-object
identity rule and its replacement semantics. An adapter cannot change fallback
identity without a semantic-version change and replay plan.

### 6.3 Aliases, merges, and replacements

Identity relationships are explicit facts:

```text
IdentityRelationFact {
  relation: Alias | SameEntity | Supersedes | ReplacedBy
  left_key
  right_key
  authority
  provenance
}
```

`Alias` affects lookup but does not by itself merge histories. `SameEntity`
permits canonical reduction only when the owning adapter contract defines a
deterministic winner and collision behavior. `Supersedes` and `ReplacedBy`
preserve both identities and lifecycle history.

Catalog canonicalization never rewrites a base RFC 012A entity key. Query rows
that represent several related base keys expose the chosen representative and
the member/relation identities required to resolve a later scoped attachment.
An attach action targets exactly one disclosed base-session
`ExternalEntityRef` plus its locator. A presentation representative that
denotes several members is not itself an observation-scope identity unless it
is also the selected base member.

Vendor-version changes do not imply any relationship without evidence.

## 7. Reducer laws

### 7.1 Assertion model

Catalog rows are reduced from one or more assertions. Removing one assertion
does not remove a row while another valid assertion remains. Every chosen value
retains the winning assertion and competing-conflict provenance.

### 7.2 Value precedence

For each field, reducers apply this total ordering:

1. adapter-declared field authority class from the promoted ADS;
2. value quality (`Exact` before `NativeClaimed` before `Derived` before
   `Estimated`; `Unknown` never defeats a value);
3. native effective time when the ADS says timestamps are comparable;
4. observation commit order; and
5. assertion key as a deterministic tie-breaker.

An ADS may assign authority classes to transcript headers, indexes, summaries,
or sidecars, but cannot inject reducer code. Equal-authority incompatible values
produce a conflict record even though the deterministic winner remains
queryable.

### 7.3 Evidence loss

- Confirmed source deletion/replacement retracts the assertions owned by that
  source generation.
- Temporary source unavailability is not deletion. It degrades source coverage
  and retains the last complete snapshot.
- When all assertions are retracted under complete source coverage, the entity
  receives a durable tombstone and is excluded from the next complete current
  snapshot.
- The previous complete snapshot may remain presentable as stale/refreshing
  until a replacement snapshot publishes; it cannot be represented as current.
- Tombstone retention and physical compaction are implementation policy, but a
  retained cursor must never resolve the tombstoned entity as live.

### 7.4 Counts

Project/session counts and display summaries are materialized transactionally
inside the catalog pack. The TypeScript layer cannot issue per-project usage or
per-session task fan-out to construct the initial library.

### 7.5 Project-association reduction

Association assertions retain independent ownership and provenance. The
section 7.2 precedence law may choose a presentation association, but every
competing project key and equal-authority conflict remains queryable. Removal
or replacement retracts only associations owned by that source generation;
partial or temporarily unavailable evidence cannot prove that an association
ended.

A catalog row with no accepted association reports association coverage as
unknown/unavailable rather than inventing an unscoped project. A presentation
representative may aggregate disclosed base members for listing, but it cannot
rewrite the association fact's session/project keys.

## 8. Snapshot-consistent query contract

Semantic calls are equivalent to:

```text
listLibraryProjects(request) -> CatalogProjectPage
listLibrarySessions(request) -> CatalogSessionPage
resolveCatalogEntity(external_ref) -> CatalogEntityResolution
getReadiness(scope?) -> ReadinessSnapshot
requestHydration(session_ref, reason) -> SchedulingReceipt
```

Project/session rows expose their RFC 012A `ExternalEntityRef`, qualified native
identity claim when available, selected association, competing/conflicting
association evidence, and association coverage. Native IDs and locator values
are returned only through an authorized policy view; a remote or otherwise
disallowed view returns `Unknown/Withheld` qualified evidence rather than the
raw ID/path/value.

Resolution is semantically equivalent to:

```text
CatalogEntityResolution =
  Live {external_ref, row}
  | Tombstoned {external_ref, provenance}
  | Superseded {external_ref, target_refs[], provenance}
  | Unknown {external_ref, reason}
```

Resolution accepts the persisted reference rather than a pagination handle.
It cannot silently return a different live entity for a tombstoned key.
Tombstone compaction may eventually change `Tombstoned` to explicit `Unknown`,
but never permits identity reuse or retargeting.

Before returning a typed page/readiness value, the query boundary selects
RFC 012A-compatible base-model, external-entity-reference, coverage, and catalog
query-pack versions advertised by the client/transport. An incompatible
semantic major returns
`IncompatibleCatalogContract`; it cannot silently deserialize the nearest
shape. The selected catalog pack version is the `pack_contract_version` carried
by `CatalogSnapshotId`. Additive unknown fields/variants follow RFC 012A's
typed-unknown preservation rule. Any change to identity, completeness, reducer
precedence, filtering, sort, or cursor meaning is semantic-major rather than an
ignorable additive field.

Catalog completeness is evaluated against an immutable coverage plan:

```text
CatalogCoveragePlan {
  coverage_plan_contract_version
  coverage_plan_id
  scope
  required_sources[] {
    adapter_id
    source_instance_key
    support_release_id
    catalog_declaration_digest
    access_policy_digest
  }
  optional_sources[] {       # same normalized entry shape as required_sources
    adapter_id
    source_instance_key
    support_release_id
    catalog_declaration_digest
    access_policy_digest
  }
}
```

`coverage_plan_id` is a deterministic digest over normalized plan content;
source registration order and transient availability do not affect it. Any
change that can alter catalog membership or interpretation—including enabling
or disabling a required source, changing its identity root, support release,
catalog declaration, or applicable access policy—creates a new plan. Optional
sources cannot silently contribute to a snapshot that still claims completeness
for an older plan; promoting or demoting optionality also creates a new plan.

The catalog membership revision identifies the admitted opaque member set and
the native membership-authority checkpoints that proved it. It does not absorb
the caller's access-policy digest. Policy drift still creates a new coverage
plan and component-completion revision, and the policy-bound coverage proof
must validate before publication, but an unchanged native member set is not
renamed merely because it is viewed through a different authorized policy.

Each complete catalog snapshot has:

```text
CatalogSnapshotId {
  pack_contract_version
  coverage_plan_id
  readiness_epoch
  complete_commit
}
```

An opaque catalog cursor binds at least:

```text
CatalogCursor {
  snapshot_id
  query_fingerprint       # filters, scope, and sort specification
  sort_spec_version
  last_sort_key
  last_entity_key
}
```

Rules:

1. Page 1 selects one complete snapshot or an explicitly requested snapshot.
2. Counts, source coverage, readiness metadata, and rows belong to that same
   snapshot and coverage plan.
3. Every continuation page is evaluated against the cursor's snapshot and
   query fingerprint using stable keyset ordering.
4. Background commits cannot alter a cursor's result set.
5. If the implementation no longer retains the snapshot, it returns
   `SnapshotExpired {latest_snapshot}`; it cannot silently continue at a newer
   commit.
6. Sort ties terminate in stable entity key order.
7. Changing filters or sort creates a new page-1 request, not a reused cursor.
8. `complete_commit` is the RFC 011 `at_commit_seq` for this catalog result; a
   response cannot advertise a different commit watermark.

The physical mechanism may use retained versioned rows, an engine-held snapshot
lease, or another proven design. `LIMIT/OFFSET` against the moving latest state
does not conform.

## 9. Durable readiness model

### 9.1 State

```text
ReadinessState {
  scope
  coverage_plan_id
  desired_contract_version
  completed_contract_version?
  epoch
  attempt
  state: Pending | Building | Partial | Ready | Degraded | Error
  complete_through_commit?
  last_complete_snapshot?
  refreshing_from_snapshot?
  source_coverage
  reason?
}
```

`last_complete_snapshot` is historical presentation state. Its presence does
not change the truth of the current epoch's `state`. It carries its own
`coverage_plan_id`; when the plan changes, the prior snapshot is explicitly
labeled as belonging to the previous plan and cannot be presented as complete
for the new one.

`epoch` identifies a semantic build lineage and increments when the coverage
plan, contract, or source generation invalidates that lineage. `attempt`
increments for a retry within the same plan/epoch. An attempt cannot reuse a
failed attempt's publication identity.

### 9.2 Transition law

| Trigger                                         | Current-epoch transition                                        | Last complete snapshot                                        |
| ----------------------------------------------- | --------------------------------------------------------------- | ------------------------------------------------------------- |
| Coverage plan registered, no work committed     | absent -> `Pending`                                             | unchanged                                                     |
| Build scheduled                                 | `Pending` -> `Building`                                         | retained if present                                           |
| Some required coverage committed                | `Building` -> `Partial`                                         | retained if present                                           |
| All required coverage and validation succeed    | `Building/Partial/Degraded` -> `Ready`                          | atomically replaced by new snapshot                           |
| Required coverage plan changes                  | any -> `Building` with `epoch + 1`, `attempt = 1`               | retained and labeled with its prior plan                      |
| Required source temporarily unavailable         | retain current state; mark source `Retrying` and publish reason | retained; a ready snapshot remains last-complete for its plan |
| Required source reaches terminal unavailability | `Building/Partial/Ready` -> `Degraded`                          | retained and labeled stale/degraded                           |
| Integrity or contract invariant fails           | any non-error -> `Error`                                        | retained only if independently safe to read                   |
| Retry after source recovery                     | `Degraded/Error` -> `Building` with `attempt + 1`               | retained                                                      |
| Contract/projection version changes             | any -> `Building` with `epoch + 1`, `attempt = 1`               | retained if schema-compatible                                 |
| Source reset invalidates current build          | any non-error -> `Building` with `epoch + 1`, `attempt = 1`     | retained                                                      |
| Ordinary refresh under the same valid contract  | remains `Ready`, sets `refreshing_from_snapshot`                | current complete snapshot remains authoritative               |

`Ready` is never rewritten in place to describe a different complete result.
A new complete result receives a new `CatalogSnapshotId`. Recovery ordinarily
follows `Degraded -> Building -> Ready`.

While a required source is retryably unavailable, `Ready` means only that the
advertised snapshot was complete for its plan and commit. Coverage and reason
must show that current reconciliation is blocked; the state cannot imply that
the native source is presently synchronized. A bounded policy determines when
retryable loss becomes terminal `Degraded`, and that policy is part of the
support/host contract rather than wall-clock guesswork in the renderer.

### 9.3 Publication and invalidation

Rows, snapshot identity, source coverage, and readiness transition for one
milestone commit atomically in the writer transaction. Durable invalidations
are emitted after commit for:

- current-epoch state transition;
- new complete snapshot publication;
- source coverage change;
- current snapshot becoming stale/degraded; and
- snapshot retirement that may expire cursors.

Invalidations may coalesce but must identify scope, epoch, snapshot, and commit.
Queries never cause these transitions.

### 9.4 Compatibility mapping

RFC 012A compatibility output is an input to source coverage:

- exact/range-supported catalog paths contribute normally;
- a fixture-backed forward-compatible catalog path for a
  `RecognizedUnverified` artifact contributes with explicit degraded quality and
  the observed-version reason;
- a required source without such a path is unavailable and prevents `Ready` for
  the current coverage plan; and
- compatibility recovery or support-release change creates a new coverage plan
  when interpretation or membership can change.

This mapping does not change RFC 012A's permitted decoding behavior and does not
reuse capability status as catalog readiness.

## 10. Startup tiers

Streams declare a readiness-oriented tier independent of dynamic urgency:

```text
Catalog
History
Enrichment
Maintenance
```

Startup performs:

1. open the durable owner and recover commits required for safe access;
2. establish a catalog-compatible query lane;
3. register every configured source and freeze the normalized coverage plan
   before full history work;
4. install watchers before the phase boundary;
5. enqueue bounded catalog-tier work across all sources using the RFC 012A
   declared overlap strategy;
6. publish complete or explicitly degraded aggregate catalog readiness for that
   exact plan;
7. expose catalog queries and render the library; and
8. continue history, usage, artifacts, search, audits, and maintenance in the
   background.

The host cannot wait for complete Claude history before beginning Codex or Grok
catalog discovery. Registering all sources does not authorize unbounded
parallel production.

## 11. Warm-start migration invariant

Every migration is classified before implementation:

| Class                                    | May block catalog presentation?  | Examples                                             |
| ---------------------------------------- | -------------------------------- | ---------------------------------------------------- |
| `BootCriticalCompatibility`              | Yes, when required for safe read | schema/open recovery needed to read catalog snapshot |
| `BackgroundProjectionPackRebuild`        | No                               | usage-v2 rebuild, capability-pack upgrade            |
| `BackgroundSearchOrMaintenanceMigration` | No                               | FTS rebuild, diagnostic compaction, secondary index  |

The last complete catalog is served as soon as its exact schema and snapshot
can be read safely. A migration unrelated to catalog interpretation cannot be
placed on that critical path merely because it shares the same database.

Background migration publishes independent pack readiness. Its failure may
degrade that pack but cannot invalidate an independently safe catalog snapshot.
If a boot-critical migration fails, the host returns an explicit catalog
unavailable/error state rather than silently opening incompatible rows.

## 12. Scheduling and selected hydration

The durable host uses bounded, starvation-limited scheduling across:

- catalog;
- live append;
- selected-session hydration;
- ordinary history;
- enrichment/search; and
- maintenance.

Startup tier determines required milestones, not permanent queue priority.
Writer depth and checkpoint pressure may reduce producer concurrency but cannot
skip a required readiness tier.

Selecting a session issues an idempotent/coalesced `requestHydration` command.
It raises bounded preference for that session without starving catalog, live
work, or eventual background convergence. Queries remain read-only.

## 13. User experience contract

- Warm start presents the last complete catalog immediately when safely
  readable and marks current reconciliation honestly.
- First cold start presents after complete or explicitly degraded aggregate
  catalog readiness, not after history or FTS convergence.
- Project/session rows expose content state such as `Metadata only`,
  `Indexing`, `Ready`, or `Unavailable`.
- Unknown counts are not rendered as zero.
- Search is disabled or labeled unavailable until `search.fts` is complete.
- A selected session may show a bounded loading/error state until a complete
  transcript page is ready.
- Renderer code does not parse agent-native files or construct a second
  catalog authority.

## 14. Performance status

The current targets are measurement hypotheses, not architecture law:

| Metric                                               | Current value          | Status                                |
| ---------------------------------------------------- | ---------------------- | ------------------------------------- |
| Cold complete catalog p95                            | <= 2 s                 | Experiment target                     |
| Cold complete catalog maximum                        | <= 5 s                 | Candidate release ceiling             |
| Warm first catalog page after transport availability | p95 <= 250 ms          | Experiment target                     |
| Catalog bytes versus selected primary transcript     | <= 1%                  | Experiment target                     |
| Additional catalog-phase RSS                         | <= 64 MiB              | Experiment target                     |
| Catalog query during background ingest               | p95 <= 100 ms          | Experiment target                     |
| Selected hydration                                   | p50 <= 1 s, p95 <= 5 s | Experiment target for defined classes |

A numeric value becomes a ratified release gate only through an amendment that
names the reference machine, corpus/support-release digests, cache state,
ordinary-session size classes, repetitions, and accepted variance. Semantic
correctness gates are release-blocking regardless of numeric calibration.

## 15. Failure and recovery

- Temporarily unavailable roots degrade coverage and retain the last complete
  snapshot; they do not retract every catalog assertion.
- Confirmed deletion under complete coverage retracts owned assertions and may
  publish entity tombstones in the next snapshot.
- Interrupted background builds resume from durable cursors without exposing a
  false-ready epoch.
- Query lanes retain the last safe complete snapshot during bounded maintenance
  quiescence.
- A metadata-only session remains catalog-visible without fabricated empty
  history.
- Search cannot return partial results as complete after crash or migration.
- A snapshot cursor either continues consistently or expires explicitly.

## 16. Rejected alternatives

### 16.1 A second fast catalog database or renderer index

Rejected because it creates another identity, deletion, migration, and query
authority.

### 16.2 Deriving catalog membership only from complete transcripts

Rejected because it omits legitimate metadata/index-only sessions and ties the
interactive boundary to full history work.

### 16.3 Moving-snapshot pagination

Rejected because a commit watermark on page 1 does not prevent duplicates or
omissions if page 2 reads newer state.

### 16.4 Blocking all migrations before warm presentation

Rejected because unrelated usage, FTS, or diagnostic rebuilds would recreate
the cold-start problem for an already complete catalog.

## 17. Conformance and acceptance

Release-blocking conformance covers:

- compatible catalog contract selection, additive typed-unknown preservation,
  and incompatible-major rejection;
- transcript-backed, index-only, nested-only, summary-only, missing-label,
  identity-conflict, and stale-metadata sessions;
- project move with and without explicit native identity evidence;
- index/cwd/directory/header/declared-ancestor project associations, competing
  project keys, association retraction, and unknown association coverage;
- policy-authorized native-ID/local-locator disclosure and withheld sensitive
  identity/locator evidence;
- index-to-transcript convergence on one session key;
- alias/supersede facts and forbidden heuristic merges;
- external project/session references across restart plus live, tombstoned,
  superseded, and unknown resolution without retargeting;
- deterministic precedence under conflicting values;
- evidence retraction, temporary root loss, confirmed deletion, and tombstone;
- every readiness transition in section 9, including version bump and recovery;
- stable coverage-plan digest under registration reorder and new epochs for
  required-source add/remove/enable/disable, support-release, declaration, and
  policy changes;
- temporary required-source loss, bounded retry, terminal degradation, and
  recovery without mislabeling a prior-plan snapshot current;
- atomic rows/readiness/outbox publication and crash injection at each boundary;
- page updates between every continuation request without duplicate/missing
  rows, plus explicit snapshot expiration;
- warm startup with boot-critical, usage, FTS, and diagnostic migrations;
- query purity and explicit hydration-command coalescing;
- all-source catalog scheduling without live/background starvation; and
- cold/warm performance reports with fixed evidence and environment digests.

RFC 012B is complete when catalog identity and final hydrated identity match on
all fixtures; a complete or degraded library renders without waiting for
history/FTS; last-complete warm presentation survives non-catalog migrations;
readiness never claims uncommitted or wrong-coverage-plan state; pagination is
snapshot-consistent; and separately ratified numeric release ceilings pass.
