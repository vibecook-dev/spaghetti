# RFC 012C: Runtime semantic contracts and usage-v2

- **Status:** Draft child RFC; proposed semantic migration and fact contract
- **Created:** 2026-08-15
- **Parent:** [RFC 012 umbrella](./012-evidence-backed-adapters-and-progressive-readiness.md)
- **Depends on:** [RFC 012A](./012a-agent-adaptation-and-engine-boundaries.md)
- **Program plan:** [RFC 012 implementation plan](./012-implementation-plan.md)
- **Evidence:** [Phase 0B runtime census](./012-runtime-observation-census-2026-08-15.md)
- **Owns:** common runtime revision/reducer/replacement law; actor/run,
  affiliation, message/content, response-level usage, effective-state,
  plan/task/tool/progress, and structured-interaction facts; capability quality;
  aggregate-facing durable/live runtime reconciliation; and usage-v2 durable
  migration semantics
- **Does not own:** source access/decoding mechanics, observer delivery epochs
  and queues, catalog readiness, process-lifetime runtime identity,
  model-capacity catalogs, burn-rate formulas, Git analytics, or contribution
  claims

## 1. Summary

Spaghetti will represent runtime evidence as revisioned, provenance-bearing
facts that are independent of durable or ephemeral delivery. Claude usage moves
from additive JSONL-row contributions to one replaceable snapshot per native
response. Actor/run identity is mandatory; team and workflow affiliations are
orthogonal metadata and never create a second token contribution.

Every usage bucket uses RFC 012A's qualified-value contract. Missing or
unsupported buckets remain unknown rather than becoming zero. Model, effort,
session mode, permission mode, plans, tasks, tools, progress, compaction, and
user-input interactions likewise preserve what native evidence actually proves
and when it proves it.

RFC 012D transports these facts to live consumers. This RFC defines their
semantic identity and reduction regardless of transport.

## 2. Evidence

The selected production corpus contained:

- 342,861 usage-bearing assistant rows;
- 149,077 file-scoped response groups;
- 193,784 repeated rows;
- 57,150 groups with evolving counters;
- 111 downward-correction groups;
- 268 rows without `requestId`; and
- eight request IDs associated with multiple message IDs.

Therefore a usage-bearing transcript row is not an additive token event, and
`requestId` is not a sufficient response identity. The current semantic model
must change rather than preserve an incorrect oracle.

The same census found native evidence for model IDs, tool calls/results,
compaction/progress, modes, plans/tasks, actor relationships, teams/workflows,
and structured questions, but quality and timing vary. The common facts cannot
claim instantaneous state transitions when evidence is only observed on a
later response.

## 3. Decisions

1. Usage is one response-keyed snapshot contribution with ordered revisions,
   not one additive contribution per source row.
2. Claude uses non-empty native `message.id` as the primary response key within
   source instance/object/generation. `requestId` remains optional correlation
   metadata.
3. Every token bucket is a `QualifiedValue<u64>`. Unknown, omitted, and exact
   zero remain distinguishable.
4. Later response snapshots replace prior bucket contributions, including
   downward corrections. Exact repeats do not change totals.
5. Actor/run attribution is mandatory before usage contributes to canonical
   totals. Affiliations are not part of contribution identity.
6. Runtime state is dimensioned and revisioned; configured intent and observed
   effective state are different evidence qualities.
7. User-input requests are a distinct typed lifecycle, not generic permission
   requests or opaque tool payloads.
8. Unknown native fields/families remain bounded evidence and drift signals.
9. `usage-v2` is an explicit fact, storage, query, and migration version with an
   independent oracle and rollback projection.
10. Spaghetti supplies canonical revisions and evidence quality. It does not
    own model-capacity catalogs or presentation burn-rate formulas.
11. Every runtime family has deterministic entity/revision identity, one
    declared reducer class, explicit retraction semantics, and a complete
    replacement representation.
12. Aggregate-facing durable runtime/history results expose the same RFC 012A
    semantic revision references and source/family coverage as scoped
    observation. Durable commit and observer delivery order remain distinct.
