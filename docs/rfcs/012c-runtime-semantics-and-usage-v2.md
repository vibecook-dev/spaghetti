# RFC 012C: Runtime semantic contracts and usage-v2

- **Status:** Implemented (landing 2026-08-23); ratification pending owner review
- **Created:** 2026-08-15 · **Trimmed to what shipped:** 2026-08-23
- **Parent:** [RFC 012 umbrella](./012-evidence-backed-adapters-and-progressive-readiness.md)
- **Depends on:** [RFC 012A](./012a-agent-adaptation-and-engine-boundaries.md)
- **Landing:** [landing plan](./012-landing-plan.md) §3.3 (consumer) and §8 (lane L3)
- **Full 2026-08-15 draft:** [archive/012c-…-2026-08-15-draft.md](./archive/012c-runtime-semantics-and-usage-v2-2026-08-15-draft.md)
- **Owns:** the common revision/reducer law for the eleven runtime fact
  families, response-level usage, actor/run and affiliation identity, effective
  state, and the qualified-value discipline that keeps unknown out of totals
- **Does not own:** source access and decoding (012A), catalog readiness
  (012B), observer delivery and epochs (012D)

## 1. What this document is now

The 2026-08-15 draft specified usage-v2 as a *migration*: a shadow projection
beside the legacy one, a durable selection record, promotion and rollback
transactions, a compatibility-comparison query, and telemetry to watch the two
disagree. All of that was built. None of it is left.

The landing deleted the legacy path instead of migrating off it. Response-level
usage is now the only usage there is, so there is nothing to select between,
nothing to promote, and nothing to roll back to. The semantic contract — what a
response is, how a revision replaces one, what a bucket asserts, what unknown
means — is unchanged and is what this file records.

## 2. Decisions (as implemented)

1. **Usage is one snapshot per native response, not one contribution per
   source row.** A response contributes at most once to its session and to its
   project no matter how many native rows revised it.
2. **Claude's primary response key is a non-empty `message.id`**, scoped by
   source instance, object, and generation. `requestId` is correlation
   metadata, never identity: the census found it absent on 268 rows and shared
   across message ids on eight.
3. **Every token bucket is independently qualified.** `exact`,
   `native_claimed`, `derived`, `estimated` assert a number; `unknown` asserts
   nothing and is never folded into a total. An omitted bucket is unknown, not
   zero.
4. **A later snapshot replaces the earlier one**, downward corrections
   included. An exact repeat changes nothing.
5. **Actor/run identity is mandatory** on every runtime fact. Team and
   workflow affiliations are orthogonal metadata; they regroup existing
   contributions and never create a second one.
6. **State is dimensioned and revisioned.** Configured intent and observed
   effective state are different evidence qualities and never collapse.
7. **One reducer law, two sinks.** Durable ingestion and the scoped observer
   run the same reduction over the same facts, so the two topologies cannot
   disagree about a revision.
8. **Unknown native evidence stays bounded evidence**, not an error and not a
   silent drop.
9. **Spaghetti supplies qualified revisions, not derived rates.** Burn rate,
   context-window percentage, and model-capacity catalogues are downstream.

## 3. Semantic rules the engine enforces

### 3.1 Common revision law

`crates/spaghetti-napi/src/runtime_semantic_reducer.rs` is topology-neutral by
construction: it imports no database and no observer type. It owns

- reduction to `Unchanged | Upsert | Retract` per entity;
- source order as the resolver within one object generation, with a
  deterministic tie-break across objects — never callback or delivery order;
- retraction of revisions owned solely by an old generation before corrected
  replay;
- complete owned-set snapshots retracting absent members, and partial evidence
  proving nothing about absence;
- a reduced-state digest (`RUNTIME_REDUCED_STATE_DIGEST_CONTRACT_VERSION`) that
  excludes epoch, sequence, phase, and observation time, so a clean bootstrap
  and a completed resync at equal coverage produce equal digests.

**What makes two facts for one entity two revisions** is a per-family rule,
decided once in `Fact::revision_binding`
(`crates/spaghetti-napi/src/adapter/semantic.rs`) so the durable coordinator,
the scoped observer, and the reducers cannot disagree while reaching a fact by
different paths:

