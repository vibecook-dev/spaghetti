# RFC 012B: Catalog, readiness, and progressive startup

- **Status:** Implemented (landing 2026-08-23); ratification pending owner review
- **Created:** 2026-08-15 · **Trimmed to what shipped:** 2026-08-23
- **Parent:** [RFC 012 umbrella](./012-evidence-backed-adapters-and-progressive-readiness.md)
- **Depends on:** [RFC 012A](./012a-agent-adaptation-and-engine-boundaries.md)
- **Landing:** [landing plan](./012-landing-plan.md) §3.3 (consumer) and §8 (lane L4)
- **Full 2026-08-15 draft:** [archive/012b-…-2026-08-15-draft.md](./archive/012b-catalog-readiness-and-progressive-startup-2026-08-15-draft.md)
- **Owns:** catalog membership facts, project/session identity and association
  evidence, external reference resolution, the readiness vector,
  snapshot-consistent catalog pagination, and catalog-first startup
- **Does not own:** adapter support policy (012A), runtime fact semantics
  (012C), observer delivery (012D)

## 1. What this document is now

The 2026-08-15 draft specified a durable readiness state machine with epochs,
coverage-plan lineage, publication records, and snapshot-identity negotiation.
That machinery was built and then deleted during the landing: it was 66,777
lines that no consumer read. What shipped keeps every semantic guarantee the
draft was written to obtain and derives them from committed rows inside one
snapshot instead of from a second state authority.

This file is the contract as enforced by code. The archived draft remains the
record of the design reasoning and the rejected alternatives.

## 2. Decisions (as implemented)

1. **Catalog membership is a first-class fact**, distinct from decoded history.
   A session that a native index names but whose transcript has not been read
   is catalog-visible and honestly labelled — it is not hidden, and it is not
   given a fabricated empty history.
2. **Catalog state is derived, not stored.** Four states, each strictly
   stronger than the last: `discovered`, `transcript_backed`, `hydrated`,
   `searchable`. Discovery writes evidence; the state is computed at read time
   from committed RFC 011 rows in the same snapshot, so the catalog and the
   history projection cannot disagree about one session.
3. **Readiness is a vector of six independent fields**, not a state machine:
   `catalog`, `history`, `usage`, `capabilities`, `artifacts`, `search`. Each
   carries its own state and the commit sequence its evidence was read at.
   `catalog` is routinely `ready` while `history` is `indexing` and `search` is
   `pending`; that is what catalog-first startup means.
4. **Catalog readiness is the interactive boundary.** Startup runs one bounded
   discovery pass per configured source, commits its rows in a single
   transaction, and publishes catalog readiness before history, usage,
   artifacts, or FTS converge.
5. **Identity is deterministic and adapter-declared.** Prompt, path, basename,
   or timestamp similarity never merges two entities. A session claimed by two
   projects keeps both claims: the losing one is retained as an explicit
   identity conflict rather than merged away.
6. **Rows expose a persistable RFC 012A `ExternalEntityRef`**, and resolution
   of a reference whose evidence is gone reports retraction rather than
   silently returning a different live entity.
7. **Pagination is snapshot-consistent.** A cursor carries the watermark it was
   minted at; a continuation page is answered from that same snapshot.
8. **Queries are pure.** Nothing in the catalog read path schedules work,
   triggers ingest, or repairs state.
9. **A source that cannot be read completely is degraded, not empty.** It keeps
   the rows it has, records why, and marks the `catalog` readiness field
   `degraded` with the reason.

## 3. Semantic rules the engine enforces

**Membership and state.** `catalog_state` is derived per row inside the reading
transaction. `hydrated` is proven by an `EXISTS` probe for a decoded message on
that session key — never a counted join, which measured 1.7 s against 1 ms on a
mid-rebuild corpus and grows with the message table. `searchable` is proven by
the durable `schema_meta.query_bootstrap_state` marker being absent, which is
the same scalar every other surface reads.

**Absence.** An unknown count is not zero and an unknown title is not an empty
string. `nativeMessageCount` is what the native surface claims;
`decodedMessageCount` is what has been read. Both are present and separate.

**Association.** Every session row carries `associationBasis`,
`associationQuality`, and `associationProvenance` for the project it is listed
under, plus `identityConflicts[]` for every competing claim that lost
precedence but kept its evidence.

**Evidence loss.** A rescan that no longer finds a transcript retracts the rows
that source generation owned. A source that is temporarily unreadable is
degraded and keeps its rows: unavailability is not deletion. An unchanged
rescan costs no commit.