13. Transcript tool evidence does not become repository truth or a contribution
    claim. A normalized `code.activity` pack requires a later evidence-backed
    contract and is not an RFC 012 release gate.

## 4. Contract maturity

| Element                                           | Classification     |
| ------------------------------------------------- | ------------------ |
| Response snapshot/upsert usage semantics          | Semantic contract  |
| Qualified missing-versus-zero bucket semantics    | Semantic contract  |
| Actor/affiliation contribution identity           | Semantic contract  |
| Cross-family revision/reducer/replacement law     | Semantic contract  |
| Durable/live runtime reconciliation               | Semantic contract  |
| Message/content identity and correction semantics | Semantic contract  |
| Effective-state evidence and timing rules         | Semantic contract  |
| User-input request lifecycle                      | Semantic contract  |
| Exact serialized/N-API field representation       | Proposed API       |
| Godview model-capacity and burn-rate presentation | Outside Spaghetti  |
| Legacy usage-v2 numeric difference                | Reviewed migration |
| Normalized historical `code.activity` pack        | Future RFC         |

## 5. Common runtime revision and reduction law

Every native-derived runtime fact family supplies semantic equivalents of:

```text
RuntimeRevisionMeta {
  family_contract_version
  entity_key
  revision_key
  semantic_revision_ref: SemanticRevisionRef
  ownership: {
    source_instance_key
    source_object
    generation
    snapshot_scope?
  }
  source_order_or_revision
  operation: Upsert | Retract
  actor_run_key?
  native_time?
  provenance
}
```

Entity and revision keys follow RFC 012A. A decoder cannot substitute delivery
phase, database commit, observer epoch, host time, or queue order for missing
native/source identity. When a family has no actor, `actor_run_key` is absent by
contract rather than guessed. `semantic_revision_ref` is the RFC 012A public
view of this fact revision; an encoding whose embedded fact/revision identity
does not match `entity_key` and `revision_key` is invalid.

Each family declares one reducer class:

```text
RevisionedEntity       latest accepted revision per entity; explicit retract
OwnedSetSnapshot       complete revision replaces the prior owned set
CorrelatedLifecycle    revisioned entities plus explicit native correlation
CurrentGenerationLog   stable events retained for current source ownership
UsageContribution      response snapshot replacement defined in section 7
```

Common laws:

1. Source order resolves revisions from one object/generation. Cross-object
   precedence uses the fact-family authority/effective-time contract and a
   deterministic tie-breaker, never callback order.
2. Reset, replacement, or confirmed deletion retracts revisions owned solely by
   the old object/generation before corrected replay.
3. Temporary source unavailability does not invent retractions.
4. A complete owned-set snapshot retracts prior members absent from the new
   revision. A partial snapshot cannot prove absence.
5. Durable and observation projections invoke the same reducer law. Storage
   commits and delivery envelopes may add topology metadata but cannot alter
   reduced semantic state.
6. An RFC 012D full snapshot contains the sufficient current-generation reducer
   input described by the family matrix in section 12. At one comparable RFC
   012A source/family coverage vector, clean bootstrap and resync therefore
   produce the same reduced state digest.

The family-specific shapes below embed or accompany this metadata even when
the repeated fields are omitted from illustrative pseudocode. A family cannot
replace `RuntimeRevisionMeta` ordering, ownership, operation, or identity with
a delivery-only field.

### 5.1 Durable/live reconciliation view

RFC 011 remains the durable query implementation authority. For any
history/runtime query intended to reconcile with scoped observation, its
semantic result is equivalent to:

```text
DurableEvidencePage<T> {
  snapshot_id: {
    query_pack_contract_version
    at_commit_seq
    readiness_epoch?
  }
  projection_status
  source_coverage: SourceCoverageSet[]
  items[] {
    subject_refs: ExternalEntityRef[]
    semantic_revision_ref: SemanticRevisionRef
    value: T
  }
  next_cursor? {
    snapshot_id
    query_fingerprint
    stable_order_key
    semantic_entity_key
  }
}
```