| Binding | Families | Rule |
| --- | --- | --- |
| `ValueAtSourceRecord` | message, content block, tool, user-input request, plan, task, native marker, effective state | the revision composes the value key **with the record that proved it** |
| `Value` | every other family — usage-v2, actor run, actor affiliation, artifact snapshot, … | a repeated value is deliberately *one* revision |
| `SourceRecord` | any fact with no revision key | the record is the revision |

The first group keys facts by an entity that outlives any single record — an
actor's model, a tool call answered several records later — so two records can
legitimately prove the same value, and only the record tells those revisions
apart. The second is idempotent by construction: re-asserted usage, an
unchanged affiliation, and a re-read snapshot are one revision however often
they are observed.

Entity identity is unaffected: `CanonicalFactId` and every `ExternalEntityRef`
are the same under either rule, and only `FactRevisionId` — hence
`SemanticRevisionRef.fact_revision_id` — differs. Changing a binding is a
semantic-version change forcing a replay, which `SCHEMA_VERSION` 64 does
locally; a downstream consumer holding those ids rebuilds its own derived state.

### 3.2 Usage

`crates/spaghetti-napi/src/engine/usage_query.rs` holds the query side.

- Four buckets: `input`, `output`, `cache_creation`, `cache_read`. Each carries
  its own resolved qualification; a bucket that asserts nothing is reported
  unknown rather than summed as zero.
- `component_total_tokens` is the sum of the four; buckets that assert nothing
  add nothing.
- A response is counted in exactly one of three classes: fully exact,
  partially known (`estimated_contribution_count`), or asserting nothing
  (`unknown_contribution_count`). `contribution_count` counts distinct
  responses, not native rows — that is the correction.
- A windowed report splits days and keeps the all-time aggregate; a response
  whose date is unusable is reported untimed rather than dropped.
- A session outside the requested project is rejected, not answered empty.
- `USAGE_QUERY_CONTRACT_VERSION` tracks the shared query-pack protocol number.
  The response-level change itself is carried by `SCHEMA_VERSION`, which
  rebuilds — there is no per-query usage version to negotiate.

### 3.3 Actor and affiliation

Actor-run and actor-affiliation are separate families with separate revisions.
A late affiliation revises grouping for the same actor; it does not copy that
actor's messages, tasks, or usage into a second contribution identity. Removing
an affiliation removes the response from that grouping and leaves the actor's
canonical contribution untouched. Ambiguous child identity stays uncorrelated
evidence — it is never assigned to the root.

### 3.4 The eleven families

`message`, `content_block`, `tool`, `user_input_request`, `plan`, `task`,
`native_marker`, `effective_state`, `actor_run`, `actor_affiliation`,
`usage_v2` (`ObserverFamily` in `crates/spaghetti-napi/src/observer/event.rs`,
`RuntimeSemanticFamily` in `runtime_semantic_reducer.rs`). Every one has a
reducer, a fixture, a typed value, a place on the observer wire, and — as of
lane L5 — a Claude emitter in
`crates/spaghetti-napi/src/claude/runtime_facts.rs`. §7 records the two places
where the evidence available is weaker than the family.

## 4. Shipped interface

**Facts.** `Fact::UsageRevisionV2`, `Fact::ActorRunRevision`,
`Fact::ActorAffiliationRevision`, `Fact::MessageRevision`,
`Fact::ContentBlockRevision`, `Fact::ToolRevision`, `Fact::TaskRevision`,
`Fact::PlanRevision`, `Fact::EffectiveStateRevision`,
`Fact::NativeRuntimeMarkerRevision`, `Fact::UserInputRequestRevision`, and
`Fact::UnknownRecord` — all in `crates/spaghetti-napi/src/adapter/facts.rs`.
`Fact::Usage`, the additive legacy variant, no longer exists.

**Generated types.** `SemanticRevisionRef`, `FactRevisionId`, `ActorRef`,
`ActorAttribution`, `SemanticOperation`, `ObserverFamily`, `SemanticEvent` in
`packages/sdk/src/generated/`.