**Snapshot consistency.** One read opens one transaction, takes the committed
watermark, and answers from it. The cursor carries the raw sort and entity key
plus that watermark, so a commit landing between page one and page two cannot
duplicate, drop, or reorder a row. Sort ties terminate in stable entity-key
order.

**Reference resolution.** `resolveCatalogEntity` accepts the persisted external
reference — not a pagination handle — and returns a live project, a live
session, `Retracted`, or `Unknown`. It never retargets a retracted key.

**Readiness derivation.** Every field is computed from row counts in one
snapshot. `history`, `capabilities`, `artifacts`, and `search` converge against
the number of transcript-backed catalog sessions. `capabilities` and
`artifacts` are reported from the hydration number on purpose: those facts are
emitted by the same decode pass that produces messages, so a separate schedule
would be a fiction. `search` is `pending` until full-text bootstrap finishes.

**A pending projection is an error, not an empty result.** While search is
still building, a search query fails with the typed `projection_pending` code
(`packages/sdk/src/client/errors.ts`) and the playground renders
`Building the search index…`. Partial search must never masquerade as complete
search, and zero hits from a half-built index is exactly that — so the boundary
refuses rather than answers.

## 4. Shipped interface

Types are generated from Rust by `pnpm generate:types`; nothing below is
hand-written.

| Concept | Generated type | Declared in |
| --- | --- | --- |
| Project row | `CatalogProjectRow` | `engine/catalog/query.rs` |
| Session row | `CatalogSessionRow` | `engine/catalog/query.rs` |
| Page | `CatalogProjectPage`, `CatalogSessionPage` | `engine/catalog/query.rs` |
| Availability | `CatalogState` | `engine/catalog.rs` |
| Identity conflict | `IdentityConflict` | `engine/catalog/query.rs` |
| Readiness vector | `Readiness`, `ReadinessField`, `ReadinessState` | `engine/catalog/readiness.rs` |
| External reference | `ExternalEntityRef`, `CanonicalEntityKey` | `adapter/semantic.rs` |

Generated TypeScript lives in `packages/sdk/src/generated/`; `index.ts` there
is the barrel.

**Native surface** (`crates/spaghetti-napi/index.d.ts`, class `SpaghettiEngine`):

```ts
listCatalogProjects(options?, signal?): Promise<EngineCatalogProjectPage>
listCatalogSessions(options?, signal?): Promise<EngineCatalogSessionPage>
resolveCatalogEntity(externalRef: string, signal?): Promise<EngineCatalogResolution>
readiness(signal?): Promise<EngineReadiness>
startConfiguredObservation(options, signal?): Promise<EngineCatalogStartup>
```

`startConfiguredObservation` resolves once every configured source has
committed its discovery rows — that is the catalog-first boundary — and returns
`{ catalogProjects, catalogSessions, degradedSources, supervisorsStarted,
historyBackground, status }`.

**SDK.** `packages/sdk/src/observation-host.ts` exposes `host.catalog`
(`SpaghettiCatalogStartup`, available immediately after
`openObservationHost`) and `host.readiness()`. `packages/sdk/src/native.ts`
re-exports the generated rows under their `Spaghetti*` names
(`SpaghettiCatalogProject`, `SpaghettiCatalogSession`, `SpaghettiReadiness`,
`SpaghettiReadinessField`, `SpaghettiReadinessState`).

**CLI.** `spag projects` and `spag sessions` list from the catalog and merge
decoded statistics only once history has converged
(`packages/cli/src/commands/{projects,sessions}.ts`, helpers in
`packages/cli/src/lib/catalog.ts`). `spag doctor` renders the readiness vector
(`packages/cli/src/commands/doctor.ts`).

## 5. Acceptance tests

`crates/spaghetti-napi/src/engine/catalog/tests.rs` — behavioural, against real
SQLite and real `.claude`-shaped trees:

| Test | Rule proven |
| --- | --- |
| `cold_catalog_is_complete_before_any_history_row_exists` | §2.4 catalog-first |
| `warm_start_serves_the_last_committed_catalog_before_rescanning` | warm presentation |
| `a_source_that_cannot_be_read_is_marked_degraded_and_keeps_its_rows` | §2.9 |
| `a_long_failure_reason_still_fits_the_row_that_records_it` | degraded reason durability |
| `a_rescan_picks_up_a_new_transcript_and_retracts_a_deleted_one` | evidence loss |
| `an_unchanged_rescan_costs_no_commit` | idempotent reconciliation |
| `a_session_claimed_by_two_projects_reports_the_conflict` | §2.5 identity conflicts |
| `pagination_is_stable_across_a_commit_between_pages` | §3 snapshot consistency |
| `history_convergence_promotes_the_catalog_state_of_a_transcript` | §2.2 derived state |
| `assert_no_legacy_catalog_tables` | the deleted machinery stays deleted |
| `catalog_startup_on_a_real_corpus` | §6 budget |