The pseudocode models a revision-granular reconciliation view. A presentation
query may return a composite row, but if that row participates in durable/live
reconciliation it exposes each native-derived semantic component and its
reference; one arbitrary “primary” reference cannot stand for several message,
content-block, tool, or state revisions. The exact API representation is
provisional; these laws are normative:

1. `SemanticRevisionRef`, `SourceCoveragePoint`, and `SourceCoverageSet` have
   the RFC 012A meaning.
   An item representing the same native-derived revision as an RFC 012D event
   exposes the same semantic reference. The aggregate negotiates compatible
   common reference/coverage versions across both boundaries before merging;
   incompatible selections fail rather than best-effort match.
2. `subject_refs` contains the relevant persistable base session/project
   references. For a session-scoped item, its session reference equals RFC
   012D `root.session_ref`; a presentation alias or aggregate cannot replace
   that base reference.
3. For durable/scoped overlay comparison, the durable result includes
   `FactFamily(version)` coverage compatible with the observer's fact-family
   coverage. `projection_status` separately proves whether the durable query
   pack has reduced/validated that fact coverage; projection-pack readiness
   cannot substitute for native fact-family coverage.
4. Rows, counts/facets, projection status, and coverage belong to the advertised
   committed snapshot. Every continuation uses that same snapshot and query
   fingerprint or returns `SnapshotExpired {latest_snapshot}`; a cursor cannot
   silently move to a newer commit.
5. `at_commit_seq` orders durable commits only. It cannot be compared with an
   observer sequence or native cursor to decide which item is newer.
6. A consumer deduplicates durable and ephemeral items by semantic revision
   reference. It may retire unmatched old overlay state only when complete
   compatible source/family coverage proves the durable reducer state subsumes
   that evidence, including required retractions or generation replacement.
7. `Partial`, `Unavailable`, incompatible-generation, or otherwise
   incomparable coverage cannot prove absence. The consumer retains or marks
   overlay state stale until direct semantic evidence or a complete replacement
   resolves it.
8. A durable invalidation identifies when a newer complete/declared result is
   available; it does not mutate a page already bound to an older snapshot.

This contract does not require one global native watermark. Coverage is a
driver-aware vector because append offsets, document revisions, source
database watermarks, and unrelated objects are not one ordinal.

## 6. Actor and affiliation model

### 6.1 Actor run

Runtime facts reference:

```text
ActorRunRef {
  root_session_key
  run_key
  role: Root | Child
  parent_run_key?
  native_session_id?
  native_agent_id?
  native_agent_type?
}
```

`run_key` is mandatory even for the root. A child may be a standard subagent,
workflow child, or team member without changing its `Child` role.

The root `run_key` is deterministically derived from the RFC 012A root
`SessionKey`, the `Root` role, and a support-release-declared native run
discriminator only when that discriminator is part of the pre-attach identity
input. Otherwise the contract uses a stable singleton-root discriminator and a
later native run ID is an attribute, not new key material. The final key must be
available before a scoped observer installs watches. A child key uses its stable
native run/agent/session identity under the same source instance; a declared
source-record fallback is allowed only when collision and replacement behavior
are fixture-tested.
Parentage and late affiliation are attributes, not key material. Ambiguous child
identity remains uncorrelated evidence and cannot be assigned to the root.

`ActorRunRef.run_key` identifies an evidence-backed native actor/execution
lineage inside the native session. It does not identify an operating-system
process, a Chopsticks host attachment, or a downstream `runtimeRunId`. Several
process-lifetime runs may resume the same native session while retaining one
Spaghetti root actor key; downstream systems correlate those runs through
their own opaque references and proven native-session claim.

### 6.2 Orthogonal affiliations

```text
ActorAffiliationRevision {
  actor_run_key
  affiliation_key
  revision_key
  team_key?
  native_team_id?
  team_name?
  member_key?
  workflow_key?
  native_workflow_id?
  state: Present | Removed | Unknown
  effective_at?
  provenance
}
```