`SemanticEvent.value` is a **`RuntimeSemanticValue`** — an externally tagged
union with one variant per family (`{ MessageRevision: MessageRevisionFact }`,
`{ ToolRevision: ToolRevisionFact }`, and so on), generated from the same
`adapter/facts.rs` types the decoder emits;
`crates/spaghetti-napi/src/adapter/runtime_value.rs` proves the wire shape and
the typed shape agree for every family. Narrowing on `family` narrows the value
with it, so nothing is cast. `value` is `null` for a retraction — the reducer
removed the entity, so there is no current value to carry.

**Native surface.** `SpaghettiEngine.getUsage(...)` and
`SpaghettiEngine.getStats(...)` in `crates/spaghetti-napi/index.d.ts` return
response-level values. The draft's `getRuntimeUsageV2`,
`getRuntimeUsageTotals`, `getRuntimeUsageCompatibility`, and the promotion and
rollback commands were deleted with the migration.

**CLI and playground.** `spag stats`
(`packages/cli/src/commands/stats.ts`) aggregates response-level totals and
labels their quality `exact`, `estimated`, or `mixed`. The playground usage
views read the same values.

## 5. Acceptance tests

`crates/spaghetti-napi/src/engine/usage_query/tests.rs`, behavioural against
real SQLite:

| Test | Rule proven |
| --- | --- |
| `evolving_counters_for_one_response_collapse_to_one_contribution` | §2.1, §2.4 |
| `a_downward_revision_corrects_the_total_instead_of_being_rejected` | §2.4 |
| `distinct_responses_each_contribute_once` | §2.1 |
| `an_omitted_bucket_stays_unknown_and_is_never_summed_as_zero` | §2.3 |
| `coverage_names_the_native_field_behind_every_qualified_bucket` | provenance |
| `a_window_splits_days_and_keeps_the_all_time_aggregate` | §3.2 |
| `a_response_without_a_usable_date_is_reported_untimed_rather_than_dropped` | §3.2 |
| `a_session_outside_the_project_is_rejected_rather_than_answered_empty` | §3.2 |
| `window_bounds_are_calendar_checked_and_bounded` | §3.2 |
| `the_in_repo_claude_fixture_matches_the_independent_oracle` | §6 |

**Independent oracle.** `scripts/usage_v2_oracle/` reproduces response grouping
and bucket qualification from native records without importing the adapter,
SDK, schema, or Rust output. `python3 scripts/usage_v2_oracle/test_oracle.py`
runs it; `scripts/validate-all.sh` runs it as the suite *RFC 012C Usage-v2
Oracle*. It was exact against the in-repo fixture (119 responses) and against a
real corpus slice (5,238 responses).

**Reducer parity across topologies.**
`crates/spaghetti-napi/src/observer/tests/families.rs` —
`all_eleven_families_reduce_and_appear_in_the_replacement_manifest`,
`an_exact_repeat_of_a_revision_adds_nothing`,
`retracting_an_objects_facts_empties_its_families`; and
`observer/tests/lifecycle.rs` —
`a_repeated_usage_row_adds_nothing_and_a_correction_replaces_it`.

**End to end, native decode to typed TypeScript.**
`packages/sdk/src/__tests__/observe-session-families.test.ts` drives a real
`.claude`-shaped tree through the observer and asserts the families arrive with
typed values, and that the bootstrap barrier reports what it actually reduced.

## 6. The correction, in numbers

On the full Claude corpus, response-level accounting reports **36.88B tokens
where the 0.7.x additive path reported 78.52B — 2.129× lower**, from 362,043
native rows resolving to 158,118 distinct responses. The old number was not a
different opinion about the same events; it counted streamed-response repeats
as separate consumption. `getStats` p95 improved 24%; `getUsage` p95 rose
0.7 ms, which was accepted.

Codex and Grok also emit response-level facts now. The Codex legacy path had
double-counted cache-read inside input and reasoning inside output; the Grok
delta was zero at the time of that measurement.

Grok usage has since become **exact per response** rather than a session-scoped
estimate: one `turn_completed` record in `updates.jsonl` is one response
contribution (`crates/spaghetti-napi/src/grok/adapter.rs`). The estimate path
is deleted. `cacheCreationTokens` is absent on some turns and stays `unknown`
there rather than being inferred, and context occupancy is explicitly *not*
treated as per-response usage.