`packages/sdk/src/__tests__/observation-host.test.ts` — host-level:
`explicit deferred bootstrap serves catalog queries while search is
incomplete`, `listProjects and listSessions return their pre-catalog fields
unchanged` (whole-response equality against a pre-landing baseline), and
`getOverview` parity.

## 6. Performance, measured

On the production-shaped Claude corpus, catalog listable in **122 ms cold /
8 ms warm** against the landing-plan §6 budgets of 10 s and 1 s — 258 ms cold on
a 3.2 GB copy. The playground library screen renders from the catalog path in
491 ms.

What catalog-first buys is now measurable end to end: on that 3.2 GB corpus the
catalog is listable in a quarter of a second while complete history arrives at
193 s and search at 202 s. The gap between those numbers is the whole point of
this RFC, and it closed from roughly three hours to three minutes when durable
ingest went from 70 to 11,653 records/s
([the performance report](./012-landing-perf-report.md)).

The draft's §14 table of experiment targets is superseded by these numbers and
by landing plan §6; no numeric value here is a ratified release ceiling.

## 7. Superseded sections of the 2026-08-15 draft

Each line names where the guarantee now lives. The draft text is in the
archive link at the top.

- §5 catalog fact model (`CatalogProjectFact`, `CatalogSessionFact`,
  `SessionProjectAssociationFact`, `NativeLocatorClaim`) — superseded by the
  generated rows in §4 and the discovery structs in
  `engine/catalog/discovery.rs`.
- §6.3 `IdentityRelationFact` alias/merge/supersede algebra — not implemented;
  see §8.
- §7 reducer laws — superseded by derivation at read time (§3) plus
  `engine/catalog/store.rs` for commit ownership.
- §8 `CatalogCoveragePlan`, `CatalogSnapshotId`, `CatalogCursor`,
  `IncompatibleCatalogContract` negotiation — superseded by the opaque
  `catalog_v1_` watermark cursor in `engine/catalog/query.rs`.
- §8 `CatalogEntityResolution` four-arm union — superseded by
  `CatalogEntityResolution` in `engine/catalog/query.rs`
  (`LiveProject | LiveSession | Retracted | Unknown`).
- §9 durable `ReadinessState` record, epoch/attempt lineage, the transition
  table, and publication/invalidation — superseded by `Readiness` in
  `engine/catalog/readiness.rs`.
- §10 startup tiers and §12 scheduling/`requestHydration` — superseded by
  `startConfiguredObservation` plus the existing RFC 011 supervisor.
- §11 warm-start migration classes — superseded by SCHEMA_VERSION gating
  (`core/schema.rs`); a schema bump rebuilds, and the catalog is the first
  thing the rebuild publishes.
- §13 UX contract — implemented in `packages/cli/src/lib/catalog.ts` and the
  playground library screen.
- §14 performance targets — superseded by §6 above.

## 8. Not implemented

Named here so no reader mistakes silence for coverage.

- **Identity relations.** Alias, `SameEntity`, `Supersedes`, and `ReplacedBy`
  facts do not exist. A moved project whose adapter cannot prove native
  identity becomes a new project. Conflicts are reported, never related.
- **Policy-gated disclosure.** Native ids and locators are returned to any
  caller of the local engine. The draft's authorized-view/`Withheld`
  distinction has no implementation and no consumer.
- **`requestHydration`.** Selecting a session does not raise its ingest
  priority; background convergence reaches it in its own order.
- **Coverage-plan lineage.** Changing the configured source set does not mint a
  new plan identity. A rebuild is driven by SCHEMA_VERSION alone.
- **Explicit snapshot expiry.** A cursor minted before a schema rebuild is
  rejected as malformed rather than answered with `SnapshotExpired`.

## 9. Acceptance

RFC 012B is met for this landing when a complete or explicitly degraded library
renders before history and FTS converge; catalog identity matches final
hydrated identity on the corpus; unknown counts are never rendered as zero;
pagination is stable across background commits; and the readiness vector never
claims a state its own snapshot does not support. All five hold as of
2026-08-23 (landing plan §8, lane L4).