Team and workflow affiliation can coexist. Neither is an actor kind. A late
affiliation revises grouping metadata for the same actor; it does not copy the
actor's messages, tasks, or usage into a new contribution identity.
`affiliation_key` identifies one actor/relation-dimension/target relation; a
team revision cannot overwrite a workflow relation or vice versa. The context
below is their deterministic union.

Reducers also expose a delivery/query context derived from accepted revisions:

```text
ActorAffiliationContext {
  actor_run_key
  team_key?
  native_team_id?
  team_name?
  member_key?
  workflow_key?
  native_workflow_id?
  completeness: Complete | Partial | Unknown
  derived_from_revision_keys[]
}
```

This context is not a second fact and does not alter another event's identity.
When no affiliation evidence exists, optional fields are absent and
`completeness` is `Unknown`. A late affiliation emits its own revision; it does
not cause unchanged message or usage revisions to be duplicated.

### 6.3 Activity and terminal state

Actor creation/discovery, native activity, explicit terminal evidence, and
correlation changes are revisioned separately. Silence or elapsed host time is
not terminal native truth. A transient assessment may label an actor idle, but
that assessment is not persisted as native evidence.

## 7. Usage-v2 contract

### 7.1 Revision shape

```text
UsageRevision {
  usage_key: {
    source_instance_key
    source_object
    generation
    response_key
  }
  revision_key: {
    usage_key
    source_revision
  }
  session_key
  actor_run_key
  native_message_id?
  request_id?
  buckets: {
    input_tokens: QualifiedValue<u64>
    output_tokens: QualifiedValue<u64>
    cache_creation_input_tokens: QualifiedValue<u64>
    cache_read_input_tokens: QualifiedValue<u64>
  }
  model?: QualifiedValue<ModelId>
  effort?: QualifiedValue<Effort>
  native_time?
  provenance
}
```

`input_tokens` means the normalized non-cache input bucket defined by the
support release. If an agent exposes only an inclusive aggregate that cannot be
split honestly, the adapter leaves incompatible common buckets `Unknown`,
reports degraded usage capability, and may retain the native aggregate in a
namespaced extension fact. It cannot relabel the aggregate as an exact common
bucket.

`usage_key` is the stable replaceable contribution identity. `revision_key` is
the ordered semantic revision identity used for correction events and event-ID
derivation. An evolving counter therefore updates one contribution with a new
revision/event ID; an exact repeated snapshot may be suppressed and does not
create a second contribution. For the common metadata law, `usage_key` is the
semantic entity key and the enclosing `revision_key` is the semantic revision
identity.

Optional `model` or `effort` means this usage record makes no assertion for that
dimension; it does not mean exact absence or reset an accepted effective-state
revision. If a record explicitly asserts that a value is unknown, it carries a
well-formed RFC 012A `QualifiedValue` with `quality = Unknown`.

### 7.2 Missing and zero

For every bucket:

- `{value: 0, quality: Exact}` is valid only when native schema semantics or
  fixture-backed decoder rules prove zero;
- an omitted field whose native meaning is not proven is `Unknown`;
- unsupported cache accounting is `Unknown`, not zero;
- a derived split is labeled `Derived` or `Estimated` and retains its method
  version in provenance; and
- an aggregate total over incomplete buckets carries partial/unknown
  completeness instead of silently summing known values as complete.

Canonical query results expose per-bucket value, quality, and coverage. A
consumer that requires exact four-bucket context accounting reports unknown
when required buckets are not exact enough for its policy.

### 7.3 Response identity

Rules:

1. For Claude, non-empty `message.id` is the primary `response_key`.
2. The key is scoped by source instance, source object, and generation.
3. `requestId` cannot be the sole key because it may be absent or shared by
   multiple message IDs.
4. Each adapter documents and fixture-tests a deterministic fallback when its
   preferred response ID is absent.
5. A fallback cannot merge unrelated responses because another metadata field
   happens to match.
6. Changing a fallback is a semantic-version change requiring replay.

### 7.4 Revision reduction

1. The first snapshot creates one response contribution.
2. A later snapshot for the same response replaces all four prior bucket
   values, qualities, and coverage in source-revision order.