## 7. Where the evidence is weaker than the family

All eleven families are emitted by the Claude decoder
(`crates/spaghetti-napi/src/claude/runtime_facts.rs`). Two places deserve a
consumer's attention, because the family exists but the native evidence behind
it is narrower than the name suggests:

- **Plans come from tool evidence, not from plan documents.** `PLAN_TOOLS`
  is `["ExitPlanMode", "EnterPlanMode"]`: a `plan` revision is derived from
  those tool calls in the transcript, which is where actor binding exists.
  The `plans/<slug>.md` sidecars remain snapshot facts with no actor binding —
  they are not a second source of `plan` revisions, and
  `plan-document-from-evidence` is not among the relations the Claude scope
  program declares.
- **An orphaned `tool_result` claims no tool entity.** If a result's call fell
  outside the bounded per-object correlation window, the result keeps its
  content-block evidence — which already carries the native call id — and no
  `tool` revision is emitted for it. RFC 012C forbids inventing the tool name a
  `tool_result` never carries, so the honest outcome is content-block evidence
  without a guessed name rather than a fabricated tool.

Not implemented, deliberately:

- **Usage migration machinery** — selection, promotion, rollback, the
  compatibility query and its telemetry, and the legacy projection they
  protected. Deleted; §1.
- **`code.activity`.** Tool evidence is still not repository truth. A file path
  or shell command inside a tool payload proves the tool evidence and nothing
  about workspace state, commits, retained code, or contribution.
- **Durable/live overlay reconciliation as a shipped helper.** The pieces exist
  — a durable query and an observer event for the same revision carry equal
  `SemanticRevisionRef` values — but no code in this repository performs the
  deduplication; the consumer does. See
  [docs/integration/vibefield-phase-a.md](../integration/vibefield-phase-a.md).

## 8. Superseded sections of the 2026-08-15 draft

- §5 `RuntimeRevisionMeta` and the reducer-class vocabulary — superseded by
  `crates/spaghetti-napi/src/runtime_semantic_reducer.rs`.
- §5.1 `DurableEvidencePage<T>` — superseded by the existing RFC 011 query
  results, which carry `atCommitSeq` and the shared `SemanticRevisionRef`.
- §6 `ActorRunRef` / `ActorAffiliationRevision` / `ActorAffiliationContext`
  wire shapes — superseded by `ActorRef` and `ActorAttribution` in
  `packages/sdk/src/generated/`.
- §7 `UsageRevision` shape — superseded by `UsageRevisionV2Fact` in
  `adapter/facts.rs` and the aggregates in `engine/usage_query.rs`.
- §9 `EffectiveStateRevision<T>` and §10–§11 family shapes — superseded by the
  corresponding `Fact::*Revision` variants in `adapter/facts.rs`.
- §12 reducer-family matrix — superseded by `RuntimeSemanticFamily` plus the
  replacement manifest the observer emits (`FamilyManifestEntry`).
- §13 durable usage-v2 migration in full, including
  `UsageV2ProjectionReadiness`, `UsageQuerySelection`, `getRuntimeUsageTotals`,
  `getRuntimeUsageCompatibility`, the rollback drill, and every
  contract-17-through-22 status paragraph — superseded by deletion; §1 and §7.
  The historical corpus reports it cites remain under
  `agent-support/claude-code/`.
- §19 (draft §12) capability `Supported | Degraded | Unsupported` reporting —
  superseded by the RFC 012B readiness vector plus the observer's family
  manifest.

## 9. Acceptance

RFC 012C is met for this landing when usage matches the independent
qualified-bucket oracle at response, session, and project scope; unknown is
never reported as zero; an exact repeat adds nothing and a downward revision
corrects; a generation reset retracts before replay; durable and observer
reduction of the same fact produce the same revision reference; and every
supported family is actually emitted with a typed value. All six hold as of
2026-08-23 (landing plan §8, lanes L1, L3, and L5). The two evidence limits in
§7 are documented behaviour, not outstanding work.