3. Upward and downward revisions are both valid.
4. Exact repeated snapshots do not change totals and need not emit another
   semantic revision, although raw native records remain dispositioned.
5. A source-generation reset retracts every contribution owned solely by the
   old generation before corrected replay.
6. Conflicting out-of-order revisions produce a diagnostic and cannot replace a
   later accepted source revision.

### 7.5 Totals and affiliation

- One response revision contributes at most once to its actor and session.
- Root/session totals aggregate response keys, not row/event counts.
- Team-member and workflow-member totals are groupings over actor-affiliation
  revisions, not separately stored copies of usage.
- Late affiliation recomputes grouping from the same response keys and cannot
  change root/session totals.
- Removing affiliation removes the response from that grouping without
  retracting the actor's canonical contribution.
- An RFC 012B catalog canonical representative never rewrites `session_key` or
  response contribution identity. A query may explicitly aggregate the
  representative's disclosed member base keys, but scoped observation remains
  keyed by the selected native/base session.

## 8. Burn-rate and correction semantics

Spaghetti does not persist “burn rate” as native truth. It provides enough
ordered response snapshots, qualified buckets, effective/native time, and
delivery classification for a downstream consumer to calculate a rate.

When carried by RFC 012D:

- `Bootstrap` usage constructs the initial cumulative baseline;
- resync or source-replay `Correction` replaces or adjusts that baseline;
- neither phase is an instantaneous consumption sample;
- only post-barrier `Live` changes are eligible for a consumer's burn-rate
  window;
- a downward revision corrects the baseline rather than representing negative
  consumption; and
- affiliation-only revisions regroup existing contributions and produce no
  consumption sample.

A consumer may update displayed cumulative totals after a correction, but it
cannot attribute the corrected difference to the observer's delivery time.

## 9. Effective runtime state

State dimensions are independent:

```text
EffectiveStateRevision<T> {
  session_key
  actor_run_key
  dimension: Model | Effort | SessionMode | PermissionMode
  revision_key
  value: QualifiedValue<T>
  evidence_kind: ConfiguredIntent | ResponseObserved | NativeTransition
  native_time?
  provenance
}
```

Rules:

- a model/effort value on a response proves it was effective for that response;
  it is not an instantaneous change notification;
- a launch or settings value is configured intent until runtime evidence
  confirms it;
- a native transition may establish an effective boundary at its native time;
- absence of evidence produces unknown, not inherited global default; and
- session mode and permission mode remain separate dimensions.

Spaghetti returns model ID and usage evidence. A downstream model-capacity
catalog owns context-window percentage and displays unknown when capacity or
required bucket evidence is unavailable.

## 10. Messages, content, plans, tasks, tools, and progress

### 10.1 Messages and content blocks

RFC 012C retains and versions RFC 011's common message/content semantics:

```text
MessageRevision {
  message_key
  revision_key
  session_key
  actor_run_key
  role_or_kind
  ordered_content_block_keys[]
  parent_or_turn_key?
  completeness
  native_time?
  provenance
}

ContentBlockRevision {
  content_block_key
  revision_key
  message_key
  ordinal
  kind
  bounded_typed_content_or_extension
  native_tool_call_or_result_id?
  completeness
  provenance
}
```

Both are `CurrentGenerationLog` families. A native message ID is primary when
stable; otherwise RFC 012A source-record identity plus a deterministic semantic
subkey is used. A correction replaces the same message/block entity rather than
appending a duplicate. A complete ordered block snapshot retracts blocks absent
from its replacement; a partial block list cannot prove absence. Source reset or
confirmed deletion removes old-generation-owned entries before replay.

### 10.2 Plans

`PlanRevision` carries stable plan key, owner actor, revision key, ordered
steps/status, native/effective time, completeness, and provenance. A complete
snapshot replaces prior plan state; tool lifecycle evidence may provide a
lower-latency provisional revision that later transcript evidence corrects.
Plans are `RevisionedEntity`; explicit removal or removal from a complete owned
plan set retracts the entity. Omission from partial evidence does not.

### 10.3 Tasks

`TaskRevision` carries stable task key, owner actor, parent/dependency keys when
native, revision key, normalized lifecycle state, native/effective time,
completeness, operation, and provenance. Tasks are `RevisionedEntity` when
individually sourced and `OwnedSetSnapshot` when supplied by a complete task-list
document. Created, updated, completed, failed, cancelled, and removed require
evidence; silence is not completion and a partial list cannot retract a task.

### 10.4 Tools

Tool call and result facts retain native call/result identifiers, tool name,
bounded typed/common content where modeled, native payload evidence according
to policy, actor/run attribution, and correlation state. An unmatched result is
retained as unmatched evidence rather than discarded. Calls and results are
separate `CorrelatedLifecycle` entities with their own revision keys. Correlation
updates relationships without changing either entity key; object deletion or
source reset follows declared ownership retraction.

A file path or shell/Git command inside a tool payload proves only the typed
tool evidence declared by the support release. It does not by itself prove a
filesystem mutation, commit creation, final repository state, retained code,
or contribution. A future `code.activity` family must distinguish intent,
observed tool result, and repository/workspace authority; retain session,
actor, tool, path-policy, quality, completeness, and provenance; and pass a
separate corpus/conformance amendment before entering common queries.

### 10.5 Compaction and progress

Compaction, progress, queue, and comparable native markers are revisioned
events with quality and provenance. Host-derived heuristics use a distinct
assessment family and cannot masquerade as native transitions.
Native markers are `CurrentGenerationLog`; a correction replaces the same
marker identity, and source ownership controls reset/deletion. Transient
host-derived assessments are not included in a native full-snapshot claim.

## 11. Structured user-input requests

```text
UserInputRequestRevision {
  interaction_key
  revision_key
  session_key
  actor_run_key
  native_tool_use_id
  kind: Choice | MultiChoice | FreeText | Mixed
  questions[] {
    question_key?
    header?
    prompt
    options[] {
      label
      description?
      preview?
    }
    multi_select
  }
  state: Pending | Resolved | Failed | Cancelled
  operation: Upsert | Retract
  completeness
  result_reference?
  native_time?
  provenance
}
```

For Claude, `AskUserQuestion` opens `Pending`; its correlated `tool_result`
resolves or fails the interaction. Cancellation requires native evidence or an
unambiguous terminal boundary. Permission requests remain a separate family.
Interactions are `CorrelatedLifecycle` entities. Repeated evidence for the same
tool-use/state revision is idempotent; a later result revises the existing
interaction. Removal from a complete owned interaction snapshot or an explicit
source retraction removes it. Partial evidence and silence do not resolve,
cancel, or retract it.

Within one revision, `questions` and each question's `options` are complete only
when `completeness` proves it. A partial revision can enrich known fields but
cannot remove a previously known question or option.

`header`, `label`, `description`, and `preview` are normalized typed strings
when native evidence supplies them. A supported consumer does not parse raw
native payloads to render question choices. Raw evidence remains available
under RFC 012D policy for drift and forward compatibility.

## 12. Reducer-family matrix and capability quality

The observation/durable reducer output and RFC 012D replacement representation
are fixed by family:

| Family                            | Reducer class                                     | Complete replacement representation                                                               |
| --------------------------------- | ------------------------------------------------- | ------------------------------------------------------------------------------------------------- |
| Actor run/activity                | `RevisionedEntity`                                | every current in-scope actor plus its accepted current lifecycle/correlation state                |
| Affiliation                       | `RevisionedEntity`                                | every current affiliation entity plus derived `ActorAffiliationContext`                           |
| Message/content block             | `CurrentGenerationLog`                            | every unretracted current-generation message/block through the watermark in stable semantic order |
| Usage                             | `UsageContribution`                               | latest accepted qualified revision for every unretracted response key                             |
| Effective state                   | `RevisionedEntity`                                | current qualified value/evidence for every supported actor/dimension, including explicit unknown  |
| Plan                              | `RevisionedEntity` or declared `OwnedSetSnapshot` | every current plan and ordered state                                                              |
| Task                              | `RevisionedEntity` or declared `OwnedSetSnapshot` | every current task and lifecycle/dependency state                                                 |
| Tool call/result                  | `CorrelatedLifecycle`                             | every current call/result entity and current correlation state                                    |
| User-input request                | `CorrelatedLifecycle`                             | every current interaction and lifecycle state                                                     |
| Native compaction/progress marker | `CurrentGenerationLog`                            | every unretracted current-generation marker through the watermark                                 |
| Unknown native evidence           | bounded `CurrentGenerationLog` plus aggregation   | retained bounded samples and exact aggregate counts/digests required by policy                    |

“Current” means reduced at the declared native/source watermark, not observed at
consumer wall-clock time. A replacement may serialize canonical reduced
snapshots instead of every losing candidate revision, provided it preserves the
winning value, quality, completeness, provenance, semantic revision identity,
and enough reducer state in the engine to emit a deterministic fallback if the
winner is later retracted. An absent entity may be semantically retracted only
when family/scope coverage is complete or by explicit retraction. RFC 012D may
instead remove prior ephemeral state when replacement coverage marks its owning
object unavailable, but it substitutes explicit unavailable/error state and
does not claim native deletion.

Every support release declares which reducer class and replacement
representation it uses for any family that permits more than one class above.
Changing that choice is a fact-family semantic-version change.

Bounded unknown-evidence sampling is deterministic over semantic/source
identity, not first-arrival or scheduling order. Aggregate counts and digests
cover the complete observed set through the watermark even when only bounded
samples are transported.

For each fact family and support release, capabilities report:

```text
Supported | Degraded | Unsupported
```

with evidence source, quality, expected timing, completeness constraints, and
known limitations. Examples:

- response-observed model: supported but not instantaneous;
- effort only from launch settings: degraded configured intent;
- usage without cache separation: degraded qualified buckets;
- absent native tasks: unsupported, not an empty exact task list.

RFC 012A compatibility output constrains these states. Exact/range support may
publish the declared result; recognized-unverified and incompatible artifacts
cannot publish typed runtime support because RFC 012A disables runtime decoding.
This capability mapping is distinct from RFC 012B catalog readiness.

Unknown fields and record families remain bounded native evidence with mapping
dispositions from RFC 012A. Adding a typed projection cannot erase that
evidence.

## 13. Durable usage-v2 migration

`usage-v2` has independent fact, projection, query, and migration versions.
Migration proceeds as follows:

1. preserve the legacy usage projection and query path;
2. build a shadow response-revision projection from source facts;
3. keep the v2 usage pack non-ready during replay;
4. compare response identities, latest qualified buckets, actor/session totals,
   and coverage to an independent frozen-corpus oracle;
5. test exact repeats, evolving counters, downward corrections, missing
   `requestId`, reused `requestId`, affiliation delay, and generation reset;
6. switch the versioned usage query in one transaction; and
7. retain the legacy projection through the compatibility window so rollback
   does not require reparsing or database deletion.

The independent oracle groups native records without importing adapter code,
selects the last complete response revision in the current generation, and
computes bucket quality/coverage as well as values. History, capability, and
FTS remain compared to their existing accepted oracles; usage is deliberately
compared to the new v2 oracle.

Current implementation status (2026-08-16): steps 1 and 2 have landed as a
non-public shadow. Claude decoder contract 17 emits a canonical
`runtime.usage-v2` fact beside the unchanged legacy delta. The fact uses
non-empty `message.id` first, an object/generation/source-record fallback when
it is absent, canonical session and actor-run keys, independently qualified
buckets, optional model/effort assertions, and an RFC 012A semantic revision.
Schema v46 interns qualification evidence and retains one source-ordered latest
revision per response; later snapshots, including downward corrections,
replace the prior row and a generation reset retracts the old namespace.

Focused conformance proves topology-independent identities, exact-repeat
non-duplication, evolving and downward counters, exact zero, missing buckets,
absent and reused `requestId`, actor/session grouping, malformed-snapshot
non-erasure, and generation replacement. The legacy projection remains the
only public usage path and intentionally retains its old row-additive result.
Steps 3 through 7 remain open: a frozen-corpus oracle independent of the
adapter, qualification/coverage comparison at corpus scale, affiliation
regrouping, readiness/replay orchestration, public query serialization and
atomic selection, and the compatibility/rollback window.

## 14. Failure and correction semantics

- A malformed usage snapshot does not erase the latest valid contribution; it
  emits bounded diagnostic evidence.
- An unsupported/missing bucket degrades only the affected bucket and derived
  aggregates.
- Reset retracts old-generation contributions before replay.
- Ambiguous actor attribution prevents contribution to actor/team/workflow
  totals until corrected; it cannot be assigned to the root silently.
- A late actor correction moves the same response contribution rather than
  copying it.
- Interaction/tool/task object deletion follows the section 5 law and section
  12 family matrix; partial evidence cannot fabricate disappearance, while
  explicit deletion or complete owned-set absence retracts with provenance.
- Unknown record growth triggers drift telemetry, not unbounded public events.

## 15. Rejected alternatives

### 15.1 Add every Claude usage row

Rejected by repeat and evolving-counter evidence.

### 15.2 Use `requestId` as response identity

Rejected because it is sometimes absent and sometimes shared across message
IDs.

### 15.3 Treat missing token buckets as zero

Rejected because it violates qualified-value semantics and creates false exact
totals for agents with incomplete native accounting.

### 15.4 Encode team/workflow as actor kinds

Rejected because one child actor can have both affiliations and affiliation can
arrive after usage.

### 15.5 Make Godview parse native tool payloads

Rejected because typed runtime semantics would no longer be a common contract
and native parsing would escape the adapter boundary.

### 15.6 Infer Git or contribution truth from transcript commands

Rejected because a recorded command or tool call does not prove the resulting
workspace/ref state, accepted branch, retained code, exclusive authorship, or
product contribution. Those require workspace/repository evidence and
downstream policy outside this RFC.

## 16. Conformance and acceptance

Release-blocking fixtures cover:

- compatible semantic-reference/coverage/query-pack selection and incompatible
  aggregate-facing rejection;
- exact repeat, evolving snapshot, upward and downward revision;
- missing response ID fallback, absent `requestId`, and reused `requestId`;
- exact zero, omitted/unknown bucket, derived bucket, and incomplete aggregate;
- reset/retract and old-generation exclusion;
- deterministic root run key before source creation and stable child keys under
  late parent/affiliation correlation;
- root, standard child, workflow child, and team-member attribution;
- late affiliation, affiliation removal, and ambiguous actor correction without
  duplicate contribution;
- bootstrap/correction baseline versus live consumption classification;
- response-observed model/effort and native state transitions;
- configured intent kept distinct from effective state;
- message/content correction, complete-block replacement, partial-block
  non-retraction, and source reset;
- plan/task lifecycle, unmatched tools, compaction/progress, and unknown facts;
- question open/resolved/failed/cancelled with typed option fields;
- explicit deletion versus partial omission for every section 12 family;
- equal semantic revision references in durable query and observer forms;
- equal external base-session references in catalog, durable runtime query, and
  scoped observer forms;
- durable runtime/history continuation pinned to one snapshot, explicit
  expiration, and coherent projection status/source coverage;
- live/durable overlay deduplication, direct absorption, coverage-based
  retirement, omitted-object/partial/incomparable coverage, reset, and
  retraction;
- process-lifetime runs remaining distinct from Spaghetti actor-run identity;
- equal durable/observation reduced-state and complete-replacement digests at
  the same comparable RFC 012A source/family coverage vector; and
- identical semantic serialization across Rust, N-API, and TypeScript facades.

RFC 012C is complete when usage-v2 matches the independent qualified-bucket
oracle at response, actor, session, affiliation-group, and aggregate scope;
legacy rollback remains available; unknown is never reported as zero; typed
runtime facts pass downstream scenarios; every supported family can produce an
exact complete-replacement state; and neither durable nor ephemeral delivery
introduces duplicate semantic contributions.
